// SPDX-License-Identifier: GPL-3.0-or-later
//! Full bridge flow over TLS between two private Niri compositors; no physical input permission needed.
mod common;
use anyhow::Result;
use common::{Desktop, ManagedChild, client::Client};
use niri_bridge::{
    activity::ActivitySource,
    bridge::{self, Config, Connection},
    identity,
    session::Status,
    transport::{self, Identity},
};
use niri_bridge::{pointer::Pointer, protocol::InputEvent, receiver::InputSink};

struct TestSink {
    keyboard: Client,
    pointer: Pointer,
}
impl InputSink for TestSink {
    fn select_output(&mut self, output: &str) -> Result<()> {
        self.pointer.select_output(output)
    }
    fn emit(&mut self, event: &InputEvent) -> Result<()> {
        match event {
            InputEvent::Key { .. } => self.keyboard.emit(event),
            _ => self.pointer.emit(event),
        }
    }
}
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

struct FileActivity {
    path: PathBuf,
    raw_keyboard: bool,
    held: std::collections::BTreeSet<u16>,
    pending: Vec<InputEvent>,
}
impl ActivitySource for FileActivity {
    fn sample(&mut self) -> Result<bool> {
        let mut active = false;
        if self.path.exists() {
            fs::remove_file(&self.path)?;
            active = true;
        }
        let keys = self.path.with_file_name("keyboard-events");
        if self.raw_keyboard && keys.exists() {
            let events: Vec<InputEvent> = serde_json::from_slice(&fs::read(&keys)?)?;
            fs::remove_file(keys)?;
            for event in events {
                if let InputEvent::Key { code, pressed } = event {
                    if pressed {
                        self.held.insert(code);
                    } else {
                        self.held.remove(&code);
                    }
                    self.pending.push(event);
                    active = true;
                }
            }
        }
        Ok(active)
    }
    fn keyboard_state(&self) -> Option<std::collections::BTreeSet<u16>> {
        self.raw_keyboard.then(|| self.held.clone())
    }
    fn take_keyboard_events(&mut self) -> Vec<InputEvent> {
        std::mem::take(&mut self.pending)
    }
}

#[test]
#[ignore = "internal child fixture; does nothing without an explicit private-test configuration"]
fn fixture_node() {
    let Some(path) = std::env::var_os("NIRI_BRIDGE_FIXTURE_CONFIG") else {
        return;
    };
    let runtime = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap());
    let display = PathBuf::from(std::env::var_os("WAYLAND_DISPLAY").unwrap());
    assert!(
        runtime.to_string_lossy().contains("niri-bridge-nested-") && display.starts_with(&runtime)
    );
    let config = Config::load(Path::new(&path)).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            let local = Identity::load(&config.certificate, &config.private_key).unwrap();
            let peer = transport::load_certificate(&config.peer_certificate).unwrap();
            let mut tls: tokio_rustls::TlsStream<tokio::net::TcpStream> = match &config.connection {
                Connection::Listen { address } => {
                    let listener = tokio::net::TcpListener::bind(address).await.unwrap();
                    let (stream, _) = listener.accept().await.unwrap();
                    transport::accept(stream, transport::server_config(&local, peer).unwrap())
                        .await
                        .unwrap()
                        .into()
                }
                Connection::Connect { address } => {
                    let settings = transport::client_config(&local, peer).unwrap();
                    let deadline = Instant::now() + Duration::from_secs(5);
                    loop {
                        match transport::connect(address, &config.peer_name, settings.clone()).await
                        {
                            Ok(stream) => break stream.into(),
                            Err(_) => {
                                assert!(Instant::now() < deadline);
                                tokio::time::sleep(Duration::from_millis(50)).await;
                            }
                        }
                    }
                }
            };
            transport::hello(&mut tls).await.unwrap();
            let (sender, status) = tokio::sync::watch::channel(Status::Unlocked);
            let locked = runtime.join("locked");
            let monitor = tokio::spawn(async move {
                loop {
                    let next = if locked.exists() {
                        Status::Unavailable
                    } else {
                        Status::Unlocked
                    };
                    sender.send_if_modified(|s| {
                        if *s == next {
                            false
                        } else {
                            *s = next;
                            true
                        }
                    });
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            });
            let keyboard = Client::new(&display, false);
            let pointer = Pointer::connect(&display, &config.edges[0].output).unwrap();
            let sink = TestSink { keyboard, pointer };
            let mut activity = FileActivity {
                path: runtime.join("takeover"),
                raw_keyboard: runtime.join("raw-keyboard").exists(),
                held: Default::default(),
                pending: Vec::new(),
            };
            let interface = if runtime.join("control-api").exists() {
                Some(
                    niri_bridge::control::Server::start(niri_bridge::control::Snapshot::new(
                        config.peer_name.clone(),
                    ))
                    .unwrap(),
                )
            } else {
                None
            };
            let outcome = bridge::coordinate_with_control(
                tls,
                &config,
                &mut activity,
                status,
                sink,
                interface.as_ref().map(|(_, c)| c),
            )
            .await;
            if interface.is_some() {
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            monitor.abort();
            if let Err(error) = outcome {
                eprintln!("Fixture bridge stopped: {error}");
            }
        });
}

fn launch(
    desktop: &Desktop,
    mode: &str,
    address: &str,
    peer_name: &str,
    edge: &str,
) -> ManagedChild {
    let path = desktop.runtime().join("bridge.toml");
    fs::write(
        &path,
        format!(
            r#"
certificate = "identity/identity.pem"
private_key = "identity/identity.key.pem"
peer_certificate = "peer.pem"
peer_name = "{peer_name}"
activity_devices = ["/test/physical-activity"]
[connection]
mode = "{mode}"
address = "{address}"
[edge]
output = "winit"
boundary = {{ edge = "{edge}", start = 0.1, end = 0.9 }}
"#
        ),
    )
    .unwrap();
    launch_path(desktop, &path)
}

fn launch_path(desktop: &Desktop, path: &Path) -> ManagedChild {
    let log = File::create(desktop.runtime().join("bridge.log")).unwrap();
    ManagedChild(
        desktop
            .command(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "fixture_node",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("NIRI_BRIDGE_FIXTURE_CONFIG", path)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    )
}

fn layer_present(desktop: &Desktop) -> bool {
    let out = desktop
        .command("niri")
        .args(["msg", "--json", "layers"])
        .stderr(Stdio::null())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains("niri-bridge-edge")
}

#[track_caller]
fn wait_for(mut condition: impl FnMut() -> bool, desktops: &[&Desktop]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        if Instant::now() >= deadline {
            for d in desktops {
                eprintln!(
                    "Bridge fixture log: {}",
                    fs::read_to_string(d.runtime().join("bridge.log")).unwrap_or_default()
                );
            }
            panic!("Bridge condition was not reached");
        }
        thread::sleep(Duration::from_millis(15));
    }
}

#[test]
#[ignore = "requires Weston and Niri; starts two isolated desktops and sends only synthetic input"]
fn encrypted_bridge_routes_both_directions_and_recovers() {
    let desktop = Desktop::start();
    let laptop = Desktop::start();
    let mut a = Client::new(&desktop.display, true);
    let mut b = Client::new(&laptop.display, true);
    a.absolute(640, 360);
    b.absolute(640, 360);
    a.pump();
    b.pump();
    a.clear();
    b.clear();
    identity::create(&desktop.runtime().join("identity"), "desktop").unwrap();
    identity::create(&laptop.runtime().join("identity"), "laptop").unwrap();
    fs::copy(
        desktop.runtime().join("identity/identity.pem"),
        laptop.runtime().join("peer.pem"),
    )
    .unwrap();
    fs::copy(
        laptop.runtime().join("identity/identity.pem"),
        desktop.runtime().join("peer.pem"),
    )
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let node_a = launch(&desktop, "listen", &address, "laptop", "bottom");
    let node_b = launch(&laptop, "connect", &address, "desktop", "top");
    wait_for(
        || layer_present(&desktop) && layer_present(&laptop),
        &[&desktop, &laptop],
    );
    thread::sleep(Duration::from_millis(100));

    // Desktop hardware origin -> laptop keyboard destination.
    a.absolute(640, 719);
    a.pump();
    wait_for(
        || {
            a.pump();
            !a.focused()
        },
        &[&desktop, &laptop],
    );
    a.key(30, true);
    a.key(30, false);
    a.pump();
    wait_for(
        || {
            b.pump();
            b.saw_key(30, false)
        },
        &[&desktop, &laptop],
    );
    assert!(!a.saw_key(30, true));
    a.motion(-200.0, 150.0);
    a.pump();
    thread::sleep(Duration::from_millis(50));
    a.button(true);
    a.button(false);
    a.pump();
    wait_for(
        || {
            b.pump();
            b.saw_button(272, false)
        },
        &[&desktop, &laptop],
    );
    a.motion(0.0, -1000.0);
    a.pump();
    wait_for(
        || {
            a.pump();
            a.focused()
        },
        &[&desktop, &laptop],
    );
    a.key(48, true);
    a.key(48, false);
    a.pump();
    assert!(a.saw_key(48, false));
    b.pump();
    assert!(!b.saw_key(48, true));

    // Ordinary Escape goes to the peer; only the full emergency chord returns locally.
    thread::sleep(Duration::from_millis(250));
    a.clear();
    b.clear();
    a.absolute(640, 360);
    a.pump();
    a.absolute(640, 719);
    a.pump();
    wait_for(
        || {
            a.pump();
            !a.focused()
        },
        &[&desktop, &laptop],
    );
    a.key(1, true);
    a.key(1, false);
    a.pump();
    wait_for(
        || {
            b.pump();
            b.saw_key(1, false)
        },
        &[&desktop, &laptop],
    );
    a.pump();
    assert!(!a.focused());
    b.clear();
    for code in [29, 56, 42, 1] {
        a.key(code, true);
    }
    a.pump();
    wait_for(
        || {
            a.pump();
            b.pump();
            a.focused() && [29, 56, 42].iter().all(|k| b.saw_key(*k, false))
        },
        &[&desktop, &laptop],
    );
    assert!(!b.saw_key(1, true));
    for code in [1, 42, 56, 29] {
        a.key(code, false);
    }
    a.pump();

    // Laptop hardware origin -> desktop keyboard destination, including local takeover.
    thread::sleep(Duration::from_millis(250));
    a.clear();
    b.clear();
    b.absolute(640, 360);
    b.pump();
    b.absolute(640, 0);
    b.pump();
    wait_for(
        || {
            b.pump();
            !b.focused()
        },
        &[&desktop, &laptop],
    );
    b.key(46, true);
    b.key(46, false);
    b.pump();
    wait_for(
        || {
            a.pump();
            a.saw_key(46, false)
        },
        &[&desktop, &laptop],
    );
    assert!(!b.saw_key(46, true));
    b.key(29, true);
    b.pump();
    wait_for(
        || {
            a.pump();
            a.saw_key(29, true)
        },
        &[&desktop, &laptop],
    );
    fs::write(desktop.runtime().join("takeover"), "").unwrap();
    wait_for(
        || {
            a.pump();
            b.pump();
            a.saw_key(29, false) && b.focused()
        },
        &[&desktop, &laptop],
    );
    b.key(29, false);
    b.pump();

    // Reconfiguration of the receiving output must also release the active input session.
    thread::sleep(Duration::from_millis(250));
    a.clear();
    b.clear();
    b.absolute(640, 360);
    b.pump();
    b.absolute(640, 0);
    b.pump();
    wait_for(
        || {
            b.pump();
            !b.focused()
        },
        &[&desktop, &laptop],
    );
    b.key(56, true);
    b.pump();
    wait_for(
        || {
            a.pump();
            a.saw_key(56, true)
        },
        &[&desktop, &laptop],
    );
    let changed = desktop
        .command("niri")
        .args(["msg", "output", "winit", "scale", "1.25"])
        .output()
        .unwrap();
    assert!(changed.status.success());
    wait_for(
        || {
            a.pump();
            b.pump();
            a.saw_key(56, false) && b.focused()
        },
        &[&desktop, &laptop],
    );
    b.key(56, false);
    b.pump();
    let restored = desktop
        .command("niri")
        .args(["msg", "output", "winit", "scale", "1.0"])
        .output()
        .unwrap();
    assert!(restored.status.success());
    wait_for(
        || layer_present(&desktop) && layer_present(&laptop),
        &[&desktop, &laptop],
    );

    // A lock status update pauses both directions and releases a held modifier.
    thread::sleep(Duration::from_millis(250));
    a.clear();
    b.clear();
    b.absolute(640, 360);
    b.pump();
    b.absolute(640, 0);
    b.pump();
    wait_for(
        || {
            b.pump();
            !b.focused()
        },
        &[&desktop, &laptop],
    );
    b.key(42, true);
    b.pump();
    wait_for(
        || {
            a.pump();
            a.saw_key(42, true)
        },
        &[&desktop, &laptop],
    );
    fs::write(desktop.runtime().join("locked"), "").unwrap();
    wait_for(
        || {
            a.pump();
            b.pump();
            a.saw_key(42, false)
                && b.focused()
                && !layer_present(&desktop)
                && !layer_present(&laptop)
        },
        &[&desktop, &laptop],
    );
    b.key(42, false);
    b.pump();
    fs::remove_file(desktop.runtime().join("locked")).unwrap();
    wait_for(
        || layer_present(&desktop) && layer_present(&laptop),
        &[&desktop, &laptop],
    );

    // Connection loss releases a held key at the receiver and removes capture surfaces.
    thread::sleep(Duration::from_millis(150));
    a.clear();
    b.clear();
    b.absolute(640, 360);
    b.pump();
    b.absolute(640, 0);
    b.pump();
    wait_for(
        || {
            b.pump();
            !b.focused()
        },
        &[&desktop, &laptop],
    );
    b.key(29, true);
    b.pump();
    wait_for(
        || {
            a.pump();
            a.saw_key(29, true)
        },
        &[&desktop, &laptop],
    );
    drop(node_b);
    wait_for(
        || {
            a.pump();
            a.saw_key(29, false) && !layer_present(&desktop)
        },
        &[&desktop, &laptop],
    );
    drop(node_a);
    println!(
        "Encrypted bridge passed: both directions, pointer return, local takeover, output reconfiguration, lock pause, and disconnect release."
    );
}

#[test]
#[ignore = "requires Weston and Niri; demonstrates the virtual-keyboard shortcut limitation in isolation"]
fn virtual_keyboard_does_not_trigger_niri_binding() {
    let desktop = Desktop::start();
    let mut client = Client::new(&desktop.display, true);
    wait_for(
        || {
            client.pump();
            client.focused()
        },
        &[],
    );
    client.key(88, true);
    client.key(88, false);
    client.pump();
    assert!(client.saw_key(88, true));
    thread::sleep(Duration::from_millis(100));
    assert!(!desktop.runtime().join("shortcut-fired").exists());
    let marker = desktop.runtime().join("spawn-baseline");
    let out = desktop
        .command("niri")
        .args(["msg", "action", "spawn", "--", "/usr/bin/touch"])
        .arg(&marker)
        .output()
        .unwrap();
    assert!(out.status.success());
    wait_for(|| marker.exists(), &[]);
}

fn raw_keys(desktop: &Desktop, events: &[(u16, bool)]) {
    let path = desktop.runtime().join("keyboard-events");
    assert!(!path.exists());
    let events: Vec<_> = events
        .iter()
        .map(|(code, pressed)| InputEvent::Key {
            code: *code,
            pressed: *pressed,
        })
        .collect();
    let temp = path.with_extension("tmp");
    fs::write(&temp, serde_json::to_vec(&events).unwrap()).unwrap();
    fs::rename(temp, path).unwrap();
}

#[test]
#[ignore = "requires Weston and Niri; physical-key fixture exercises the IME-independent source path"]
fn physical_keyboard_routes_without_wayland_keys_and_releases_on_emergency() {
    let desktop = Desktop::start();
    let laptop = Desktop::start();
    let mut a = Client::new(&desktop.display, true);
    let mut b = Client::new(&laptop.display, true);
    a.absolute(640, 360);
    b.absolute(640, 360);
    a.pump();
    b.pump();
    identity::create(&desktop.runtime().join("identity"), "desktop").unwrap();
    identity::create(&laptop.runtime().join("identity"), "laptop").unwrap();
    fs::copy(
        desktop.runtime().join("identity/identity.pem"),
        laptop.runtime().join("peer.pem"),
    )
    .unwrap();
    fs::copy(
        laptop.runtime().join("identity/identity.pem"),
        desktop.runtime().join("peer.pem"),
    )
    .unwrap();
    fs::write(desktop.runtime().join("raw-keyboard"), "").unwrap();
    fs::write(laptop.runtime().join("raw-keyboard"), "").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let node_a = launch(&desktop, "listen", &address, "laptop", "bottom");
    let node_b = launch(&laptop, "connect", &address, "desktop", "top");
    wait_for(
        || layer_present(&desktop) && layer_present(&laptop),
        &[&desktop, &laptop],
    );
    thread::sleep(Duration::from_millis(100));
    raw_keys(&desktop, &[(29, true)]);
    wait_for(
        || !desktop.runtime().join("keyboard-events").exists(),
        &[&desktop, &laptop],
    );
    a.absolute(640, 719);
    a.pump();
    wait_for(
        || {
            b.pump();
            b.saw_key(29, true)
        },
        &[&desktop, &laptop],
    );
    // A duplicate Wayland key is ignored when the physical backend is authoritative.
    a.key(30, true);
    a.key(30, false);
    a.pump();
    b.pump();
    assert!(!b.saw_key(30, true));
    raw_keys(&desktop, &[(29, false), (30, true), (30, false)]);
    wait_for(
        || {
            b.pump();
            b.saw_key(29, false) && b.saw_key(30, false)
        },
        &[&desktop, &laptop],
    );
    assert!(!a.saw_key(30, true));
    // No wl_keyboard event is delivered for this emergency chord.
    raw_keys(&desktop, &[(29, true), (56, true), (42, true), (1, true)]);
    wait_for(
        || {
            a.pump();
            b.pump();
            a.focused() && [29, 56, 42].iter().all(|code| b.saw_key(*code, false))
        },
        &[&desktop, &laptop],
    );
    assert!(!b.saw_key(1, true));
    raw_keys(
        &desktop,
        &[(1, false), (42, false), (56, false), (29, false)],
    );
    wait_for(
        || !desktop.runtime().join("keyboard-events").exists(),
        &[&desktop, &laptop],
    );
    thread::sleep(Duration::from_millis(250));
    a.clear();
    b.clear();
    b.absolute(640, 360);
    b.pump();
    b.absolute(640, 0);
    b.pump();
    wait_for(
        || {
            b.pump();
            !b.focused()
        },
        &[&desktop, &laptop],
    );
    // Wait for the authenticated peer to accept the reverse control session.
    thread::sleep(Duration::from_millis(50));
    raw_keys(&laptop, &[(46, true), (46, false), (42, true)]);
    wait_for(
        || {
            a.pump();
            a.saw_key(46, false) && a.saw_key(42, true)
        },
        &[&desktop, &laptop],
    );
    drop(node_b);
    wait_for(
        || {
            a.pump();
            a.saw_key(42, false)
        },
        &[&desktop, &laptop],
    );
    drop(node_a);
}

fn control_request(desktop: &Desktop, request: serde_json::Value) -> serde_json::Value {
    use std::io::{BufRead, Write};
    let mut stream =
        std::os::unix::net::UnixStream::connect(desktop.runtime().join("niri-bridge/control.sock"))
            .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    serde_json::to_writer(&mut stream, &request).unwrap();
    stream.write_all(b"\n").unwrap();
    let mut line = String::new();
    std::io::BufReader::new(stream)
        .read_line(&mut line)
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
#[ignore = "requires Weston and Niri; verifies both routes and return through a different route"]
fn multiple_edges_route_both_directions_and_return_through_the_other_connection() {
    let desktop = Desktop::start();
    let laptop = Desktop::start();
    let mut a = Client::new(&desktop.display, true);
    let mut b = Client::new(&laptop.display, true);
    a.fullscreen();
    b.fullscreen();
    wait_for(
        || {
            a.pump();
            b.pump();
            a.size() == (1280, 720) && b.size() == (1280, 720)
        },
        &[&desktop, &laptop],
    );
    identity::create(&desktop.runtime().join("identity"), "desktop").unwrap();
    identity::create(&laptop.runtime().join("identity"), "laptop").unwrap();
    fs::copy(
        desktop.runtime().join("identity/identity.pem"),
        laptop.runtime().join("peer.pem"),
    )
    .unwrap();
    fs::copy(
        laptop.runtime().join("identity/identity.pem"),
        desktop.runtime().join("peer.pem"),
    )
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let first = |side| serde_json::json!({"id":"primary","output":"winit","boundary":{"edge":side,"start":0.1,"end":0.9}});
    let side = |edge, start, end| serde_json::json!({"id":"side","output":"winit","boundary":{"edge":edge,"start":start,"end":end}});
    for (node, mode, peer, edges) in [
        (
            &desktop,
            "listen",
            "laptop",
            vec![first("bottom"), side("right", 0.55, 0.95)],
        ),
        // Deliberately reverse the order: the protocol must match stable IDs.
        (
            &laptop,
            "connect",
            "desktop",
            vec![side("left", 0.2, 0.85), first("top")],
        ),
    ] {
        fs::write(node.runtime().join("control-api"), "").unwrap();
        let config = serde_json::json!({"certificate":"identity/identity.pem","private_key":"identity/identity.key.pem",
            "peer_certificate":"peer.pem","peer_name":peer,"activity_devices":["/test/physical-activity"],
            "connection":{"mode":mode,"address":address},"edges":edges});
        fs::write(
            node.runtime().join("bridge.toml"),
            toml::to_string(&config).unwrap(),
        )
        .unwrap();
    }
    a.absolute(640, 360);
    b.absolute(640, 360);
    a.pump();
    b.pump();
    let node_a = launch_path(&desktop, &desktop.runtime().join("bridge.toml"));
    let node_b = launch_path(&laptop, &laptop.runtime().join("bridge.toml"));
    let two_layers = |node: &Desktop| {
        let out = node
            .command("niri")
            .args(["msg", "--json", "layers"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .matches("niri-bridge-edge")
            .count()
            == 2
    };
    for (reverse, x, y, dx, dy, returned_side) in [
        (false, 640, 719, -2000., 350., "right"),
        (false, 1279, 560, 600., -2000., "bottom"),
        (true, 640, 0, 2000., -180., "left"),
        (true, 0, 400, -600., 2000., "top"),
    ] {
        eprintln!("Multiple-edge route: reverse={reverse}, returning at {returned_side}");
        wait_for(
            || {
                two_layers(&desktop)
                    && two_layers(&laptop)
                    && [&desktop, &laptop].iter().all(|node| {
                        let state = control_request(node, serde_json::json!({"command":"status"}))
                            ["data"]
                            .clone();
                        state["capture_ready"] == true && state["role"] == "local"
                    })
            },
            &[&desktop, &laptop],
        );
        let (source, target, source_desktop, target_desktop) = if reverse {
            (&mut b, &mut a, &laptop, &desktop)
        } else {
            (&mut a, &mut b, &desktop, &laptop)
        };
        source.absolute(640, 360);
        target.absolute(640, 360);
        source.pump();
        target.pump();
        source.clear();
        target.clear();
        source.absolute(x, y);
        source.pump();
        wait_for(
            || {
                source.pump();
                !source.focused()
                    && control_request(target_desktop, serde_json::json!({"command":"status"}))["data"]
                        ["role"]
                        == "receiving"
            },
            &[&desktop, &laptop],
        );
        source.key(30, true);
        source.key(30, false);
        source.key(29, true);
        source.pump();
        wait_for(
            || {
                target.pump();
                target.saw_key(30, false) && target.saw_key(29, true)
            },
            &[&desktop, &laptop],
        );
        assert!(!source.saw_key(30, true));
        source.motion(dx, dy);
        source.pump();
        wait_for(
            || {
                source.pump();
                target.pump();
                // The target app loses focus to its return strip, so release
                // may arrive as Leave followed by an Enter with no held keys.
                source.focused()
                    && target.focused()
                    && !target.key_held(29)
                    && control_request(source_desktop, serde_json::json!({"command":"status"}))["data"]
                        ["role"]
                        == "local"
            },
            &[&desktop, &laptop],
        );
        wait_for(
            || {
                source.pump();
                source
                    .pointer_position()
                    .is_some_and(|(x, y)| match returned_side {
                        "left" => x < 10. && y > 140. && y < 620.,
                        "right" => x > 1270. && y > 390. && y < 690.,
                        "top" => y < 10. && x > 120. && x < 1160.,
                        "bottom" => y > 710. && x > 120. && x < 1160.,
                        _ => false,
                    })
            },
            &[&desktop, &laptop],
        );
        source.key(29, false);
        source.pump();
    }
    drop(node_a);
    drop(node_b);
}

#[test]
#[ignore = "requires Weston and Niri; verifies actual paired configuration transactions through the local UI API"]
fn desktop_ui_synchronizes_both_files_and_rejects_conflicts() {
    let desktop = Desktop::start();
    let laptop = Desktop::start();
    identity::create(&desktop.runtime().join("identity"), "desktop").unwrap();
    identity::create(&laptop.runtime().join("identity"), "laptop").unwrap();
    fs::copy(
        desktop.runtime().join("identity/identity.pem"),
        laptop.runtime().join("peer.pem"),
    )
    .unwrap();
    fs::copy(
        laptop.runtime().join("identity/identity.pem"),
        desktop.runtime().join("peer.pem"),
    )
    .unwrap();
    fs::write(desktop.runtime().join("control-api"), "").unwrap();
    fs::write(laptop.runtime().join("control-api"), "").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let node_a = launch(&desktop, "listen", &address, "laptop", "bottom");
    let node_b = launch(&laptop, "connect", &address, "desktop", "top");
    wait_for(
        || layer_present(&desktop) && layer_present(&laptop),
        &[&desktop, &laptop],
    );
    let state = control_request(&desktop, serde_json::json!({"command":"status"}))["data"].clone();
    let original_a = fs::read(desktop.runtime().join("bridge.toml")).unwrap();
    let original_b = fs::read(laptop.runtime().join("bridge.toml")).unwrap();
    let mut local = state["local"]["edges"][0].clone();
    local["boundary"]["edge"] = serde_json::json!("left");
    local["boundary"]["start"] = serde_json::json!(0.2);
    let mut peer = state["peer"]["edges"][0].clone();
    peer["boundary"]["edge"] = serde_json::json!("right");
    let local_side = serde_json::json!({"id":"side","output":"winit","boundary":{"edge":"right","start":0.3,"end":0.7}});
    let peer_side = serde_json::json!({"id":"side","output":"winit","boundary":{"edge":"left","start":0.2,"end":0.8}});
    let request = serde_json::json!({"command":"apply_layout","layout":{"local_edges":[local,local_side],"peer_edges":[peer,peer_side],"local_revision":state["local"]["revision"],"peer_revision":state["peer"]["revision"]}});
    let mut conflict = request.clone();
    conflict["layout"]["peer_revision"] = serde_json::json!("0".repeat(64));
    let reply = control_request(&desktop, conflict);
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["error"], "settings_conflict");
    assert_eq!(
        fs::read(desktop.runtime().join("bridge.toml")).unwrap(),
        original_a
    );
    assert_eq!(
        fs::read(laptop.runtime().join("bridge.toml")).unwrap(),
        original_b
    );
    let mut unavailable = request.clone();
    unavailable["layout"]["peer_edges"][0]["output"] = serde_json::json!("missing-output");
    let reply = control_request(&desktop, unavailable);
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["error"], "output_unavailable");
    assert_eq!(
        fs::read(desktop.runtime().join("bridge.toml")).unwrap(),
        original_a
    );
    assert_eq!(
        fs::read(laptop.runtime().join("bridge.toml")).unwrap(),
        original_b
    );
    let reply = control_request(&desktop, request);
    assert_eq!(reply["ok"], true);
    let a = Config::load(&desktop.runtime().join("bridge.toml")).unwrap();
    let b = Config::load(&laptop.runtime().join("bridge.toml")).unwrap();
    assert_eq!(a.edges[0].boundary.edge, niri_bridge::geometry::Edge::Left);
    assert_eq!(b.edges[0].boundary.edge, niri_bridge::geometry::Edge::Right);
    assert_eq!(a.edges[0].boundary.start, 0.2);
    assert_eq!(a.edges.len(), 2);
    assert_eq!(b.edges.len(), 2);
    assert_eq!(a.edges[1].id, "side");
    assert_eq!(b.edges[1].id, "side");
    assert_eq!(
        reply["data"]["local_revision"],
        niri_bridge::manager::revision(&desktop.runtime().join("bridge.toml")).unwrap()
    );
    assert_eq!(
        reply["data"]["peer_revision"],
        niri_bridge::manager::revision(&laptop.runtime().join("bridge.toml")).unwrap()
    );
    drop(node_a);
    drop(node_b);
}
