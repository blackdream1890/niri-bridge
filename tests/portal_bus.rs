// SPDX-License-Identifier: GPL-3.0-or-later
//! Exercise the real Portal D-Bus messages and descriptor transfer on a private bus.
use niri_bridge::portal::Session;
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    os::{fd::OwnedFd, unix::fs::PermissionsExt, unix::net::UnixStream},
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;
use zbus::{Connection, OwnedObjectPath, OwnedVariant, Variant, message::Header};

const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const DESKTOP: &str = "/org/freedesktop/portal/desktop";
const SESSION: &str = "/org/freedesktop/portal/desktop/session/fixture/session";
type Options = HashMap<String, OwnedVariant>;

struct State {
    scenario: String,
    calls: Mutex<Vec<&'static str>>,
    wire_peer: Mutex<Option<UnixStream>>,
    closed: Notify,
}

struct RemoteDesktop(Arc<State>);

async fn response(
    connection: &Connection,
    header: &Header<'_>,
    options: &Options,
    suffix: &str,
    code: u32,
    results: HashMap<&str, Variant<'_>>,
) -> zbus::fdo::Result<OwnedObjectPath> {
    let token = <&str>::try_from(options.get("handle_token").unwrap()).unwrap();
    assert_eq!(token, format!("niribridge_{}_{suffix}", std::process::id()));
    let sender = header.sender().unwrap();
    let sender_path = sender.as_str().trim_start_matches(':').replace('.', "_");
    let path =
        OwnedObjectPath::try_from(format!("{DESKTOP}/request/{sender_path}/{token}")).unwrap();
    // The client subscribes before making the method call. Send before the
    // method reply as well, so losing an early Response makes the test fail.
    connection
        .emit_signal(
            Some(sender),
            &path,
            "org.freedesktop.portal.Request",
            "Response",
            &(code, results),
        )
        .await
        .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
    Ok(path)
}

#[zbus::interface(interface = "org.freedesktop.portal.RemoteDesktop")]
impl RemoteDesktop {
    async fn create_session(
        &self,
        options: Options,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        assert_eq!(header.signature(), "a{sv}");
        assert_eq!(options.len(), 2);
        assert_eq!(
            <&str>::try_from(options.get("session_handle_token").unwrap()).unwrap(),
            format!("niribridge_{}", std::process::id())
        );
        self.0.calls.lock().unwrap().push("CreateSession");
        response(
            connection,
            &header,
            &options,
            "create",
            0,
            HashMap::from([("session_handle", Variant::from(SESSION))]),
        )
        .await
    }

    async fn select_devices(
        &self,
        session: OwnedObjectPath,
        options: Options,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        assert_eq!(header.signature(), "oa{sv}");
        assert_eq!(session.as_str(), SESSION);
        assert_eq!(options.len(), 2);
        // Request only pointer access, never keyboard, video or clipboard access.
        assert_eq!(u32::try_from(options.get("types").unwrap()).unwrap(), 2);
        self.0.calls.lock().unwrap().push("SelectDevices");
        response(connection, &header, &options, "select", 0, HashMap::new()).await
    }

    async fn start(
        &self,
        session: OwnedObjectPath,
        parent_window: String,
        options: Options,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        assert_eq!(header.signature(), "osa{sv}");
        assert_eq!(session.as_str(), SESSION);
        assert!(parent_window.is_empty());
        assert_eq!(options.len(), 1);
        self.0.calls.lock().unwrap().push("Start");
        let code = if self.0.scenario == "denied" { 2 } else { 0 };
        let devices = if self.0.scenario == "keyboard-only" {
            1u32
        } else {
            2u32
        };
        response(
            connection,
            &header,
            &options,
            "start",
            code,
            HashMap::from([("devices", Variant::from(devices))]),
        )
        .await
    }

    #[zbus(name = "ConnectToEIS")]
    fn connect_to_eis(
        &self,
        session: OwnedObjectPath,
        options: Options,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::OwnedFd {
        assert_eq!(header.signature(), "oa{sv}");
        assert_eq!(session.as_str(), SESSION);
        assert!(options.is_empty());
        assert_eq!(self.0.scenario, "allowed");
        self.0.calls.lock().unwrap().push("ConnectToEIS");
        let (returned, peer) = UnixStream::pair().unwrap();
        *self.0.wire_peer.lock().unwrap() = Some(peer);
        OwnedFd::from(returned).into()
    }
}

struct PortalSession(Arc<State>);

#[zbus::interface(interface = "org.freedesktop.portal.Session")]
impl PortalSession {
    fn close(&self) {
        self.0.calls.lock().unwrap().push("Close");
        self.0.closed.notify_one();
    }
}

fn private_bus(scenario: &str) {
    let directory = tempfile::Builder::new()
        .prefix("niri-bridge-portal-bus-")
        .tempdir()
        .unwrap();
    let root = directory.path();
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
    let bus_path = root.join("bus");
    let escaped_path = bus_path
        .to_str()
        .unwrap()
        .replace('&', "&amp;")
        .replace('<', "&lt;");
    let config = root.join("bus.conf");
    // No service directories: this test bus cannot activate the user's Portal.
    fs::write(
        &config,
        format!(
            "<busconfig><type>session</type><listen>unix:path={escaped_path}</listen>\
             <auth>EXTERNAL</auth><policy context=\"default\">\
             <allow own=\"*\"/><allow send_destination=\"*\"/>\
             <allow receive_sender=\"*\"/></policy></busconfig>"
        ),
    )
    .unwrap();
    let result = Command::new("dbus-run-session")
        .arg(format!("--config-file={}", config.display()))
        .arg("--")
        .arg(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "private_portal_worker",
            "--nocapture",
        ])
        .env("NIRI_BRIDGE_PORTAL_TEST_ROOT", root)
        .env("NIRI_BRIDGE_PORTAL_TEST_SCENARIO", scenario)
        .env("XDG_RUNTIME_DIR", root)
        .env("XDG_CONFIG_HOME", root)
        .env("XDG_CACHE_HOME", root)
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env_remove("DBUS_STARTER_ADDRESS")
        .env_remove("DBUS_STARTER_BUS_TYPE")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("NIRI_SOCKET")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "private Portal scenario {scenario} failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn portal_consent_transfers_a_usable_descriptor_and_closes_session() {
    private_bus("allowed");
}

#[test]
fn portal_denial_never_connects_to_eis() {
    private_bus("denied");
}

#[test]
fn portal_without_pointer_permission_never_connects_to_eis() {
    private_bus("keyboard-only");
}

#[test]
#[ignore = "worker launched only on a private test bus"]
fn private_portal_worker() {
    let root = PathBuf::from(std::env::var_os("NIRI_BRIDGE_PORTAL_TEST_ROOT").unwrap());
    assert!(root.is_absolute());
    assert!(
        root.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("niri-bridge-portal-bus-")
    );
    assert_eq!(std::env::var_os("XDG_RUNTIME_DIR").unwrap(), root);
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap();
    assert_eq!(
        address.split(',').next().unwrap(),
        format!("unix:path={}", root.join("bus").display())
    );
    assert!(std::env::var_os("WAYLAND_DISPLAY").is_none());
    let scenario = std::env::var("NIRI_BRIDGE_PORTAL_TEST_SCENARIO").unwrap();
    assert!(["allowed", "denied", "keyboard-only"].contains(&scenario.as_str()));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            let state = Arc::new(State {
                scenario,
                calls: Mutex::new(Vec::new()),
                wire_peer: Mutex::new(None),
                closed: Notify::new(),
            });
            let _server = zbus::connection::Builder::session()
                .name(DESTINATION)
                .serve_at(DESKTOP, RemoteDesktop(state.clone()))
                .serve_at(SESSION, PortalSession(state.clone()))
                .build()
                .await
                .unwrap();
            let result = Session::create().await;
            if state.scenario == "allowed" {
                let (session, fd) = result.expect("pointer consent must open EIS");
                let mut client = UnixStream::from(fd);
                let mut peer = state.wire_peer.lock().unwrap().take().unwrap();
                client
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                client.write_all(b"client").unwrap();
                let mut bytes = [0u8; 6];
                peer.read_exact(&mut bytes).unwrap();
                assert_eq!(&bytes, b"client");
                peer.write_all(b"server").unwrap();
                client.read_exact(&mut bytes).unwrap();
                assert_eq!(&bytes, b"server");
                drop(session);
            } else {
                assert!(result.is_err(), "missing consent must fail");
                assert!(state.wire_peer.lock().unwrap().is_none());
            }
            tokio::time::timeout(Duration::from_secs(2), state.closed.notified())
                .await
                .expect("Session must send Close when consent ends or is rejected");
            let mut expected = vec!["CreateSession", "SelectDevices", "Start"];
            if state.scenario == "allowed" {
                expected.push("ConnectToEIS");
            }
            expected.push("Close");
            assert_eq!(*state.calls.lock().unwrap(), expected);
        })
        .await
        .expect("private Portal scenario timed out");
    });
}
