// SPDX-License-Identifier: GPL-3.0-or-later
//! Configuration operations shared by the desktop UI and paired layout updates.
use crate::{
    bridge::{Config, Connection, EdgeConfig},
    identity, niri, transport,
};
use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    process::Command as Process,
    time::Duration,
};
use toml_edit::{DocumentMut, Item, value};

pub fn digest(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn revision(path: &Path) -> Result<String> {
    Ok(digest(&fs::read(path)?))
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    pub connection: Connection,
    pub activity_devices: Vec<PathBuf>,
    pub native_touchpads: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveRequest {
    pub revision: String,
    pub settings: Settings,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitializeRequest {
    pub settings: Settings,
    pub edge: EdgeConfig,
}
#[derive(Serialize)]
pub struct CertificateInfo {
    pub name: String,
    pub fingerprint: String,
    pub expires: String,
    pub pem: String,
}
#[derive(Serialize)]
pub struct View {
    pub revision: String,
    pub settings: Settings,
    pub edge: EdgeConfig,
    pub peer_name: String,
    pub identity: Option<CertificateInfo>,
    pub peer: Option<CertificateInfo>,
}

fn openssl(path: &Path, args: &[&str]) -> Result<String> {
    let mut command = Process::new("openssl");
    command
        .args(["x509", "-in"])
        .arg(path)
        .args(args)
        .env("LC_ALL", "C");
    crate::doctor::bounded_output(&mut command, Duration::from_secs(3))
        .context("certificate_invalid")
}
pub fn certificate(path: &Path) -> Result<CertificateInfo> {
    ensure!(fs::metadata(path)?.len() <= 65536, "certificate_invalid");
    let raw = fs::read_to_string(path)?;
    ensure!(!raw.contains("PRIVATE KEY"), "private_key_selected");
    let mut snapshot = tempfile::NamedTempFile::new()?;
    snapshot.write_all(raw.as_bytes())?;
    snapshot.flush()?;
    let path = snapshot.path();
    let cert = transport::load_certificate(path).context("certificate_invalid")?;
    let san = openssl(path, &["-noout", "-ext", "subjectAltName"])?;
    let names = san
        .split([',', '\n'])
        .filter_map(|v| v.trim().strip_prefix("DNS:"))
        .collect::<Vec<_>>();
    ensure!(names.len() == 1, "certificate_name_ambiguous");
    let name = names[0].trim().to_owned();
    ensure!(
        matches!(
            rustls::pki_types::ServerName::try_from(name.clone()),
            Ok(rustls::pki_types::ServerName::DnsName(_))
        ),
        "certificate_invalid"
    );
    ensure!(
        !name.contains('*') && name.len() <= 253,
        "certificate_invalid"
    );
    let expires = openssl(path, &["-noout", "-enddate"])?;
    Ok(CertificateInfo {
        name,
        fingerprint: digest(cert.as_ref()),
        expires: expires.trim().trim_start_matches("notAfter=").to_owned(),
        pem: openssl(path, &["-outform", "PEM"])?,
    })
}
pub fn view(path: &Path) -> Result<View> {
    let c = Config::load(path).context("config_invalid")?;
    Ok(View {
        revision: revision(path)?,
        settings: Settings {
            connection: c.connection,
            activity_devices: c.activity_devices,
            native_touchpads: c.native_touchpads,
        },
        edge: c.edge,
        peer_name: c.peer_name,
        identity: certificate(&c.certificate).ok(),
        peer: certificate(&c.peer_certificate).ok(),
    })
}
fn lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path.with_extension("edit.lock"))?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "settings_permissions"
    );
    ensure!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "settings_busy"
    );
    Ok(file)
}
fn atomic(path: &Path, text: &str) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().context("config_invalid")?)?;
    temporary.write_all(text.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|_| anyhow::anyhow!("settings_write_failed"))?;
    Ok(())
}
pub struct Prepared {
    path: PathBuf,
    before: String,
    next: String,
    pub before_revision: String,
    pub after_revision: String,
    _lock: File,
    committed: bool,
    finished: bool,
    target_output: Option<String>,
}
impl Prepared {
    fn edit(
        path: &Path,
        expected: &str,
        edit: impl FnOnce(&mut DocumentMut) -> Result<()>,
    ) -> Result<Self> {
        let guard = lock(path)?;
        let before = fs::read_to_string(path)?;
        ensure!(digest(before.as_bytes()) == expected, "settings_conflict");
        let mut doc = before.parse::<DocumentMut>().context("config_invalid")?;
        edit(&mut doc)?;
        let next = doc.to_string();
        let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        temp.write_all(next.as_bytes())?;
        temp.as_file().sync_all()?;
        Config::load(temp.path()).context("config_invalid")?;
        Ok(Self {
            path: path.to_path_buf(),
            before,
            next: next.clone(),
            before_revision: expected.to_owned(),
            after_revision: digest(next.as_bytes()),
            _lock: guard,
            committed: false,
            finished: false,
            target_output: None,
        })
    }
    pub fn commit(&mut self) -> Result<()> {
        if let Some(output) = &self.target_output {
            ensure!(
                niri::outputs()?
                    .get(output)
                    .is_some_and(|o| o.logical.is_some()),
                "output_unavailable"
            );
        }
        ensure!(
            revision(&self.path)? == self.before_revision,
            "settings_conflict"
        );
        let backup = self.path.with_extension("before-ui.toml");
        if !backup.exists() {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(backup)?;
            f.write_all(self.before.as_bytes())?;
            f.sync_all()?;
        }
        atomic(&self.path, &self.next)?;
        self.committed = true;
        Ok(())
    }
    pub fn finish(mut self) {
        self.finished = true;
    }
}
impl Drop for Prepared {
    fn drop(&mut self) {
        if self.committed
            && !self.finished
            && revision(&self.path).is_ok_and(|r| r == self.after_revision)
        {
            let _ = atomic(&self.path, &self.before);
        }
    }
}
pub fn prepare_edge(path: &Path, expected: &str, edge: &EdgeConfig) -> Result<Prepared> {
    edge.boundary.validate().context("layout_invalid")?;
    let outputs = niri::outputs()?;
    let output = outputs
        .get(&edge.output)
        .and_then(|o| o.logical)
        .context("output_unavailable")?;
    let extent = if matches!(
        edge.boundary.edge,
        crate::geometry::Edge::Top | crate::geometry::Edge::Bottom
    ) {
        output.width
    } else {
        output.height
    };
    ensure!(
        (edge.boundary.end - edge.boundary.start) * extent as f64 >= 1.,
        "layout_invalid"
    );
    let mut prepared = Prepared::edit(path, expected, |doc| {
        doc["edge"]["output"] = value(&edge.output);
        doc["edge"]["boundary"]["edge"] = value(edge.boundary.edge.as_str());
        doc["edge"]["boundary"]["start"] = value(edge.boundary.start);
        doc["edge"]["boundary"]["end"] = value(edge.boundary.end);
        Ok(())
    })?;
    prepared.target_output = Some(edge.output.clone());
    Ok(prepared)
}
fn settings(doc: &mut DocumentMut, s: &Settings) -> Result<()> {
    ensure!(
        !s.activity_devices.is_empty() && s.activity_devices.len() <= 32,
        "input_selection_empty"
    );
    ensure!(
        s.activity_devices
            .iter()
            .all(|p| p.is_absolute() && p.starts_with("/dev/input")),
        "input_path_invalid"
    );
    let (mode, address) = match &s.connection {
        Connection::Listen { address } => ("listen", address),
        Connection::Connect { address } => ("connect", address),
    };
    ensure!(
        address.len() <= 300
            && !address.chars().any(char::is_whitespace)
            && address
                .rsplit_once(':')
                .and_then(|(_, p)| p.parse::<u16>().ok())
                .is_some_and(|p| p > 0),
        "address_invalid"
    );
    doc["connection"]["mode"] = value(mode);
    doc["connection"]["address"] = value(address);
    doc["native_touchpads"] = value(s.native_touchpads);
    let mut array = toml_edit::Array::new();
    for p in &s.activity_devices {
        array.push(p.to_str().context("input_path_invalid")?);
    }
    doc["activity_devices"] = Item::Value(array.into());
    Ok(())
}
pub fn save(path: &Path, request: SaveRequest) -> Result<String> {
    let mut prepared = Prepared::edit(path, &request.revision, |doc| {
        settings(doc, &request.settings)
    })?;
    prepared.commit()?;
    let result = prepared.after_revision.clone();
    prepared.finish();
    Ok(result)
}
pub fn import_peer(
    path: &Path,
    candidate: &Path,
    expected: &str,
    fingerprint: &str,
) -> Result<String> {
    let info = certificate(candidate)?;
    ensure!(info.fingerprint == fingerprint, "certificate_changed");
    let config = Config::load(path)?;
    ensure!(
        info.fingerprint != certificate(&config.certificate)?.fingerprint,
        "pair_same_device"
    );
    let destination = path.parent().unwrap().join("peer.pem");
    let previous = fs::read_to_string(&destination).ok();
    let mut prepared = Prepared::edit(path, expected, |doc| {
        doc["peer_name"] = value(&info.name);
        doc["peer_certificate"] = value("peer.pem");
        Ok(())
    })?;
    atomic(&destination, &info.pem)?;
    if let Err(error) = prepared.commit() {
        if let Some(before) = previous {
            atomic(&destination, &before)?;
        } else {
            fs::remove_file(destination)?;
        }
        return Err(error);
    }
    let revision = prepared.after_revision.clone();
    prepared.finish();
    Ok(revision)
}
pub fn initialize(path: &Path, name: &str, request: InitializeRequest) -> Result<()> {
    ensure!(!path.exists(), "config_exists");
    request.edge.boundary.validate()?;
    let directory = path.parent().context("config_invalid")?;
    if !directory.exists() {
        fs::create_dir_all(directory)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    let mut doc = DocumentMut::new();
    doc["certificate"] = value("identity.pem");
    doc["private_key"] = value("identity.key.pem");
    doc["peer_certificate"] = value("peer.pem");
    doc["peer_name"] = value("unpaired");
    settings(&mut doc, &request.settings)?;
    doc["edge"]["output"] = value(request.edge.output);
    doc["edge"]["boundary"]["edge"] = value(request.edge.boundary.edge.as_str());
    doc["edge"]["boundary"]["start"] = value(request.edge.boundary.start);
    doc["edge"]["boundary"]["end"] = value(request.edge.boundary.end);
    if !directory.join("identity.pem").exists() {
        identity::create(directory, name)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(doc.to_string().as_bytes())?;
    file.sync_all()?;
    Ok(())
}
pub fn error_code(error: &anyhow::Error) -> &'static str {
    const CODES: &[&str] = &[
        "certificate_invalid",
        "certificate_changed",
        "private_key_selected",
        "certificate_name_ambiguous",
        "config_invalid",
        "settings_permissions",
        "settings_busy",
        "settings_conflict",
        "settings_write_failed",
        "layout_invalid",
        "output_unavailable",
        "input_selection_empty",
        "input_path_invalid",
        "address_invalid",
        "pair_same_device",
        "config_exists",
        "session_locked",
        "peer_offline",
        "layout_unconfirmed",
    ];
    for cause in error.chain() {
        let text = cause.to_string();
        if let Some(code) = CODES.iter().find(|c| text == **c) {
            return code;
        }
    }
    "operation_failed"
}
#[derive(Subcommand)]
pub enum Command {
    Show {
        #[arg(long)]
        config: PathBuf,
    },
    Save {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        request: PathBuf,
    },
    Certificate {
        path: PathBuf,
    },
    ImportPeer {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        candidate: PathBuf,
        #[arg(long)]
        revision: String,
        #[arg(long)]
        fingerprint: String,
    },
    Initialize {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long)]
        request: PathBuf,
    },
}
pub fn run(command: Command) {
    let outcome = (|| -> Result<serde_json::Value> {
        Ok(match command {
            Command::Show { config } => serde_json::to_value(view(&config)?)?,
            Command::Save { config, request } => {
                serde_json::json!({"revision":save(&config,serde_json::from_slice(&fs::read(request)?)?)?})
            }
            Command::Certificate { path } => serde_json::to_value(certificate(&path)?)?,
            Command::ImportPeer {
                config,
                candidate,
                revision,
                fingerprint,
            } => {
                serde_json::json!({"revision":import_peer(&config,&candidate,&revision,&fingerprint)?})
            }
            Command::Initialize {
                config,
                name,
                request,
            } => {
                initialize(&config, &name, serde_json::from_slice(&fs::read(request)?)?)?;
                serde_json::json!({})
            }
        })
    })();
    let result = match outcome {
        Ok(data) => serde_json::json!({"ok":true,"data":data}),
        Err(error) => serde_json::json!({"ok":false,"error":error_code(&error)}),
    };
    println!("{result}");
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path,"# Keep this comment\ncertificate = 'identity.pem'\nprivate_key = 'identity.key.pem'\npeer_certificate = 'peer.pem'\npeer_name = 'desktop'\nactivity_devices = ['/dev/input/by-path/test-kbd']\nnative_touchpads = true\n[connection]\nmode = 'connect'\naddress = 'desktop.local:42420'\n[edge]\noutput = 'screen'\nboundary = { edge = 'top', start = 0.0, end = 1.0 }\n").unwrap();
        (directory, path)
    }
    #[test]
    fn staged_config_is_not_visible_until_committed_and_unfinished_commit_rolls_back() {
        let (_directory, path) = config();
        let original = fs::read_to_string(&path).unwrap();
        let hash = revision(&path).unwrap();
        {
            let mut prepared = Prepared::edit(&path, &hash, |doc| {
                doc["edge"]["boundary"]["start"] = value(0.1);
                Ok(())
            })
            .unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
            prepared.commit().unwrap();
            assert!(
                fs::read_to_string(&path)
                    .unwrap()
                    .contains("# Keep this comment")
            );
            assert_ne!(revision(&path).unwrap(), hash);
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }
    #[test]
    fn stale_revision_and_external_edit_are_never_overwritten() {
        let (_directory, path) = config();
        let hash = revision(&path).unwrap();
        assert!(Prepared::edit(&path, "stale", |_| Ok(())).is_err());
        let mut prepared = Prepared::edit(&path, &hash, |doc| {
            doc["edge"]["output"] = value("other");
            Ok(())
        })
        .unwrap();
        prepared.commit().unwrap();
        let user_edit = fs::read_to_string(&path).unwrap() + "\n# External edit\n";
        fs::write(&path, &user_edit).unwrap();
        drop(prepared);
        assert_eq!(fs::read_to_string(&path).unwrap(), user_edit);
    }
    #[test]
    fn confirmed_commit_preserves_other_settings_and_initial_backup() {
        let (_directory, path) = config();
        let original = fs::read_to_string(&path).unwrap();
        let hash = revision(&path).unwrap();
        let mut prepared = Prepared::edit(&path, &hash, |doc| {
            doc["edge"]["boundary"]["edge"] = value("bottom");
            Ok(())
        })
        .unwrap();
        prepared.commit().unwrap();
        prepared.finish();
        let next = Config::load(&path).unwrap();
        assert_eq!(next.edge.boundary.edge, crate::geometry::Edge::Bottom);
        assert!(next.native_touchpads);
        assert_eq!(next.peer_name, "desktop");
        assert_eq!(
            fs::read_to_string(path.with_extension("before-ui.toml")).unwrap(),
            original
        );
    }
    #[test]
    fn pairing_binds_the_reviewed_fingerprint_and_never_imports_a_private_key() {
        let (directory, path) = config();
        let other = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        for path in [directory.path(), other.path()] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        identity::create(directory.path(), "local").unwrap();
        identity::create(other.path(), "peer").unwrap();
        let candidate = other.path().join("identity.pem");
        let hash = revision(&path).unwrap();
        assert!(import_peer(&path, &candidate, &hash, "wrong-fingerprint").is_err());
        assert!(!directory.path().join("peer.pem").exists());
        assert!(certificate(&other.path().join("identity.key.pem")).is_err());
        let info = certificate(&candidate).unwrap();
        assert_eq!(info.name, "peer");
        import_peer(&path, &candidate, &hash, &info.fingerprint).unwrap();
        assert_eq!(Config::load(&path).unwrap().peer_name, "peer");
        assert_eq!(
            certificate(&directory.path().join("peer.pem"))
                .unwrap()
                .fingerprint,
            info.fingerprint
        );
    }
}
