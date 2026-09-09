// SPDX-License-Identifier: GPL-3.0-or-later
use std::{
    fs::{self, File},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
#[allow(dead_code)]
pub mod client;

pub struct ManagedChild(pub Child);
impl Drop for ManagedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        // Only the process group created by this test is signalled.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGTERM);
        }
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

pub fn isolated<'a>(command: &'a mut Command, runtime: &Path) -> &'a mut Command {
    command
        .env("XDG_RUNTIME_DIR", runtime)
        .env("XDG_CONFIG_HOME", runtime)
        .env("XDG_CACHE_HOME", runtime)
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}/no-bus", runtime.display()),
        )
        .env("LIBGL_ALWAYS_SOFTWARE", "1")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("NIRI_SOCKET")
        .env_remove("NOTIFY_SOCKET")
        .env_remove("XDG_SESSION_ID")
        .stdin(Stdio::null())
        .process_group(0)
}

fn find_socket(runtime: &Path, prefix: &str) -> Option<PathBuf> {
    fs::read_dir(runtime)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            name.starts_with(prefix) && name.ends_with(".sock")
        })
}

pub fn await_ready(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "Isolated test compositor did not become ready"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

pub struct Desktop {
    _niri: ManagedChild,
    _weston: ManagedChild,
    directory: tempfile::TempDir,
    pub display: PathBuf,
    pub niri_socket: PathBuf,
}
impl Desktop {
    pub fn start() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("niri-bridge-nested-")
            .tempdir()
            .unwrap();
        let runtime = directory.path();
        fs::set_permissions(runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let config = runtime.join("niri.kdl");
        fs::write(&config,"hotkey-overlay { skip-at-startup; }\nxwayland-satellite { off; }\ninput { keyboard { xkb { layout \"us\"; }; }; }\n").unwrap();
        let mut text = fs::read_to_string(&config).unwrap();
        text.push_str(&format!(
            "binds {{ F12 {{ spawn \"/usr/bin/touch\" {}; }}; }}\n",
            serde_json::to_string(&runtime.join("shortcut-fired")).unwrap()
        ));
        fs::write(&config, text).unwrap();
        let westonlog = File::create(runtime.join("weston.log")).unwrap();
        let weston = ManagedChild(
            isolated(
                Command::new("weston").args([
                    "--backend=headless",
                    "--renderer=pixman",
                    "--shell=kiosk-shell.so",
                    "--socket=parent",
                    "--width=1280",
                    "--height=720",
                    "--idle-time=0",
                    "--no-config",
                ]),
                runtime,
            )
            .stdout(westonlog.try_clone().unwrap())
            .stderr(westonlog)
            .spawn()
            .unwrap(),
        );
        await_ready(|| runtime.join("parent").exists());
        let nirilog = File::create(runtime.join("niri.log")).unwrap();
        let niri = ManagedChild(
            isolated(Command::new("niri").arg("-c").arg(&config), runtime)
                .env("WAYLAND_DISPLAY", "parent")
                .stdout(nirilog.try_clone().unwrap())
                .stderr(nirilog)
                .spawn()
                .unwrap(),
        );
        await_ready(|| find_socket(runtime, "niri.").is_some());
        let niri_socket = find_socket(runtime, "niri.").unwrap();
        let displays: Vec<_> = fs::read_dir(runtime)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("wayland-")
                    && !p.to_string_lossy().ends_with(".lock")
            })
            .collect();
        assert_eq!(displays.len(), 1);
        let display = displays.into_iter().next().unwrap();

        Self {
            _niri: niri,
            _weston: weston,
            directory,
            display,
            niri_socket,
        }
    }
    pub fn runtime(&self) -> &Path {
        self.directory.path()
    }
    #[allow(dead_code)]
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        isolated(&mut command, self.runtime())
            .env("WAYLAND_DISPLAY", &self.display)
            .env("NIRI_SOCKET", &self.niri_socket);
        command
    }
}
