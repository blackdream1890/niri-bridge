// SPDX-License-Identifier: GPL-3.0-or-later
//! Explicit live gesture probe. It moves workspace/overview state briefly and restores focus.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use niri_bridge::{
    bridge::Config,
    niri, session,
    touchpad::{Descriptor, Event, Kind, VirtualTouchpad},
};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::Duration,
};
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Mode,
}
#[derive(Subcommand)]
enum Mode {
    Profile {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    Verify {
        #[arg(long)]
        profile: PathBuf,
    },
}
fn action(args: &[&str]) -> Result<()> {
    ensure!(
        Command::new("niri")
            .args(["msg", "action"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success(),
        "Niri action failed"
    );
    Ok(())
}
fn overview() -> Result<bool> {
    niri::query("OverviewState")?["OverviewState"]["is_open"]
        .as_bool()
        .context("Overview state unavailable")
}
fn workspace() -> Result<u64> {
    niri::query("Workspaces")?["Workspaces"]
        .as_array()
        .context("Workspace state unavailable")?
        .iter()
        .find(|w| w["is_focused"] == true)
        .and_then(|w| w["id"].as_u64())
        .context("No focused workspace")
}
fn key(code: u16, value: i32) -> Event {
    Event {
        kind: Kind::Key,
        code,
        value,
    }
}
fn abs(code: u16, value: i32) -> Event {
    Event {
        kind: Kind::Absolute,
        code,
        value,
    }
}
fn swipe(pad: &mut VirtualTouchpad, d: &Descriptor, fingers: usize, up: bool) -> Result<()> {
    let x = d.axes.iter().find(|a| a.code == 53).unwrap();
    let y = d.axes.iter().find(|a| a.code == 54).unwrap();
    let initial = if up { 0.8 } else { 0.2 };
    let mut frame = vec![key(330, 1), key(if fingers == 4 { 335 } else { 334 }, 1)];
    for i in 0..fingers {
        let px = x.minimum + ((x.maximum - x.minimum) as f64 * (0.2 + 0.17 * i as f64)) as i32;
        let py = y.minimum + ((y.maximum - y.minimum) as f64 * initial) as i32;
        frame.extend([
            abs(47, i as i32),
            abs(57, 100 + i as i32),
            abs(53, px),
            abs(54, py),
        ]);
        if i == 0 {
            frame.extend([abs(0, px), abs(1, py)]);
        }
    }
    pad.emit(&frame)?;
    for step in 1..=50 {
        thread::sleep(Duration::from_millis(12));
        let fraction = initial + if up { -0.6 } else { 0.6 } * step as f64 / 50.0;
        let py = y.minimum + ((y.maximum - y.minimum) as f64 * fraction) as i32;
        let mut frame = vec![abs(1, py)];
        for i in 0..fingers {
            frame.extend([abs(47, i as i32), abs(54, py)]);
        }
        pad.emit(&frame)?;
    }
    pad.reset()?;
    thread::sleep(Duration::from_millis(500));
    Ok(())
}
fn run() -> Result<()> {
    match Cli::parse().command {
        Mode::Profile { config, output } => {
            let config = Config::load(&config)?;
            let mut seen = BTreeSet::new();
            let mut descriptors = Vec::new();
            for path in config.activity_devices {
                let canonical = path.canonicalize()?;
                if !seen.insert(canonical.clone()) {
                    continue;
                }
                let file = OpenOptions::new().read(true).open(canonical)?;
                let device = evdev::raw_stream::RawDevice::from_fd(file.into())?;
                if device
                    .supported_keys()
                    .is_some_and(|keys| keys.contains(evdev::KeyCode::BTN_TOOL_FINGER))
                {
                    descriptors.push(Descriptor::from_device(&device)?);
                }
            }
            ensure!(
                descriptors.len() == 1,
                "This probe expects one selected physical touchpad"
            );
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(output)?;
            file.write_all(&serde_json::to_vec(&descriptors[0])?)?;
            println!("Touchpad capability profile saved without device identifiers.");
        }
        Mode::Verify { profile } => {
            ensure!(
                session::check_current()? == session::Status::Unlocked,
                "Unlock the desktop before this probe"
            );
            let descriptor: Descriptor = serde_json::from_slice(&fs::read(profile)?)?;
            let initial_overview = overview()?;
            let focus = niri::query("FocusedWindow")?["FocusedWindow"]["id"].as_u64();
            let mut pad = VirtualTouchpad::create(&descriptor)?;
            let result = (|| -> Result<(bool, bool)> {
                if overview()? {
                    action(&["toggle-overview"])?;
                    thread::sleep(Duration::from_millis(300));
                }
                swipe(&mut pad, &descriptor, 4, true)?;
                let four = overview()?;
                if overview()? {
                    action(&["toggle-overview"])?;
                    thread::sleep(Duration::from_millis(300));
                }
                let before = workspace()?;
                swipe(&mut pad, &descriptor, 3, true)?;
                let mut three = workspace()? != before;
                if !three {
                    swipe(&mut pad, &descriptor, 3, false)?;
                    three = workspace()? != before;
                }
                Ok((four, three))
            })();
            let _ = pad.reset();
            drop(pad);
            if let Some(id) = focus {
                action(&["focus-window", "--id", &id.to_string()])?;
            }
            if overview()? != initial_overview {
                action(&["toggle-overview"])?;
            }
            let (four, three) = result?;
            println!(
                "{}",
                serde_json::json!({"four_finger_overview":four,"three_finger_workspace":three,"overview_restored":overview()?==initial_overview,"focus_restored":niri::query("FocusedWindow")?["FocusedWindow"]["id"].as_u64()==focus})
            );
        }
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("Touchpad probe failed: {e}");
        std::process::exit(1);
    }
}
