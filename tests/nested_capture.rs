// SPDX-License-Identifier: GPL-3.0-or-later
//! Opt-in integration test. All synthetic input is sent to a private, headless Niri instance.
use std::{
    os::{
        fd::{AsFd, OwnedFd},
        unix::net::UnixStream,
    },
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use niri_bridge::capture_probe::Counts;
use wayland_client::{
    Connection, Dispatch, QueueHandle, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_keyboard, wl_pointer, wl_registry, wl_seat},
};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1 as lock_manager, ext_session_lock_v1 as session_lock,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1 as keyboard_manager,
    zwp_virtual_keyboard_v1 as virtual_keyboard,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1 as pointer_manager, zwlr_virtual_pointer_v1 as virtual_pointer,
};

mod common;
use common::{ManagedChild, await_ready, isolated};

#[derive(Default)]
struct Driver {
    keymap: Option<(u32, OwnedFd, u32)>,
}
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Driver {
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
impl Dispatch<wl_keyboard::WlKeyboard, ()> for Driver {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap { format, fd, size } = event {
            state.keymap = Some((format.into(), fd, size));
        }
    }
}
delegate_noop!(Driver:ignore wl_seat::WlSeat);
delegate_noop!(Driver:ignore pointer_manager::ZwlrVirtualPointerManagerV1);
delegate_noop!(Driver:ignore virtual_pointer::ZwlrVirtualPointerV1);
delegate_noop!(Driver:ignore keyboard_manager::ZwpVirtualKeyboardManagerV1);
delegate_noop!(Driver:ignore virtual_keyboard::ZwpVirtualKeyboardV1);
delegate_noop!(Driver:ignore lock_manager::ExtSessionLockManagerV1);
delegate_noop!(Driver:ignore session_lock::ExtSessionLockV1);

#[test]
#[ignore = "requires installed Weston (headless/pixman) and Niri; launches isolated test compositors"]
fn isolated_niri_captures_and_releases_on_escape_timeout_and_lock() {
    let desktop = common::Desktop::start();
    let runtime = desktop.runtime();
    let display = &desktop.display;
    let niri_socket = &desktop.niri_socket;
    // Deliberately connect to the socket we created, never the user's WAYLAND_DISPLAY.
    let connection = Connection::from_socket(UnixStream::connect(display).unwrap()).unwrap();
    let (globals, mut queue) = registry_queue_init::<Driver>(&connection).unwrap();
    let qh = queue.handle();
    let mut driver = Driver::default();
    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=9, ()).unwrap();
    let _keyboard = seat.get_keyboard(&qh, ());
    queue.roundtrip(&mut driver).unwrap();
    let pm: pointer_manager::ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).unwrap();
    let pointer = pm.create_virtual_pointer(Some(&seat), &qh, ());
    let km: keyboard_manager::ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).unwrap();
    let keyboard = km.create_virtual_keyboard(&seat, &qh, ());
    let (format, fd, size) = driver.keymap.as_ref().unwrap();
    keyboard.keymap(*format, fd.as_fd(), *size);
    queue.roundtrip(&mut driver).unwrap();

    for (ending, seconds) in [("escape", 5), ("timeout", 2), ("lock", 5)] {
        pointer.motion_absolute(0, 640, 360, 1280, 720);
        pointer.frame();
        queue.roundtrip(&mut driver).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_niri-bridge"));
        let capture = isolated(&mut command, runtime)
            .env("WAYLAND_DISPLAY", display)
            .env("NIRI_SOCKET", niri_socket)
            .args([
                "capture-test",
                "--output",
                "winit",
                "--edge",
                "top",
                "--seconds",
                &seconds.to_string(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut capture = ManagedChild(capture);
        // Wait until the test layer appears in this isolated Niri instance.
        await_ready(|| {
            let out = isolated(
                Command::new("niri").args(["msg", "--json", "layers"]),
                runtime,
            )
            .env("NIRI_SOCKET", niri_socket)
            .output()
            .unwrap();
            String::from_utf8_lossy(&out.stdout).contains("niri-bridge-capture-test")
        });
        pointer.motion_absolute(1, 640, 0, 1280, 720);
        pointer.frame();
        queue.roundtrip(&mut driver).unwrap();
        thread::sleep(Duration::from_millis(150));
        pointer.motion(2, 4.0, 2.0);
        pointer.frame();
        pointer.button(3, 272, wl_pointer::ButtonState::Pressed);
        pointer.frame();
        pointer.button(4, 272, wl_pointer::ButtonState::Released);
        pointer.frame();
        pointer.axis_source(wl_pointer::AxisSource::Finger);
        pointer.axis(5, wl_pointer::Axis::VerticalScroll, 10.0);
        pointer.frame();
        keyboard.key(6, 30, 1);
        keyboard.key(7, 30, 0);
        queue.roundtrip(&mut driver).unwrap();
        thread::sleep(Duration::from_millis(100));
        if ending == "escape" {
            keyboard.key(8, 1, 1);
            keyboard.key(9, 1, 0);
            queue.roundtrip(&mut driver).unwrap();
        }
        let lock = if ending == "lock" {
            let manager: lock_manager::ExtSessionLockManagerV1 =
                globals.bind(&qh, 1..=1, ()).unwrap();
            let lock = manager.lock(&qh, ());
            queue.roundtrip(&mut driver).unwrap();
            Some(lock)
        } else {
            None
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = capture.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "Capture probe exceeded watchdog deadline"
            );
            thread::sleep(Duration::from_millis(20));
        };
        use std::io::Read;
        let mut stdout = String::new();
        capture
            .0
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        let mut stderr = String::new();
        capture
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(status.success(), "Capture probe failed: {stderr}");
        let counts: Counts = serde_json::from_str(&stdout).unwrap();
        assert!(counts.pointer_lock_observed, "{counts:?}");
        assert!(counts.keyboard_focus_observed, "{counts:?}");
        assert!(counts.shortcuts_inhibited_observed, "{counts:?}");
        assert!(counts.relative_motion_events > 0, "{counts:?}");
        assert!(counts.keyboard_events >= 2, "{counts:?}");
        assert!(counts.button_events >= 2, "{counts:?}");
        assert!(counts.scroll_events > 0, "{counts:?}");
        if ending == "lock" {
            assert!(
                ["keyboard_focus_lost", "pointer_unlocked"].contains(&counts.stop_reason.as_str()),
                "{counts:?}"
            );
        } else {
            assert_eq!(counts.stop_reason, ending);
        }
        if let Some(lock) = lock {
            lock.unlock_and_destroy();
            queue.roundtrip(&mut driver).unwrap();
        }
        // Check compositor state after the process exits, not just its success message.
        let out = isolated(
            Command::new("niri").args(["msg", "--json", "layers"]),
            runtime,
        )
        .env("NIRI_SOCKET", niri_socket)
        .output()
        .unwrap();
        assert!(!String::from_utf8_lossy(&out.stdout).contains("niri-bridge-capture-test"));
        println!(
            "Isolated capture: {}",
            serde_json::to_string(&counts).unwrap()
        );
    }
    keyboard.destroy();
    pointer.destroy();
    let _ = connection.flush();
    drop(desktop);
}
