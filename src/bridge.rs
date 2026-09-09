// SPDX-License-Identifier: GPL-3.0-or-later
//! Two-device bridge coordinator. Keyboard data is never formatted for logs.
use crate::{
    activity::{ActivityMonitor, ActivitySource},
    capture_probe::{self, Event, StreamContext, StreamEvent},
    control::{self, LayoutMessage, Reply},
    geometry::{Boundary, Rect},
    input::StopReason,
    manager, niri,
    pointer::Hybrid,
    protocol::{self, EdgePosition, InputEvent, Message},
    receiver::{InputSink, Receiver},
    session::{Monitor, Status},
    transport::{self, Identity},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpListener,
    sync::{mpsc, watch},
    time::timeout,
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub certificate: PathBuf,
    pub private_key: PathBuf,
    pub peer_certificate: PathBuf,
    pub peer_name: String,
    pub connection: Connection,
    #[serde(alias = "edge", deserialize_with = "deserialize_edges")]
    pub edges: Vec<EdgeConfig>,
    pub activity_devices: Vec<PathBuf>,
    #[serde(default)]
    pub native_touchpads: bool,
    #[serde(skip)]
    pub source_path: Option<PathBuf>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Connection {
    Listen { address: String },
    Connect { address: String },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeConfig {
    #[serde(default = "default_edge_id")]
    pub id: String,
    pub output: String,
    pub boundary: Boundary,
}

pub const MAX_EDGES: usize = 16;

fn default_edge_id() -> String {
    "default".into()
}

fn deserialize_edges<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<EdgeConfig>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Edges {
        One(EdgeConfig),
        Many(Vec<EdgeConfig>),
    }
    Ok(match Edges::deserialize(deserializer)? {
        Edges::One(edge) => vec![edge],
        Edges::Many(edges) => edges,
    })
}

pub fn valid_edge_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}

pub fn validate_edges(edges: &[EdgeConfig]) -> Result<()> {
    ensure!(
        !edges.is_empty() && edges.len() <= MAX_EDGES,
        "layout_count_invalid"
    );
    let mut ids = BTreeSet::new();
    for (index, edge) in edges.iter().enumerate() {
        ensure!(
            valid_edge_id(&edge.id) && ids.insert(&edge.id),
            "layout_ids_invalid"
        );
        ensure!(
            !edge.output.is_empty()
                && edge.output.len() <= 128
                && !edge.output.chars().any(char::is_control),
            "layout_invalid"
        );
        edge.boundary.validate().context("layout_invalid")?;
        for previous in &edges[..index] {
            ensure!(
                edge.output != previous.output
                    || edge.boundary.edge != previous.boundary.edge
                    || edge.boundary.start >= previous.boundary.end
                    || previous.boundary.start >= edge.boundary.end,
                "layout_overlap"
            );
        }
    }
    Ok(())
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let mut value: Self = toml::from_str(&std::fs::read_to_string(path)?)?;
        value.source_path = Some(path.to_path_buf());
        let parent = path.parent().unwrap_or(Path::new("."));
        for file in [
            &mut value.certificate,
            &mut value.private_key,
            &mut value.peer_certificate,
        ] {
            if file.is_relative() {
                *file = parent.join(&*file);
            }
        }
        validate_edges(&value.edges)?;
        ensure!(
            !value.activity_devices.is_empty() && value.activity_devices.len() <= 32,
            "Configure physical input devices for takeover detection"
        );
        Ok(value)
    }
}

pub fn run(config: Config) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(run_async(config))
}

async fn shutdown() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("signal handler");
    tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
}
fn unlocked(status: &watch::Receiver<Status>) -> bool {
    *status.borrow() == Status::Unlocked
}

fn connection_error_code(error: &anyhow::Error) -> &'static str {
    if error.chain().any(|cause| {
        let tls_error = cause.downcast_ref::<rustls::Error>().or_else(|| {
            cause
                .downcast_ref::<std::io::Error>()?
                .get_ref()?
                .downcast_ref::<rustls::Error>()
        });
        matches!(
            tls_error,
            Some(
                rustls::Error::NoApplicationProtocol
                    | rustls::Error::AlertReceived(rustls::AlertDescription::NoApplicationProtocol)
            )
        )
    }) {
        "protocol_mismatch"
    } else if error.chain().any(|cause| {
        let text = cause.to_string().to_ascii_lowercase();
        text.contains("certificate") || text.contains("certificat")
    }) {
        "authentication_failed"
    } else {
        "connection_failed"
    }
}

async fn run_async(mut config: Config) -> Result<()> {
    let (_control_server, control) =
        control::Server::start(control::Snapshot::new(config.peer_name.clone()))?;
    let session_id =
        std::env::var("XDG_SESSION_ID").context("Run inside the Niri graphical session")?;
    let mut monitor = Monitor::start(session_id);
    let local = Identity::load(&config.certificate, &config.private_key)?;
    let peer = transport::load_certificate(&config.peer_certificate)?;
    let client = transport::client_config(&local, peer.clone())?;
    let server = transport::server_config(&local, peer)?;
    let mut activity = ActivityMonitor::new(&config.activity_devices, config.native_touchpads)?;
    let listener = if let Connection::Listen { address } = &config.connection {
        Some(TcpListener::bind(address).await?)
    } else {
        None
    };
    let mut last_error = String::new();
    let exit_signal = shutdown();
    tokio::pin!(exit_signal);
    loop {
        if let Some(path) = &config.source_path {
            config = Config::load(path)?;
        }
        let desktop = control::Desktop::read(&config).ok();
        control.update(|s| {
            s.config_path = config
                .source_path
                .as_ref()
                .and_then(|p| p.canonicalize().ok());
            s.local = desktop;
            s.peer = None;
            s.role = "local".into();
            s.configuring = false;
            s.capture_ready = false;
            s.peer_unlocked = None;
            s.latency_ms = None;
        });
        while !unlocked(&monitor.status) {
            control.update(|s| {
                s.connection = "paused".into();
                s.local_unlocked = false;
                s.reason = Some("session_locked".into());
            });
            tokio::select! {_=&mut exit_signal=>return Ok(()),r=monitor.status.changed()=>{r?;}}
        }
        control.update(|s| {
            s.connection = match config.connection {
                Connection::Listen { .. } => "waiting",
                Connection::Connect { .. } => "connecting",
            }
            .into();
            s.reason = None;
            s.local_unlocked = true;
        });
        let connected = async {
            match &config.connection {
                Connection::Listen { .. } => {
                    let (stream, _) = listener.as_ref().unwrap().accept().await?;
                    let tls = transport::accept(stream, server.clone()).await?;
                    Ok::<tokio_rustls::TlsStream<_>, anyhow::Error>(tls.into())
                }
                Connection::Connect { address } => {
                    Ok(
                        transport::connect(address, &config.peer_name, client.clone())
                            .await?
                            .into(),
                    )
                }
            }
        };
        let result = tokio::select! {_=&mut exit_signal=>return Ok(()),result=connected=>result};
        let result = match result {
            Ok(mut stream) => {
                let session = async {
                    transport::hello(&mut stream).await?;
                    run_session(
                        stream,
                        &config,
                        &mut activity,
                        monitor.status.clone(),
                        &control,
                    )
                    .await
                };
                tokio::select! {_=&mut exit_signal=>return Ok(()),result=session=>result}
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => {
                control.update(|s| {
                    s.connection = "problem".into();
                    s.role = "local".into();
                    s.configuring = false;
                    s.reason = Some(connection_error_code(&error).into());
                });
                if error.downcast_ref::<CaptureReleaseFailed>().is_some() {
                    return Err(error);
                }
                let message = error.to_string();
                if message != last_error {
                    eprintln!("Sharing paused: {message}. Retrying the paired connection.");
                    last_error = message;
                }
            }
        }
        tokio::select! {_=&mut exit_signal=>return Ok(()),_=tokio::time::sleep(Duration::from_secs(2))=>{}}
    }
}

#[derive(Debug)]
struct CaptureReleaseFailed;
impl std::fmt::Display for CaptureReleaseFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Capture backend did not release in time; exiting to restore local input"
        )
    }
}
impl std::error::Error for CaptureReleaseFailed {}

pub(crate) struct CaptureTask {
    cancel: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<Result<capture_probe::Counts>>>,
}

impl CaptureTask {
    pub(crate) fn start(
        edges: &[EdgeConfig],
        generation: u64,
        sender: mpsc::Sender<StreamEvent>,
    ) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let options = edges
            .iter()
            .map(|edge| capture_probe::Options {
                output: edge.output.clone(),
                edge: edge.boundary.edge,
                start: edge.boundary.start,
                end: edge.boundary.end,
                seconds: 15,
            })
            .collect();
        let context = StreamContext {
            generation,
            sender,
            cancel: cancel.clone(),
        };
        let handle = std::thread::spawn(move || capture_probe::worker_stream(options, context));
        Self {
            cancel,
            handle: Some(handle),
        }
    }

    pub(crate) async fn stop(mut self) -> Result<()> {
        self.cancel.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !self.handle.as_ref().unwrap().is_finished() {
            if Instant::now() > deadline {
                return Err(CaptureReleaseFailed.into());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.handle
            .take()
            .unwrap()
            .join()
            .map_err(|_| anyhow::anyhow!("Capture worker failed"))??;
        Ok(())
    }
    fn finished(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| h.is_finished())
    }
}
impl Drop for CaptureTask {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

async fn stop_capture(capture: &mut Option<CaptureTask>) -> Result<()> {
    if let Some(task) = capture.take() {
        task.stop().await?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Role {
    Local,
    Sending {
        session: u64,
        sequence: u64,
        edge: usize,
        fraction: f64,
    },
    Receiving {
        session: u64,
    },
}
impl Role {
    fn session(&self) -> Option<u64> {
        match *self {
            Self::Local => None,
            Self::Sending { session, .. } | Self::Receiving { session } => Some(session),
        }
    }
}

/// Returns true when the source's emergency chord must stop sharing immediately.
async fn forward_keyboard(
    role: &mut Role,
    held: &mut BTreeSet<u16>,
    writer: &mut (impl AsyncWrite + Unpin),
    event: InputEvent,
) -> Result<bool> {
    let Role::Sending {
        session, sequence, ..
    } = role
    else {
        return Ok(false);
    };
    let InputEvent::Key { code, pressed } = event else {
        anyhow::bail!("Physical keyboard returned a non-key event");
    };
    if pressed {
        held.insert(code);
    } else {
        held.remove(&code);
    }
    if code == 1 && pressed && crate::input::emergency_modifiers(held) {
        return Ok(true);
    }
    send(
        writer,
        &Message::Input {
            session: *session,
            sequence: *sequence,
            event,
        },
    )
    .await?;
    *sequence = sequence
        .checked_add(1)
        .context("Input sequence exhausted")?;
    Ok(false)
}
struct ReaderTask(tokio::task::JoinHandle<()>);
impl Drop for ReaderTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn send(writer: &mut (impl AsyncWrite + Unpin), message: &Message) -> Result<()> {
    timeout(
        Duration::from_millis(250),
        protocol::write_frame(writer, message),
    )
    .await
    .context("Peer is too slow to receive input safely")??;
    Ok(())
}

fn placement(edge: &EdgeConfig, fraction: f64) -> Result<InputEvent> {
    let output = niri::outputs()?
        .remove(&edge.output)
        .context("Boundary output disappeared")?;
    let logical = output.logical.context("Boundary output is disabled")?;
    let rect = Rect {
        x: 0.0,
        y: 0.0,
        width: f64::from(logical.width),
        height: f64::from(logical.height),
    };
    let (x, y) = edge.boundary.point_at(rect, fraction, 4.0)?;
    Ok(InputEvent::Absolute {
        x: x as u32,
        y: y as u32,
        width: logical.width,
        height: logical.height,
    })
}

fn place<S: InputSink>(receiver: &mut Receiver<S>, edge: &EdgeConfig, fraction: f64) -> Result<()> {
    receiver.position_on(&edge.output, &placement(edge, fraction)?)
}

fn active_edges(local: &control::Desktop, peer: Option<&control::Desktop>) -> Vec<usize> {
    local
        .edges
        .iter()
        .enumerate()
        .filter_map(|(index, edge)| {
            (local.outputs.contains_key(&edge.output)
                && peer.is_some_and(|peer| {
                    peer.edges.iter().any(|other| {
                        other.id == edge.id && peer.outputs.contains_key(&other.output)
                    })
                }))
            .then_some(index)
        })
        .collect()
}

fn wayland_socket() -> Result<PathBuf> {
    let display = std::env::var_os("WAYLAND_DISPLAY").context("WAYLAND_DISPLAY is not set")?;
    let path = PathBuf::from(display);
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(PathBuf::from(
            std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?,
        )
        .join(path))
    }
}

async fn run_session<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    stream: S,
    config: &Config,
    activity: &mut ActivityMonitor,
    status: watch::Receiver<Status>,
    control: &control::Control,
) -> Result<bool> {
    let outputs = niri::outputs()?;
    let output = if outputs
        .get(&config.edges[0].output)
        .is_some_and(|o| o.logical.is_some())
    {
        config.edges[0].output.clone()
    } else {
        outputs
            .into_iter()
            .find_map(|(name, o)| o.logical.map(|_| name))
            .context("No active output is available")?
    };
    let sink = Hybrid::connect(&wayland_socket()?, &output)?;
    coordinate_with_control(stream, config, activity, status, sink, Some(control)).await
}

/// Backend-independent coordinator, also exercised by isolated integration tests.
pub async fn coordinate<
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    A: ActivitySource,
    B: InputSink,
>(
    stream: S,
    config: &Config,
    activity: &mut A,
    status: watch::Receiver<Status>,
    sink: B,
) -> Result<bool> {
    coordinate_with_control(stream, config, activity, status, sink, None).await
}

struct PendingLayout {
    id: String,
    prepared: manager::Prepared,
    reply: Option<tokio::sync::oneshot::Sender<Reply>>,
    deadline: Instant,
}

pub async fn coordinate_with_control<
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    A: ActivitySource,
    B: InputSink,
>(
    stream: S,
    config: &Config,
    activity: &mut A,
    mut status: watch::Receiver<Status>,
    sink: B,
    control: Option<&control::Control>,
) -> Result<bool> {
    let config = config.clone();
    validate_edges(&config.edges)?;
    let mut desktop = control::Desktop::read(&config)?;
    let mut peer_desktop: Option<control::Desktop> = None;
    let mut pending_layout: Option<PendingLayout> = None;
    let mut pings = std::collections::BTreeMap::new();
    let mut receiver = Receiver::new(sink);
    let local_touchpads = activity.prepare_touchpads()?;
    let physical_keyboard = activity.keyboard_state().is_some();
    let mut source_keys = BTreeSet::new();
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (incoming_tx, mut incoming) = mpsc::channel(512);
    let reader_task = ReaderTask(tokio::spawn(async move {
        loop {
            let frame = timeout(Duration::from_secs(2), protocol::read_frame(&mut reader)).await;
            let Ok(Ok(message)) = frame else {
                break;
            };
            if incoming_tx.send((Instant::now(), message)).await.is_err() {
                break;
            }
        }
    }));
    let (capture_tx, mut capture_events) = mpsc::channel(512);
    let mut capture: Option<CaptureTask> = None;
    let mut capture_armed = false;
    let mut capture_edges = Vec::new();
    let mut desktop_epoch = 0u64;
    let mut capture_epoch = 0u64;
    let mut generation = 0u64;
    let mut next_session = 1u64;
    let mut role = Role::Local;
    let mut local_ready = unlocked(&status);
    let mut peer_ready = false;
    let mut peer_touchpads_configured = false;
    let mut timer = tokio::time::interval(Duration::from_millis(16));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_heartbeat = Instant::now();
    let mut heartbeat = 0u64;
    let mut rearm_at = Instant::now();
    receiver.local_lock(!local_ready)?;
    send(
        &mut writer,
        &Message::Desktop {
            info: desktop.clone(),
        },
    )
    .await?;
    send(
        &mut writer,
        &Message::Touchpads {
            devices: local_touchpads,
        },
    )
    .await?;
    send(
        &mut writer,
        &Message::LockState {
            locked: !local_ready,
        },
    )
    .await?;
    eprintln!(
        "Paired connection authenticated. Keyboard follows the pointer. Emergency return: Ctrl+Alt+Shift+Escape."
    );
    if let Some(c) = control {
        c.update(|s| {
            s.connection = "connected".into();
            s.config_path = config
                .source_path
                .as_ref()
                .and_then(|p| p.canonicalize().ok());
            s.reason = None;
            s.local = Some(desktop.clone());
        });
    }
    let exit_signal = shutdown();
    tokio::pin!(exit_signal);
    let outcome=async {
        loop {
            let available_edges = active_edges(&desktop, peer_desktop.as_ref());
            let layout_ready = !available_edges.is_empty();
            if (!layout_ready && role.session().is_some()) || (capture.is_some() && capture_epoch != desktop_epoch) {
                if let Some(session) = role.session() {
                    receiver.end(session)?;
                    send(
                        &mut writer,
                        &Message::End {
                            session,
                            exit: None,
                        },
                    )
                    .await?;
                }
                stop_capture(&mut capture).await?;
                if let Role::Sending{edge,fraction,..}=role && local_ready && desktop.outputs.contains_key(&config.edges[edge].output) {
                    place(&mut receiver, &config.edges[edge], fraction)?;
                }
                role = Role::Local;
                rearm_at=Instant::now()+Duration::from_millis(150);
            }
            if let Some(c)=control{c.update(|s|{
                s.role=match role{Role::Local=>"local",Role::Sending{..}=>"sending",Role::Receiving{..}=>"receiving"}.into();
                s.local_unlocked=local_ready;
                s.reason=if !layout_ready&&peer_desktop.is_some(){
                    let missing_output=desktop.edges.iter().any(|edge|!desktop.outputs.contains_key(&edge.output))
                        ||peer_desktop.as_ref().is_some_and(|peer|peer.edges.iter().any(|edge|!peer.outputs.contains_key(&edge.output)));
                    Some(if missing_output{"output_unavailable"}else{"layout_ids_invalid"}.into())
                }else{None};
                s.configuring=pending_layout.is_some();
                s.capture_ready=capture.is_some()&&capture_armed;
            });}
            for event in activity.route_touchpads(matches!(role,Role::Sending{..}))? {
                if let Role::Sending{session,sequence,..}=&mut role {
                    send(&mut writer,&Message::Input{session:*session,sequence:*sequence,event}).await?;
                    *sequence=sequence.checked_add(1).context("Input sequence exhausted")?;
                }
            }
            if local_ready&&peer_ready&&layout_ready&&pending_layout.is_none()&&capture.is_none()&&Instant::now()>=rearm_at {
                generation=generation.wrapping_add(1);
                capture_edges=available_edges.clone();
                capture_epoch=desktop_epoch;
                capture_armed=false;
                let edges=capture_edges.iter().map(|&index|config.edges[index].clone()).collect::<Vec<_>>();
                capture=Some(CaptureTask::start(&edges,generation,capture_tx.clone()));
            }
            tokio::select! {
                biased;
                _=&mut exit_signal=>return Ok(true),
                outputs=async {match control{Some(c)=>c.next_outputs().await,None=>std::future::pending().await}}=>{
                    if let Some(outputs)=outputs {
                        desktop_epoch=desktop_epoch.wrapping_add(1);
                        desktop.outputs=outputs;
                        if let Some(c)=control{c.update(|s|s.local=Some(desktop.clone()));}
                        send(&mut writer,&Message::Desktop{info:desktop.clone()}).await?;
                    }
                }
                command=async {match control{Some(c)=>c.next().await,None=>std::future::pending().await}}=>{
                    if let Some(command)=command {
                        match command.request {
                            control::Request::Release{..}=>{
                                if let Some(session)=role.session(){receiver.end(session)?;send(&mut writer,&Message::End{session,exit:None}).await?;}
                                stop_capture(&mut capture).await?;
                                if let Role::Sending{edge,fraction,..}=role{place(&mut receiver,&config.edges[edge],fraction)?;}
                                role=Role::Local;rearm_at=Instant::now()+Duration::from_millis(150);
                                let _=command.reply.send(Reply::success(serde_json::json!({})));
                            }
                            control::Request::ApplyLayout{layout,..}=>{
                                let prepared=(||->Result<_>{
                                    ensure!(pending_layout.is_none(),"settings_busy");ensure!(local_ready&&peer_ready,"session_locked");layout.validate()?;
                                    let peer=peer_desktop.as_ref().context("peer_offline")?;ensure!(peer.revision==layout.peer_revision,"settings_conflict");
                                    let path=config.source_path.as_ref().context("config_invalid")?;manager::prepare_edges(path,&layout.local_revision,&layout.local_edges)
                                })();
                                match prepared {
                                    Ok(prepared)=>{
                                        if let Some(session)=role.session(){receiver.end(session)?;send(&mut writer,&Message::End{session,exit:None}).await?;}
                                        stop_capture(&mut capture).await?;role=Role::Local;
                                        let id=control::transaction_id()?;
                                        pending_layout=Some(PendingLayout{id:id.clone(),prepared,reply:Some(command.reply),deadline:Instant::now()+Duration::from_secs(6)});
                                        send(&mut writer,&Message::Layout{message:LayoutMessage::Prepare{id,edges:layout.peer_edges,revision:layout.peer_revision}}).await?;
                                    }
                                    Err(error)=>{let _=command.reply.send(Reply::failure(manager::error_code(&error)));}
                                }
                            }
                            control::Request::Status=>{}
                        }
                    }
                }
                changed=status.changed()=>{
                    changed?;
                    local_ready=unlocked(&status);
                    receiver.local_lock(!local_ready)?;
                    send(&mut writer,&Message::LockState{locked:!local_ready}).await?;
                    if !local_ready {
                        activity.suspend();
                        if let Some(session)=role.session(){send(&mut writer,&Message::End{session,exit:None}).await?;}
                        role=Role::Local;stop_capture(&mut capture).await?;
                        eprintln!("Sharing paused while the graphical session is locked or unavailable.");
                    }
                }
                ready=async {
                    tokio::select!{
                        result=activity.wait_input(),if local_ready&&peer_ready=>result,
                        _=timer.tick()=>Ok(()),
                    }
                }=>{
                    ready?;
                    if pending_layout.as_ref().is_some_and(|p|Instant::now()>=p.deadline) {
                        let mut pending=pending_layout.take().unwrap();
                        send(&mut writer,&Message::Layout{message:LayoutMessage::Abort{id:pending.id.clone()}}).await?;
                        if let Some(reply)=pending.reply.take(){let _=reply.send(Reply::failure("layout_unconfirmed"));}
                    }
                    let physical=if local_ready&&peer_ready{activity.sample()?}else{false};
                    let keyboard_events = activity.take_keyboard_events();
                    let touchpad_events = activity.take_touchpad_events();
                    if physical&&matches!(role,Role::Receiving{..}) {
                        receiver.physical_activity()?;
                        if let Some(session)=role.session(){send(&mut writer,&Message::End{session,exit:None}).await?;}
                        role=Role::Local;stop_capture(&mut capture).await?;rearm_at=Instant::now()+Duration::from_millis(150);
                        eprintln!("Physical input took back local control.");
                    }
                    for event in keyboard_events {
                        if forward_keyboard(&mut role, &mut source_keys, &mut writer, event).await? {
                            if let Role::Sending{session,edge,fraction,..}=role {
                                send(&mut writer,&Message::End{session,exit:None}).await?;
                                stop_capture(&mut capture).await?;
                                place(&mut receiver,&config.edges[edge],fraction)?;
                            }
                            role=Role::Local;source_keys.clear();rearm_at=Instant::now()+Duration::from_millis(150);
                            break;
                        }
                    }
                    for event in touchpad_events {
                        if let Role::Sending{session,sequence,..}=&mut role {
                            send(&mut writer,&Message::Input{session:*session,sequence:*sequence,event}).await?;
                            *sequence=sequence.checked_add(1).context("Input sequence exhausted")?;
                        }
                    }
                    if capture.as_ref().is_some_and(CaptureTask::finished)&&capture_events.is_empty() {
                        stop_capture(&mut capture).await?;
                        if let Role::Sending{session,..}=role {send(&mut writer,&Message::End{session,exit:None}).await?;role=Role::Local;}
                        rearm_at=Instant::now()+Duration::from_millis(150);
                    }
                    if last_heartbeat.elapsed()>=Duration::from_millis(200) {
                        pings.insert(heartbeat,Instant::now());if pings.len()>16{pings.pop_first();}
                        send(&mut writer,&Message::Ping{nonce:heartbeat}).await?;
                        heartbeat=heartbeat.wrapping_add(1);last_heartbeat=Instant::now();
                    }
                }
                event=capture_events.recv()=>{
                    let event=event.context("Capture event channel closed")?;
                    if event.generation!=generation||capture.is_none(){continue;}
                    match event.event {
                        Event::Started{edge,fraction}=>{
                            let edge=*capture_edges.get(edge).context("Capture selected an unknown edge")?;
                            ensure!(available_edges.contains(&edge), "Capture edge is no longer available");
                            let selected=&config.edges[edge];
                            match role {
                            Role::Local if local_ready&&peer_ready=>{
                                activity.sample()?;
                                let initial_keys=activity.keyboard_state().unwrap_or_default();
                                activity.take_keyboard_events();
                                if initial_keys.contains(&1)&&crate::input::emergency_modifiers(&initial_keys) {
                                    stop_capture(&mut capture).await?;rearm_at=Instant::now()+Duration::from_millis(150);continue;
                                }
                                let session=next_session;next_session=next_session.checked_add(1).context("Session identifiers exhausted")?;
                                send(&mut writer,&Message::Begin{session,entry:EdgePosition{edge_id:selected.id.clone(),fraction}}).await?;
                                role=Role::Sending{session,sequence:0,edge,fraction};
                                source_keys.clear();
                                for code in initial_keys {
                                    forward_keyboard(&mut role,&mut source_keys,&mut writer,InputEvent::Key{code,pressed:true}).await?;
                                }
                                eprintln!("Controlling the paired computer.");
                            }
                            Role::Receiving{session}=>{
                                receiver.end(session)?;
                                send(&mut writer,&Message::End{session,exit:Some(EdgePosition{edge_id:selected.id.clone(),fraction})}).await?;
                                role=Role::Local;stop_capture(&mut capture).await?;
                                place(&mut receiver,selected,fraction)?;
                                rearm_at=Instant::now()+Duration::from_millis(150);
                            }
                            _=>{stop_capture(&mut capture).await?;}
                            }
                        },
                        Event::Input(event_input)=>{
                            if physical_keyboard&&matches!(event_input,InputEvent::Key{..}) {continue;}
                            if let Role::Sending{session,sequence,..}=&mut role {
                                ensure!(event.captured_at.elapsed()<Duration::from_millis(200),"Input queue became too delayed; restoring local control");
                                send(&mut writer,&Message::Input{session:*session,sequence:*sequence,event:event_input}).await?;
                                *sequence=sequence.checked_add(1).context("Input sequence exhausted")?;
                            }
                        }
                        Event::Finished{reason}=>{
                            let previous=role;
                            if let Some(session)=previous.session() {
                                receiver.end(session)?;
                                send(&mut writer,&Message::End{session,exit:None}).await?;
                            }
                            role=Role::Local;stop_capture(&mut capture).await?;
                            if let Role::Sending{edge,fraction,..}=previous && local_ready&&reason=="escape"{place(&mut receiver,&config.edges[edge],fraction)?;}
                            if reason=="unsupported_input"{eprintln!("Sharing stopped: this input is not supported by the current backend.");}
                            rearm_at=Instant::now()+Duration::from_millis(150);
                        }
                        Event::Armed=>{capture_armed=true;}
                        Event::Ready=>{}
                    }
                }
                frame=incoming.recv()=>{
                    let (received_at,message)=frame.context("Paired connection closed or timed out")?;
                    match message {
                        Message::Desktop{info}=>{desktop_epoch=desktop_epoch.wrapping_add(1);peer_desktop=Some(info.clone());if let Some(c)=control{c.update(|s|s.peer=Some(info));}},
                        Message::Layout{message}=>match message {
                            LayoutMessage::Prepare{id,edges,revision}=>{
                                let prepared=(||->Result<_>{ensure!(pending_layout.is_none(),"settings_busy");ensure!(local_ready&&peer_ready,"session_locked");manager::prepare_edges(config.source_path.as_ref().context("config_invalid")?,&revision,&edges)})();
                                match prepared {
                                    Ok(prepared)=>{
                                        if let Some(session)=role.session(){receiver.end(session)?;send(&mut writer,&Message::End{session,exit:None}).await?;}
                                        stop_capture(&mut capture).await?;role=Role::Local;
                                        pending_layout=Some(PendingLayout{id:id.clone(),prepared,reply:None,deadline:Instant::now()+Duration::from_secs(6)});
                                        send(&mut writer,&Message::Layout{message:LayoutMessage::Prepared{id,ok:true,error:None}}).await?;
                                    }
                                    Err(error)=>send(&mut writer,&Message::Layout{message:LayoutMessage::Prepared{id,ok:false,error:Some(manager::error_code(&error).into())}}).await?,
                                }
                            }
                            LayoutMessage::Prepared{id,ok,error}=>{
                                if pending_layout.as_ref().is_some_and(|p|p.id==id&&p.reply.is_some()) {
                                    let mut pending=pending_layout.take().unwrap();
                                    let committed=if ok{pending.prepared.commit()}else{Err(anyhow::anyhow!(error.unwrap_or_else(||"operation_failed".into())))};
                                    if let Err(error)=committed{
                                        send(&mut writer,&Message::Layout{message:LayoutMessage::Abort{id}}).await?;
                                        if let Some(reply)=pending.reply.take(){let _=reply.send(Reply::failure(manager::error_code(&error)));}
                                    }else{
                                        pending_layout=Some(pending);send(&mut writer,&Message::Layout{message:LayoutMessage::Commit{id}}).await?;
                                    }
                                }
                            }
                            LayoutMessage::Commit{id}=>{
                                if pending_layout.as_ref().is_some_and(|p|p.id==id&&p.reply.is_none()) {
                                    let mut pending=pending_layout.take().unwrap();
                                    match pending.prepared.commit(){
                                        Ok(())=>{
                                            let revision=pending.prepared.after_revision.clone();pending.prepared.finish();
                                            send(&mut writer,&Message::Layout{message:LayoutMessage::Result{id,ok:true,revision:Some(revision),error:None}}).await?;
                                            return Ok(false);
                                        }
                                        Err(error)=>send(&mut writer,&Message::Layout{message:LayoutMessage::Result{id,ok:false,revision:None,error:Some(manager::error_code(&error).into())}}).await?,
                                    }
                                }
                            }
                            LayoutMessage::Result{id,ok,revision,error}=>{
                                if pending_layout.as_ref().is_some_and(|p|p.id==id&&p.reply.is_some()) {
                                    let mut pending=pending_layout.take().unwrap();
                                    if ok{
                                        let local_revision=pending.prepared.after_revision.clone();pending.prepared.finish();
                                        if let Some(reply)=pending.reply.take(){let _=reply.send(Reply::success(serde_json::json!({"local_revision":local_revision,"peer_revision":revision,"reconnecting":true})));}
                                        return Ok(false);
                                    }else if let Some(reply)=pending.reply.take(){let _=reply.send(Reply::failure(error.as_deref().unwrap_or("operation_failed")));}
                                }
                            }
                            LayoutMessage::Abort{id}=>{if pending_layout.as_ref().is_some_and(|p|p.id==id){let mut pending=pending_layout.take().unwrap();if let Some(reply)=pending.reply.take(){let _=reply.send(Reply::failure("layout_unconfirmed"));}}}
                        },
                        Message::Touchpads{devices}=>{
                            ensure!(!peer_touchpads_configured,"Touchpad negotiation was repeated");
                            receiver.configure_touchpads(&devices)?;peer_touchpads_configured=true;
                        }
                        Message::Hello{..}=>anyhow::bail!("Unexpected repeated protocol handshake"),
                        Message::Ping{nonce}=>send(&mut writer,&Message::Pong{nonce}).await?,
                        Message::Pong{nonce}=>{if let Some(sent)=pings.remove(&nonce)&&let Some(c)=control{c.update(|s|s.latency_ms=Some(sent.elapsed().as_secs_f64()*1000.));}},
                        Message::LockState{locked}=>{
                            ensure!(peer_touchpads_configured,"Touchpad negotiation is incomplete");
                            peer_ready = !locked;receiver.peer_lock(locked)?;if let Some(c)=control{c.update(|s|s.peer_unlocked=Some(peer_ready));}
                            if locked {
                                activity.suspend();
                                if let Some(session)=role.session(){send(&mut writer,&Message::End{session,exit:None}).await?;}
                                if let Role::Sending{edge,fraction,..}=role {stop_capture(&mut capture).await?;if local_ready{place(&mut receiver,&config.edges[edge],fraction)?;}}
                                role=Role::Local;stop_capture(&mut capture).await?;
                            }
                        }
                        Message::Begin{session,entry}=>{
                            let edge=config.edges.iter().enumerate().find(|(index,edge)|edge.id==entry.edge_id&&available_edges.contains(index));
                            if let Some((_,edge))=edge && matches!(role,Role::Local)&&local_ready&&peer_ready&&layout_ready&&pending_layout.is_none()&&!activity.sample()? {
                                ensure!(receiver.begin(session)?,"Receiver is busy");
                                place(&mut receiver,edge,entry.fraction)?;
                                role=Role::Receiving{session};eprintln!("The paired computer is controlling this desktop.");
                            }else if matches!(role,Role::Receiving{..}){anyhow::bail!("Unexpected overlapping input session");}
                            else {send(&mut writer,&Message::End{session,exit:None}).await?;}
                        }
                        Message::Input{session,sequence,event}=>{
                            ensure!(received_at.elapsed()<Duration::from_millis(200),"Received input was delayed; restoring local control");
                            if let Role::Receiving{session:current}=role && current==session {
                                if activity.sample()? {
                                    receiver.physical_activity()?;send(&mut writer,&Message::End{session,exit:None}).await?;
                                    role=Role::Local;stop_capture(&mut capture).await?;
                                }else{receiver.input(session,sequence,&event)?;}
                            }
                        }
                        Message::End{session,exit}=>{
                            if role.session()==Some(session) {
                                receiver.end(session)?;stop_capture(&mut capture).await?;
                                if let Role::Sending{edge,fraction,..}=role && local_ready {
                                    let (edge,fraction)=match exit {
                                        Some(exit)=>{
                                            let (index,edge)=config.edges.iter().enumerate().find(|(_,edge)|edge.id==exit.edge_id).context("Unknown return edge")?;
                                            ensure!(available_edges.contains(&index), "Return edge is unavailable");
                                            (edge,exit.fraction)
                                        }
                                        None=>(&config.edges[edge],fraction),
                                    };
                                    place(&mut receiver,edge,fraction)?;
                                }
                                role=Role::Local;rearm_at=Instant::now()+Duration::from_millis(150);eprintln!("Local control restored.");
                            }
                        }
                    }
                }
            }
        }
    }.await;
    reader_task.0.abort();
    if let Some(control) = control {
        control.update(|state| state.capture_ready = false);
    }
    let release = receiver.stop(StopReason::Disconnected);
    activity.suspend();
    let capture_release = stop_capture(&mut capture).await;
    capture_release?;
    release?;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Edge;

    #[test]
    fn incompatible_peer_versions_are_reported_separately_from_connection_failures() {
        for error in [
            rustls::Error::NoApplicationProtocol,
            rustls::Error::AlertReceived(rustls::AlertDescription::NoApplicationProtocol),
        ] {
            let error =
                anyhow::Error::from(std::io::Error::other(error)).context("TLS handshake failed");
            assert_eq!(connection_error_code(&error), "protocol_mismatch");
        }
        assert_eq!(
            connection_error_code(&anyhow::anyhow!("connection refused")),
            "connection_failed"
        );
    }

    fn edge(id: &str, output: &str, side: Edge, start: f64, end: f64) -> EdgeConfig {
        EdgeConfig {
            id: id.into(),
            output: output.into(),
            boundary: Boundary {
                edge: side,
                start,
                end,
            },
        }
    }

    #[test]
    fn connection_ranges_reject_ambiguity_but_allow_adjacent_ranges_and_different_sides() {
        let first = edge("first", "screen", Edge::Top, 0., 0.5);
        assert!(validate_edges(&[]).is_err());
        assert!(
            validate_edges(&[first.clone(), edge("first", "screen", Edge::Left, 0., 1.)]).is_err()
        );
        assert!(
            validate_edges(&[first.clone(), edge("second", "screen", Edge::Top, 0.4, 0.9)])
                .is_err()
        );
        assert!(
            validate_edges(&[first.clone(), edge("second", "screen", Edge::Top, 0.5, 1.)]).is_ok()
        );
        assert!(
            validate_edges(&[first.clone(), edge("second", "screen", Edge::Left, 0., 1.)]).is_ok()
        );
        assert!(validate_edges(&[first, edge("second", "portrait", Edge::Top, 0., 1.)]).is_ok());
    }

    #[test]
    fn connections_match_by_id_and_an_unavailable_display_only_disables_its_own_route() {
        let output: niri::LogicalOutput = serde_json::from_value(serde_json::json!({
            "x":0,"y":0,"width":1920,"height":1200,"scale":1.6,"transform":"Normal"
        }))
        .unwrap();
        let local = control::Desktop {
            edges: vec![
                edge("first", "laptop", Edge::Top, 0., 1.),
                edge("side", "laptop", Edge::Left, 0., 1.),
            ],
            revision: "a".repeat(64),
            outputs: [("laptop".into(), output)].into(),
        };
        let mut peer = control::Desktop {
            edges: vec![
                edge("side", "portrait", Edge::Right, 0.7, 1.),
                edge("first", "landscape", Edge::Bottom, 0.1, 0.6),
            ],
            revision: "b".repeat(64),
            outputs: [("landscape".into(), output), ("portrait".into(), output)].into(),
        };
        assert_eq!(active_edges(&local, Some(&peer)), [0, 1]);
        let mut request = control::LayoutRequest {
            local_edges: local.edges.clone(),
            peer_edges: peer.edges.clone(),
            local_revision: local.revision.clone(),
            peer_revision: peer.revision.clone(),
        };
        assert!(request.validate().is_ok());
        request.peer_edges[0].id = "unmatched".into();
        assert!(request.validate().is_err());
        peer.outputs.remove("portrait");
        assert_eq!(active_edges(&local, Some(&peer)), [0]);
        assert!(active_edges(&local, None).is_empty());
    }
}
