// SPDX-License-Identifier: GPL-3.0-or-later
//! Saved peer trust and per-peer settings. Only the selected certificate enters TLS.
use crate::{
    bridge::{Config, Connection, EdgeConfig, validate_edges},
    manager::{CertificateInfo, certificate},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
};
use toml_edit::{ArrayOfTables, DocumentMut, Item, value};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SavedPeer {
    pub fingerprint: String,
    pub name: String,
    pub certificate: PathBuf,
    pub connection: Connection,
    pub edges: Vec<EdgeConfig>,
}

#[derive(Serialize)]
pub struct Summary {
    pub name: String,
    pub fingerprint: String,
    pub expires: String,
    pub active: bool,
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn validate(config: &Config) -> Result<()> {
    let mut fingerprints = BTreeSet::new();
    for peer in &config.paired_devices {
        ensure!(
            valid_fingerprint(&peer.fingerprint)
                && fingerprints.insert(&peer.fingerprint)
                && !peer.name.is_empty()
                && peer.name.len() <= 253
                && !peer.name.chars().any(char::is_control),
            "config_invalid"
        );
        validate_edges(&peer.edges)?;
    }
    if !config.paired_devices.is_empty() {
        ensure!(
            config
                .paired_devices
                .iter()
                .any(|p| p.certificate == config.peer_certificate && p.name == config.peer_name),
            "config_invalid"
        );
    }
    Ok(())
}

pub fn summaries(config: &Config) -> Result<Vec<Summary>> {
    if config.paired_devices.is_empty() {
        return Ok(certificate(&config.peer_certificate)
            .ok()
            .map(|info| Summary {
                name: info.name,
                fingerprint: info.fingerprint,
                expires: info.expires,
                active: true,
            })
            .into_iter()
            .collect());
    }
    config
        .paired_devices
        .iter()
        .map(|peer| {
            // Keep damaged or missing saved records visible so an explicit, reviewed
            // re-import can repair them. They cannot be selected or used by TLS.
            let info = checked_certificate(peer).ok();
            Ok(Summary {
                name: peer.name.clone(),
                fingerprint: peer.fingerprint.clone(),
                expires: info.map(|i| i.expires).unwrap_or_default(),
                active: peer.certificate == config.peer_certificate,
            })
        })
        .collect()
}

pub fn selected_certificate(config: &Config) -> Result<rustls::pki_types::CertificateDer<'static>> {
    let certificate = crate::transport::load_certificate(&config.peer_certificate)?;
    if !config.paired_devices.is_empty() {
        let selected = config
            .paired_devices
            .iter()
            .find(|p| p.certificate == config.peer_certificate)
            .context("config_invalid")?;
        ensure!(
            crate::manager::digest(certificate.as_ref()) == selected.fingerprint,
            "peer_certificate_changed"
        );
    }
    Ok(certificate)
}

fn checked_certificate(peer: &SavedPeer) -> Result<CertificateInfo> {
    let info = certificate(&peer.certificate).context("peer_certificate_changed")?;
    ensure!(
        info.fingerprint == peer.fingerprint && info.name == peer.name,
        "peer_certificate_changed"
    );
    Ok(info)
}

fn raw_config(doc: &DocumentMut) -> Result<Config> {
    toml::from_str(&doc.to_string()).context("config_invalid")
}

fn store_profiles(doc: &mut DocumentMut, peers: &[SavedPeer]) -> Result<()> {
    let mut previous = std::collections::BTreeMap::new();
    if let Some(tables) = doc.get("paired_devices").and_then(Item::as_array_of_tables) {
        for table in tables {
            if let Some(id) = table.get("fingerprint").and_then(Item::as_str) {
                previous.insert(id.to_owned(), table.clone());
            }
        }
    }
    let mut tables = ArrayOfTables::new();
    for peer in peers {
        let mut table = previous.remove(&peer.fingerprint).unwrap_or_default();
        table.set_position(None);
        let mut saved: DocumentMut = table.into();
        for (key, text) in [
            ("fingerprint", peer.fingerprint.as_str()),
            ("name", peer.name.as_str()),
            (
                "certificate",
                peer.certificate.to_str().context("config_invalid")?,
            ),
        ] {
            replace(&mut saved[key], value(text));
        }
        write_connection(&mut saved, &peer.connection);
        crate::manager::write_edges(&mut saved, &peer.edges);
        tables.push(saved.as_table().clone());
    }
    doc["paired_devices"] = Item::ArrayOfTables(tables);
    Ok(())
}

fn replace(item: &mut Item, mut next: Item) {
    if let (Some(old), Some(new)) = (item.as_value(), next.as_value_mut()) {
        *new.decor_mut() = old.decor().clone();
    }
    *item = next;
}

pub(crate) fn write_connection(doc: &mut DocumentMut, connection: &Connection) {
    let (mode, address) = match connection {
        Connection::Listen { address } => ("listen", address),
        Connection::Connect { address } => ("connect", address),
    };
    replace(&mut doc["connection"]["mode"], value(mode));
    replace(&mut doc["connection"]["address"], value(address));
}

/// Top-level values stay compatible with the coordinator; snapshots update atomically.
pub fn sync_selected(doc: &mut DocumentMut) -> Result<()> {
    let mut config = raw_config(doc)?;
    if config.paired_devices.is_empty() {
        return Ok(());
    }
    validate(&config)?;
    let peer = config
        .paired_devices
        .iter_mut()
        .find(|p| p.certificate == config.peer_certificate)
        .context("config_invalid")?;
    peer.connection = config.connection;
    peer.edges = config.edges;
    store_profiles(doc, &config.paired_devices)
}

fn stored_certificate(directory: &Path, info: &CertificateInfo, repair: bool) -> Result<PathBuf> {
    let peers = directory.join("peers");
    match fs::symlink_metadata(&peers) {
        Ok(metadata) => ensure!(
            metadata.is_dir()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o077 == 0,
            "settings_permissions"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new().mode(0o700).create(&peers)?;
        }
        Err(error) => return Err(error.into()),
    }
    ensure!(valid_fingerprint(&info.fingerprint), "certificate_invalid");
    let relative = PathBuf::from("peers").join(format!("{}.pem", info.fingerprint));
    let destination = directory.join(&relative);
    let verify_existing = || -> Result<()> {
        let metadata = fs::symlink_metadata(&destination)?;
        ensure!(
            metadata.is_file()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o077 == 0,
            "settings_permissions"
        );
        ensure!(
            certificate(&destination)?.fingerprint == info.fingerprint,
            "peer_certificate_changed"
        );
        Ok(())
    };
    if fs::symlink_metadata(&destination).is_ok() {
        let metadata = fs::symlink_metadata(&destination)?;
        ensure!(
            metadata.is_file()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o077 == 0,
            "settings_permissions"
        );
        if verify_existing().is_err() && repair {
            let mut temporary = tempfile::NamedTempFile::new_in(&peers)?;
            temporary.write_all(info.pem.as_bytes())?;
            temporary.as_file().sync_all()?;
            temporary
                .persist(&destination)
                .map_err(|_| anyhow::anyhow!("settings_write_failed"))?;
            fs::File::open(&peers)?.sync_all()?;
        } else {
            verify_existing()?;
        }
    } else {
        let mut temporary = tempfile::NamedTempFile::new_in(&peers)?;
        temporary.write_all(info.pem.as_bytes())?;
        temporary.as_file().sync_all()?;
        if let Err(error) = temporary.persist_noclobber(&destination) {
            if error.error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.error.into());
            }
            verify_existing()?;
        }
        fs::File::open(&peers)?.sync_all()?;
    }
    Ok(relative)
}

fn activate(doc: &mut DocumentMut, peer: &SavedPeer) -> Result<()> {
    doc["peer_name"] = value(&peer.name);
    doc["peer_certificate"] = value(peer.certificate.to_str().context("config_invalid")?);
    write_connection(doc, &peer.connection);
    crate::manager::write_edges(doc, &peer.edges);
    Ok(())
}

/// Called inside the configuration edit lock, after the caller's fingerprint review.
pub fn add(doc: &mut DocumentMut, directory: &Path, info: &CertificateInfo) -> Result<()> {
    let mut config = raw_config(doc)?;
    if config.paired_devices.is_empty() {
        let old = if config.peer_certificate.is_absolute() {
            config.peer_certificate.clone()
        } else {
            directory.join(&config.peer_certificate)
        };
        if old.try_exists()? {
            let current = certificate(&old)?;
            let peer = SavedPeer {
                fingerprint: current.fingerprint.clone(),
                name: current.name.clone(),
                certificate: stored_certificate(
                    directory,
                    &current,
                    current.fingerprint == info.fingerprint,
                )?,
                connection: config.connection.clone(),
                edges: config.edges.clone(),
            };
            activate(doc, &peer)?;
            config.paired_devices.push(peer);
        }
    }
    if !config
        .paired_devices
        .iter()
        .any(|p| p.fingerprint == info.fingerprint)
    {
        let first = config.paired_devices.is_empty();
        let mut edge = config.edges.first().context("layout_invalid")?.clone();
        edge.id = "default".to_owned();
        let peer = SavedPeer {
            fingerprint: info.fingerprint.clone(),
            name: info.name.clone(),
            certificate: stored_certificate(directory, info, true)?,
            connection: if first {
                config.connection
            } else {
                Connection::Listen {
                    address: "0.0.0.0:42420".into(),
                }
            },
            edges: if first { config.edges } else { vec![edge] },
        };
        if first {
            activate(doc, &peer)?;
        }
        config.paired_devices.push(peer);
    } else {
        stored_certificate(directory, info, true)?;
        let saved = config
            .paired_devices
            .iter()
            .find(|p| p.fingerprint == info.fingerprint)
            .unwrap();
        let path = if saved.certificate.is_absolute() {
            saved.certificate.clone()
        } else {
            directory.join(&saved.certificate)
        };
        let mut saved = saved.clone();
        saved.certificate = path;
        checked_certificate(&saved)?;
    }
    store_profiles(doc, &config.paired_devices)
}

pub fn select(doc: &mut DocumentMut, directory: &Path, fingerprint: &str) -> Result<()> {
    let config = raw_config(doc)?;
    let peer = config
        .paired_devices
        .iter()
        .find(|p| p.fingerprint == fingerprint)
        .context("peer_not_found")?;
    let mut checked = peer.clone();
    if checked.certificate.is_relative() {
        checked.certificate = directory.join(&checked.certificate);
    }
    checked_certificate(&checked)?;
    activate(doc, peer)
}
