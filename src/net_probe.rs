// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{
    protocol::{self, Message},
    transport::{self, Identity},
};
use anyhow::{Result, ensure};
use clap::{Args, Subcommand};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, time::timeout};

#[derive(Args)]
pub struct Files {
    #[arg(long)]
    pub certificate: PathBuf,
    #[arg(long)]
    pub key: PathBuf,
    #[arg(long)]
    pub peer_certificate: PathBuf,
}

#[derive(Subcommand)]
pub enum Mode {
    /// Wait for one authenticated peer and answer measurement messages only.
    Listen {
        #[command(flatten)]
        files: Files,
        #[arg(long)]
        bind: String,
    },
    /// Authenticate the peer and measure round-trip time; never sends keyboard events.
    Connect {
        #[command(flatten)]
        files: Files,
        #[arg(long)]
        address: String,
        #[arg(long)]
        peer_name: String,
    },
}

pub fn run(mode: Mode) -> Result<()> {
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async move {
        match mode {
            Mode::Listen{files,bind}=>{
                let local=Identity::load(&files.certificate,&files.key)?;
                let config=transport::server_config(&local,transport::load_certificate(&files.peer_certificate)?)?;
                let listener=TcpListener::bind(bind).await?;
                eprintln!("Waiting up to 60 seconds for the paired device; input injection is disabled in this command.");
                let (stream,_)=timeout(Duration::from_secs(60),listener.accept()).await??;
                let mut tls=transport::accept(stream,config).await?;transport::hello(&mut tls).await?;
                let mut count=0;
                loop {
                    let next=timeout(Duration::from_secs(5),protocol::read_frame(&mut tls)).await?;
                    let Ok(message)=next else {break;};
                    let Message::Ping{nonce}=message else {anyhow::bail!("Network probe accepts measurements only");};
                    ensure!(count<1000,"Measurement limit exceeded");
                    protocol::write_frame(&mut tls,&Message::Pong{nonce}).await?;count+=1;
                }
                println!("{{\"paired_peer_authenticated\":true,\"measurements\":{count}}}");
            }
            Mode::Connect{files,address,peer_name}=>{
                let local=Identity::load(&files.certificate,&files.key)?;
                let config=transport::client_config(&local,transport::load_certificate(&files.peer_certificate)?)?;
                let mut tls=transport::connect(&address,&peer_name,config).await?;transport::hello(&mut tls).await?;
                let mut timings=Vec::new();
                for nonce in 0..30 {
                    let start=Instant::now();protocol::write_frame(&mut tls,&Message::Ping{nonce}).await?;
                    ensure!(matches!(timeout(Duration::from_secs(3),protocol::read_frame(&mut tls)).await??,Message::Pong{nonce:n} if n==nonce),"Unexpected measurement response");
                    timings.push(start.elapsed().as_secs_f64()*1000.0);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                timings.sort_by(f64::total_cmp);
                println!("{}",serde_json::json!({"paired_peer_authenticated":true,"measurements":timings.len(),"median_rtt_ms":timings[timings.len()/2],"max_rtt_ms":timings[timings.len()-1]}));
            }
        }
        Ok(())
    })
}
