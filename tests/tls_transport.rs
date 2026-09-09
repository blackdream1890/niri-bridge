// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::Result;
use niri_bridge::{
    protocol::{self, InputEvent, Message},
    receiver::{InputSink, Receiver},
    transport::{self, Identity},
};
use rustls::pki_types::PrivatePkcs8KeyDer;
use std::sync::{Arc, Mutex};
use tokio::{io::AsyncWriteExt, net::TcpListener};

fn identity(name: &str) -> Identity {
    let generated = rcgen::generate_simple_self_signed(vec![name.to_owned()]).unwrap();
    Identity::from_der(
        generated.cert.der().clone(),
        PrivatePkcs8KeyDer::from(generated.signing_key.serialize_der()).into(),
    )
}

#[test]
fn pem_identity_loading_preserves_private_key_permissions_and_symlink_checks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let directory = tempfile::tempdir().unwrap();
    let generated = rcgen::generate_simple_self_signed(vec!["pem.test".into()]).unwrap();
    let certificate = directory.path().join("identity.pem");
    let key = directory.path().join("identity.key.pem");
    std::fs::write(&certificate, generated.cert.pem()).unwrap();
    std::fs::write(&key, generated.signing_key.serialize_pem()).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let loaded = Identity::load(&certificate, &key).unwrap();
    assert_eq!(loaded.certificate, *generated.cert.der());
    transport::client_config(&loaded, loaded.certificate.clone()).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Identity::load(&certificate, &key).is_err());
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let linked_key = directory.path().join("linked.key.pem");
    symlink(&key, &linked_key).unwrap();
    assert!(Identity::load(&certificate, &linked_key).is_err());
}

#[test]
fn pem_certificate_loading_requires_exactly_one_certificate() {
    let directory = tempfile::tempdir().unwrap();
    let generated = rcgen::generate_simple_self_signed(vec!["single.test".into()]).unwrap();
    let path = directory.path().join("peer.pem");
    std::fs::write(&path, generated.cert.pem()).unwrap();
    assert_eq!(
        transport::load_certificate(&path).unwrap(),
        *generated.cert.der()
    );
    std::fs::write(&path, generated.cert.pem().repeat(2)).unwrap();
    assert!(transport::load_certificate(&path).is_err());
    std::fs::write(&path, generated.signing_key.serialize_pem()).unwrap();
    assert!(transport::load_certificate(&path).is_err());
    std::fs::write(&path, "not a PEM certificate").unwrap();
    assert!(transport::load_certificate(&path).is_err());
}

struct Sink(Arc<Mutex<Vec<InputEvent>>>);
impl InputSink for Sink {
    fn emit(&mut self, event: &InputEvent) -> Result<()> {
        self.0.lock().unwrap().push(event.clone());
        Ok(())
    }
}

#[tokio::test]
async fn paired_peers_exchange_input_and_disconnect_releases_held_keys() {
    let a = identity("desktop.test");
    let b = identity("laptop.test");
    let server = transport::server_config(&a, b.certificate.clone()).unwrap();
    let client = transport::client_config(&b, a.certificate.clone()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut tls = transport::accept(stream, server).await.unwrap();
        transport::hello(&mut tls).await.unwrap();
        let mut receiver = Receiver::new(Sink(captured));
        while let Ok(message) = protocol::read_frame(&mut tls).await {
            match message {
                Message::Begin { session, .. } => {
                    assert!(receiver.begin(session).unwrap());
                }
                Message::Input {
                    session,
                    sequence,
                    event,
                } => {
                    receiver.input(session, sequence, &event).unwrap();
                }
                _ => panic!("Unexpected test message"),
            }
        }
        // Receiver drop is the cleanup path used when the connection disappears.
    });
    let mut tls = transport::connect(&address.to_string(), "desktop.test", client)
        .await
        .unwrap();
    transport::hello(&mut tls).await.unwrap();
    protocol::write_frame(
        &mut tls,
        &Message::Begin {
            session: 1,
            entry: niri_bridge::protocol::EdgePosition {
                edge_id: "default".into(),
                fraction: 0.5,
            },
        },
    )
    .await
    .unwrap();
    protocol::write_frame(
        &mut tls,
        &Message::Input {
            session: 1,
            sequence: 0,
            event: InputEvent::Key {
                code: 29,
                pressed: true,
            },
        },
    )
    .await
    .unwrap();
    protocol::write_frame(
        &mut tls,
        &Message::Input {
            session: 1,
            sequence: 1,
            event: InputEvent::Button {
                code: 272,
                pressed: true,
            },
        },
    )
    .await
    .unwrap();
    tls.shutdown().await.unwrap();
    drop(tls);
    task.await.unwrap();
    let actual = events.lock().unwrap();
    assert!(
        actual.as_slice()
            == [
                InputEvent::Key {
                    code: 29,
                    pressed: true
                },
                InputEvent::Button {
                    code: 272,
                    pressed: true
                },
                InputEvent::Key {
                    code: 29,
                    pressed: false
                },
                InputEvent::Button {
                    code: 272,
                    pressed: false
                }
            ]
    );
}

#[tokio::test]
async fn unpaired_client_is_rejected_before_protocol_input() {
    let a = identity("desktop.test");
    let expected = identity("laptop.test");
    let stranger = identity("stranger.test");
    let server = transport::server_config(&a, expected.certificate.clone()).unwrap();
    let client = transport::client_config(&stranger, a.certificate.clone()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        transport::accept(stream, server).await.is_err()
    });
    if let Ok(mut tls) = transport::connect(&address.to_string(), "desktop.test", client).await {
        assert!(transport::hello(&mut tls).await.is_err());
    }
    assert!(task.await.unwrap());
}

#[tokio::test]
async fn wrong_server_name_is_rejected_even_with_a_trusted_certificate() {
    let a = identity("desktop.test");
    let b = identity("laptop.test");
    let server = transport::server_config(&a, b.certificate.clone()).unwrap();
    let client = transport::client_config(&b, a.certificate.clone()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _ = transport::accept(stream, server).await;
    });
    assert!(
        transport::connect(&address.to_string(), "wrong.test", client)
            .await
            .is_err()
    );
    task.await.unwrap();
}

#[tokio::test]
async fn untrusted_server_is_rejected() {
    let a = identity("desktop.test");
    let b = identity("laptop.test");
    let wrong = identity("desktop.test");
    let server = transport::server_config(&a, b.certificate.clone()).unwrap();
    let client = transport::client_config(&b, wrong.certificate.clone()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _ = transport::accept(stream, server).await;
    });
    assert!(
        transport::connect(&address.to_string(), "desktop.test", client)
            .await
            .is_err()
    );
    task.await.unwrap();
}
