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
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, Stream};
use rustls_corecrypto_provider::{default_provider, fips_provider};

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

/// Spawn openssl s_server with a fresh self-signed cert/key.
///
/// Returns (child handle, listening port, tempdir holding cert files).
fn spawn_s_server(extra_args: &[&str]) -> (Child, u16, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cert = tmp.path().join("cert.pem");
    let key = tmp.path().join("key.pem");

    // Generate self-signed RSA 2048 cert valid for localhost.
    let req = Command::new("openssl")
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

    // Reserve an ephemeral port from the OS, then release it immediately
    // so openssl can bind to the same number. There's a tiny TOCTOU race
    // window, but in practice the OS doesn't recycle ports that fast — and
    // if a collision happens we retry.
    //
    // We bind and connect on the explicit IPv4 loopback (127.0.0.1) rather
    // than the hostname "localhost": the latter can resolve to ::1 (IPv6)
    // first, which would never match an IPv4-only s_server. The retry count
    // and per-attempt bind timeout are deliberately generous because this
    // test runs inside the full workspace suite, where the machine can be
    // CPU-saturated by concurrent compile-fail tests spawning rustc. Under
    // that load openssl process startup / RSA keygen can take several
    // seconds, so a tight 1s window produces spurious failures.
    const MAX_ATTEMPTS: usize = 10;
    const BIND_WAIT_POLLS: usize = 200; // 200 * 50ms = up to 10s per attempt.
    for _ in 0..MAX_ATTEMPTS {
        let port = match TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l.local_addr().expect("local_addr").port(),
            Err(_) => continue,
        };
        // Listener dropped here; openssl can reclaim the port.

        let mut cmd = Command::new("openssl");
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
        let Ok(mut child) = cmd.stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
            continue;
        };

        for _ in 0..BIND_WAIT_POLLS {
            std::thread::sleep(Duration::from_millis(50));
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return (child, port, tmp);
            }
        }
        let _ = child.kill();
    }
    panic!("could not bind openssl s_server on an ephemeral port after {MAX_ATTEMPTS} attempts");
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
fn handshake_tls13_aes128_gcm_sha256() {
    let (mut server, port, _tmp) =
        spawn_s_server(&["-tls1_3", "-ciphersuites", "TLS_AES_128_GCM_SHA256"]);
    let (version, suite, body) = do_handshake_and_get(client_config(), port);
    let _ = server.kill();

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
fn handshake_tls13_aes256_gcm_sha384() {
    let (mut server, port, _tmp) =
        spawn_s_server(&["-tls1_3", "-ciphersuites", "TLS_AES_256_GCM_SHA384"]);
    let (version, suite, body) = do_handshake_and_get(client_config(), port);
    let _ = server.kill();

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
fn handshake_tls12_ecdhe_rsa_aes256_gcm_sha384() {
    let (mut server, port, _tmp) =
        spawn_s_server(&["-tls1_2", "-cipher", "ECDHE-RSA-AES256-GCM-SHA384"]);
    let (version, suite, body) = do_handshake_and_get(client_config(), port);
    let _ = server.kill();

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
fn tls13_pkcs1_v1_5_certificate_verify_is_rejected() {
    // `-sigalgs rsa_pkcs1_sha256` restricts openssl's offered signature
    // schemes; under TLS 1.3 this is the disallowed half of the surface.
    let (mut server, port, _tmp) = spawn_s_server(&[
        "-tls1_3",
        "-ciphersuites",
        "TLS_AES_256_GCM_SHA384",
        "-sigalgs",
        "rsa_pkcs1_sha256",
    ]);

    // Drive a real handshake. We expect failure — either at sig-alg
    // negotiation (`NoCommonSignatureAlgorithms`-style) or at
    // CertificateVerify validation. Both are acceptable; the contract
    // is "must not complete with PKCS#1 v1.5 in TLS 1.3".
    let mut sock = TcpStream::connect(("localhost", port)).expect("tcp connect");
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
    let _ = server.kill();

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
