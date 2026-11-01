// SPDX-License-Identifier: GPL-3.0-or-later
//! Pairing management uses temporary identities, loopback TLS and no input devices.
use niri_bridge::{
    bridge::{Config, Connection, EdgeConfig},
    geometry::{Boundary, Edge},
    manager::{self, InitializeRequest, Settings},
    transport,
};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn initialize(root: &Path, folder: &str, name: &str) -> std::path::PathBuf {
    let path = root.join(folder).join("config.toml");
    manager::initialize(
        &path,
        name,
        InitializeRequest {
            settings: Settings {
                connection: Connection::Listen {
                    address: "127.0.0.1:42420".into(),
                },
                activity_devices: vec!["/dev/input/by-path/test-keyboard".into()],
                native_touchpads: false,
            },
            edge: EdgeConfig {
                id: "default".into(),
                output: "test-output".into(),
                boundary: Boundary {
                    edge: Edge::Top,
                    start: 0.1,
                    end: 0.9,
                },
            },
        },
    )
    .unwrap();
    path
}

fn import(config: &Path, other: &Path) -> String {
    let candidate = other.parent().unwrap().join("identity.pem");
    let info = manager::certificate(&candidate).unwrap();
    manager::import_peer(
        config,
        &candidate,
        &manager::revision(config).unwrap(),
        &info.fingerprint,
    )
    .unwrap();
    info.fingerprint
}

fn select_reply(
    config: &Path,
    fingerprint: &str,
    revision: &str,
    runtime: &Path,
) -> serde_json::Value {
    fs::create_dir_all(runtime).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_niri-bridge"))
        .args(["manage", "select-peer", "--config"])
        .arg(config)
        .args(["--revision", revision, "--fingerprint", fingerprint])
        .env("XDG_RUNTIME_DIR", runtime)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("NIRI_SOCKET")
        .output()
        .unwrap();
    assert!(result.status.success());
    serde_json::from_slice(&result.stdout).unwrap()
}

fn select(config: &Path, fingerprint: &str, root: &Path) {
    let reply = select_reply(
        config,
        fingerprint,
        &manager::revision(config).unwrap(),
        &root.join("runtime"),
    );
    assert_eq!(reply.get("error"), None);
    assert_eq!(reply["ok"], true);
}

fn connection_address(config: &Config) -> &str {
    match &config.connection {
        Connection::Listen { address } | Connection::Connect { address } => address,
    }
}

#[test]
fn legacy_pairing_is_read_only_until_a_management_write_and_is_retained() {
    let root = tempfile::tempdir().unwrap();
    let local = initialize(root.path(), "local", "local");
    let old = initialize(root.path(), "old", "old-peer");
    let new = initialize(root.path(), "new", "new-peer");
    let old_certificate = fs::read(old.parent().unwrap().join("identity.pem")).unwrap();
    fs::write(local.parent().unwrap().join("peer.pem"), &old_certificate).unwrap();
    let text = fs::read_to_string(&local)
        .unwrap()
        .replace("unpaired", "old-peer");
    fs::write(&local, format!("# retain this user comment\n{text}")).unwrap();
    let original = fs::read(&local).unwrap();
    let key = fs::read(local.parent().unwrap().join("identity.key.pem")).unwrap();
    let first = manager::view(&local).unwrap();
    assert_eq!(first.paired_devices.len(), 1);
    assert!(first.paired_devices[0].active);
    assert!(fs::read(&local).unwrap() == original);
    assert!(!local.parent().unwrap().join("peers").exists());
    let new_id = import(&local, &new);
    let view = manager::view(&local).unwrap();
    assert_eq!(view.paired_devices.len(), 2);
    assert_eq!(view.peer_name, "old-peer");
    assert!(
        view.paired_devices
            .iter()
            .any(|p| p.fingerprint == new_id && !p.active)
    );
    assert!(fs::read(local.parent().unwrap().join("peer.pem")).unwrap() == old_certificate);
    assert!(fs::read(local.parent().unwrap().join("identity.key.pem")).unwrap() == key);
    assert!(
        fs::read_to_string(&local)
            .unwrap()
            .contains("# retain this user comment")
    );
    select(&local, &new_id, root.path());
    select(&local, &first.paired_devices[0].fingerprint, root.path());
    assert_eq!(manager::view(&local).unwrap().peer_name, "old-peer");
}

#[test]
fn switching_restores_each_peers_settings_and_keeps_physical_preferences_global() {
    let root = tempfile::tempdir().unwrap();
    let local = initialize(root.path(), "local", "local");
    let desktop = initialize(root.path(), "desktop", "desktop");
    let travel = initialize(root.path(), "travel", "travel");
    let desktop_id = import(&local, &desktop);
    let mut settings = manager::view(&local).unwrap().settings;
    settings.connection = Connection::Connect {
        address: "desktop.local:42421".into(),
    };
    manager::save(
        &local,
        manager::SaveRequest {
            revision: manager::revision(&local).unwrap(),
            settings,
        },
    )
    .unwrap();
    let travel_id = import(&local, &travel);
    assert_eq!(manager::view(&local).unwrap().peer_name, "desktop");
    select(&local, &travel_id, root.path());
    assert_eq!(
        connection_address(&Config::load(&local).unwrap()),
        "0.0.0.0:42420"
    );
    let mut settings = manager::view(&local).unwrap().settings;
    settings.connection = Connection::Connect {
        address: "travel.local:42422".into(),
    };
    settings.native_touchpads = true;
    settings
        .activity_devices
        .push("/dev/input/by-path/test-touchpad".into());
    manager::save(
        &local,
        manager::SaveRequest {
            revision: manager::revision(&local).unwrap(),
            settings,
        },
    )
    .unwrap();
    select(&local, &desktop_id, root.path());
    let config = Config::load(&local).unwrap();
    assert_eq!(connection_address(&config), "desktop.local:42421");
    assert!(config.native_touchpads);
    assert_eq!(config.activity_devices.len(), 2);
    select(&local, &travel_id, root.path());
    import(&local, &desktop);
    import(&local, &travel);
    let config = Config::load(&local).unwrap();
    assert_eq!(connection_address(&config), "travel.local:42422");
    assert_eq!(config.peer_name, "travel");
    assert_eq!(config.paired_devices.len(), 2);
    assert_eq!(
        fs::metadata(local.parent().unwrap().join("peers"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    for peer in &config.paired_devices {
        assert_eq!(
            fs::metadata(&peer.certificate)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn equal_device_names_have_distinct_trust_and_reimport_does_not_duplicate() {
    let root = tempfile::tempdir().unwrap();
    let local = initialize(root.path(), "local", "local");
    let first = initialize(root.path(), "first", "same-name");
    let second = initialize(root.path(), "second", "same-name");
    let first_id = import(&local, &first);
    let second_id = import(&local, &second);
    assert_ne!(first_id, second_id);
    import(&local, &first);
    let view = manager::view(&local).unwrap();
    assert_eq!(view.paired_devices.len(), 2);
    assert!(view.paired_devices.iter().all(|p| p.name == "same-name"));
    assert!(
        view.paired_devices
            .iter()
            .any(|p| p.fingerprint == first_id && p.active)
    );
    select(&local, &second_id, root.path());
    assert_eq!(
        manager::view(&local).unwrap().peer.unwrap().fingerprint,
        second_id
    );
}

#[test]
fn rejected_imports_and_switches_preserve_configuration_and_saved_certificates() {
    let root = tempfile::tempdir().unwrap();
    let local = initialize(root.path(), "local", "local");
    let first = initialize(root.path(), "first", "first");
    let second = initialize(root.path(), "second", "second");
    import(&local, &first);
    let stale = manager::revision(&local).unwrap();
    let second_id = import(&local, &second);
    let before = fs::read(&local).unwrap();
    let own = local.parent().unwrap().join("identity.pem");
    let own_id = manager::certificate(&own).unwrap().fingerprint;
    assert!(
        manager::import_peer(&local, &own, &manager::revision(&local).unwrap(), &own_id).is_err()
    );
    let candidate = second.parent().unwrap().join("identity.pem");
    assert!(manager::import_peer(&local, &candidate, &stale, &second_id).is_err());
    assert!(
        manager::import_peer(
            &local,
            &candidate,
            &manager::revision(&local).unwrap(),
            "unreviewed"
        )
        .is_err()
    );
    let reply = select_reply(&local, &second_id, &stale, &root.path().join("runtime"));
    assert_eq!(reply["error"], "settings_conflict");
    let reply = select_reply(
        &local,
        &"0".repeat(64),
        &manager::revision(&local).unwrap(),
        &root.path().join("runtime"),
    );
    assert_eq!(reply["error"], "peer_not_found");
    let config = Config::load(&local).unwrap();
    let second_certificate = config
        .paired_devices
        .iter()
        .find(|p| p.fingerprint == second_id)
        .unwrap()
        .certificate
        .clone();
    fs::copy(&own, &second_certificate).unwrap();
    let reply = select_reply(
        &local,
        &second_id,
        &manager::revision(&local).unwrap(),
        &root.path().join("runtime"),
    );
    assert_eq!(reply["error"], "peer_certificate_changed");
    assert!(fs::read(&local).unwrap() == before);
    assert_eq!(manager::view(&local).unwrap().paired_devices.len(), 2);
    import(&local, &second);
    assert_eq!(
        manager::certificate(&second_certificate)
            .unwrap()
            .fingerprint,
        second_id
    );
    select(&local, &second_id, root.path());
    fs::copy(&own, &second_certificate).unwrap();
    assert!(niri_bridge::pairing::selected_certificate(&Config::load(&local).unwrap()).is_err());
    import(&local, &second);
    assert_eq!(manager::view(&local).unwrap().paired_devices.len(), 2);
}

#[test]
fn a_running_instance_lock_blocks_switching_before_configuration_changes() {
    use std::{fs::OpenOptions, os::fd::AsRawFd};
    let root = tempfile::tempdir().unwrap();
    let local = initialize(root.path(), "local", "local");
    let first = initialize(root.path(), "first", "first");
    let second = initialize(root.path(), "second", "second");
    import(&local, &first);
    let second_id = import(&local, &second);
    let runtime = root.path().join("runtime");
    let control = runtime.join("niri-bridge");
    fs::create_dir_all(&control).unwrap();
    fs::set_permissions(&control, fs::Permissions::from_mode(0o700)).unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(control.join("instance.lock"))
        .unwrap();
    fs::set_permissions(
        control.join("instance.lock"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let before = fs::read(&local).unwrap();
    let reply = select_reply(
        &local,
        &second_id,
        &manager::revision(&local).unwrap(),
        &runtime,
    );
    assert_eq!(reply["error"], "sharing_running");
    assert!(fs::read(&local).unwrap() == before);
}

async fn handshake(selected: &Config, other: &Config) -> bool {
    let local = transport::Identity::load(&selected.certificate, &selected.private_key).unwrap();
    let server = transport::server_config(
        &local,
        niri_bridge::pairing::selected_certificate(selected).unwrap(),
    )
    .unwrap();
    let other_identity = transport::Identity::load(&other.certificate, &other.private_key).unwrap();
    let client = transport::client_config(&other_identity, local.certificate.clone()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        match transport::accept(stream, server).await {
            Ok(mut tls) => transport::hello(&mut tls).await.is_ok(),
            Err(_) => false,
        }
    });
    let connected = match transport::connect(&address, "local", client).await {
        Ok(mut tls) => transport::hello(&mut tls).await.is_ok(),
        Err(_) => false,
    };
    let accepted = task.await.unwrap();
    connected && accepted
}

#[tokio::test]
async fn saved_but_unselected_devices_are_rejected_by_mutual_tls_after_each_switch() {
    let root = tempfile::tempdir().unwrap();
    let local = initialize(root.path(), "local", "local");
    let first = initialize(root.path(), "first", "first");
    let second = initialize(root.path(), "second", "second");
    import(&local, &first);
    let second_id = import(&local, &second);
    let config = Config::load(&local).unwrap();
    assert!(handshake(&config, &Config::load(&first).unwrap()).await);
    assert!(!handshake(&config, &Config::load(&second).unwrap()).await);
    select(&local, &second_id, root.path());
    let config = Config::load(&local).unwrap();
    assert!(handshake(&config, &Config::load(&second).unwrap()).await);
    assert!(!handshake(&config, &Config::load(&first).unwrap()).await);
}

#[test]
fn configuration_running_lock_cannot_be_bypassed_by_missing_or_different_session_environment() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let local = initialize(root.path(), "local", "local");
    let first = initialize(root.path(), "first", "first");
    let second = initialize(root.path(), "second", "second");
    import(&local, &first);
    let second_id = import(&local, &second);
    let revision = manager::revision(&local).unwrap();
    let before = fs::read(&local).unwrap();
    let alias = root.path().join("configuration-alias.toml");
    symlink(&local, &alias).unwrap();
    let _running = niri_bridge::control::configuration_guard(&local).unwrap();
    for (path, remove_runtime) in [(&local, true), (&local, false), (&alias, true)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_niri-bridge"));
        command
            .args(["manage", "select-peer", "--config"])
            .arg(path)
            .args(["--revision", &revision, "--fingerprint", &second_id]);
        if remove_runtime {
            command.env_remove("XDG_RUNTIME_DIR");
        } else {
            command.env("XDG_RUNTIME_DIR", root.path());
        }
        let output = command.output().unwrap();
        assert!(output.status.success());
        let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(reply["error"], "sharing_running");
        assert!(fs::read(&local).unwrap() == before);
    }
}

#[test]
fn reviewed_legacy_certificate_can_repair_a_damaged_interrupted_migration_copy() {
    let root = tempfile::tempdir().unwrap();
    let local = initialize(root.path(), "local", "local");
    let other = initialize(root.path(), "other", "other");
    fs::copy(
        other.parent().unwrap().join("identity.pem"),
        local.parent().unwrap().join("peer.pem"),
    )
    .unwrap();
    fs::write(
        &local,
        fs::read_to_string(&local)
            .unwrap()
            .replace("unpaired", "other"),
    )
    .unwrap();
    let info = manager::certificate(&local.parent().unwrap().join("peer.pem")).unwrap();
    let peers = local.parent().unwrap().join("peers");
    fs::create_dir(&peers).unwrap();
    fs::set_permissions(&peers, fs::Permissions::from_mode(0o700)).unwrap();
    let damaged = peers.join(format!("{}.pem", info.fingerprint));
    fs::write(&damaged, "damaged interrupted migration copy").unwrap();
    fs::set_permissions(&damaged, fs::Permissions::from_mode(0o600)).unwrap();
    import(&local, &other);
    assert_eq!(manager::view(&local).unwrap().paired_devices.len(), 1);
    assert_eq!(
        manager::certificate(&damaged).unwrap().fingerprint,
        info.fingerprint
    );
}
