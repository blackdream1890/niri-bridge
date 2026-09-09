// SPDX-License-Identifier: GPL-3.0-or-later
use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use niri_bridge::{bridge, capture_probe, doctor, geometry::Layout, identity, net_probe};

#[derive(Parser)]
#[command(
    version,
    about = "Development tools for Niri keyboard and pointer sharing"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Structured configuration operations used by the desktop application.
    Manage {
        #[command(subcommand)]
        action: niri_bridge::manager::Command,
    },
    /// Verify actual uinput delivery inside a bounded capture owned by this application.
    VerifyInput(niri_bridge::verify_input::Options),
    #[command(hide = true)]
    VerifyInputWorker(niri_bridge::verify_input::Options),
    /// Validate a bridge configuration and its identities without opening input devices.
    CheckConfig {
        #[arg(long)]
        config: PathBuf,
    },
    /// Read the active graphical session's lock state without changing it.
    CheckSession,
    /// Run the paired keyboard and pointer bridge in the current graphical session.
    Run {
        #[arg(long)]
        config: PathBuf,
    },
    /// Create a new application identity (never overwrites existing credentials).
    IdentityInit {
        #[arg(long)]
        directory: PathBuf,
        #[arg(long)]
        name: String,
    },
    /// Verify a paired TLS connection without capturing or injecting input.
    NetTest {
        #[command(subcommand)]
        mode: net_probe::Mode,
    },
    /// Opt-in desktop experiment: captures input on an edge, Escape or timeout returns control.
    CaptureTest(capture_probe::Options),
    #[command(hide = true)]
    ProbeCapture(capture_probe::Options),
    #[command(hide = true)]
    ProbeWayland,
    /// Inspect interfaces and permissions without capturing or injecting input.
    Doctor {
        /// Print a structured report suitable for sharing (no device serials or hostnames).
        #[arg(long)]
        json: bool,
    },
    /// Validate a proposed logical screen boundary map. Does not apply configuration.
    CheckLayout { path: PathBuf },
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Manage { action } => niri_bridge::manager::run(action),
        Command::VerifyInput(options) => println!(
            "{}",
            serde_json::to_string_pretty(&niri_bridge::verify_input::run(options)?)?
        ),
        Command::VerifyInputWorker(options) => println!(
            "{}",
            serde_json::to_string(&niri_bridge::verify_input::worker(options)?)?
        ),
        Command::CheckConfig { config } => {
            let config = bridge::Config::load(&config)?;
            let identity =
                niri_bridge::transport::Identity::load(&config.certificate, &config.private_key)?;
            let peer = niri_bridge::transport::load_certificate(&config.peer_certificate)?;
            niri_bridge::transport::client_config(&identity, peer.clone())?;
            niri_bridge::transport::server_config(&identity, peer)?;
            println!(
                "Configuration and paired identities are valid. No input devices were opened."
            );
        }
        Command::CheckSession => println!(
            "Session state: {:?}",
            niri_bridge::session::check_current()?
        ),
        Command::Run { config } => bridge::run(bridge::Config::load(&config)?)?,
        Command::IdentityInit { directory, name } => {
            identity::create(&directory, &name)?;
            println!(
                "Local identity created. Keep the private key on this device; share only identity.pem with the paired device."
            );
        }
        Command::NetTest { mode } => net_probe::run(mode)?,
        Command::CaptureTest(options) => println!(
            "{}",
            serde_json::to_string_pretty(&capture_probe::run(options)?)?
        ),
        Command::ProbeCapture(options) => println!(
            "{}",
            serde_json::to_string(&capture_probe::worker(options)?)?
        ),
        Command::ProbeWayland => doctor::print_wayland_probe()?,
        Command::Doctor { json } => {
            let report = doctor::inspect();
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{}", report.summary());
            }
        }
        Command::CheckLayout { path } => {
            let text = std::fs::read_to_string(path)?;
            let layout: Layout = toml::from_str(&text)?;
            layout.validate()?;
            println!("Layout is valid; no desktop settings were changed.");
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("niri-bridge: {error}");
            ExitCode::FAILURE
        }
    }
}
