// SPDX-License-Identifier: GPL-3.0-or-later
//! Pointer injection through Niri's supported Wayland protocol.
use crate::{
    protocol::{Axis, InputEvent, ScrollSource},
    receiver::InputSink,
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    os::{fd::AsRawFd, unix::net::UnixStream},
    path::{Path, PathBuf},
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_output, wl_pointer, wl_registry, wl_seat},
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1 as manager, zwlr_virtual_pointer_v1 as pointer,
};

#[derive(Default)]
struct State {
    outputs: BTreeMap<String, wl_output::WlOutput>,
    transforms: BTreeMap<u32, wl_output::Transform>,
}

pub struct Pointer {
    socket: PathBuf,
    connection: Connection,
    pointer: pointer::ZwlrVirtualPointerV1,
    queue: wayland_client::EventQueue<State>,
    state: State,
    output_id: u32,
}

impl Pointer {
    pub fn connect(socket: &Path, output_name: &str) -> Result<Self> {
        let connection = Connection::from_socket(UnixStream::connect(socket)?)?;
        let (globals, mut queue) = registry_queue_init::<State>(&connection)?;
        let qh = queue.handle();
        let mut state = State::default();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=9, ())?;
        let manager: manager::ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 2..=2, ())?;
        for global in globals
            .contents()
            .clone_list()
            .iter()
            .filter(|g| g.interface == "wl_output" && g.version >= 4)
        {
            globals
                .registry()
                .bind::<wl_output::WlOutput, _, _>(global.name, 4, &qh, ());
        }
        queue.roundtrip(&mut state)?;
        let output = state
            .outputs
            .get(output_name)
            .context("Target output is not available")?;
        let pointer =
            manager.create_virtual_pointer_with_output(Some(&seat), Some(output), &qh, ());
        let output_id = output.id().protocol_id();
        connection.flush()?;
        Ok(Self {
            socket: socket.to_path_buf(),
            connection,
            pointer,
            queue,
            state,
            output_id,
        })
    }
}

impl InputSink for Pointer {
    fn select_output(&mut self, output: &str) -> Result<()> {
        // The virtual-pointer output binding is immutable. Reconnect only at a
        // boundary placement, after held input has been released, to also pick
        // up outputs that were unplugged and reconnected under the same name.
        let next = Self::connect(&self.socket, output)?;
        *self = next;
        Ok(())
    }
    fn emit(&mut self, event: &InputEvent) -> Result<()> {
        event.validate()?;
        self.queue.dispatch_pending(&mut self.state)?;
        if let Some(guard) = self.queue.prepare_read() {
            let mut fd = libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut fd, 1, 0) } > 0 {
                guard.read()?;
            }
        }
        self.queue.dispatch_pending(&mut self.state)?;
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        ensure!(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } == 0,
            "Could not read the input event clock"
        );
        let time = ((now.tv_sec as u64) * 1000 + (now.tv_nsec as u64) / 1_000_000) as u32;
        match *event {
            InputEvent::Key { .. } | InputEvent::Touchpad { .. } => {
                anyhow::bail!("Pointer backend received a non-pointer event")
            }
            InputEvent::Button { code, pressed } => self.pointer.button(
                time,
                u32::from(code),
                if pressed {
                    wl_pointer::ButtonState::Pressed
                } else {
                    wl_pointer::ButtonState::Released
                },
            ),
            InputEvent::Motion { dx, dy } => self.pointer.motion(time, dx, dy),
            InputEvent::Absolute {
                x,
                y,
                width,
                height,
            } => {
                let transform = *self
                    .state
                    .transforms
                    .get(&self.output_id)
                    .context("Output transform is unavailable")?;
                let (nx, ny) = inverse_normalized(
                    transform,
                    f64::from(x) / f64::from(width),
                    f64::from(y) / f64::from(height),
                )?;
                const EXTENT: u32 = 1 << 24;
                self.pointer.motion_absolute(
                    time,
                    (nx * f64::from(EXTENT)).round() as u32,
                    (ny * f64::from(EXTENT)).round() as u32,
                    EXTENT,
                    EXTENT,
                );
            }
            InputEvent::Scroll {
                axis,
                amount,
                source,
            } => {
                self.pointer.axis(time, axis.wayland(), amount);
                self.pointer.axis_source(source.wayland());
            }
            InputEvent::ScrollStop { axis, source } => {
                self.pointer.axis_stop(time, axis.wayland());
                self.pointer.axis_source(source.wayland());
            }
        }
        self.pointer.frame();
        self.connection.flush()?;
        Ok(())
    }
}

impl Drop for Pointer {
    fn drop(&mut self) {
        self.pointer.destroy();
        let _ = self.connection.flush();
    }
}

impl Axis {
    fn wayland(self) -> wl_pointer::Axis {
        match self {
            Self::Horizontal => wl_pointer::Axis::HorizontalScroll,
            Self::Vertical => wl_pointer::Axis::VerticalScroll,
        }
    }
}
impl ScrollSource {
    fn wayland(self) -> wl_pointer::AxisSource {
        match self {
            Self::Wheel => wl_pointer::AxisSource::Wheel,
            Self::Finger => wl_pointer::AxisSource::Finger,
            Self::Continuous => wl_pointer::AxisSource::Continuous,
        }
    }
}

pub struct Hybrid {
    pointer: Pointer,
    keyboard: crate::uinput::Keyboard,
    touchpads: Vec<crate::touchpad::VirtualTouchpad>,
}

impl Hybrid {
    pub fn connect(socket: &Path, output_name: &str) -> Result<Self> {
        let pointer = Pointer::connect(socket, output_name)?;
        let keyboard = crate::uinput::Keyboard::create()?;
        Ok(Self {
            pointer,
            keyboard,
            touchpads: Vec::new(),
        })
    }
}
impl InputSink for Hybrid {
    fn select_output(&mut self, output: &str) -> Result<()> {
        self.pointer.select_output(output)
    }
    fn emit(&mut self, event: &InputEvent) -> Result<()> {
        match event {
            InputEvent::Touchpad {
                device,
                time_us,
                events,
            } => self
                .touchpads
                .get_mut(*device as usize)
                .context("Touchpad was not negotiated")?
                .emit_remote(events, *time_us),
            InputEvent::Key { .. } => self.keyboard.emit(event),
            _ => self.pointer.emit(event),
        }
    }
    fn configure_touchpads(&mut self, devices: &[crate::touchpad::Descriptor]) -> Result<()> {
        ensure!(
            devices.len() <= 4 && self.touchpads.is_empty(),
            "Touchpad negotiation may occur only once"
        );
        self.touchpads = devices
            .iter()
            .map(crate::touchpad::VirtualTouchpad::create)
            .collect::<Result<_>>()?;
        Ok(())
    }
    fn reset_touchpads(&mut self) -> Result<()> {
        for pad in &mut self.touchpads {
            pad.reset()?;
        }
        Ok(())
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_output::Event::Name { name } => {
                state.outputs.insert(name, proxy.clone());
            }
            wl_output::Event::Geometry {
                transform: WEnum::Value(transform),
                ..
            } => {
                state.transforms.insert(proxy.id().protocol_id(), transform);
            }
            _ => {}
        }
    }
}
delegate_noop!(State:ignore wl_seat::WlSeat);
delegate_noop!(State:ignore manager::ZwlrVirtualPointerManagerV1);
delegate_noop!(State:ignore pointer::ZwlrVirtualPointerV1);

fn inverse_normalized(transform: wl_output::Transform, x: f64, y: f64) -> Result<(f64, f64)> {
    use wl_output::Transform::*;
    Ok(match transform {
        Normal => (x, y),
        wl_output::Transform::_90 => (y, 1.0 - x),
        wl_output::Transform::_180 => (1.0 - x, 1.0 - y),
        wl_output::Transform::_270 => (1.0 - y, x),
        Flipped => (1.0 - x, y),
        Flipped90 => (y, x),
        Flipped180 => (x, 1.0 - y),
        Flipped270 => (1.0 - y, 1.0 - x),
        _ => anyhow::bail!("Unsupported output transform"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inverse_mapping_preserves_logical_positions_on_rotated_and_flipped_outputs() {
        use wl_output::Transform::*;
        for transform in [
            Normal, _90, _180, _270, Flipped, Flipped90, Flipped180, Flipped270,
        ] {
            let (x, y) = inverse_normalized(transform, 0.2, 0.7).unwrap();
            let point = match transform {
                Normal => (x, y),
                wl_output::Transform::_90 => (1.0 - y, x),
                wl_output::Transform::_180 => (1.0 - x, 1.0 - y),
                wl_output::Transform::_270 => (y, 1.0 - x),
                Flipped => (1.0 - x, y),
                Flipped90 => (y, x),
                Flipped180 => (x, 1.0 - y),
                Flipped270 => (1.0 - y, 1.0 - x),
                _ => unreachable!(),
            };
            assert!((point.0 - 0.2).abs() < 1e-12 && (point.1 - 0.7).abs() < 1e-12);
        }
    }
}
