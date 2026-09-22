// SPDX-License-Identifier: GPL-3.0-or-later
//! Real KWin in a private virtual framebuffer and private session bus. No host input.
use niri_bridge::{desktop, ei, protocol::InputEvent, receiver::InputSink};
use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    process::{Command, Stdio},
    time::Duration,
};
#[allow(dead_code)]
mod common;

#[test]
#[ignore = "requires KWin Wayland and dbus-run-session; isolated virtual framebuffer only"]
fn isolated_kwin_pointer_capture_and_release() {
    let directory = tempfile::Builder::new()
        .prefix("niri-bridge-nested-kde-")
        .tempdir()
        .unwrap();
    let runtime = directory.path();
    fs::set_permissions(runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let exe = std::env::current_exe().unwrap();
    let result = common::isolated(
        Command::new("dbus-run-session").arg("--").arg(exe).args([
            "--ignored",
            "--exact",
            "isolated_kwin_worker",
            "--nocapture",
        ]),
        runtime,
    )
    .env("NIRI_BRIDGE_KDE_TEST", runtime)
    .env(
        "NIRI_BRIDGE_TEST_BINARY",
        std::env::var("NIRI_BRIDGE_TEST_BINARY")
            .unwrap_or_else(|_| env!("CARGO_BIN_EXE_niri-bridge").into()),
    )
    .output()
    .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
#[ignore = "private worker, invoked only by isolated_kwin_pointer_capture_and_release"]
fn isolated_kwin_worker() {
    let Ok(root) = std::env::var("NIRI_BRIDGE_KDE_TEST") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    assert!(
        root.is_absolute()
            && root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("niri-bridge-nested-kde-")
    );
    assert_eq!(
        std::env::var_os("XDG_RUNTIME_DIR").unwrap(),
        root.as_os_str()
    );
    assert!(std::env::var_os("WAYLAND_DISPLAY").is_none());
    let log = fs::File::create(root.join("kwin.log")).unwrap();
    let mut kwin = common::ManagedChild(
        Command::new("kwin_wayland")
            .args([
                "--virtual",
                "--width",
                "1280",
                "--height",
                "720",
                "--output-count",
                "2",
                "--scale",
                "1.5",
                "--no-lockscreen",
                "--no-kactivities",
                "--socket",
                "test-kde",
            ])
            .env("QT_QPA_PLATFORM", "offscreen")
            .env("KWIN_COMPOSE", "Q")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    common::await_ready(|| {
        assert!(
            kwin.0.try_wait().unwrap().is_none(),
            "KWin exited: {}",
            fs::read_to_string(root.join("kwin.log")).unwrap()
        );
        root.join("test-kde").exists()
    });
    // The worker runs one test on one thread. Never inherit the user's display.
    unsafe {
        std::env::set_var("WAYLAND_DISPLAY", root.join("test-kde"));
        // A previous login may leave this variable in the user manager.
        std::env::set_var("NIRI_SOCKET", root.join("stale-niri.sock"));
    }
    assert_eq!(desktop::detect().unwrap(), desktop::Kind::Kde);
    let outputs = desktop::outputs().unwrap();
    let (name, output) = outputs.iter().next().unwrap();
    assert_eq!(outputs.len(), 2);
    assert!(output.logical.unwrap().width > 0);
    assert_ne!(
        outputs.values().next().unwrap().logical.unwrap().x,
        outputs.values().last().unwrap().logical.unwrap().x
    );
    // Only the private test bus exposes this direct KWin fixture path. Production uses Portal consent.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (connection, fd, cookie) = rt.block_on(async {
        let connection = zbus::Connection::session().await.unwrap();
        let proxy = zbus::Proxy::new(
            &connection,
            "org.kde.KWin",
            "/org/kde/KWin/EIS/RemoteDesktop",
            "org.kde.KWin.EIS.RemoteDesktop",
        )
        .await
        .unwrap();
        let (fd, cookie): (zbus::zvariant::OwnedFd, i32) =
            proxy.call("connectToEIS", &(2i32,)).await.unwrap();
        (connection, fd, cookie)
    });
    let mut pointer = ei::Pointer::connect(fd.into(), name).unwrap();
    for name in outputs.keys() {
        pointer.select_output(name).unwrap();
        pointer
            .emit(&InputEvent::Absolute {
                x: 640,
                y: 360,
                width: 1280,
                height: 720,
            })
            .unwrap();
        let binary = std::env::var_os("NIRI_BRIDGE_TEST_BINARY").unwrap();
        let capture = Command::new(binary)
            .args([
                "capture-test",
                "--output",
                name,
                "--edge",
                "top",
                "--seconds",
                "3",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut capture = common::ManagedChild(capture);
        std::thread::sleep(Duration::from_millis(600));
        pointer
            .emit(&InputEvent::Absolute {
                x: 640,
                y: 0,
                width: 1280,
                height: 720,
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        pointer
            .emit(&InputEvent::Motion { dx: 5.0, dy: 2.0 })
            .unwrap();
        pointer
            .emit(&InputEvent::Button {
                code: 272,
                pressed: true,
            })
            .unwrap();
        pointer
            .emit(&InputEvent::Button {
                code: 272,
                pressed: false,
            })
            .unwrap();
        common::await_ready(|| capture.0.try_wait().unwrap().is_some());
        use std::io::Read;
        let mut text = String::new();
        capture
            .0
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        let mut error = String::new();
        capture
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut error)
            .unwrap();
        assert!(capture.0.wait().unwrap().success(), "{error}");
        let counts: niri_bridge::capture_probe::Counts = serde_json::from_str(&text).unwrap();
        assert!(counts.pointer_lock_observed, "{counts:?}");
        assert!(counts.keyboard_focus_observed, "{counts:?}");
        assert!(counts.shortcuts_inhibited_observed, "{counts:?}");
        assert!(counts.relative_motion_events > 0, "{counts:?}");
        assert_eq!(counts.button_events, 2, "{counts:?}");
        assert_eq!(counts.stop_reason, "timeout");
    }
    rt.block_on(async {
        let proxy = zbus::Proxy::new(
            &connection,
            "org.kde.KWin",
            "/org/kde/KWin/EIS/RemoteDesktop",
            "org.kde.KWin.EIS.RemoteDesktop",
        )
        .await
        .unwrap();
        proxy
            .call::<_, _, ()>("disconnect", &(cookie,))
            .await
            .unwrap();
    });
    std::thread::sleep(Duration::from_millis(100));
    assert!(pointer.poll().is_err(), "Revoked input must fail closed");
}
