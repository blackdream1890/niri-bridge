// SPDX-License-Identifier: GPL-3.0-or-later
use std::{
    collections::BTreeMap,
    env,
    io::{BufRead, BufReader, Read, Write},
    os::{fd::AsRawFd, unix::net::UnixStream},
    path::Path,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const RESPONSE_LIMIT: u64 = 1024 * 1024;

/// The subset of Niri's output metadata needed for geometry; EDID fields are omitted.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Output {
    pub name: String,
    pub logical: Option<LogicalOutput>,
    pub current_mode: Option<usize>,
    pub modes: Vec<Mode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub struct LogicalOutput {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub transform: Transform,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub enum Transform {
    Normal,
    #[serde(rename = "90")]
    Rotate90,
    #[serde(rename = "180")]
    Rotate180,
    #[serde(rename = "270")]
    Rotate270,
    Flipped,
    Flipped90,
    Flipped180,
    Flipped270,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub refresh_rate: u32,
}

pub fn query(request: &str) -> Result<Value> {
    let path = env::var_os("NIRI_SOCKET").context("NIRI_SOCKET is not set")?;
    query_at(Path::new(&path), request)
}

/// Identify the current compositor through its Unix socket, rather than a process-name search.
pub(crate) fn compositor_pid() -> Result<libc::pid_t> {
    let path = env::var_os("NIRI_SOCKET").context("NIRI_SOCKET is not set")?;
    let stream = UnixStream::connect(path).context("Cannot connect to the Niri session")?;
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut size = std::mem::size_of_val(&credentials) as libc::socklen_t;
    ensure!(
        unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut size,
            )
        } == 0,
        "Cannot identify the Niri session"
    );
    ensure!(
        credentials.pid > 0 && credentials.uid == unsafe { libc::geteuid() },
        "The compositor must belong to the current user"
    );
    Ok(credentials.pid)
}

fn query_at(path: &Path, request: &str) -> Result<Value> {
    let mut stream = UnixStream::connect(path).context("Cannot connect to the Niri session")?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    let mut reply = String::new();
    BufReader::new(stream)
        .take(RESPONSE_LIMIT)
        .read_line(&mut reply)?;
    if !reply.ends_with('\n') {
        bail!("Niri returned an incomplete or oversized response");
    }
    decode_reply(&reply)
}

fn decode_reply(reply: &str) -> Result<Value> {
    let value: Value = serde_json::from_str(reply)?;
    if let Some(ok) = value.get("Ok") {
        return Ok(ok.clone());
    }
    // Do not echo an arbitrary response or environment details into a shared report.
    bail!("Niri rejected the request")
}

pub fn outputs() -> Result<BTreeMap<String, Output>> {
    let reply = query("Outputs")?;
    let outputs = reply.get("Outputs").context("Niri returned no outputs")?;
    Ok(serde_json::from_value(outputs.clone())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::net::UnixListener, thread};

    #[test]
    fn queries_the_socket_using_niri_framing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("niri.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            assert_eq!(request, "\"Version\"\n");
            stream
                .write_all(b"{\"Ok\":{\"Version\":\"example\"}}\n")
                .unwrap();
        });
        assert_eq!(query_at(&path, "Version").unwrap()["Version"], "example");
        server.join().unwrap();
    }

    #[test]
    fn does_not_report_an_error_reply_as_a_success() {
        assert!(decode_reply(r#"{"Err":"private detail"}"#).is_err());
    }

    #[test]
    fn reads_rotated_fractionally_scaled_output_without_private_metadata() {
        let output: Output = serde_json::from_str(
            r#"{
            "name":"DP-2", "make":"private", "serial":"private",
            "logical":{"x":-1490,"y":-579,"width":1489,"height":2648,"scale":1.45,"transform":"90"},
            "current_mode":0, "modes":[{"width":3840,"height":2160,"refresh_rate":119993}]
        }"#,
        )
        .unwrap();
        assert_eq!(output.logical.unwrap().width, 1489);
        assert!(!serde_json::to_string(&output).unwrap().contains("private"));
    }
}
