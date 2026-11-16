//! An `IdP` served over TLS with a private CA (discovery and JWKS only) and a
//! CONNECT-only forward proxy, for tests that go through the production HTTP
//! client: OS trust store, custom CA bundles, `HTTPS_PROXY`.
//!
//! Trimmed from `libs/toolkit-http/tests/common/mod.rs`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};

use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const REALM: &str = "realms/tls";
const DISCOVERY_SUFFIX: &str = "/.well-known/openid-configuration";
const JWKS_SUFFIX: &str = "/protocol/openid-connect/certs";

fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        drop(toolkit_security::tls::configured_provider().install_default());
    });
}

/// A private CA and a `localhost` leaf it signed.
struct Pki {
    ca_pem: String,
    leaf: CertificateDer<'static>,
    leaf_key: PrivateKeyDer<'static>,
}

fn private_pki() -> Pki {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "oidc-authn-plugin test CA");
    let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().expect("CA key"))
        .expect("CA certificate");

    let leaf_key = KeyPair::generate().expect("leaf key");
    let leaf = CertificateParams::new(vec!["localhost".to_owned()])
        .expect("leaf params")
        .signed_by(&leaf_key, &ca)
        .expect("leaf certificate");

    Pki {
        ca_pem: ca.pem(),
        leaf: leaf.der().clone(),
        leaf_key: PrivatePkcs8KeyDer::from(leaf_key.serialize_der()).into(),
    }
}

/// `IdP` on `https://localhost:<port>/realms/tls`, signed by a private CA.
pub struct TlsIdp {
    addr: SocketAddr,
    ca_pem: String,
    connections: Arc<AtomicUsize>,
}

impl TlsIdp {
    /// Start the server; it serves discovery and the test JWKS.
    ///
    /// # Panics
    ///
    /// Panics if the certificates, TLS config or listener cannot be set up.
    pub async fn spawn() -> Self {
        install_crypto_provider();
        let pki = private_pki();
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![pki.leaf.clone()], pki.leaf_key.clone_key())
            .expect("server TLS config");
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind IdP");
        let addr = listener.local_addr().expect("IdP address");
        let connections = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&connections);
        let issuer = issuer_for(addr);
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let acceptor = acceptor.clone();
                let issuer = issuer.clone();
                tokio::spawn(async move {
                    // A rejected handshake is the point of some tests.
                    if let Ok(mut tls) = acceptor.accept(tcp).await {
                        serve_idp(&mut tls, &issuer).await;
                    }
                });
            }
        });

        Self {
            addr,
            ca_pem: pki.ca_pem,
            connections,
        }
    }

    /// The issuer URL; its discovery document names the same issuer.
    #[must_use]
    pub fn issuer(&self) -> String {
        issuer_for(self.addr)
    }

    /// `localhost:<port>`, as a CONNECT request names it.
    #[must_use]
    pub fn authority(&self) -> String {
        format!("localhost:{}", self.addr.port())
    }

    /// TCP connections accepted, handshake or not.
    #[must_use]
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    /// Write the CA certificate to a fresh PEM file and return its path.
    ///
    /// # Panics
    ///
    /// Panics if the file cannot be written.
    #[must_use]
    pub fn write_ca_bundle(&self) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "oidc-authn-plugin-tls-idp-ca-{}-{}.pem",
            std::process::id(),
            self.addr.port()
        ));
        std::fs::write(&path, &self.ca_pem).expect("write CA bundle");
        path
    }
}

fn issuer_for(addr: SocketAddr) -> String {
    format!("https://localhost:{}/{REALM}", addr.port())
}

/// Answer requests on one keep-alive connection until the client stops.
async fn serve_idp<S>(stream: &mut S, issuer: &str)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    while let Some(head) = read_head(stream).await {
        let path = head
            .lines()
            .next()
            .and_then(|line| line.split(' ').nth(1))
            .unwrap_or_default();
        let body = if path.ends_with(DISCOVERY_SUFFIX) {
            Some(
                json!({
                    "issuer": issuer,
                    "jwks_uri": format!("{issuer}{JWKS_SUFFIX}"),
                })
                .to_string(),
            )
        } else if path.ends_with(JWKS_SUFFIX) {
            Some(super::test_jwk_json().to_owned())
        } else {
            None
        };
        let response = body.map_or_else(
            || "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n".to_owned(),
            |body| {
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                )
            },
        );
        if stream.write_all(response.as_bytes()).await.is_err() {
            return;
        }
    }
}

/// Read up to the end of a request head; `None` when the peer is gone.
async fn read_head<S: AsyncRead + Unpin>(stream: &mut S) -> Option<String> {
    let mut buf = Vec::new();
    let mut byte = [0_u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await.ok()? == 0 || buf.len() > 16 * 1024 {
            return None;
        }
        buf.push(byte[0]);
    }
    String::from_utf8(buf).ok()
}

/// CONNECT-only forward proxy that records the target of every tunnel.
pub struct ConnectProxy {
    addr: SocketAddr,
    targets: Arc<Mutex<Vec<String>>>,
}

impl ConnectProxy {
    /// Start the proxy on an ephemeral loopback port.
    ///
    /// # Panics
    ///
    /// Panics if the listener cannot be bound.
    pub async fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
        let addr = listener.local_addr().expect("proxy address");
        let targets = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&targets);
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let log = Arc::clone(&log);
                tokio::spawn(tunnel(client, log));
            }
        });
        Self { addr, targets }
    }

    /// `http://127.0.0.1:<port>`, for `HTTPS_PROXY`.
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// CONNECT targets seen so far, in order.
    ///
    /// # Panics
    ///
    /// Panics if the log lock is poisoned.
    #[must_use]
    pub fn targets(&self) -> Vec<String> {
        self.targets.lock().expect("proxy log").clone()
    }
}

async fn tunnel(mut client: TcpStream, log: Arc<Mutex<Vec<String>>>) {
    let Some(head) = read_head(&mut client).await else {
        return;
    };
    let Some(target) = head
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("CONNECT "))
        .and_then(|rest| rest.split(' ').next())
        .map(str::to_owned)
    else {
        drop(
            client
                .write_all(b"HTTP/1.1 405 Method Not Allowed\r\ncontent-length: 0\r\n\r\n")
                .await,
        );
        return;
    };
    log.lock().expect("proxy log").push(target.clone());
    let Ok(mut upstream) = TcpStream::connect(&target).await else {
        drop(client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await);
        return;
    };
    if client
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .await
        .is_ok()
    {
        drop(tokio::io::copy_bidirectional(&mut client, &mut upstream).await);
    }
}
