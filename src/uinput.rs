// SPDX-License-Identifier: GPL-3.0-or-later
//! Kernel keyboard backend. Construction creates a device and must be an explicit operation.
use anyhow::{Context, Result, ensure};
use evdev::{
    AttributeSet, BusType, EventType, InputEvent as KernelEvent, InputId, KeyCode,
    uinput::VirtualDevice,
};

use crate::{protocol::InputEvent, receiver::InputSink};

pub const DEVICE_NAME: &str = "NiriBridge Virtual Keyboard";
pub const DEVICE_PHYS: &std::ffi::CStr = c"niri-bridge/keyboard";

pub struct Keyboard {
    device: VirtualDevice,
}

impl Keyboard {
    pub fn create() -> Result<Self> {
        let keys: AttributeSet<KeyCode> = (1..=255).map(KeyCode::new).collect();
        let mut device = VirtualDevice::builder()
            .context("Cannot open uinput; reviewed device permission setup is required")?
            .name(DEVICE_NAME)
            .input_id(InputId::new(BusType::BUS_VIRTUAL, 0, 0, 1))
            .with_phys(DEVICE_PHYS)
            .context("Could not tag the virtual keyboard")?
            .with_keys(&keys)
            .context("Could not configure virtual keyboard keys")?
            .build()
            .context("Could not create the virtual keyboard")?;
        wait_for_niri(&mut device)?;
        Ok(Self { device })
    }
}

impl InputSink for Keyboard {
    fn emit(&mut self, event: &InputEvent) -> Result<()> {
        event.validate()?;
        let InputEvent::Key { code, pressed } = event else {
            anyhow::bail!("Keyboard backend received a pointer event");
        };
        ensure!((1..=255).contains(code), "Unsupported keyboard code");
        self.device.emit(&[KernelEvent::new(
            EventType::KEY.0,
            *code,
            i32::from(*pressed),
        )])?;
        Ok(())
    }
}

/// Wait until the compositor has consumed the udev addition for an input device.
pub(crate) fn wait_for_niri(device: &mut VirtualDevice) -> Result<()> {
    let pid = crate::niri::compositor_pid()?;
    let syspath = device.get_syspath()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let nodes: Vec<_> = std::fs::read_dir(&syspath)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("event"))
            .map(|entry| std::path::Path::new("/dev/input").join(entry.file_name()))
            .collect();
        let opened = std::fs::read_dir(format!("/proc/{pid}/fd"))
            .context("Cannot verify whether Niri opened the virtual input device")?
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_link(entry.path()).ok())
            .any(|path| nodes.contains(&path));
        if opened {
            // Round-trip through Niri after hotplug processing, before permitting injection.
            crate::niri::query("Version")?;
            break;
        }
        ensure!(
            std::time::Instant::now() < deadline,
            "Niri did not open the virtual input device in time"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Ok(())
}
