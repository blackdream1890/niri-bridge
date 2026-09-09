// SPDX-License-Identifier: GPL-3.0-or-later
//! Reads selected physical inputs for takeover and keyboard forwarding while capture is active.
use crate::{input::PhysicalKeys, protocol::InputEvent};
use anyhow::{Context, Result, ensure};
use evdev::{Device, EventType};
use std::{collections::BTreeSet, fs::File, io, path::PathBuf};

struct Slot {
    path: PathBuf,
    device: Option<Device>,
}
pub struct ActivityMonitor {
    slots: Vec<Slot>,
    keyboard: PhysicalKeys,
    native_touchpads: bool,
    touchpads: Option<crate::touchpad::Router>,
    touchpad_profiles: Option<Vec<crate::touchpad::Descriptor>>,
}
pub trait ActivitySource {
    fn wait_input(&self) -> impl std::future::Future<Output = Result<()>> {
        std::future::pending()
    }
    fn sample(&mut self) -> Result<bool>;
    fn suspend(&mut self) {}
    /// None is for backends such as isolated fixtures that use Wayland keyboard events.
    fn keyboard_state(&self) -> Option<BTreeSet<u16>> {
        None
    }
    fn take_keyboard_events(&mut self) -> Vec<InputEvent> {
        Vec::new()
    }
    fn prepare_touchpads(&mut self) -> Result<Vec<crate::touchpad::Descriptor>> {
        Ok(Vec::new())
    }
    fn route_touchpads(&mut self, _remote: bool) -> Result<Vec<InputEvent>> {
        Ok(Vec::new())
    }
    fn take_touchpad_events(&mut self) -> Vec<InputEvent> {
        Vec::new()
    }
}
impl ActivitySource for ActivityMonitor {
    async fn wait_input(&self) -> Result<()> {
        if let Some(router) = &self.touchpads {
            router.wait_ready().await
        } else {
            std::future::pending().await
        }
    }
    fn sample(&mut self) -> Result<bool> {
        ActivityMonitor::sample(self)
    }
    fn suspend(&mut self) {
        for slot in &mut self.slots {
            slot.device = None;
        }
        self.keyboard = PhysicalKeys::default();
        self.touchpads = None;
    }
    fn keyboard_state(&self) -> Option<BTreeSet<u16>> {
        Some(self.keyboard.held())
    }
    fn take_keyboard_events(&mut self) -> Vec<InputEvent> {
        self.keyboard.drain()
    }
    fn prepare_touchpads(&mut self) -> Result<Vec<crate::touchpad::Descriptor>> {
        if !self.native_touchpads {
            return Ok(Vec::new());
        }
        let paths = self
            .slots
            .iter()
            .map(|s| s.path.clone())
            .collect::<Vec<_>>();
        let router = crate::touchpad::Router::new(&paths)?;
        let profiles = router.descriptors();
        self.touchpad_profiles = Some(profiles.clone());
        self.touchpads = Some(router);
        Ok(profiles)
    }
    fn route_touchpads(&mut self, remote: bool) -> Result<Vec<InputEvent>> {
        Ok(match self.touchpads.as_mut() {
            Some(router) => router
                .route(remote)?
                .into_iter()
                .map(|(device, time_us, events)| InputEvent::Touchpad {
                    device,
                    time_us,
                    events,
                })
                .collect(),
            None => Vec::new(),
        })
    }
    fn take_touchpad_events(&mut self) -> Vec<InputEvent> {
        self.touchpads
            .as_mut()
            .map(|r| {
                r.drain()
                    .into_iter()
                    .map(|(device, time_us, events)| InputEvent::Touchpad {
                        device,
                        time_us,
                        events,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl ActivityMonitor {
    pub fn new(paths: &[PathBuf], native_touchpads: bool) -> Result<Self> {
        ensure!(
            !paths.is_empty() && paths.len() <= 32,
            "Configure between one and 32 physical activity devices"
        );
        Ok(Self {
            slots: paths
                .iter()
                .map(|path| Slot {
                    path: path.clone(),
                    device: None,
                })
                .collect(),
            keyboard: PhysicalKeys::default(),
            native_touchpads,
            touchpads: None,
            touchpad_profiles: None,
        })
    }

    pub fn sample(&mut self) -> Result<bool> {
        let mut active = false;
        if self.native_touchpads && self.touchpads.is_none() && self.touchpad_profiles.is_some() {
            let previous = self.touchpad_profiles.clone().unwrap();
            let next = self.prepare_touchpads()?;
            ensure!(
                previous == next,
                "Touchpad capabilities changed; reconnect to renegotiate them"
            );
        }
        if let Some(pads) = &mut self.touchpads {
            active |= pads.sample()?;
        }
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.device.is_none() {
                let file = match File::open(&slot.path) {
                    Ok(file) => file,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(_) => anyhow::bail!(
                        "Cannot read a configured physical input device; check its permissions"
                    ),
                };
                let device = Device::from_fd(file.into())
                    .context("Configured activity path is not an input device")?;
                ensure!(
                    !device
                        .physical_path()
                        .is_some_and(|p| p.starts_with("niri-bridge/"))
                        && device.name() != Some(crate::uinput::DEVICE_NAME),
                    "A NiriBridge virtual device cannot be used for physical takeover detection"
                );
                device.set_nonblocking(true)?;
                for code in device.get_key_state()?.iter() {
                    self.keyboard.update(index, code.code(), true)?;
                }
                slot.device = Some(device);
            }
            let disconnected = {
                let result = slot.device.as_mut().unwrap().fetch_events();
                match result {
                    Ok(events) => {
                        for event in events {
                            if event.event_type() == EventType::KEY && event.value() != 2 {
                                self.keyboard
                                    .update(index, event.code(), event.value() != 0)?;
                            }
                            active |= match event.event_type() {
                                EventType::KEY | EventType::ABSOLUTE => true,
                                EventType::RELATIVE => event.value() != 0,
                                _ => false,
                            };
                        }
                        false
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
                    Err(error) if error.raw_os_error() == Some(libc::ENODEV) => true,
                    Err(_) => anyhow::bail!("Physical input activity monitoring failed"),
                }
            };
            if disconnected {
                slot.device = None;
                self.keyboard.disconnect(index)?;
            }
        }
        Ok(active)
    }
}
