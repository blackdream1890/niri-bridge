// SPDX-License-Identifier: GPL-3.0-or-later
//! Mutually authenticated TLS. Only explicitly supplied peer certificates are trusted.
use std::{io::BufReader, path::Path, sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject},
    server::WebPkiClientVerifier,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    time::timeout,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::protocol::{self, Message};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const ALPN: &[u8] = b"niri-bridge/5";

pub struct Identity {
    pub certificate: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
}

impl Identity {
    pub fn from_der(certificate: CertificateDer<'static>, key: PrivateKeyDer<'static>) -> Self {
        Self { certificate, key }
    }

    pub fn load(certificate: &Path, key: &Path) -> Result<Self> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let mut options = std::fs::OpenOptions::new();
        options.read(true).custom_flags(libc::O_NOFOLLOW);
        let file = options
            .open(key)
            .context("Cannot open the local private key")?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.len() <= 64 * 1024 && metadata.mode() & 0o077 == 0,
            "Private key must be an owner-only regular file within the size limit"
        );
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() },
            "Private key must belong to the current user"
        );
        let private = PrivateKeyDer::from_pem_reader(BufReader::new(file))
            .context("No readable private key found")?;
        Ok(Self::from_der(load_certificate(certificate)?, private))
    }
}

pub fn load_certificate(path: &Path) -> Result<CertificateDer<'static>> {
    let bytes = std::fs::read(path).context("Cannot read the certificate")?;
    ensure!(
        bytes.len() <= 64 * 1024,
        "Certificate file exceeds size limit"
    );
    let certificates =
        CertificateDer::pem_slice_iter(&bytes).collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        certificates.len() == 1,
        "Expected exactly one peer certificate"
    );
    Ok(certificates.into_iter().next().unwrap())
}

fn roots(peer: CertificateDer<'static>) -> Result<RootCertStore> {
    let mut roots = RootCertStore::empty();
    roots.add(peer)?;
    Ok(roots)
}

pub fn client_config(local: &Identity, peer: CertificateDer<'static>) -> Result<Arc<ClientConfig>> {
    let mut config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_root_certificates(roots(peer)?)
            .with_client_auth_cert(vec![local.certificate.clone()], local.key.clone_key())?;
    config.alpn_protocols = vec![ALPN.to_vec()];
    Ok(Arc::new(config))
}

pub fn server_config(local: &Identity, peer: CertificateDer<'static>) -> Result<Arc<ServerConfig>> {
    let verifier = WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots(peer)?),
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .build()?;
    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![local.certificate.clone()], local.key.clone_key())?;
    config.alpn_protocols = vec![ALPN.to_vec()];
    Ok(Arc::new(config))
}

pub async fn connect(
    address: &str,
    peer_name: &str,
    config: Arc<ClientConfig>,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let stream = timeout(HANDSHAKE_TIMEOUT, TcpStream::connect(address))
        .await
        .context("Connection timed out")??;
    stream.set_nodelay(true)?;
    let name =
        ServerName::try_from(peer_name.to_owned()).context("Invalid peer certificate name")?;
    let stream = timeout(
        HANDSHAKE_TIMEOUT,
        TlsConnector::from(config).connect(name, stream),
    )
    .await
    .context("TLS handshake timed out")??;
    ensure!(
        stream.get_ref().1.alpn_protocol() == Some(ALPN),
        "Peer protocol negotiation failed"
    );
    Ok(stream)
}

pub async fn accept(
    stream: TcpStream,
    config: Arc<ServerConfig>,
) -> Result<tokio_rustls::server::TlsStream<TcpStream>> {
    stream.set_nodelay(true)?;
    let stream = timeout(HANDSHAKE_TIMEOUT, TlsAcceptor::from(config).accept(stream))
        .await
        .context("TLS handshake timed out")??;
    ensure!(
        stream.get_ref().1.alpn_protocol() == Some(ALPN),
        "Peer protocol negotiation failed"
    );
    Ok(stream)
}

pub async fn hello(stream: &mut (impl AsyncRead + AsyncWrite + Unpin)) -> Result<()> {
    timeout(HANDSHAKE_TIMEOUT, async {
        protocol::write_frame(
            stream,
            &Message::Hello {
                version: protocol::VERSION,
            },
        )
        .await?;
        ensure!(
            matches!(
                protocol::read_frame(stream).await?,
                Message::Hello {
                    version: protocol::VERSION
                }
            ),
            "Expected protocol hello"
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("Protocol handshake timed out")??;
    Ok(())
}
