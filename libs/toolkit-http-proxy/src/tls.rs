//! TLS connectors, one per ALPN offer, built from `toolkit-http`.

use std::sync::Arc;

use tokio_rustls::TlsConnector;

use crate::config::{CertVerification, RootStore, TlsOptions};
use crate::error::{ErrorKind, ProxyError};

/// What the client offers via ALPN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlpnOffer {
    /// `h2` and `http/1.1`.
    H2AndH1,
    /// `http/1.1` only.
    H1,
    /// `h2` only.
    H2,
}

/// Connectors sharing one base `ClientConfig` (and thus one TLS session
/// cache per ALPN variant).
pub struct TlsConnectors {
    h2_and_h1: TlsConnector,
    h1: TlsConnector,
    h2: TlsConnector,
}

impl TlsConnectors {
    /// Build one connector per ALPN offer from `opts` (fails closed under
    /// `fips`).
    pub fn build(opts: &TlsOptions) -> Result<Self, ProxyError> {
        let mut base = base_config(opts)?;
        apply_verification(&mut base, opts.verification)?;

        let variant = |alpn: &[&[u8]]| -> Result<TlsConnector, ProxyError> {
            let mut cfg = base.clone();
            cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
            check_fips(&cfg)?;
            Ok(TlsConnector::from(Arc::new(cfg)))
        };

        Ok(Self {
            h2_and_h1: variant(&[b"h2", b"http/1.1"])?,
            h1: variant(&[b"http/1.1"])?,
            h2: variant(&[b"h2"])?,
        })
    }

    /// The connector for `offer`.
    pub fn get(&self, offer: AlpnOffer) -> &TlsConnector {
        match offer {
            AlpnOffer::H2AndH1 => &self.h2_and_h1,
            AlpnOffer::H1 => &self.h1,
            AlpnOffer::H2 => &self.h2,
        }
    }
}

fn base_config(opts: &TlsOptions) -> Result<rustls::ClientConfig, ProxyError> {
    let config_err = |e: toolkit_http::TlsConfigError| {
        ProxyError::new(
            ErrorKind::Configuration,
            "failed to build TLS client config",
        )
        .with_source(e)
    };
    match &opts.roots {
        RootStore::Native => {
            toolkit_http::native_roots_client_config(&opts.settings).map_err(config_err)
        }
        RootStore::WebPki => {
            toolkit_http::webpki_roots_client_config(&opts.settings).map_err(config_err)
        }
        RootStore::Custom(roots) => {
            toolkit_http::custom_roots_client_config((**roots).clone(), &opts.settings)
                .map_err(config_err)
        }
        RootStore::NativeOrWebPki => toolkit_http::native_roots_client_config(&opts.settings)
            .or_else(|e| {
                tracing::warn!(error = %e, "native root store unavailable, using webpki roots");
                toolkit_http::webpki_roots_client_config(&opts.settings)
            })
            .map_err(config_err),
    }
}

#[allow(clippy::unnecessary_wraps)] // fallible only with `test-utils`
#[cfg_attr(not(feature = "test-utils"), allow(unused_variables))]
fn apply_verification(
    cfg: &mut rustls::ClientConfig,
    mode: CertVerification,
) -> Result<(), ProxyError> {
    match mode {
        CertVerification::Verify => Ok(()),
        #[cfg(feature = "test-utils")]
        CertVerification::DangerAcceptAny => {
            if cfg!(feature = "fips") {
                return Err(ProxyError::new(
                    ErrorKind::Configuration,
                    "certificate verification cannot be disabled in a FIPS build",
                ));
            }
            let provider = Arc::clone(cfg.crypto_provider());
            cfg.dangerous()
                .set_certificate_verifier(Arc::new(danger::AcceptAny(provider)));
            Ok(())
        }
    }
}

/// Defence in depth: `toolkit-http` already hardened the base config; make
/// sure nothing done here (ALPN, verifier) changed the FIPS status.
#[allow(clippy::unnecessary_wraps)] // fallible only with `fips`
fn check_fips(cfg: &rustls::ClientConfig) -> Result<(), ProxyError> {
    #[cfg(feature = "fips")]
    if !cfg.fips() {
        return Err(ProxyError::new(
            ErrorKind::Configuration,
            "upstream TLS client config does not report FIPS mode",
        ));
    }
    #[cfg(not(feature = "fips"))]
    let _ = cfg;
    Ok(())
}

#[cfg(feature = "test-utils")]
mod danger {
    use std::sync::Arc;

    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};

    /// Accepts any server certificate but still checks handshake signatures.
    #[derive(Debug)]
    pub(super) struct AcceptAny(pub(super) Arc<CryptoProvider>);

    impl ServerCertVerifier for AcceptAny {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            verify_tls12_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            verify_tls13_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }
}
