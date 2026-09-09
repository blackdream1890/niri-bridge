// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::Path,
};

/// Creates only this application's new identity, never overwriting an existing key or certificate.
pub fn create(directory: &Path, name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 63
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !name.starts_with('-')
            && !name.ends_with('-'),
        "Identity name must be a lowercase DNS label"
    );
    if !directory.exists() {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(directory)
            .context("Create the identity parent directory first")?;
    }
    let metadata = fs::symlink_metadata(directory)?;
    ensure!(
        metadata.is_dir()
            && metadata.mode() & 0o077 == 0
            && metadata.uid() == unsafe { libc::geteuid() },
        "Identity directory must be owned by you and have mode 0700"
    );
    let key_path = directory.join("identity.key.pem");
    let cert_path = directory.join("identity.pem");
    ensure!(
        !key_path.exists() && !cert_path.exists(),
        "An identity already exists; it was not changed"
    );
    let key = rcgen::KeyPair::generate()?;
    let mut params = rcgen::CertificateParams::new(vec![name.to_owned()])?;
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(365);
    params.extended_key_usages = vec![
        rcgen::ExtendedKeyUsagePurpose::ServerAuth,
        rcgen::ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let certificate = params.self_signed(&key)?;
    let mut private = tempfile::NamedTempFile::new_in(directory)?;
    private.write_all(key.serialize_pem().as_bytes())?;
    private.as_file().sync_all()?;
    let mut public = tempfile::NamedTempFile::new_in(directory)?;
    public.write_all(certificate.pem().as_bytes())?;
    public.as_file().sync_all()?;
    private.persist_noclobber(&key_path).map_err(|_| {
        anyhow::anyhow!("Could not create the private key without overwriting a file")
    })?;
    if public.persist_noclobber(&cert_path).is_err() {
        let _ = fs::remove_file(&key_path);
        anyhow::bail!("Could not create the certificate without overwriting a file");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_is_private_and_cannot_be_silently_replaced() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("identity");
        create(&path, "desktop").unwrap();
        let original = fs::read(path.join("identity.key.pem")).unwrap();
        assert_eq!(
            fs::metadata(path.join("identity.key.pem")).unwrap().mode() & 0o777,
            0o600
        );
        assert!(create(&path, "other").is_err());
        assert!(fs::read(path.join("identity.key.pem")).unwrap() == original);
        assert!(
            crate::transport::Identity::load(
                &path.join("identity.pem"),
                &path.join("identity.key.pem")
            )
            .is_ok()
        );
    }
}
