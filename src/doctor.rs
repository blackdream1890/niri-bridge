// SPDX-License-Identifier: GPL-3.0-or-later
use std::{
    collections::BTreeMap,
    env,
    ffi::CString,
    io::{Read, Write},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::wl_registry,
};

use crate::niri;

const CAPTURE_PROTOCOLS: &[&str] = &[
    "zwlr_layer_shell_v1",
    "zwp_relative_pointer_manager_v1",
    "zwp_pointer_constraints_v1",
    "zwp_keyboard_shortcuts_inhibit_manager_v1",
];
const EMULATION_PROTOCOLS: &[&str] = &[
    "zwlr_virtual_pointer_manager_v1",
    "zwp_virtual_keyboard_manager_v1",
];

#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub niri_version: Option<String>,
    pub outputs: Vec<niri::Output>,
    pub niri_query_ok: bool,
    pub wayland: Option<WaylandCapabilities>,
    pub portal: PortalCapabilities,
    pub uinput: DeviceAccess,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaylandCapabilities {
    pub protocols: BTreeMap<String, u32>,
    pub layer_shell_capture_interfaces_present: bool,
    pub virtual_input_interfaces_present: bool,
    pub input_behavior_tested: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PortalCapabilities {
    pub introspection_ok: bool,
    pub input_capture_advertised: bool,
    pub remote_desktop_advertised: bool,
    pub mutter_input_capture_running: Option<bool>,
    pub mutter_remote_desktop_running: Option<bool>,
    pub session_creation_tested: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceAccess {
    pub exists: bool,
    pub writable: bool,
}

impl Report {
    pub fn summary(&self) -> String {
        let mut lines = vec![
            "NiriBridge read-only capability report".to_string(),
            format!(
                "Niri: {}",
                self.niri_version.as_deref().unwrap_or("unavailable")
            ),
            format!(
                "Active outputs: {}",
                self.outputs.iter().filter(|o| o.logical.is_some()).count()
            ),
        ];
        for output in &self.outputs {
            if let Some(logical) = output.logical {
                lines.push(format!(
                    "  {}: {}x{} logical, scale {}, position ({}, {})",
                    output.name, logical.width, logical.height, logical.scale, logical.x, logical.y
                ));
            }
        }
        if let Some(w) = &self.wayland {
            lines.push(format!(
                "Layer-shell capture interfaces: {}",
                present(w.layer_shell_capture_interfaces_present)
            ));
            lines.push(format!(
                "Virtual input interfaces: {}",
                present(w.virtual_input_interfaces_present)
            ));
        } else {
            lines.push("Wayland registry: unavailable".to_string());
        }
        lines.push(format!(
            "InputCapture portal advertised: {}",
            self.portal.input_capture_advertised
        ));
        lines.push(format!(
            "RemoteDesktop portal advertised: {}",
            self.portal.remote_desktop_advertised
        ));
        lines.push(format!(
            "uinput: exists={}, writable={}",
            self.uinput.exists, self.uinput.writable
        ));
        lines.push(
            "Input capture, injection and portal sessions have NOT been exercised.".to_string(),
        );
        lines.extend(self.notes.iter().cloned());
        lines.join("\n")
    }
}

fn present(value: bool) -> &'static str {
    if value {
        "present (behavior unverified)"
    } else {
        "incomplete"
    }
}

pub fn inspect() -> Report {
    let mut notes = Vec::new();
    let outputs = niri::outputs();
    let niri_query_ok = outputs.is_ok();
    if !niri_query_ok {
        notes.push("Niri outputs could not be queried; run from the graphical session.".into());
    }
    let niri_version = niri::query("Version")
        .ok()
        .and_then(|v| v.get("Version")?.as_str().map(str::to_string));
    // Isolate the blocking registry roundtrip so a stalled compositor cannot hang doctor.
    let wayland = env::current_exe().ok().and_then(|exe| {
        bounded_output(
            Command::new(exe).arg("probe-wayland"),
            Duration::from_secs(4),
        )
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
    });
    if wayland.is_none() {
        notes
            .push("Wayland registry inspection failed or timed out; no input was captured.".into());
    }
    let portal = inspect_portal();
    if portal.input_capture_advertised && portal.mutter_input_capture_running == Some(false) {
        notes.push("InputCapture is advertised, but the Mutter InputCapture service is not running. Advertisement does not prove a usable backend.".into());
    }
    let uinput = access("/dev/uinput");
    if uinput.exists && !uinput.writable {
        notes.push("uinput is present but not writable; a reviewed permission setup is needed before testing this backend.".into());
    }
    Report {
        schema_version: 1,
        niri_version,
        outputs: outputs.unwrap_or_default().into_values().collect(),
        niri_query_ok,
        wayland,
        portal,
        uinput,
        notes,
    }
}

struct RegistryProbe;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for RegistryProbe {
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

/// Enumerates globals only: does not bind seats, create surfaces, or request input.
pub fn probe_wayland() -> Result<WaylandCapabilities> {
    let connection = Connection::connect_to_env().context("Wayland connection unavailable")?;
    let (globals, _queue) = registry_queue_init::<RegistryProbe>(&connection)?;
    let protocols: BTreeMap<_, _> = globals
        .contents()
        .clone_list()
        .into_iter()
        .filter(|g| {
            CAPTURE_PROTOCOLS.contains(&g.interface.as_str())
                || EMULATION_PROTOCOLS.contains(&g.interface.as_str())
                || [
                    "wl_seat",
                    "wl_compositor",
                    "wl_shm",
                    "zxdg_output_manager_v1",
                ]
                .contains(&g.interface.as_str())
        })
        .map(|g| (g.interface, g.version))
        .collect();
    Ok(WaylandCapabilities {
        layer_shell_capture_interfaces_present: CAPTURE_PROTOCOLS
            .iter()
            .all(|p| protocols.contains_key(*p)),
        virtual_input_interfaces_present: EMULATION_PROTOCOLS
            .iter()
            .all(|p| protocols.contains_key(*p)),
        input_behavior_tested: false,
        protocols,
    })
}

pub fn print_wayland_probe() -> Result<()> {
    serde_json::to_writer(std::io::stdout().lock(), &probe_wayland()?)?;
    std::io::stdout().write_all(b"\n")?;
    Ok(())
}

fn inspect_portal() -> PortalCapabilities {
    let xml = bounded_output(
        busctl().args([
            "--xml-interface",
            "introspect",
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
        ]),
        Duration::from_secs(4),
    );
    let names = bounded_output(
        busctl().args(["--no-pager", "--no-legend", "list"]),
        Duration::from_secs(4),
    );
    let running = |name: &str| {
        names.as_ref().ok().map(|s| {
            s.lines()
                .any(|line| line.split_whitespace().next() == Some(name))
        })
    };
    PortalCapabilities {
        introspection_ok: xml.is_ok(),
        input_capture_advertised: xml
            .as_ref()
            .is_ok_and(|s| s.contains("<interface name=\"org.freedesktop.portal.InputCapture\">")),
        remote_desktop_advertised: xml
            .as_ref()
            .is_ok_and(|s| s.contains("<interface name=\"org.freedesktop.portal.RemoteDesktop\">")),
        mutter_input_capture_running: running("org.gnome.Mutter.InputCapture"),
        mutter_remote_desktop_running: running("org.gnome.Mutter.RemoteDesktop"),
        session_creation_tested: false,
    }
}

fn busctl() -> Command {
    let mut command = Command::new("busctl");
    command.args([
        "--user",
        "--auto-start=no",
        "--allow-interactive-authorization=no",
        "--timeout=3",
    ]);
    command
}

fn access(path: &str) -> DeviceAccess {
    let cpath = CString::new(path).expect("constant device path contains no NUL");
    // access() checks credentials without opening or creating an input device.
    let writable = unsafe { libc::access(cpath.as_ptr(), libc::W_OK) == 0 };
    DeviceAccess {
        exists: std::path::Path::new(path).exists(),
        writable,
    }
}

pub(crate) fn bounded_output(command: &mut Command, timeout: Duration) -> Result<String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().context("Cannot read probe output")?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let bytes = reader
        .join()
        .map_err(|_| anyhow::anyhow!("Probe reader failed"))??;
    if !status.is_some_and(|s| s.success()) {
        bail!("Probe failed or timed out");
    }
    if bytes.len() > 1024 * 1024 {
        bail!("Probe output exceeded limit");
    }
    Ok(String::from_utf8(bytes)?)
}
