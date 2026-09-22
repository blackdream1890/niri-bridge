// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal ownership wrapper around the stable libei 1.x sender API.
//! API source: https://libinput.pages.freedesktop.org/libei/api/group__libei.html
//! No event payloads or library diagnostics are logged.
use crate::{
    desktop::{self, LogicalOutput},
    protocol::{Axis, InputEvent},
    receiver::InputSink,
};
use anyhow::{Context, Result, ensure};
use std::{
    ffi::{c_char, c_int, c_void},
    os::fd::{IntoRawFd, OwnedFd},
    ptr::NonNull,
    time::{Duration, Instant},
};
type Raw = c_void;
const POINTER: c_int = 1;
const ABSOLUTE: c_int = 2;
const SCROLL: c_int = 16;
const BUTTON: c_int = 32;
// libei is a system dependency; SONAME linking also works without development headers.
#[link(name = "libei.so.1", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn ei_new_sender(data: *mut c_void) -> *mut Raw;
    fn ei_unref(ei: *mut Raw) -> *mut Raw;
    fn ei_configure_name(ei: *mut Raw, name: *const c_char);
    fn ei_log_set_handler(
        ei: *mut Raw,
        handler: Option<unsafe extern "C" fn(*mut Raw, c_int, *const c_char, *mut c_void)>,
    );
    fn ei_setup_backend_fd(ei: *mut Raw, fd: c_int) -> c_int;
    fn ei_get_fd(ei: *mut Raw) -> c_int;
    fn ei_dispatch(ei: *mut Raw);
    fn ei_get_event(ei: *mut Raw) -> *mut Raw;
    fn ei_event_unref(event: *mut Raw) -> *mut Raw;
    fn ei_event_get_type(event: *mut Raw) -> c_int;
    fn ei_event_get_seat(event: *mut Raw) -> *mut Raw;
    fn ei_event_get_device(event: *mut Raw) -> *mut Raw;
    fn ei_seat_bind_capabilities(seat: *mut Raw, ...);
    fn ei_device_ref(device: *mut Raw) -> *mut Raw;
    fn ei_device_unref(device: *mut Raw) -> *mut Raw;
    fn ei_device_has_capability(device: *mut Raw, cap: c_int) -> bool;
    fn ei_device_start_emulating(device: *mut Raw, sequence: u32);
    fn ei_device_stop_emulating(device: *mut Raw);
    fn ei_device_frame(device: *mut Raw, time: u64);
    fn ei_now(ei: *mut Raw) -> u64;
    fn ei_device_pointer_motion(device: *mut Raw, x: f64, y: f64);
    fn ei_device_pointer_motion_absolute(device: *mut Raw, x: f64, y: f64);
    fn ei_device_button_button(device: *mut Raw, button: u32, pressed: bool);
    fn ei_device_scroll_delta(device: *mut Raw, x: f64, y: f64);
    fn ei_device_scroll_stop(device: *mut Raw, x: bool, y: bool);
    fn ei_device_get_region_at(device: *mut Raw, x: f64, y: f64) -> *mut Raw;
}
struct Device {
    ptr: NonNull<Raw>,
    active: bool,
}
impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            if self.active {
                ei_device_stop_emulating(self.ptr.as_ptr());
            }
            ei_device_unref(self.ptr.as_ptr());
        }
    }
}
unsafe extern "C" fn silent_log(_: *mut Raw, _: c_int, _: *const c_char, _: *mut c_void) {}
pub struct Pointer {
    devices: Vec<Device>,
    context: NonNull<Raw>,
    disconnected: bool,
    output: LogicalOutput,
}
impl Pointer {
    pub fn connect(fd: OwnedFd, output: &str) -> Result<Self> {
        let output = desktop::outputs()?
            .remove(output)
            .and_then(|o| o.logical)
            .context("Selected output unavailable")?;
        let context = NonNull::new(unsafe { ei_new_sender(std::ptr::null_mut()) })
            .context("Cannot initialize desktop input")?;
        let mut this = Self {
            devices: Vec::new(),
            context,
            disconnected: false,
            output,
        };
        unsafe {
            ei_log_set_handler(context.as_ptr(), Some(silent_log));
            ei_configure_name(context.as_ptr(), c"NiriBridge".as_ptr());
            ensure!(
                ei_setup_backend_fd(context.as_ptr(), fd.into_raw_fd()) == 0,
                "Cannot connect desktop input"
            );
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            this.poll()?;
            if [POINTER, ABSOLUTE, SCROLL, BUTTON]
                .into_iter()
                .all(|c| this.device(c).is_ok())
            {
                return Ok(this);
            }
            ensure!(
                Instant::now() < deadline,
                "Desktop did not provide pointer devices in time"
            );
            let mut poll = libc::pollfd {
                fd: unsafe { ei_get_fd(context.as_ptr()) },
                events: libc::POLLIN,
                revents: 0,
            };
            unsafe {
                libc::poll(&mut poll, 1, 10);
            }
        }
    }
    fn device(&self, cap: c_int) -> Result<*mut Raw> {
        self.devices
            .iter()
            .find(|d| d.active && unsafe { ei_device_has_capability(d.ptr.as_ptr(), cap) })
            .map(|d| d.ptr.as_ptr())
            .context("Desktop input is paused or unavailable")
    }
    pub fn poll(&mut self) -> Result<()> {
        unsafe {
            ei_dispatch(self.context.as_ptr());
            loop {
                let event = ei_get_event(self.context.as_ptr());
                if event.is_null() {
                    break;
                }
                let kind = ei_event_get_type(event);
                let ptr = if (5..=8).contains(&kind) {
                    ei_event_get_device(event)
                } else {
                    std::ptr::null_mut()
                };
                match kind {
                    2 => self.disconnected = true,
                    3 => ei_seat_bind_capabilities(
                        ei_event_get_seat(event),
                        POINTER,
                        ABSOLUTE,
                        SCROLL,
                        BUTTON,
                        std::ptr::null::<c_void>(),
                    ),
                    5 => {
                        if let Some(ptr) = NonNull::new(ei_device_ref(ptr)) {
                            self.devices.push(Device { ptr, active: false })
                        }
                    }
                    6 => {
                        for d in &mut self.devices {
                            if d.ptr.as_ptr() == ptr {
                                d.active = false;
                            }
                        }
                        self.devices.retain(|d| d.ptr.as_ptr() != ptr)
                    }
                    7 => {
                        if let Some(d) = self.devices.iter_mut().find(|d| d.ptr.as_ptr() == ptr) {
                            d.active = false;
                        }
                    }
                    8 => {
                        if let Some(d) = self.devices.iter_mut().find(|d| d.ptr.as_ptr() == ptr) {
                            ei_device_start_emulating(ptr, 0);
                            d.active = true;
                        }
                    }
                    _ => {}
                }
                ei_event_unref(event);
            }
        }
        ensure!(
            !self.disconnected,
            "Desktop input authorization ended; start sharing again"
        );
        Ok(())
    }
}
impl InputSink for Pointer {
    fn select_output(&mut self, name: &str) -> Result<()> {
        self.output = desktop::outputs()?
            .remove(name)
            .and_then(|o| o.logical)
            .context("Selected output unavailable")?;
        Ok(())
    }
    fn emit(&mut self, event: &InputEvent) -> Result<()> {
        event.validate()?;
        self.poll()?;
        unsafe {
            let device = match *event {
                InputEvent::Motion { dx, dy } => {
                    let d = self.device(POINTER)?;
                    ei_device_pointer_motion(d, dx, dy);
                    d
                }
                InputEvent::Absolute {
                    x,
                    y,
                    width,
                    height,
                } => {
                    let (x, y) = logical_point(self.output, x, y, width, height)?;
                    let d = self
                        .devices
                        .iter()
                        .filter(|d| d.active && ei_device_has_capability(d.ptr.as_ptr(), ABSOLUTE))
                        .find(|d| !ei_device_get_region_at(d.ptr.as_ptr(), x, y).is_null())
                        .context("Desktop input does not cover the selected display")?
                        .ptr
                        .as_ptr();
                    ei_device_pointer_motion_absolute(d, x, y);
                    d
                }
                InputEvent::Button { code, pressed } => {
                    let d = self.device(BUTTON)?;
                    ei_device_button_button(d, u32::from(code), pressed);
                    d
                }
                InputEvent::Scroll { axis, amount, .. } => {
                    let d = self.device(SCROLL)?;
                    let (x, y) = match axis {
                        Axis::Horizontal => (amount, 0.0),
                        Axis::Vertical => (0.0, amount),
                    };
                    ei_device_scroll_delta(d, x, y);
                    d
                }
                InputEvent::ScrollStop { axis, .. } => {
                    let d = self.device(SCROLL)?;
                    ei_device_scroll_stop(d, axis == Axis::Horizontal, axis == Axis::Vertical);
                    d
                }
                _ => anyhow::bail!("Pointer backend received a non-pointer event"),
            };
            ei_device_frame(device, ei_now(self.context.as_ptr()));
        }
        Ok(())
    }
}
impl Drop for Pointer {
    fn drop(&mut self) {
        self.devices.clear();
        unsafe {
            ei_unref(self.context.as_ptr());
        }
    }
}
fn logical_point(
    output: LogicalOutput,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<(f64, f64)> {
    ensure!(
        width > 0 && height > 0 && x < width && y < height,
        "Invalid pointer coordinates"
    );
    Ok((
        f64::from(output.x) + f64::from(x) * f64::from(output.width) / f64::from(width),
        f64::from(output.y) + f64::from(y) * f64::from(output.height) / f64::from(height),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absolute_uses_global_logical_coordinates_without_double_rotation() {
        let o = LogicalOutput {
            x: -1200,
            y: 40,
            width: 1200,
            height: 1920,
            scale: 1.5,
            transform: desktop::Transform::Rotate90,
        };
        assert_eq!(
            logical_point(o, 600, 960, 1200, 1920).unwrap(),
            (-600.0, 1000.0)
        );
        assert!(logical_point(o, 1200, 0, 1200, 1920).is_err());
        assert!(logical_point(o, 0, 0, 0, 0).is_err());
    }
}
