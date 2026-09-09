// SPDX-License-Identifier: GPL-3.0-or-later
//! Monitors the current graphical session. Unknown state is treated as unavailable for sharing.
use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use std::time::Duration;
use tokio::{sync::watch, time::timeout};
use zbus::{Proxy, proxy::CacheProperties, zvariant::OwnedObjectPath};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Unknown,
    Unlocked,
    Unavailable,
}

pub struct Monitor {
    pub status: watch::Receiver<Status>,
    task: tokio::task::JoinHandle<()>,
}

impl Monitor {
    pub fn start(session_id: String) -> Self {
        let (sender, status) = watch::channel(Status::Unknown);
        let task = tokio::spawn(async move {
            loop {
                let _ = monitor_session(&session_id, &sender).await;
                let _ = sender.send(Status::Unknown);
                if sender.is_closed() {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
        Self { status, task }
    }
}
impl Drop for Monitor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn check_current() -> Result<Status> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(current_status())
}

pub async fn current_status() -> Result<Status> {
    let id = std::env::var("XDG_SESSION_ID").context("Run inside the Niri graphical session")?;
    let mut monitor = Monitor::start(id);
    timeout(Duration::from_secs(5), async {
        loop {
            let status = *monitor.status.borrow();
            if status != Status::Unknown {
                return Ok(status);
            }
            monitor.status.changed().await?;
        }
    })
    .await
    .context("Could not verify the graphical session safety state")?
}

async fn monitor_session(session_id: &str, sender: &watch::Sender<Status>) -> Result<()> {
    let connection = timeout(Duration::from_secs(3), zbus::Connection::system()).await??;
    let manager = Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;
    let path: OwnedObjectPath = timeout(
        Duration::from_secs(3),
        manager.call("GetSession", &(session_id)),
    )
    .await??;
    let session: Proxy<'_> = zbus::proxy::Builder::new(&connection)
        .destination("org.freedesktop.login1")?
        .path(path.clone())?
        .interface("org.freedesktop.login1.Session")?
        .cache_properties(CacheProperties::No)
        .build()
        .await?;
    let properties = Proxy::new(
        &connection,
        "org.freedesktop.login1",
        path,
        "org.freedesktop.DBus.Properties",
    )
    .await?;
    let mut changes = properties.receive_signal("PropertiesChanged").await?;
    let mut timer = tokio::time::interval(Duration::from_secs(2));
    loop {
        let status = timeout(Duration::from_secs(2), read_status(&session)).await??;
        sender.send_if_modified(|current| {
            if *current == status {
                false
            } else {
                *current = status;
                true
            }
        });
        tokio::select! {
            signal=changes.next()=>{signal.context("Session status connection closed")?;}
            _=timer.tick()=>{}
            _=sender.closed()=>return Ok(()),
        }
    }
}

async fn read_status(proxy: &Proxy<'_>) -> Result<Status> {
    let kind: String = proxy.get_property("Type").await?;
    let user: (u32, OwnedObjectPath) = proxy.get_property("User").await?;
    ensure!(
        kind == "wayland" && user.0 == unsafe { libc::geteuid() },
        "The selected session is not this user's Wayland session"
    );
    let active: bool = proxy.get_property("Active").await?;
    let locked: bool = proxy.get_property("LockedHint").await?;
    Ok(if active && !locked {
        Status::Unlocked
    } else {
        Status::Unavailable
    })
}
