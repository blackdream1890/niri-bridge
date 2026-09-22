// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only compositor metadata. No capture surfaces or input devices are created here.
pub use crate::niri::{LogicalOutput, Mode, Output, Transform};
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeMap, os::fd::AsRawFd, path::PathBuf};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop,
    protocol::{wl_callback, wl_output, wl_registry},
};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1 as manager, zxdg_output_v1 as output,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Kind {
    Niri,
    Kde,
}
pub fn detect() -> Result<Kind> {
    let pid = socket_owner()?;
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm"))?;
    match name.trim() {
        "niri" => Ok(Kind::Niri),
        "kwin_wayland" | "kwin_wayland_wr" => Ok(Kind::Kde),
        _ => anyhow::bail!("This compositor is not supported; use Niri or KDE Plasma Wayland"),
    }
}
pub fn is_niri() -> bool {
    detect().is_ok_and(|k| k == Kind::Niri)
}

pub fn socket_path() -> Result<PathBuf> {
    let display =
        PathBuf::from(std::env::var_os("WAYLAND_DISPLAY").context("WAYLAND_DISPLAY is not set")?);
    Ok(if display.is_absolute() {
        display
    } else {
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?)
            .join(display)
    })
}

pub fn outputs() -> Result<BTreeMap<String, Output>> {
    if detect()? == Kind::Niri {
        return crate::niri::outputs();
    }
    probe_outputs()
}

fn socket_owner() -> Result<libc::pid_t> {
    let socket = std::os::unix::net::UnixStream::connect(socket_path()?)?;
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of_val(&credentials) as libc::socklen_t;
    ensure!(
        unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut size,
            )
        } == 0,
        "Cannot identify compositor"
    );
    ensure!(
        credentials.pid > 0 && credentials.uid == unsafe { libc::geteuid() },
        "Compositor must belong to the current user"
    );
    Ok(credentials.pid)
}

pub(crate) fn compositor_pid() -> Result<libc::pid_t> {
    let pid = socket_owner()?;
    if detect()? == Kind::Niri {
        ensure!(
            crate::niri::compositor_pid()? == pid,
            "Niri IPC belongs to a different graphical session"
        );
        return Ok(pid);
    }
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm"))?;
    if name.trim() == "kwin_wayland" {
        return Ok(pid);
    }
    // KWin's wrapper owns the listening socket; only inspect its direct children.
    for child in
        std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))?.split_whitespace()
    {
        let child: libc::pid_t = child.parse()?;
        let name = std::fs::read_to_string(format!("/proc/{child}/comm"));
        if name.is_ok_and(|n| n.trim() == "kwin_wayland") {
            return Ok(child);
        }
    }
    anyhow::bail!("Cannot identify the active KWin compositor")
}

pub(crate) fn roundtrip() -> Result<()> {
    if is_niri() {
        crate::niri::query("Version")?;
    } else {
        probe_outputs()?;
    }
    Ok(())
}

#[derive(Default)]
struct Metadata {
    name: Option<String>,
    position: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
    mode: Option<Mode>,
    transform: Option<Transform>,
}
#[derive(Default)]
struct State {
    outputs: BTreeMap<u32, Metadata>,
    globals: Vec<(u32, String, u32)>,
    synced: bool,
}

fn probe_outputs() -> Result<BTreeMap<String, Output>> {
    let connection = Connection::connect_to_env()?;
    let mut queue = connection.new_event_queue::<State>();
    let qh = queue.handle();
    let registry = connection.display().get_registry(&qh, ());
    let mut state = State::default();
    sync(&connection, &mut queue, &mut state)?;
    let &(name, _, _) = state
        .globals
        .iter()
        .find(|(_, interface, version)| interface == "zxdg_output_manager_v1" && *version >= 3)
        .context("Logical output protocol unavailable")?;
    let manager: manager::ZxdgOutputManagerV1 = registry.bind(name, 3, &qh, ());
    for &(name, ref interface, version) in &state.globals {
        if interface != "wl_output" || version < 4 {
            continue;
        }
        let wl: wl_output::WlOutput = registry.bind(name, 4, &qh, ());
        let id = wl.id().protocol_id();
        state.outputs.insert(id, Metadata::default());
        manager.get_xdg_output(&wl, &qh, id);
    }
    sync(&connection, &mut queue, &mut state)?;
    state
        .outputs
        .into_values()
        .map(|m| {
            let name = m.name.context("Output name unavailable")?;
            let (x, y) = m.position.context("Logical position unavailable")?;
            let (width, height) = m.size.context("Logical size unavailable")?;
            ensure!(width > 0 && height > 0, "Invalid logical output size");
            let mode = m.mode.context("Current output mode unavailable")?;
            let transform = m.transform.context("Output transform unavailable")?;
            let rotated = matches!(
                transform,
                Transform::Rotate90
                    | Transform::Rotate270
                    | Transform::Flipped90
                    | Transform::Flipped270
            );
            let physical_width = if rotated { mode.height } else { mode.width };
            let logical = LogicalOutput {
                x,
                y,
                width: width as u32,
                height: height as u32,
                scale: f64::from(physical_width) / f64::from(width),
                transform,
            };
            Ok((
                name.clone(),
                Output {
                    name,
                    logical: Some(logical),
                    current_mode: Some(0),
                    modes: vec![mode],
                },
            ))
        })
        .collect()
}
fn sync(
    connection: &Connection,
    queue: &mut wayland_client::EventQueue<State>,
    state: &mut State,
) -> Result<()> {
    state.synced = false;
    connection.display().sync(&queue.handle(), ());
    connection.flush()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        queue.dispatch_pending(state)?;
        if state.synced {
            return Ok(());
        }
        let now = std::time::Instant::now();
        ensure!(now < deadline, "Compositor metadata request timed out");
        if let Some(guard) = queue.prepare_read() {
            let mut fd = libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let result =
                unsafe { libc::poll(&mut fd, 1, (deadline - now).as_millis().min(3000) as i32) };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            ensure!(result > 0, "Compositor metadata request timed out");
            guard.read()?;
        }
    }
}
impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(
        s: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        s.synced = true;
    }
}
impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        s: &mut Self,
        _: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = e
        {
            s.globals.push((name, interface, version));
        }
    }
}
impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        s: &mut Self,
        p: &wl_output::WlOutput,
        e: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(m) = s.outputs.get_mut(&p.id().protocol_id()) else {
            return;
        };
        match e {
            wl_output::Event::Name { name } => m.name = Some(name),
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                refresh,
            } if flags.contains(wl_output::Mode::Current)
                && width > 0
                && height > 0
                && refresh >= 0 =>
            {
                m.mode = Some(Mode {
                    width: width as u32,
                    height: height as u32,
                    refresh_rate: refresh as u32,
                })
            }
            wl_output::Event::Geometry {
                transform: WEnum::Value(t),
                ..
            } => {
                m.transform = Some(match t {
                    wl_output::Transform::Normal => Transform::Normal,
                    wl_output::Transform::_90 => Transform::Rotate90,
                    wl_output::Transform::_180 => Transform::Rotate180,
                    wl_output::Transform::_270 => Transform::Rotate270,
                    wl_output::Transform::Flipped => Transform::Flipped,
                    wl_output::Transform::Flipped90 => Transform::Flipped90,
                    wl_output::Transform::Flipped180 => Transform::Flipped180,
                    wl_output::Transform::Flipped270 => Transform::Flipped270,
                    _ => return,
                })
            }
            _ => {}
        }
    }
}
impl Dispatch<output::ZxdgOutputV1, u32> for State {
    fn event(
        s: &mut Self,
        _: &output::ZxdgOutputV1,
        e: output::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(m) = s.outputs.get_mut(id) else {
            return;
        };
        match e {
            output::Event::LogicalPosition { x, y } => m.position = Some((x, y)),
            output::Event::LogicalSize { width, height } => m.size = Some((width, height)),
            _ => {}
        }
    }
}
delegate_noop!(State:ignore manager::ZxdgOutputManagerV1);
