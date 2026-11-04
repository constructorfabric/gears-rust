//! Local upstreams for engine tests and benches.

// Every test binary includes this module, each with its own lint
// allowances; the discarded results here (provider already installed, a
// test server connection ending) are deliberate in all of them.
#![allow(dead_code, clippy::let_underscore_must_use)]

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use toolkit_http_proxy::{
    ProtocolPolicy, ProxyClient, ProxyClientConfig, RootStore, Target, Transport,
};

pub type RespBody = BoxBody<Bytes, Infallible>;

/// Install the process crypto provider (FIPS builds require one). Same
/// per-platform choice as `toolkit-http`: Apple corecrypto on macOS, CNG on
/// Windows (empty unless the host is in FIPS mode), AWS-LC FIPS elsewhere.
pub fn install_provider() {
    let provider = toolkit_security::tls::configured_provider();
    #[cfg(all(feature = "fips", target_os = "windows"))]
    assert!(
        !provider.cipher_suites.is_empty(),
        "fips tests need Windows FIPS policy enabled"
    );
    drop(provider.install_default());
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    install_provider();
    rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .expect("crypto provider installed")
}

/// One self-signed identity per test process for `localhost` /
/// `127.0.0.1`; clients trust it through [`RootStore::Custom`].
struct Identity {
    cert: rustls_pki_types::CertificateDer<'static>,
    key: Vec<u8>,
}

fn identity() -> &'static Identity {
    static ID: OnceLock<Identity> = OnceLock::new();
    ID.get_or_init(|| {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("key");
        let params =
            rcgen::CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
                .expect("params");
        let cert = params.self_signed(&key).expect("cert");
        Identity {
            cert: cert.der().clone(),
            key: key.serialize_der(),
        }
    })
}

/// Server config for the shared identity with `alpn`.
pub fn server_tls(alpn: &[&[u8]]) -> rustls::ServerConfig {
    let id = identity();
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![id.cert.clone()],
            rustls_pki_types::PrivateKeyDer::Pkcs8(id.key.clone().into()),
        )
        .expect("server cert");
    cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    cfg
}

/// Root store trusting the shared identity.
pub fn test_roots() -> Arc<rustls::RootCertStore> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(identity().cert.clone()).expect("add root");
    Arc::new(roots)
}

/// What an upstream speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    Plain,
    PlainH2,
    TlsH1,
    TlsH2,
    TlsBoth,
    /// TLS without any ALPN support.
    TlsNoAlpn,
}

/// A running upstream; counts accepted TCP connections.
pub struct Upstream {
    pub addr: SocketAddr,
    pub connections: Arc<AtomicUsize>,
}

impl Upstream {
    pub fn accepted(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    pub fn target(&self, wire: Wire) -> Target {
        let transport = match wire {
            Wire::Plain | Wire::PlainH2 => Transport::Plain,
            _ => Transport::Tls,
        };
        let t = Target::new(self.addr, "localhost", transport);
        if wire == Wire::PlainH2 {
            t.with_policy(ProtocolPolicy::Http2Only)
        } else {
            t
        }
    }
}

/// Start a hyper upstream speaking `wire` and answering with `handler`.
pub async fn upstream<F, Fut>(wire: Wire, handler: F) -> Upstream
where
    F: Fn(Request<Incoming>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response<RespBody>> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let connections = Arc::new(AtomicUsize::new(0));
    let acceptor = match wire {
        Wire::TlsH1 => Some(server_tls(&[b"http/1.1"])),
        Wire::TlsH2 => Some(server_tls(&[b"h2"])),
        Wire::TlsBoth => Some(server_tls(&[b"h2", b"http/1.1"])),
        Wire::TlsNoAlpn => Some(server_tls(&[])),
        Wire::Plain | Wire::PlainH2 => None,
    }
    .map(|cfg| TlsAcceptor::from(Arc::new(cfg)));

    let counter = connections.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                continue;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            let (handler, acceptor) = (handler.clone(), acceptor.clone());
            tokio::spawn(async move {
                let svc = service_fn(move |req| {
                    let handler = handler.clone();
                    async move { Ok::<_, Infallible>(handler(req).await) }
                });
                let mut builder = auto::Builder::new(TokioExecutor::new());
                match (wire, acceptor) {
                    (Wire::Plain, None) => {
                        let _ = builder
                            .http1_only()
                            .serve_connection_with_upgrades(TokioIo::new(tcp), svc)
                            .await;
                    }
                    (Wire::PlainH2, None) => {
                        let _ = builder
                            .http2_only()
                            .serve_connection(TokioIo::new(tcp), svc)
                            .await;
                    }
                    (_, Some(acceptor)) => {
                        let Ok(tls) = acceptor.accept(tcp).await else {
                            return;
                        };
                        if wire == Wire::TlsH2 {
                            builder = builder.http2_only();
                        }
                        let _ = builder
                            .serve_connection_with_upgrades(TokioIo::new(tls), svc)
                            .await;
                    }
                    _ => {}
                }
            });
        }
    });
    Upstream { addr, connections }
}

/// Response with a static body.
pub fn text(body: impl Into<Bytes>) -> Response<RespBody> {
    Response::new(Full::new(body.into()).boxed())
}

/// Upstream echoing method, URI, version and the received body length.
pub async fn echo(wire: Wire) -> Upstream {
    upstream(wire, |req: Request<Incoming>| async move {
        let line = format!("{} {} {:?}", req.method(), req.uri(), req.version());
        let host = req
            .headers()
            .get(http::header::HOST)
            .map(|h| h.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let n = req
            .into_body()
            .collect()
            .await
            .map_or(0, |b| b.to_bytes().len());
        text(format!("{line} host={host} len={n}"))
    })
    .await
}

/// Client config for local tests: trusts the shared test identity, with
/// full verification (works under `fips`).
pub fn test_config() -> ProxyClientConfig {
    install_provider();
    let mut cfg = ProxyClientConfig::default();
    cfg.tls.roots = RootStore::Custom(test_roots());
    cfg
}

pub fn client() -> ProxyClient {
    ProxyClient::new(test_config()).expect("client")
}

/// Read a response body to a string.
pub async fn body_string(resp: Response<toolkit_http_proxy::ResponseBody>) -> String {
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    String::from_utf8_lossy(&bytes).into_owned()
}
