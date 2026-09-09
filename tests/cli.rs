// SPDX-License-Identifier: GPL-3.0-or-later
use std::{fs, process::Command};

#[test]
fn example_layout_is_accepted_without_connecting_to_a_desktop() {
    let result = Command::new(env!("CARGO_BIN_EXE_niri-bridge"))
        .args(["check-layout", "examples/layout.toml"])
        .env_remove("NIRI_SOCKET")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn layout_typo_is_rejected_instead_of_silently_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("invalid.toml");
    let mut text = fs::read_to_string("examples/layout.toml").unwrap();
    text = text.replace("start = 0.1", "strat = 0.1");
    fs::write(&path, text).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_niri-bridge"))
        .arg("check-layout")
        .arg(path)
        .output()
        .unwrap();
    assert!(!result.status.success());
}

#[test]
fn missing_session_is_reported_without_claiming_input_support() {
    let dir = tempfile::tempdir().unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_niri-bridge"))
        .args(["doctor", "--json"])
        .env("XDG_RUNTIME_DIR", dir.path())
        .env("WAYLAND_DISPLAY", "not-present")
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}/not-present", dir.path().display()),
        )
        .env_remove("NIRI_SOCKET")
        .output()
        .unwrap();
    assert!(result.status.success());
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["niri_query_ok"], false);
    assert!(report["wayland"].is_null());
    assert_eq!(report["portal"]["session_creation_tested"], false);
}

#[test]
fn capture_duration_is_rejected_before_connecting_to_wayland() {
    let result = Command::new(env!("CARGO_BIN_EXE_niri-bridge"))
        .args([
            "capture-test",
            "--output",
            "not-present",
            "--edge",
            "top",
            "--seconds",
            "31",
        ])
        .env_remove("NIRI_SOCKET")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    assert!(!result.status.success());
}
