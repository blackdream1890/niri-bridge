// SPDX-License-Identifier: GPL-3.0-or-later
//! Opt-in live validation; probe keys are sent only after this application's capture is ready.
use crate::{
    bridge::{CaptureTask, Config, EdgeConfig},
    capture_probe::{Event, StreamEvent},
    doctor,
    geometry::{Boundary, Edge},
    niri,
    pointer::Pointer,
    protocol::InputEvent,
    receiver::{InputSink, Receiver},
    session::{self, Status},
    uinput::Keyboard,
};
use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Args)]
pub struct Options {
    #[arg(long)]
    pub config: PathBuf,
    /// The configured Niri overview chord, such as Super+Shift+O. Omit to skip this test.
    #[arg(long)]
    pub overview_shortcut: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Report {
    kernel_keyboard_verified: bool,
    pointer_motion_verified: bool,
    scroll_verified: bool,
    capture_released: bool,
    original_focus_restored: bool,
    shortcut_verified: Option<bool>,
    overview_restored: Option<bool>,
    temporary_keyboard_removed: bool,
}

pub fn run(options: Options) -> Result<Report> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("verify-input-worker")
        .arg("--config")
        .arg(&options.config);
    if let Some(chord) = options.overview_shortcut {
        command.arg("--overview-shortcut").arg(chord);
    }
    let output = doctor::bounded_output(&mut command, Duration::from_secs(25))?;
    Ok(serde_json::from_str(&output)?)
}

fn keyboard_count() -> usize {
    std::fs::read_dir("/sys/class/input")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("event"))
        .filter(|e| {
            std::fs::read_to_string(e.path().join("device/name"))
                .ok()
                .is_some_and(|s| s.trim() == crate::uinput::DEVICE_NAME)
        })
        .count()
}

fn layer_exists() -> Result<bool> {
    let reply = niri::query("Layers")?;
    let layers = reply
        .get("Layers")
        .and_then(|v| v.as_array())
        .context("Cannot inspect capture layers")?;
    Ok(layers.iter().any(|v| {
        v["namespace"]
            .as_str()
            .is_some_and(|s| s.starts_with("niri-bridge-"))
    }))
}

fn focus_id() -> Option<u64> {
    niri::query("FocusedWindow")
        .ok()?
        .get("FocusedWindow")?
        .get("id")?
        .as_u64()
}
fn overview() -> Result<bool> {
    niri::query("OverviewState")?
        .get("OverviewState")
        .and_then(|v| v.get("is_open"))
        .and_then(|v| v.as_bool())
        .context("Cannot read overview state")
}

fn action(arguments: &[&str]) -> Result<()> {
    ensure!(
        Command::new("niri")
            .args(["msg", "action"])
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success(),
        "Could not restore the Niri state"
    );
    Ok(())
}

fn chord(value: &str) -> Result<Vec<u16>> {
    let parts: Vec<_> = value.split('+').collect();
    ensure!(
        (2..=5).contains(&parts.len()),
        "Use a modifier chord for the overview test"
    );
    let mut codes = Vec::new();
    for (index, name) in parts.iter().enumerate() {
        let key = match name.to_ascii_lowercase().as_str() {
            "super" | "mod" => "KEY_LEFTMETA".to_owned(),
            "ctrl" | "control" => "KEY_LEFTCTRL".to_owned(),
            "alt" => "KEY_LEFTALT".to_owned(),
            "shift" => "KEY_LEFTSHIFT".to_owned(),
            _ => {
                ensure!(
                    index == parts.len() - 1,
                    "Only the last part may be a non-modifier key"
                );
                format!("KEY_{}", name.to_ascii_uppercase())
            }
        };
        let code = key
            .parse::<evdev::KeyCode>()
            .map_err(|_| anyhow::anyhow!("Unsupported shortcut key name"))?
            .code();
        ensure!(
            (1..=255).contains(&code) && !codes.contains(&code),
            "Invalid shortcut key sequence"
        );
        codes.push(code);
    }
    Ok(codes)
}

fn physical_keys_idle(config: &Config) -> Result<bool> {
    for path in &config.activity_devices {
        let file = std::fs::File::open(path)?;
        let device = evdev::Device::from_fd(file.into())?;
        let keys = device.get_key_state()?;
        if keys.iter().any(|k| (1..=279).contains(&k.code())) {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn next(events: &mut tokio::sync::mpsc::Receiver<StreamEvent>) -> Result<Event> {
    Ok(tokio::time::timeout(Duration::from_secs(3), events.recv())
        .await
        .context("Input verification timed out")?
        .context("Capture worker ended")?
        .event)
}

pub fn worker(options: Options) -> Result<Report> {
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async move {
        let config=Config::load(&options.config)?;
        ensure!(session::current_status().await?==Status::Unlocked,"Unlock the graphical session before testing");
        ensure!(!layer_exists()?,"Stop the active NiriBridge service before input verification");
        let original_focus=focus_id();
        let before_keyboards=keyboard_count();
        let display=PathBuf::from(std::env::var_os("WAYLAND_DISPLAY").context("WAYLAND_DISPLAY is missing")?);
        let socket=if display.is_absolute(){display}else{PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is missing")?).join(display)};
        let logical=niri::outputs()?.remove(&config.edges[0].output).context("Output is missing")?.logical.context("Output is disabled")?;
        let mut pointer=Pointer::connect(&socket,&config.edges[0].output)?;
        let mut receiver=Receiver::new(Keyboard::create()?);
        pointer.emit(&InputEvent::Absolute{x:logical.width/2,y:logical.height/2,width:logical.width,height:logical.height})?;
        let edge=EdgeConfig{id:"probe".into(),output:config.edges[0].output.clone(),boundary:Boundary{edge:Edge::Top,start:0.35,end:0.65}};
        let (sender,mut events)=tokio::sync::mpsc::channel(128);
        let task=CaptureTask::start(&[edge],1,sender);
        let capture_result=async {
            loop{match next(&mut events).await.context("Capture surface setup failed")?{Event::Armed=>break,Event::Finished{reason}=>anyhow::bail!("Capture ended before verification: {reason}"),_=>{}}}
            pointer.emit(&InputEvent::Absolute{x:logical.width/2,y:logical.height/2,width:logical.width,height:logical.height})?;
            pointer.emit(&InputEvent::Absolute{x:logical.width/2,y:0,width:logical.width,height:logical.height})?;
            loop{match next(&mut events).await.context("Input capture was not granted")?{Event::Ready=>break,Event::Finished{reason}=>anyhow::bail!("Capture was interrupted: {reason}"),_=>{}}}
            // Client delivery waits for the old IME grab to release. Production source keys
            // instead come directly from the selected physical devices.
            tokio::time::sleep(Duration::from_millis(200)).await;
            receiver.begin(1)?;
            for (sequence,(code,pressed)) in [(42,true),(42,false),(194,true),(194,false)].into_iter().enumerate(){
                receiver.input(1,sequence as u64,&InputEvent::Key{code,pressed})?;
            }
            pointer.emit(&InputEvent::Motion{dx:7.0,dy:5.0})?;
            pointer.emit(&InputEvent::Scroll{axis:crate::protocol::Axis::Vertical,amount:2.0,source:crate::protocol::ScrollSource::Finger})?;
            let mut keys=BTreeSet::new();let mut motion=false;let mut scroll=false;
            while keys.len()<4||!motion||!scroll {
                match next(&mut events).await.with_context(||format!("Probe events missing: F24 down/up {}/{}, Shift down/up {}/{}, motion {motion}, scroll {scroll}",keys.contains(&(194,true)),keys.contains(&(194,false)),keys.contains(&(42,true)),keys.contains(&(42,false))))? {
                    Event::Input(InputEvent::Key{code,pressed}) if code==194||code==42=>{keys.insert((code,pressed));}
                    Event::Input(InputEvent::Motion{..})=>motion=true,
                    Event::Input(InputEvent::Scroll{source:crate::protocol::ScrollSource::Finger,..})=>scroll=true,
                    Event::Finished{..}=>anyhow::bail!("Capture was interrupted"),_=>{}
                }
            }
            Ok::<_,anyhow::Error>((true,motion,scroll))
        }.await;
        let _=receiver.end(1);
        task.stop().await?;
        let (keyboard,motion,scroll)=capture_result?;
        ensure!(session::current_status().await?==Status::Unlocked,"Session became unavailable during verification");
        pointer.emit(&InputEvent::Absolute{x:logical.width/2,y:8,width:logical.width,height:logical.height})?;
        if let Some(id)=original_focus {let _=action(&["focus-window","--id",&id.to_string()]);}
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut report=Report{kernel_keyboard_verified:keyboard,pointer_motion_verified:motion,scroll_verified:scroll,capture_released:!layer_exists()?,original_focus_restored:focus_id()==original_focus,shortcut_verified:None,overview_restored:None,temporary_keyboard_removed:false};
        if let Some(shortcut)=options.overview_shortcut {
            let keys=chord(&shortcut)?;
            let until=Instant::now()+Duration::from_secs(3);
            while !physical_keys_idle(&config)?&&Instant::now()<until {tokio::time::sleep(Duration::from_millis(20)).await;}
            ensure!(physical_keys_idle(&config)?,"Physical input is busy; the global shortcut test was not sent");
            let initial=overview()?;
            receiver.begin(2)?;
            for (sequence,(code,pressed)) in keys.iter().map(|k|(*k,true)).chain(keys.iter().rev().map(|k|(*k,false))).enumerate(){
                receiver.input(2,sequence as u64,&InputEvent::Key{code,pressed})?;
            }
            receiver.end(2)?;
            let until=Instant::now()+Duration::from_secs(1);
            let mut changed=false;
            while Instant::now()<until {if overview()?!=initial{changed=true;break;}tokio::time::sleep(Duration::from_millis(20)).await;}
            report.shortcut_verified=Some(changed);
            if overview()?!=initial {action(&["toggle-overview"])?;}
            tokio::time::sleep(Duration::from_millis(50)).await;
            report.overview_restored=Some(overview()?==initial);
        }
        drop(receiver);
        let until=Instant::now()+Duration::from_secs(1);
        while keyboard_count()>before_keyboards&&Instant::now()<until {tokio::time::sleep(Duration::from_millis(20)).await;}
        report.temporary_keyboard_removed=keyboard_count()==before_keyboards;
        Ok(report)
    })
}
