//! Shared native provider policy for primitives and protocol integration.

use rustls::crypto::CryptoProvider;

/// Select the native provider independently of the process-wide TLS provider.
pub fn configured_provider() -> CryptoProvider {
    #[cfg(all(feature = "fips", target_os = "macos"))]
    {
        rustls_corecrypto_provider::fips_provider()
    }
    #[cfg(all(feature = "fips", target_os = "windows"))]
    {
        rustls_cng_crypto::fips_provider()
    }
    #[cfg(not(all(feature = "fips", any(target_os = "macos", target_os = "windows"))))]
    {
        rustls::crypto::aws_lc_rs::default_provider()
    }
}
