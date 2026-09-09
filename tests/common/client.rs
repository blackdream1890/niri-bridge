// SPDX-License-Identifier: GPL-3.0-or-later
//! Synthetic input and an observer window, restricted to the test-created socket supplied by caller.
use anyhow::Result;
use niri_bridge::{
    protocol::{Axis, InputEvent, ScrollSource},
    receiver::InputSink,
};
use std::{
    collections::BTreeSet,
    fs::File,
    io::Write,
    os::{
        fd::{AsFd, OwnedFd},
        unix::net::UnixStream,
    },
    path::Path,
};
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm,
        wl_shm_pool, wl_surface,
    },
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1 as km, zwp_virtual_keyboard_v1 as vk,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1 as pm, zwlr_virtual_pointer_v1 as vp,
};

#[derive(Default)]
struct State {
    keymap: Option<(u32, OwnedFd, u32)>,
    focused: bool,
    held: BTreeSet<u32>,
    keys: Vec<(u32, u32)>,
    buttons: Vec<(u32, u32)>,
    surface: Option<wl_surface::WlSurface>,
    toplevel: Option<xdg_toplevel::XdgToplevel>,
    pointer_position: Option<(f64, f64)>,
    shm: Option<wl_shm::WlShm>,
    buffer: Option<wl_buffer::WlBuffer>,
    size: (u32, u32),
}

pub struct Client {
    connection: Connection,
    queue: wayland_client::EventQueue<State>,
    state: State,
    keyboard: vk::ZwpVirtualKeyboardV1,
    pointer: vp::ZwlrVirtualPointerV1,
    pressed: BTreeSet<u16>,
}

impl Client {
    pub fn new(socket: &Path, window: bool) -> Self {
        assert!(
            socket.is_absolute() && socket.to_string_lossy().contains("niri-bridge-nested-"),
            "Only a private test compositor may receive synthetic events"
        );
        let connection = Connection::from_socket(UnixStream::connect(socket).unwrap()).unwrap();
        let (globals, mut queue) = registry_queue_init::<State>(&connection).unwrap();
        let qh = queue.handle();
        let mut state = State::default();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=9, ()).unwrap();
        let _keyboard = seat.get_keyboard(&qh, ());
        let _pointer = seat.get_pointer(&qh, ());
        queue.roundtrip(&mut state).unwrap();
        let keyboard_manager: km::ZwpVirtualKeyboardManagerV1 =
            globals.bind(&qh, 1..=1, ()).unwrap();
        let keyboard = keyboard_manager.create_virtual_keyboard(&seat, &qh, ());
        let (format, fd, size) = state.keymap.as_ref().unwrap();
        keyboard.keymap(*format, fd.as_fd(), *size);
        let pointer_manager: pm::ZwlrVirtualPointerManagerV1 =
            globals.bind(&qh, 1..=2, ()).unwrap();
        let pointer = pointer_manager.create_virtual_pointer(Some(&seat), &qh, ());
        if window {
            let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 4..=6, ()).unwrap();
            let wm: xdg_wm_base::XdgWmBase = globals.bind(&qh, 1..=6, ()).unwrap();
            state.shm = Some(globals.bind(&qh, 1..=2, ()).unwrap());
            let surface = compositor.create_surface(&qh, ());
            let xdg = wm.get_xdg_surface(&surface, &qh, ());
            let toplevel = xdg.get_toplevel(&qh, ());
            toplevel.set_app_id("niri-bridge-test-observer".into());
            toplevel.set_title("NiriBridge isolated observer".into());
            state.toplevel = Some(toplevel);
            state.surface = Some(surface);
            state.surface.as_ref().unwrap().commit();
        }
        queue.roundtrip(&mut state).unwrap();
        queue.roundtrip(&mut state).unwrap();
        Self {
            connection,
            queue,
            state,
            keyboard,
            pointer,
            pressed: BTreeSet::new(),
        }
    }
    pub fn pump(&mut self) {
        self.queue.roundtrip(&mut self.state).unwrap();
    }
    pub fn focused(&self) -> bool {
        self.state.focused
    }
    pub fn fullscreen(&mut self) {
        self.state.toplevel.as_ref().unwrap().set_fullscreen(None);
        self.connection.flush().unwrap();
    }
    pub fn size(&self) -> (u32, u32) {
        self.state.size
    }
    pub fn pointer_position(&self) -> Option<(f64, f64)> {
        self.state.pointer_position
    }
    pub fn saw_key(&self, code: u32, pressed: bool) -> bool {
        self.state.keys.contains(&(code, u32::from(pressed)))
    }
    pub fn key_held(&self, code: u32) -> bool {
        self.state.held.contains(&code)
    }
    pub fn saw_button(&self, code: u32, pressed: bool) -> bool {
        self.state.buttons.contains(&(code, u32::from(pressed)))
    }
    pub fn clear(&mut self) {
        self.state.keys.clear();
        self.state.buttons.clear();
    }
    pub fn key(&mut self, code: u16, pressed: bool) {
        self.emit(&InputEvent::Key { code, pressed }).unwrap();
    }
    pub fn absolute(&mut self, x: u32, y: u32) {
        self.emit(&InputEvent::Absolute {
            x,
            y,
            width: 1280,
            height: 720,
        })
        .unwrap();
    }
    pub fn motion(&mut self, dx: f64, dy: f64) {
        self.emit(&InputEvent::Motion { dx, dy }).unwrap();
    }
    pub fn button(&mut self, pressed: bool) {
        self.emit(&InputEvent::Button { code: 272, pressed })
            .unwrap();
    }
}

impl InputSink for Client {
    fn emit(&mut self, event: &InputEvent) -> Result<()> {
        event.validate()?;
        match *event {
            InputEvent::Touchpad { .. } => {
                anyhow::bail!("The isolated Wayland emitter does not create kernel devices")
            }
            InputEvent::Key { code, pressed } => {
                if pressed {
                    self.pressed.insert(code);
                } else {
                    self.pressed.remove(&code);
                }
                self.keyboard.key(0, u32::from(code), u32::from(pressed));
                // This test fixture explicitly uses the standard US keymap, not production mapping.
                let mut mask = 0;
                for (keys, bit) in [
                    (&[42, 54][..], 0),
                    (&[29, 97][..], 2),
                    (&[56, 100][..], 3),
                    (&[125, 126][..], 6),
                ] {
                    if keys.iter().any(|k| self.pressed.contains(k)) {
                        mask |= 1 << bit;
                    }
                }
                self.keyboard.modifiers(mask, 0, 0, 0);
            }
            InputEvent::Button { code, pressed } => self.pointer.button(
                0,
                u32::from(code),
                if pressed {
                    wl_pointer::ButtonState::Pressed
                } else {
                    wl_pointer::ButtonState::Released
                },
            ),
            InputEvent::Absolute {
                x,
                y,
                width,
                height,
            } => self.pointer.motion_absolute(0, x, y, width, height),
            InputEvent::Motion { dx, dy } => self.pointer.motion(0, dx, dy),
            InputEvent::Scroll {
                axis,
                amount,
                source,
            } => {
                self.pointer.axis_source(match source {
                    ScrollSource::Wheel => wl_pointer::AxisSource::Wheel,
                    ScrollSource::Finger => wl_pointer::AxisSource::Finger,
                    ScrollSource::Continuous => wl_pointer::AxisSource::Continuous,
                });
                self.pointer.axis(
                    0,
                    if axis == Axis::Vertical {
                        wl_pointer::Axis::VerticalScroll
                    } else {
                        wl_pointer::Axis::HorizontalScroll
                    },
                    amount,
                );
            }
            InputEvent::ScrollStop { axis, .. } => self.pointer.axis_stop(
                0,
                if axis == Axis::Vertical {
                    wl_pointer::Axis::VerticalScroll
                } else {
                    wl_pointer::Axis::HorizontalScroll
                },
            ),
        }
        self.pointer.frame();
        self.connection.flush()?;
        Ok(())
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.keyboard.destroy();
        self.pointer.destroy();
        let _ = self.connection.flush();
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
impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        s: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        e: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_keyboard::Event::Keymap { format, fd, size } => {
                s.keymap = Some((format.into(), fd, size))
            }
            wl_keyboard::Event::Enter { keys, .. } => {
                s.focused = true;
                let (keys, remainder) = keys.as_chunks::<4>();
                assert!(remainder.is_empty());
                s.held = keys
                    .iter()
                    .map(|bytes| u32::from_ne_bytes(*bytes))
                    .collect();
            }
            wl_keyboard::Event::Leave { .. } => {
                s.focused = false;
                s.held.clear();
            }
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(state),
                ..
            } => {
                s.keys.push((key, state as u32));
                if state == wl_keyboard::KeyState::Pressed {
                    s.held.insert(key);
                } else {
                    s.held.remove(&key);
                }
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        s: &mut Self,
        _: &wl_pointer::WlPointer,
        e: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            }
            | wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                s.pointer_position = Some((surface_x, surface_y));
            }
            wl_pointer::Event::Button {
                button,
                state: WEnum::Value(state),
                ..
            } => {
                s.buttons.push((button, state as u32));
            }
            _ => {}
        }
    }
}
impl Dispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        p: &xdg_wm_base::XdgWmBase,
        e: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            p.pong(serial);
        }
    }
}
impl Dispatch<xdg_toplevel::XdgToplevel, ()> for State {
    fn event(
        s: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        e: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = e {
            s.size = (
                if width > 0 { width as u32 } else { 1000 },
                if height > 0 { height as u32 } else { 640 },
            );
        }
    }
}
impl Dispatch<xdg_surface::XdgSurface, ()> for State {
    fn event(
        s: &mut Self,
        p: &xdg_surface::XdgSurface,
        e: xdg_surface::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            p.ack_configure(serial);
            let (width, height) = s.size;
            let mut file: File = tempfile::tempfile().unwrap();
            file.write_all(
                &0xff204060u32
                    .to_ne_bytes()
                    .repeat((width * height) as usize),
            )
            .unwrap();
            let pool = s.shm.as_ref().unwrap().create_pool(
                file.as_fd(),
                (width * height * 4) as i32,
                qh,
                (),
            );
            let buffer = pool.create_buffer(
                0,
                width as i32,
                height as i32,
                (width * 4) as i32,
                wl_shm::Format::Argb8888,
                qh,
                (),
            );
            let surface = s.surface.as_ref().unwrap();
            surface.attach(Some(&buffer), 0, 0);
            surface.damage_buffer(0, 0, width as i32, height as i32);
            surface.commit();
            pool.destroy();
            s.buffer = Some(buffer);
        }
    }
}
delegate_noop!(State:ignore wl_seat::WlSeat);
delegate_noop!(State:ignore wl_compositor::WlCompositor);
delegate_noop!(State:ignore wl_surface::WlSurface);
delegate_noop!(State:ignore wl_shm::WlShm);
delegate_noop!(State:ignore wl_shm_pool::WlShmPool);
delegate_noop!(State:ignore wl_buffer::WlBuffer);
delegate_noop!(State:ignore km::ZwpVirtualKeyboardManagerV1);
delegate_noop!(State:ignore vk::ZwpVirtualKeyboardV1);
delegate_noop!(State:ignore pm::ZwlrVirtualPointerManagerV1);
delegate_noop!(State:ignore vp::ZwlrVirtualPointerV1);
