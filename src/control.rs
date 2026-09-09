// SPDX-License-Identifier: GPL-3.0-or-later
//! Same-user Unix control API. It exposes state and bounded management actions, never input data.
use crate::{
    bridge::{Config, EdgeConfig},
    manager, niri,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Desktop {
    pub edge: EdgeConfig,
    pub revision: String,
    pub outputs: BTreeMap<String, niri::LogicalOutput>,
}
impl Desktop {
    pub fn read(config: &Config) -> Result<Self> {
        let outputs = niri::outputs()?
            .into_iter()
            .filter_map(|(name, o)| o.logical.map(|logical| (name, logical)))
            .collect();
        let revision = match &config.source_path {
            Some(path) => manager::revision(path)?,
            None => String::new(),
        };
        Ok(Self {
            edge: config.edge.clone(),
            revision,
            outputs,
        })
    }
    pub fn validate(&self) -> Result<()> {
        self.edge.boundary.validate()?;
        ensure!(
            self.outputs.len() <= 32 && self.revision.len() <= 64,
            "Invalid desktop metadata"
        );
        for (name, o) in &self.outputs {
            ensure!(
                name.len() <= 128
                    && o.width > 0
                    && o.width <= 32768
                    && o.height > 0
                    && o.height <= 32768
                    && o.scale.is_finite()
                    && o.scale > 0.,
                "Invalid desktop output"
            );
        }
        Ok(())
    }
}
#[derive(Clone, Serialize)]
pub struct Snapshot {
    pub pid: u32,
    pub config_path: Option<PathBuf>,
    pub version: &'static str,
    pub connection: String,
    pub reason: Option<String>,
    pub role: String,
    pub local_unlocked: bool,
    pub peer_unlocked: Option<bool>,
    pub peer_name: String,
    pub latency_ms: Option<f64>,
    pub local: Option<Desktop>,
    pub peer: Option<Desktop>,
    pub configuring: bool,
}
impl Snapshot {
    pub fn new(peer_name: String) -> Self {
        Self {
            pid: std::process::id(),
            config_path: None,
            version: env!("CARGO_PKG_VERSION"),
            connection: "starting".into(),
            reason: None,
            role: "local".into(),
            local_unlocked: false,
            peer_unlocked: None,
            peer_name,
            latency_ms: None,
            local: None,
            peer: None,
            configuring: false,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutRequest {
    pub local_edge: EdgeConfig,
    pub peer_edge: EdgeConfig,
    pub local_revision: String,
    pub peer_revision: String,
}
impl LayoutRequest {
    pub fn validate(&self) -> Result<()> {
        self.local_edge.boundary.validate()?;
        self.peer_edge.boundary.validate()?;
        ensure!(
            self.local_revision.len() == 64 && self.peer_revision.len() == 64,
            "Invalid layout revision"
        );
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Status,
    Release {
        #[serde(default)]
        config_path: Option<PathBuf>,
    },
    ApplyLayout {
        layout: LayoutRequest,
        #[serde(default)]
        config_path: Option<PathBuf>,
    },
}
#[derive(Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
impl Reply {
    pub fn success(data: serde_json::Value) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }
    pub fn failure(code: &str) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(code.into()),
        }
    }
}
pub struct Command {
    pub request: Request,
    pub reply: oneshot::Sender<Reply>,
}
pub struct Control {
    pub state: Arc<Mutex<Snapshot>>,
    commands: tokio::sync::Mutex<mpsc::Receiver<Command>>,
    outputs: tokio::sync::Mutex<
        tokio::sync::watch::Receiver<Option<BTreeMap<String, niri::LogicalOutput>>>,
    >,
    output_sender: tokio::sync::watch::Sender<Option<BTreeMap<String, niri::LogicalOutput>>>,
}
impl Control {
    pub fn update(&self, f: impl FnOnce(&mut Snapshot)) {
        if let Ok(mut s) = self.state.lock() {
            f(&mut s);
        }
    }
    pub async fn next(&self) -> Option<Command> {
        self.commands.lock().await.recv().await
    }
    pub async fn next_outputs(&self) -> Option<BTreeMap<String, niri::LogicalOutput>> {
        let mut receiver = self.outputs.lock().await;
        receiver.changed().await.ok()?;
        receiver.borrow().clone()
    }
}
pub struct Server {
    _lock: File,
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
    output_task: Option<tokio::task::JoinHandle<()>>,
}
pub fn socket_path() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("XDG_RUNTIME_DIR").context("Graphical runtime directory is missing")?,
    )
    .join("niri-bridge/control.sock"))
}
impl Server {
    pub fn start(snapshot: Snapshot) -> Result<(Self, Control)> {
        let (mut server, control) = Self::start_at(&socket_path()?, snapshot)?;
        let sender = control.output_sender.clone();
        server.output_task = Some(tokio::spawn(async move {
            loop {
                if let Ok(Ok(outputs)) = tokio::task::spawn_blocking(niri::outputs).await {
                    let outputs = outputs
                        .into_iter()
                        .filter_map(|(name, out)| out.logical.map(|value| (name, value)))
                        .collect::<BTreeMap<_, _>>();
                    sender.send_if_modified(|current| {
                        if current.as_ref() == Some(&outputs) {
                            false
                        } else {
                            *current = Some(outputs);
                            true
                        }
                    });
                }
                tokio::select! {_=tokio::time::sleep(Duration::from_secs(2))=>{},_=sender.closed()=>return}
            }
        }));
        Ok((server, control))
    }
    fn start_at(path: &Path, snapshot: Snapshot) -> Result<(Self, Control)> {
        let directory = path.parent().context("Invalid control path")?;
        if !directory.exists() {
            fs::create_dir(directory)?;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        }
        let m = fs::symlink_metadata(directory)?;
        ensure!(
            m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0,
            "Control directory must be private and user-owned"
        );
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(directory.join("instance.lock"))?;
        ensure!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "NiriBridge is already running"
        );
        if let Ok(m) = fs::symlink_metadata(path) {
            ensure!(
                m.file_type().is_socket() && m.uid() == unsafe { libc::geteuid() },
                "Existing control path was preserved"
            );
            fs::remove_file(path)?;
        }
        let listener = UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        let state = Arc::new(Mutex::new(snapshot));
        let (tx, rx) = mpsc::channel::<Command>(4);
        let (output_sender, outputs) = tokio::sync::watch::channel(None);
        let snapshot = state.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let state = snapshot.clone();
                let sender = tx.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, state, sender).await;
                });
            }
        });
        Ok((
            Self {
                _lock: lock,
                path: path.to_path_buf(),
                task,
                output_task: None,
            },
            Control {
                state,
                commands: tokio::sync::Mutex::new(rx),
                outputs: tokio::sync::Mutex::new(outputs),
                output_sender,
            },
        ))
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
        if let Some(task) = &self.output_task {
            task.abort();
        }
        let _ = fs::remove_file(&self.path);
    }
}
async fn serve(
    mut stream: UnixStream,
    state: Arc<Mutex<Snapshot>>,
    sender: mpsc::Sender<Command>,
) -> Result<()> {
    ensure!(
        stream.peer_cred()?.uid() == unsafe { libc::geteuid() },
        "Control client is not the current user"
    );
    let mut bytes = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(2),
        BufReader::new((&mut stream).take(16384)).read_until(b'\n', &mut bytes),
    )
    .await??;
    ensure!(bytes.last() == Some(&b'\n'), "Invalid control request");
    let request: Request = serde_json::from_slice(&bytes)?;
    let response = match request {
        Request::Status => Reply::success(serde_json::to_value(
            state
                .lock()
                .map_err(|_| anyhow::anyhow!("Status unavailable"))?
                .clone(),
        )?),
        _ => {
            let requested = match &request {
                Request::Release { config_path } | Request::ApplyLayout { config_path, .. } => {
                    config_path.as_ref()
                }
                Request::Status => None,
            };
            let context_matches = match requested {
                Some(path) => state
                    .lock()
                    .is_ok_and(|s| s.config_path.as_ref() == path.canonicalize().ok().as_ref()),
                None => true,
            };
            let ready = state.lock().is_ok_and(|s| s.connection == "connected");
            if !context_matches {
                Reply::failure("config_changed")
            } else if !ready {
                Reply::failure("peer_offline")
            } else {
                let (reply, wait) = oneshot::channel();
                if sender.try_send(Command { request, reply }).is_err() {
                    Reply::failure("busy")
                } else {
                    match tokio::time::timeout(Duration::from_secs(10), wait).await {
                        Ok(Ok(result)) => result,
                        _ => Reply::failure("layout_unconfirmed"),
                    }
                }
            }
        }
    };
    let mut encoded = serde_json::to_vec(&response)?;
    encoded.push(b'\n');
    tokio::time::timeout(Duration::from_secs(2), stream.write_all(&encoded)).await??;
    Ok(())
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum LayoutMessage {
    Prepare {
        id: String,
        edge: EdgeConfig,
        revision: String,
    },
    Prepared {
        id: String,
        ok: bool,
        error: Option<String>,
    },
    Commit {
        id: String,
    },
    Result {
        id: String,
        ok: bool,
        revision: Option<String>,
        error: Option<String>,
    },
    Abort {
        id: String,
    },
}
impl LayoutMessage {
    pub fn validate(&self) -> Result<()> {
        let id = match self {
            Self::Prepare { id, edge, revision } => {
                edge.boundary.validate()?;
                ensure!(revision.len() == 64, "Invalid layout revision");
                id
            }
            Self::Prepared { id, error, .. } | Self::Result { id, error, .. } => {
                ensure!(
                    error.as_ref().is_none_or(|e| e.len() <= 64),
                    "Invalid layout result"
                );
                id
            }
            Self::Commit { id } | Self::Abort { id } => id,
        };
        ensure!(
            id.len() == 32 && id.bytes().all(|c| c.is_ascii_hexdigit()),
            "Invalid layout transaction"
        );
        Ok(())
    }
}
pub fn transaction_id() -> Result<String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("Random source unavailable"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn private_control_socket_reports_state_and_requires_exclusive_instance() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.path().join("control.sock");
        let (server, control) = Server::start_at(&path, Snapshot::new("desktop".into())).unwrap();
        assert!(Server::start_at(&path, Snapshot::new("other".into())).is_err());
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        control.update(|s| {
            s.connection = "connected".into();
            s.role = "sending".into();
        });
        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream
            .write_all(b"{\"command\":\"status\"}\n")
            .await
            .unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).await.unwrap();
        let reply: Reply = serde_json::from_str(&reply).unwrap();
        assert!(reply.ok);
        assert_eq!(reply.data.unwrap()["role"], "sending");
        drop(server);
        assert!(!path.exists());
    }
    #[tokio::test]
    async fn control_acknowledges_release_only_after_the_coordinator_handles_it() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.path().join("control.sock");
        let (_server, control) = Server::start_at(&path, Snapshot::new("desktop".into())).unwrap();
        control.update(|s| s.connection = "connected".into());
        let mut stream = UnixStream::connect(path).await.unwrap();
        stream
            .write_all(b"{\"command\":\"release\"}\n")
            .await
            .unwrap();
        let command = control.next().await.unwrap();
        assert!(matches!(command.request, Request::Release { .. }));
        command
            .reply
            .send(Reply::success(serde_json::json!({"released":true})))
            .ok();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).await.unwrap();
        assert!(serde_json::from_str::<Reply>(&reply).unwrap().ok);
    }
}
