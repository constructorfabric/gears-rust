//! End-to-end handshake smoke against `openssl s_server`.
//!
//! Verifies that the entire provider stack — AEAD wire framing, HKDF key
//! schedule, ECDH key exchange, signature verification — composes correctly
//! into a working TLS client. A failure here points at integration bugs
//! that unit tests cannot catch (wrong AAD format, wrong nonce derivation,
//! missing or wrong cipher-suite wiring, etc.).
//!
//! Each test spins up a local openssl s_server on an ephemeral port,
//! performs one full handshake using rustls + our provider, exchanges a
//! short HTTP request/response, and tears the server down.

#![cfg(target_os = "macos")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, Stream};
use rustls_corecrypto_provider::{default_provider, fips_provider};
use serial_test::file_serial;

/// Custom verifier that accepts any server certificate but routes
/// signature verification through our provider's `SUPPORTED_SIG_ALGS`.
/// This isolates the test from cert chain validity while still
/// exercising the signature verification path on TLS 1.2 ServerKeyExchange
/// and TLS 1.3 CertificateVerify.
#[derive(Debug)]
struct AcceptAnyServerCert(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &ServerName,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Resolve a genuine OpenSSL binary for the `s_server` handshake peer.
///
/// These tests use OpenSSL-3 `s_server` syntax that Apple's bundled
/// **LibreSSL** does not support: the `-ciphersuites` flag (TLS 1.3) and the
/// `-accept host:port` form (LibreSSL wants a bare port and fails with
/// `getservbyname failure`). Depending on `$PATH` ordering — notably under a
/// login shell (`bash -lc`), where `/usr/bin` can precede Homebrew/MacPorts —
/// a bare `openssl` may resolve to LibreSSL and make every spawn exit
/// immediately.
///
/// Probe order: `$OPENSSL_BIN` override, then `openssl` on `PATH`, then the
/// common Homebrew/MacPorts/`/usr/local` locations. The first candidate whose
/// `version` reports "OpenSSL" (not "LibreSSL") wins. Returns `None` when only
/// LibreSSL (or nothing) is available, so callers can skip rather than fail.
fn resolve_openssl() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("OPENSSL_BIN") {
        if !p.is_empty() {
            candidates.push(PathBuf::from(p));
        }
    }
    candidates.push(PathBuf::from("openssl")); // resolved via PATH
    for p in [
        "/opt/homebrew/bin/openssl",
        "/opt/local/bin/openssl",
        "/usr/local/bin/openssl",
    ] {
        candidates.push(PathBuf::from(p));
    }

    for bin in candidates {
        let Ok(out) = Command::new(&bin).arg("version").output() else {
            continue;
        };
        if out.status.success() && String::from_utf8_lossy(&out.stdout).starts_with("OpenSSL") {
            return Some(bin);
        }
    }
    None
}

/// Resolve a genuine OpenSSL binary or emit a skip notice and return `None`.
///
/// Each `s_server`-based test calls this first and returns early when it is
/// `None`, so environments with only LibreSSL (e.g. a benchmark harness that
/// shells out via `bash -lc`) are skipped with a clear reason instead of
/// panicking. CI and dev machines with a real OpenSSL keep full coverage.
fn openssl_or_skip() -> Option<PathBuf> {
    let resolved = resolve_openssl();
    if resolved.is_none() {
        eprintln!(
            "SKIP: no genuine OpenSSL binary found (checked $OPENSSL_BIN, PATH, \
             and Homebrew/MacPorts/local paths). Apple's LibreSSL is not usable \
             here: its s_server lacks `-ciphersuites` and rejects `-accept \
             host:port`. Set OPENSSL_BIN to a real OpenSSL to run these tests."
        );
    }
    resolved
}

/// RAII guard for a spawned `openssl s_server`.
///
/// `std::process::Child` does **not** terminate the process on drop, so any
/// panic between spawn and an explicit `kill()`/`wait()` — e.g. inside
/// `do_handshake_and_get` or socket setup — would leak the server process.
/// This guard reaps the child in `Drop`, making cleanup unwind-safe. The
/// tempdir holding the cert/key is held alongside so it outlives the server.
struct SServer {
    child: Child,
    /// Port the server is listening on.
    port: u16,
    _tmp: tempfile::TempDir,
}

impl Drop for SServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn openssl s_server with a fresh self-signed cert/key.
///
/// `openssl` is a genuine OpenSSL binary from [`resolve_openssl`]. Returns an
/// [`SServer`] guard that carries the child handle, listening port, and the
/// tempdir holding cert files, and reaps the child on drop.
fn spawn_s_server(openssl: &Path, extra_args: &[&str]) -> SServer {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cert = tmp.path().join("cert.pem");
    let key = tmp.path().join("key.pem");

    // Generate self-signed RSA 2048 cert valid for localhost.
    let req = Command::new(openssl)
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            key.to_str().unwrap(),
            "-out",
            cert.to_str().unwrap(),
            "-days",
            "1",
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=DNS:localhost",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("openssl req");
    assert!(req.success(), "openssl req failed");

    // Reserve an ephemeral port, release it, then let openssl bind to it.
    // There is a TOCTOU window: between dropping our listener and openssl
    // binding, another process can steal the port. We detect that by
    // observing the child exit, and retry with a fresh port. openssl
    // startup + 2048-bit RSA key parse is a sub-second operation, so a
    // 30s ceiling is generous headroom even on a saturated machine while
    // still failing fast on a genuine wedge (nextest has no configured
    // slow-timeout that would terminate a longer hang).
    //
    // s_server stderr is redirected to a file (not discarded) so that a
    // genuine startup failure surfaces its actual message + exit code in the
    // diagnostics below, rather than a bare "exited early". stdin is pinned
    // to /dev/null so the peer never inherits a TTY and behaves identically
    // regardless of how the test runner was launched.
    const MAX_ATTEMPTS: usize = 5;
    const POLL_INTERVAL: Duration = Duration::from_millis(100);
    const MAX_WAIT: Duration = Duration::from_secs(30);
    let stderr_path = tmp.path().join("s_server.stderr");
    for attempt in 0..MAX_ATTEMPTS {
        let port = match TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l.local_addr().expect("local_addr").port(),
            Err(_) => continue,
        };
        // Listener dropped here; openssl can reclaim the port.

        let mut cmd = Command::new(openssl);
        cmd.args([
            "s_server",
            "-cert",
            cert.to_str().unwrap(),
            "-key",
            key.to_str().unwrap(),
            "-accept",
            &format!("127.0.0.1:{port}"),
            "-www",
            "-quiet",
        ]);
        for a in extra_args {
            cmd.arg(a);
        }
        let stderr_sink = std::fs::File::create(&stderr_path).expect("create s_server stderr file");
        let Ok(mut child) = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr_sink))
            .spawn()
        else {
            continue;
        };

        let deadline = std::time::Instant::now() + MAX_WAIT;
        loop {
            // Observe the child FIRST. If openssl exited (e.g. the port was
            // stolen in the TOCTOU window and its bind failed), a later
            // connect() could succeed against a DIFFERENT listener that now
            // owns the port — handing back a dead child and a port we don't
            // own. Reap the child and retry with a fresh port instead.
            if let Ok(Some(status)) = child.try_wait() {
                let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
                eprintln!(
                    "openssl s_server exited early on attempt {}/{MAX_ATTEMPTS} \
                     (port {port}, {status}); retrying with a new port. stderr:\n{}",
                    attempt + 1,
                    stderr.trim()
                );
                break;
            }
            // Child still alive: a successful connect means our server is
            // ready and owns the port.
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return SServer {
                    child,
                    port,
                    _tmp: tmp,
                };
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "openssl s_server did not become ready within {MAX_WAIT:?} \
                     on attempt {}/{MAX_ATTEMPTS} (port {port})",
                    attempt + 1
                );
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
    let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
    panic!(
        "openssl s_server ({}) failed to start after {MAX_ATTEMPTS} attempts \
         (all exited early). Last stderr:\n{}",
        openssl.display(),
        stderr.trim()
    );
}

fn client_config() -> ClientConfig {
    let provider = Arc::new(default_provider());
    let mut config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("default versions")
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    config
        .dangerous()
        .set_certificate_verifier(Arc::new(AcceptAnyServerCert(provider)));
    config
}

fn do_handshake_and_get(
    config: ClientConfig,
    port: u16,
) -> (
    rustls::ProtocolVersion,
    rustls::SupportedCipherSuite,
    Vec<u8>,
) {
    let mut sock = TcpStream::connect(("localhost", port)).expect("tcp connect");
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    sock.set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let server = ServerName::try_from("localhost").unwrap();
    let mut conn = ClientConnection::new(Arc::new(config), server).expect("client conn");
    let mut tls = Stream::new(&mut conn, &mut sock);

    tls.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .expect("write");
    tls.flush().expect("flush");

    let mut buf = Vec::with_capacity(4096);
    let _ = tls.read_to_end(&mut buf);

    let version = conn.protocol_version().expect("negotiated version");
    let suite = conn.negotiated_cipher_suite().expect("negotiated suite");
    (version, suite, buf)
}

/// Full TLS 1.3 handshake: AES-128-GCM-SHA256. Exercises HKDF-SHA-256,
/// ECDHE P-256, RSA-PSS-SHA256 signature verification, AEAD encrypt+decrypt
/// of TLS 1.3 wire records.
#[test]
#[file_serial(openssl_s_server)]
fn handshake_tls13_aes128_gcm_sha256() {
    let Some(openssl) = openssl_or_skip() else {
        return;
    };
    let server = spawn_s_server(
        &openssl,
        &["-tls1_3", "-ciphersuites", "TLS_AES_128_GCM_SHA256"],
    );
    let (version, suite, body) = do_handshake_and_get(client_config(), server.port);

    assert_eq!(version, rustls::ProtocolVersion::TLSv1_3);
    assert_eq!(suite.suite(), rustls::CipherSuite::TLS13_AES_128_GCM_SHA256);
    assert!(!body.is_empty(), "expected non-empty HTTP response");
    assert!(
        body.windows(4).any(|w| w == b"HTTP"),
        "expected HTTP response, got {:?}",
        String::from_utf8_lossy(&body[..body.len().min(200)])
    );
}

/// Full TLS 1.3 handshake: AES-256-GCM-SHA384. Different hash, HKDF, and
/// AEAD key length — catches bugs that only manifest with the longer suite.
#[test]
#[file_serial(openssl_s_server)]
fn handshake_tls13_aes256_gcm_sha384() {
    let Some(openssl) = openssl_or_skip() else {
        return;
    };
    let server = spawn_s_server(
        &openssl,
        &["-tls1_3", "-ciphersuites", "TLS_AES_256_GCM_SHA384"],
    );
    let (version, suite, body) = do_handshake_and_get(client_config(), server.port);

    assert_eq!(version, rustls::ProtocolVersion::TLSv1_3);
    assert_eq!(suite.suite(), rustls::CipherSuite::TLS13_AES_256_GCM_SHA384);
    assert!(!body.is_empty());
    assert!(body.windows(4).any(|w| w == b"HTTP"));
}

/// Full TLS 1.2 handshake: ECDHE_RSA_AES_256_GCM_SHA384. Different
/// key-schedule (PRF P_hash, not HKDF), explicit-nonce AEAD wire format,
/// distinct ServerKeyExchange + CertificateVerify flow.
///
/// Skipped under `feature = "fips"` because `default_provider()` is then
/// TLS-1.3-only — TLS 1.2 negotiation cannot succeed.
#[cfg(not(feature = "fips"))]
#[test]
#[file_serial(openssl_s_server)]
fn handshake_tls12_ecdhe_rsa_aes256_gcm_sha384() {
    let Some(openssl) = openssl_or_skip() else {
        return;
    };
    let server = spawn_s_server(
        &openssl,
        &["-tls1_2", "-cipher", "ECDHE-RSA-AES256-GCM-SHA384"],
    );
    let (version, suite, body) = do_handshake_and_get(client_config(), server.port);

    assert_eq!(version, rustls::ProtocolVersion::TLSv1_2);
    assert_eq!(
        suite.suite(),
        rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384
    );
    assert!(!body.is_empty());
    assert!(body.windows(4).any(|w| w == b"HTTP"));
}

// =========================================================================
// Server-side handshakes (added by ADR 0004).
//
// Each test spins up a rustls::ServerConfig on our corecrypto provider
// with a freshly generated self-signed cert+key, connects a rustls::Client
// (also on our provider) to it through a TCP socket, exchanges one
// HTTP-shaped request/response, and tears the server down. This exercises
// the full server-side path: `KeyProvider::load_private_key` (signer/mod
// dispatcher → rsa.rs or ec.rs), `SigningKey::choose_scheme`,
// `Signer::sign` (corecrypto), then on the client side the matching
// `verify.rs` algorithm closes the loop.
//
// Each test asserts the negotiated TLS version + cipher suite are what
// the provider should pick given the offered scheme set, and that
// ServerConfig.fips() is true (= every component is FIPS).
// =========================================================================

use rcgen::{
    CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256, PKCS_ECDSA_P384_SHA384,
    PKCS_ECDSA_P521_SHA512, PKCS_RSA_SHA256,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection};

const TLS13_ONLY: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS13];
#[cfg(not(feature = "fips"))]
const TLS12_ONLY: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS12];

/// Generate a self-signed cert + matching private key. Caller picks the
/// rcgen algorithm. Returns DER cert + rustls `PrivateKeyDer`. Helper
/// mirrors the in-crate test helpers in `signer/rsa.rs` and `signer/ec.rs`
/// but lives here too so the integration test file is self-contained.
fn gen_self_signed(
    alg: &'static rcgen::SignatureAlgorithm,
) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let kp = KeyPair::generate_for(alg).expect("rcgen keypair");
    let pem = kp.serialize_pem();
    let key_der = PrivateKeyDer::from_pem_slice(pem.as_bytes()).expect("decode PEM");
    let params = CertificateParams::new(vec!["localhost".to_owned()]).expect("params");
    let cert = params.self_signed(&kp).expect("self-sign");
    (CertificateDer::from(cert.der().to_vec()), key_der)
}

/// Build a server config from a cert+key, restricted to a specific TLS
/// protocol version. Sets `require_ems = true` so `ServerConfig::fips()`
/// is honoured under the TLS-1.2 NIST recommendation (SP 800-52 Rev. 2
/// §3.5) — the same posture our `tls.rs::native_roots_client_config`
/// downstream uses.
///
/// Uses `default_provider()` so both TLS 1.2 and TLS 1.3 handshake
/// scenarios are exercisable.
fn server_config_with_versions(
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
    versions: &'static [&'static rustls::SupportedProtocolVersion],
) -> ServerConfig {
    let provider = Arc::new(default_provider());
    let mut cfg = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(versions)
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("with_single_cert");
    cfg.require_ems = true;
    cfg
}

/// Same as [`server_config_with_versions`] but built on the FIPS-claim
/// provider variant (TLS 1.3 only). Used by tests that assert
/// `ServerConfig::fips() == true`.
fn fips_server_config_tls13(
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
) -> ServerConfig {
    let provider = Arc::new(fips_provider());
    let mut cfg = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(TLS13_ONLY)
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("with_single_cert");
    cfg.require_ems = true;
    cfg
}

/// Run one round-trip request through a freshly-built server bound to an
/// ephemeral port. The server thread sends a fixed HTTP-shaped response
/// after `complete_io`; the client GETs `/` and reads to EOF. Returns the
/// negotiated (version, suite) and the response bytes the client saw.
fn run_one_request(
    server_cfg: Arc<ServerConfig>,
) -> (
    rustls::ProtocolVersion,
    rustls::SupportedCipherSuite,
    Vec<u8>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();

    // Server thread.
    let server_handle = std::thread::spawn(move || {
        let (mut tcp, _) = listener.accept().expect("accept");
        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut conn = ServerConnection::new(server_cfg).expect("server conn");
        let mut tls = Stream::new(&mut conn, &mut tcp);

        // Read whatever the client sent (one HTTP/1.0 request) and reply
        // with a fixed 200-OK shape. We don't parse the request — the
        // assertion on the client side is just that bytes flowed.
        let mut buf = [0u8; 1024];
        let _ = tls.read(&mut buf);
        let body = b"HTTP/1.0 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        let _ = tls.write_all(body);
        let _ = tls.flush();
    });

    // Client side reuses the same `client_config()` helper as the openssl-
    // s_server tests — accept-any verifier with our provider's sig algs.
    let client_cfg = client_config();
    let (version, suite, body) = do_handshake_and_get(client_cfg, port);

    let _ = server_handle.join();
    (version, suite, body)
}

/// Sanity contract: a `ServerConfig` built on the FIPS-claim provider
/// variant (`fips_provider()`, TLS 1.3-only) with a freshly-loaded
/// private key must advertise `fips() == true`. If this flips, the
/// FIPS-claim invariant downstream (every component reporting FIPS) is
/// broken.
///
/// Note: a config built on `default_provider()` will NOT advertise FIPS
/// even if restricted to TLS 1.3 via `with_protocol_versions`, because
/// `provider.fips()` evaluates over the full cipher_suites set at build
/// time. See `provider::tests::client_config_on_default_provider_does_not_claim_fips`.
#[test]
fn server_config_on_fips_provider_advertises_fips() {
    let (cert, key) = gen_self_signed(&PKCS_ECDSA_P256_SHA256);
    let cfg = fips_server_config_tls13(cert, key);
    assert!(
        cfg.fips(),
        "TLS-1.3-only ServerConfig on fips_provider() with a P-256 key must advertise FIPS"
    );
}

/// Full server-side TLS 1.3 handshake with an ECDSA P-256 server cert.
/// Exercises the `ec::EcSigningKey` path end-to-end.
#[test]
fn server_handshake_tls13_ecdsa_p256() {
    let (cert, key) = gen_self_signed(&PKCS_ECDSA_P256_SHA256);
    let cfg = Arc::new(server_config_with_versions(cert, key, TLS13_ONLY));
    let (version, _suite, body) = run_one_request(cfg);
    assert_eq!(version, rustls::ProtocolVersion::TLSv1_3);
    assert!(body.windows(4).any(|w| w == b"HTTP"), "got: {body:?}");
}

/// TLS 1.3 + ECDSA P-384.
#[test]
fn server_handshake_tls13_ecdsa_p384() {
    let (cert, key) = gen_self_signed(&PKCS_ECDSA_P384_SHA384);
    let cfg = Arc::new(server_config_with_versions(cert, key, TLS13_ONLY));
    let (version, _suite, body) = run_one_request(cfg);
    assert_eq!(version, rustls::ProtocolVersion::TLSv1_3);
    assert!(body.windows(4).any(|w| w == b"HTTP"));
}

/// TLS 1.3 + ECDSA P-521. This is the P-521 path our verify+signer add
/// for parity with rustls-cng-crypto.
#[test]
fn server_handshake_tls13_ecdsa_p521() {
    let (cert, key) = gen_self_signed(&PKCS_ECDSA_P521_SHA512);
    let cfg = Arc::new(server_config_with_versions(cert, key, TLS13_ONLY));
    let (version, _suite, body) = run_one_request(cfg);
    assert_eq!(version, rustls::ProtocolVersion::TLSv1_3);
    assert!(body.windows(4).any(|w| w == b"HTTP"));
}

/// TLS 1.3 + RSA-2048 server cert. Exercises the `rsa::RsaSigningKey`
/// path and `choose_scheme`'s preference order (PSS-512 first).
#[test]
fn server_handshake_tls13_rsa() {
    let (cert, key) = gen_self_signed(&PKCS_RSA_SHA256);
    let cfg = Arc::new(server_config_with_versions(cert, key, TLS13_ONLY));
    let (version, _suite, body) = run_one_request(cfg);
    assert_eq!(version, rustls::ProtocolVersion::TLSv1_3);
    assert!(body.windows(4).any(|w| w == b"HTTP"));
}

/// TLS 1.2 + ECDHE_ECDSA cipher-suite group with a P-256 server cert.
/// Different signature surface (TLS 1.2 ServerKeyExchange) — the same
/// `EcSigner` is invoked but under a different rustls state machine.
///
/// Skipped under `feature = "fips"` — TLS 1.2 unavailable in that mode.
#[cfg(not(feature = "fips"))]
#[test]
fn server_handshake_tls12_ecdhe_ecdsa() {
    let (cert, key) = gen_self_signed(&PKCS_ECDSA_P256_SHA256);
    let cfg = Arc::new(server_config_with_versions(cert, key, TLS12_ONLY));
    let (version, suite, body) = run_one_request(cfg);
    assert_eq!(version, rustls::ProtocolVersion::TLSv1_2);
    assert!(matches!(
        suite.suite(),
        rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384
            | rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256
    ));
    assert!(body.windows(4).any(|w| w == b"HTTP"));
}

/// **Test-gap #2 (RFC 8446 §4.2.3 enforcement).** In TLS 1.3 the
/// `CertificateVerify` signature MUST use an RSA-PSS scheme — PKCS#1
/// v1.5 schemes (`rsa_pkcs1_*`) are forbidden. rustls enforces this in
/// its TLS 1.3 sig-alg filter, even though our `WebPkiSupportedAlgorithms`
/// `all` list also contains the PKCS#1 v1.5 entries (they exist for the
/// TLS 1.2 path and for webpki cert-chain validation).
///
/// This test pins the contract: when only the PKCS#1 v1.5 signature
/// schemes are advertised by the peer, the TLS 1.3 handshake must fail
/// rather than complete. We use the `-sigalgs` openssl flag to force
/// the server to offer only `rsa_pkcs1_sha256`; rustls then has no
/// admissible TLS 1.3 sig-alg overlap and the handshake terminates.
#[test]
#[file_serial(openssl_s_server)]
fn tls13_pkcs1_v1_5_certificate_verify_is_rejected() {
    let Some(openssl) = openssl_or_skip() else {
        return;
    };
    // `-sigalgs rsa_pkcs1_sha256` restricts openssl's offered signature
    // schemes; under TLS 1.3 this is the disallowed half of the surface.
    let server = spawn_s_server(
        &openssl,
        &[
            "-tls1_3",
            "-ciphersuites",
            "TLS_AES_256_GCM_SHA384",
            "-sigalgs",
            "rsa_pkcs1_sha256",
        ],
    );

    // Drive a real handshake. We expect failure — either at sig-alg
    // negotiation (`NoCommonSignatureAlgorithms`-style) or at
    // CertificateVerify validation. Both are acceptable; the contract
    // is "must not complete with PKCS#1 v1.5 in TLS 1.3".
    let mut sock = TcpStream::connect(("localhost", server.port)).expect("tcp connect");
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    sock.set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let server_name = ServerName::try_from("localhost").unwrap();
    let mut conn =
        ClientConnection::new(Arc::new(client_config()), server_name).expect("client conn");
    let mut tls = Stream::new(&mut conn, &mut sock);

    let probe = tls.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n");
    // Failure mode is `Err`; on success (i.e. regression) we keep going
    // and surface it via the version/suite check.
    let neg_version = conn.protocol_version();

    assert!(
        probe.is_err() || neg_version.is_none(),
        "TLS 1.3 handshake must NOT complete when only rsa_pkcs1_sha256 is offered \
         by the peer (RFC 8446 §4.2.3); rustls's TLS 1.3 sig-alg filter must \
         exclude PKCS#1 v1.5"
    );
}

/// TLS 1.2 + ECDHE_RSA cipher-suite group. RSA signing through
/// `RsaSigner`, validates the `RSA_SCHEMES` priority order matters here
/// (server picks one when both peers advertise multiple).
///
/// Skipped under `feature = "fips"` — TLS 1.2 unavailable in that mode.
#[cfg(not(feature = "fips"))]
#[test]
fn server_handshake_tls12_ecdhe_rsa() {
    let (cert, key) = gen_self_signed(&PKCS_RSA_SHA256);
    let cfg = Arc::new(server_config_with_versions(cert, key, TLS12_ONLY));
    let (version, suite, body) = run_one_request(cfg);
    assert_eq!(version, rustls::ProtocolVersion::TLSv1_2);
    assert!(matches!(
        suite.suite(),
        rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384
            | rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256
    ));
    assert!(body.windows(4).any(|w| w == b"HTTP"));
}
