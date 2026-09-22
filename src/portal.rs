// SPDX-License-Identifier: GPL-3.0-or-later
//! User-authorized pointer-only RemoteDesktop session. No screen or clipboard requests.
use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use std::{collections::HashMap, os::fd::OwnedFd, time::Duration};
use zbus::{
    Connection, Proxy,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};
const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const REMOTE: &str = "org.freedesktop.portal.RemoteDesktop";
type Options<'a> = HashMap<&'a str, Value<'a>>;
type Results = HashMap<String, OwnedValue>;

pub struct Session {
    connection: Connection,
    path: OwnedObjectPath,
}
impl Session {
    pub async fn create() -> Result<(Self, OwnedFd)> {
        let connection = Connection::session().await?;
        let remote = Proxy::new(&connection, DEST, PATH, REMOTE).await?;
        let token = format!("niribridge_{}", std::process::id());
        let mut options = Options::new();
        options.insert("session_handle_token", Value::from(token.as_str()));
        let results = request(
            &connection,
            &remote,
            "CreateSession",
            None,
            options,
            "create",
        )
        .await?;
        let path = results
            .get("session_handle")
            .context("Portal did not return a session")?;
        let path = OwnedObjectPath::try_from(<&str>::try_from(path)?)?;
        let session = Self {
            connection: connection.clone(),
            path,
        };
        let mut options = Options::new();
        options.insert("types", Value::from(2u32));
        request(
            &connection,
            &remote,
            "SelectDevices",
            Some(&session.path),
            options,
            "select",
        )
        .await?;
        let result = request(
            &connection,
            &remote,
            "Start",
            Some(&session.path),
            Options::new(),
            "start",
        )
        .await?;
        let granted = result
            .get("devices")
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        ensure!(granted & 2 != 0, "Desktop pointer control was not granted");
        let fd: zbus::zvariant::OwnedFd = tokio::time::timeout(
            Duration::from_secs(5),
            remote.call("ConnectToEIS", &(&session.path, Options::new())),
        )
        .await??;
        Ok((session, fd.into()))
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        // Closing the bus connection also revokes its session. Explicit Close is best effort.
        let connection = self.connection.clone();
        let path = self.path.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(1), async {
                    let proxy =
                        Proxy::new(&connection, DEST, path, "org.freedesktop.portal.Session")
                            .await?;
                    proxy.call::<_, _, ()>("Close", &()).await
                })
                .await;
            });
        }
    }
}
async fn request(
    connection: &Connection,
    remote: &Proxy<'_>,
    method: &str,
    session: Option<&OwnedObjectPath>,
    mut options: Options<'_>,
    suffix: &str,
) -> Result<Results> {
    let token = format!("niribridge_{}_{}", std::process::id(), suffix);
    let sender = connection
        .unique_name()
        .context("Portal connection has no unique name")?
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_");
    let path = OwnedObjectPath::try_from(format!(
        "/org/freedesktop/portal/desktop/request/{sender}/{token}"
    ))?;
    let request = Proxy::new(
        connection,
        DEST,
        path.clone(),
        "org.freedesktop.portal.Request",
    )
    .await?;
    let mut responses = request.receive_signal("Response").await?;
    options.insert("handle_token", Value::from(token.clone()));
    let result = tokio::time::timeout(Duration::from_secs(120), async {
        let returned: OwnedObjectPath = match session {
            None => remote.call(method, &(options,)).await?,
            Some(s) if method == "Start" => remote.call(method, &(s, "", options)).await?,
            Some(s) => remote.call(method, &(s, options)).await?,
        };
        ensure!(
            returned == path,
            "Portal returned an unexpected request path"
        );
        let response = responses
            .next()
            .await
            .context("Desktop authorization ended")?;
        let (code, results): (u32, Results) = response.body().deserialize()?;
        ensure!(
            code == 0,
            "Desktop control was cancelled or denied; start sharing to try again"
        );
        Ok(results)
    })
    .await;
    match result {
        Ok(r) => r,
        Err(_) => {
            let _ = tokio::time::timeout(
                Duration::from_secs(1),
                request.call::<_, _, ()>("Close", &()),
            )
            .await;
            anyhow::bail!("Desktop authorization timed out; start sharing to try again")
        }
    }
}
