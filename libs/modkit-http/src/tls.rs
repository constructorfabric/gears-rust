//! TLS utilities for the HTTP client.
//!
//! This module provides cached loading of native root certificates to avoid
//! repeated OS certificate store lookups (which can be slow on some platforms).

use rustls_pki_types::CertificateDer;
use std::sync::{Arc, OnceLock};

/// Cached native root certificates.
/// Always stores Ok; empty vec means no certs found (warned, not errored).
static NATIVE_ROOTS_CACHE: OnceLock<Vec<CertificateDer<'static>>> = OnceLock::new();

/// Counter for test verification that the loader only runs once.
#[cfg(test)]
static LOAD_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Load native root certificates from the OS certificate store.
///
/// This function is called once and the result is cached for subsequent calls.
/// Returns Ok with potentially empty vec; missing certs are warned, not errored.
fn load_native_certs_inner() -> Vec<CertificateDer<'static>> {
    #[cfg(test)]
    LOAD_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    let result = rustls_native_certs::load_native_certs();

    // Log any errors encountered during loading
    if !result.errors.is_empty() {
        for err in &result.errors {
            tracing::warn!(error = %err, "error loading native root certificate");
        }
    }

    let certs: Vec<CertificateDer<'static>> = result.certs;

    if certs.is_empty() {
        tracing::warn!("no native root CA certificates found");
    } else {
        tracing::debug!(count = certs.len(), "loaded native root certificates");
    }

    certs
}

/// Get cached native root certificates.
///
/// Returns a reference to the cached certificates (may be empty).
/// The certificates are loaded lazily on first call and cached for all subsequent calls.
pub fn native_root_certs() -> &'static [CertificateDer<'static>] {
    NATIVE_ROOTS_CACHE
        .get_or_init(load_native_certs_inner)
        .as_slice()
}

/// Get the crypto provider for TLS connections.
///
/// This function follows the reqwest pattern:
/// 1. Check if a default provider is already installed globally
/// 2. If yes, use that (respects user configuration)
/// 3. If no, create a new aws-lc-rs / corecrypto / AWS-LC FIPS provider
///    **without** installing it globally
///
/// This avoids global state mutation and is safe to call from multiple
/// threads.
///
/// ## Two-providers caveat (non-FIPS only)
///
/// The fallback at step (3) does **not** call `install_default()`. If
/// `modkit::bootstrap::init_crypto_provider` (the canonical install
/// site) has not yet run, every call into `get_crypto_provider()` will
/// rebuild a provider — and `CryptoProvider::get_default()` continues
/// to observe the absence. In practice this is benign because each
/// build returns an `Arc` over the same module-level statics inside
/// the underlying provider crate (corecrypto's `default_provider()`
/// itself caches a process-wide `Arc<CryptoProvider>`, so the
/// "concurrent providers" are byte-identical handles), but it is **not
/// the same** as having a global default installed: code paths that
/// later call `get_default()` (e.g. rustls internals that re-detect)
/// will still see `None`.
///
/// **The canonical entry point is `modkit::bootstrap::init_crypto_provider`**.
/// Callers outside the bootstrap path (probe binaries, ad-hoc tests)
/// should invoke it first. The fallback here is a safety net for code
/// that genuinely cannot run bootstrap, not a substitute for it.
///
/// Under `--features fips`, `build_client_config` bypasses this fallback
/// entirely and instead returns [`TlsConfigError::NoCryptoProvider`] when
/// no global provider is installed — silently constructing an uninstalled
/// FIPS provider would mask a misconfigured bootstrap.
// Under `--features fips`, `build_client_config` no longer routes through this
// function — it goes straight to `CryptoProvider::get_default()` and fails
// closed when none is installed. The function still exists for the non-FIPS
// path; suppress the unused-function lint for the FIPS build.
#[cfg_attr(feature = "fips", allow(dead_code))]
pub fn get_crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| {
            // Provider selection mirrors `modkit::bootstrap::init_crypto_provider`:
            //   - fips + macOS    → Apple corecrypto, TLS 1.3-only (via the
            //                       corecrypto-provider's own `fips` feature
            //                       forwarded by modkit-http's `fips`).
            //                       `default_provider()` under `feature = "fips"`
            //                       is aliased to `fips_provider()` semantics by
            //                       the corecrypto crate itself — same single-
            //                       entry-point pattern as `rustls-cng-crypto`.
            //   - fips + Windows  → Windows CNG (FIPS-Approved set).
            //                       No `fips::enabled()` re-check here: that
            //                       gate is owned by `init_crypto_provider`
            //                       and runs once at startup.
            //   - fips + other    → AWS-LC FIPS.
            //   - non-fips        → AWS-LC default.
            #[cfg(all(feature = "fips", target_os = "macos"))]
            {
                Arc::new(rustls_corecrypto_provider::default_provider())
            }
            #[cfg(all(feature = "fips", target_os = "windows"))]
            {
                let provider = rustls_cng_crypto::fips_provider();
                assert!(
                    !provider.cipher_suites.is_empty(),
                    "Windows is not in FIPS mode (FipsAlgorithmPolicy != 1). \
                     Enable system-wide FIPS via Group Policy and reboot, \
                     or call modkit::bootstrap::init_crypto_provider() first \
                     for the canonical fail-closed path."
                );
                Arc::new(provider)
            }
            #[cfg(all(feature = "fips", not(any(target_os = "macos", target_os = "windows"))))]
            {
                Arc::new(rustls::crypto::default_fips_provider())
            }
            #[cfg(not(feature = "fips"))]
            {
                Arc::new(rustls::crypto::aws_lc_rs::default_provider())
            }
        })
}

/// Error type returned by the fallible TLS-config builders in this module.
///
/// The `Other` variant carries a boxed `dyn Error` so the source-error chain
/// from rustls (and any future foreign error) is preserved end-to-end —
/// downstream `HttpError::Tls(_)` wraps this and reports a proper `.source()`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TlsConfigError {
    /// `rustls::crypto::CryptoProvider::get_default()` returned `None` when a
    /// FIPS-mode TLS config was requested. Under `--features fips` the
    /// crypto provider must be installed up-front via
    /// `modkit::bootstrap::init_crypto_provider`; falling back to an
    /// uninstalled provider here would silently bypass the canonical
    /// install path and yield a config whose FIPS-validation status is
    /// indeterminate.
    #[error(
        "rustls CryptoProvider has not been installed; call \
         modkit::bootstrap::init_crypto_provider() before building any TLS \
         config under --features fips"
    )]
    NoCryptoProvider,

    /// `apply_fips_hardening` rejected the freshly-built `ClientConfig`
    /// because `ClientConfig::fips()` reported `false`. Carries the
    /// human-readable diagnostic. Under normal bootstrap flow the
    /// provider-level witness is already asserted by
    /// `init_crypto_provider`; this error surfaces per-config issues
    /// (e.g. missing `require_ems`) or a missed `init_crypto_provider()`
    /// call.
    #[error("{0}")]
    FipsHardeningFailed(String),

    /// Catch-all for foreign errors (`rustls::Error`, etc.) propagated
    /// via `?`. Marked `#[error(transparent)]` so the `Display` and
    /// `source()` impls forward to the inner error unchanged.
    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync + 'static>),
}

/// Test-only auto-install of the platform-appropriate FIPS crypto provider.
///
/// Gated on `cfg(all(test, feature = "fips"))` — `cfg(test)` is only set when
/// compiling the crate's own unit-test target, NOT when compiling the lib for
/// use by integration tests under `tests/`. The regression test in
/// `tests/no_crypto_provider_fips.rs` therefore sees a clean process state and
/// can still assert `TlsConfigError::NoCryptoProvider`.
///
/// Forced from inside `build_client_config` so every TLS-config-building
/// entry point (`webpki_roots_client_config`, `native_roots_client_config`,
/// and `HttpClientBuilder::build`) auto-installs uniformly. nextest's default
/// "process-per-test" execution means a builder-level `LazyLock` would not
/// cover tests that bypass the builder.
#[cfg(all(test, feature = "fips"))]
mod fips_test_provider {
    use std::sync::LazyLock;

    pub(super) static INSTALL: LazyLock<()> = LazyLock::new(|| {
        // `install_default()` returns `Err` if a provider is already installed
        // (benign here); drop the result. `drop` rather than `let _ =`
        // satisfies clippy::let_underscore_must_use.
        #[cfg(target_os = "macos")]
        drop(rustls_corecrypto_provider::default_provider().install_default());
        #[cfg(target_os = "windows")]
        drop(rustls_cng_crypto::fips_provider().install_default());
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        drop(rustls::crypto::default_fips_provider().install_default());
    });
}

/// Build a rustls `ClientConfig` from the given root store.
///
/// Under the `fips` feature, applies [`apply_fips_hardening`] which:
///   1. forces `require_ems = true` (NIST SP 800-52 Rev. 2 §3.5), and
///   2. verifies `config.fips() == true` and returns `Err` otherwise.
///
/// The bootstrap `assert!` in `init_crypto_provider` is the primary
/// guard against a non-FIPS provider; this TLS-layer check is
/// defence-in-depth for per-config settings (`require_ems`, protocol
/// versions) that also affect `ClientConfig::fips()`.
fn build_client_config(
    root_store: rustls::RootCertStore,
) -> Result<rustls::ClientConfig, TlsConfigError> {
    // Test-only: ensure the platform-appropriate FIPS crypto provider is
    // installed before this funnel runs. The fail-closed path below requires
    // `modkit::bootstrap::init_crypto_provider` to have run in production;
    // in-crate tests that don't go through bootstrap (most of them) would
    // otherwise hit `NoCryptoProvider`. Placed here rather than in the
    // builder so it covers every TLS-config-building entry point uniformly
    // (`webpki_roots_client_config`, `native_roots_client_config`, and any
    // future addition). `cfg(test)` is set only for in-crate unit tests, NOT
    // for integration tests under `tests/` — so
    // `tests/no_crypto_provider_fips.rs` still sees a clean process state.
    #[cfg(all(test, feature = "fips"))]
    std::sync::LazyLock::force(&fips_test_provider::INSTALL);

    // Under `--features fips` the crypto provider MUST have been installed
    // up-front via `modkit::bootstrap::init_crypto_provider` — otherwise
    // `get_crypto_provider()`'s fallback would mint an *uninstalled* provider
    // whose FIPS-validation status is indeterminate from the rustls global's
    // point of view. Fail closed instead.
    //
    // The non-FIPS branch preserves the historical infallible fallback
    // (see `get_crypto_provider` docstring).
    #[cfg(feature = "fips")]
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .ok_or(TlsConfigError::NoCryptoProvider)?;
    #[cfg(not(feature = "fips"))]
    let provider = get_crypto_provider();

    #[allow(unused_mut)]
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| TlsConfigError::Other(Box::new(e)))?
        .with_root_certificates(root_store)
        .with_no_client_auth();

    #[cfg(feature = "fips")]
    {
        apply_fips_hardening(&mut config)?;
    }

    Ok(config)
}

/// Apply FIPS-mode hardening to a freshly-built `ClientConfig`:
///   * set `require_ems = true` (NIST SP 800-52 Rev. 2 §3.5 — required for
///     `ClientConfig::fips()` to consider TLS 1.2 sessions FIPS-compliant
///     when the EMS extension is honoured by the peer).
///   * verify the full FIPS chain reports `fips() == true`. If not, return
///     `Err` — the bootstrap `assert!` in `init_crypto_provider` already
///     guarantees the provider reports `fips() == true`, so a failure here
///     indicates a per-config issue (e.g. missing `require_ems` or
///     restricted protocol versions) rather than a provider-level problem.
#[cfg(feature = "fips")]
fn apply_fips_hardening(cfg: &mut rustls::ClientConfig) -> Result<(), TlsConfigError> {
    cfg.require_ems = true;
    if !cfg.fips() {
        return Err(TlsConfigError::FipsHardeningFailed(
            "TLS ClientConfig does not advertise FIPS after enabling require_ems. \
             The bootstrap assert in init_crypto_provider should have caught a \
             non-FIPS provider at startup; if you see this error the provider is \
             FIPS-OK but a per-config setting (protocol versions, require_ems) is \
             preventing ClientConfig::fips() from reporting true. \
             If init_crypto_provider was not called, call it before building any \
             TLS config."
                .to_owned(),
        ));
    }
    Ok(())
}

/// Build a rustls `ClientConfig` using the cached native root certificates.
///
/// # Errors
///
/// Returns an error if no valid root certificates are available:
/// - OS certificate store is empty
/// - All certificates failed to parse
///
/// This fail-fast behavior ensures TLS configuration errors are caught at client
/// construction time rather than failing later during TLS handshakes.
pub fn native_roots_client_config() -> Result<rustls::ClientConfig, TlsConfigError> {
    let certs = native_root_certs();

    let mut root_store = rustls::RootCertStore::empty();

    if certs.is_empty() {
        return Err(TlsConfigError::Other(
            "no native root CA certificates found in OS certificate store".into(),
        ));
    }

    let (added, ignored) = root_store.add_parsable_certificates(certs.iter().cloned());

    if ignored > 0 {
        tracing::warn!(
            added = added,
            ignored = ignored,
            "some native root certificates could not be parsed"
        );
    }

    if added == 0 {
        return Err(TlsConfigError::Other(
            format!(
                "no valid native root CA certificates parsed (found {}, all {} failed to parse)",
                certs.len(),
                ignored
            )
            .into(),
        ));
    }

    build_client_config(root_store)
}

/// Build a rustls `ClientConfig` using Mozilla's webpki-roots trust anchors.
///
/// Under the `fips` feature, `require_ems` is forced on (see
/// [`build_client_config`]). This is the FIPS-conformant counterpart to the
/// `hyper_rustls::HttpsConnectorBuilder::with_provider_and_webpki_roots`
/// one-liner — we must build the config ourselves so we can flip the EMS bit.
pub fn webpki_roots_client_config() -> Result<rustls::ClientConfig, TlsConfigError> {
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    build_client_config(root_store)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// Test that native root certs are cached after the first load.
    ///
    /// NOTE: This test verifies "at most one load" rather than "exactly one load"
    /// because `LOAD_COUNT` is a global atomic shared across all tests. If another
    /// test (or parallel test) calls `native_root_certs()` before this test runs,
    /// the cache will already be initialized and `final_count - initial_count`
    /// will be 0. The assertion handles this correctly.
    #[test]
    fn test_native_roots_cached() {
        // Capture count before our calls (may be non-zero if cache already initialized)
        let initial_count = LOAD_COUNT.load(Ordering::SeqCst);

        // First call - loads if not cached, otherwise uses existing cache
        let result1 = native_root_certs();

        // Second call should use cache
        let result2 = native_root_certs();

        // Third call should also use cache
        let result3 = native_root_certs();

        // Verify loader was called at most once more than initial (0 if already cached, 1 if we triggered the load)
        let final_count = LOAD_COUNT.load(Ordering::SeqCst);
        assert!(
            final_count <= initial_count + 1,
            "loader should run at most once, but ran {} times since test start",
            final_count - initial_count
        );

        // Results should be consistent (same slice pointer)
        assert_eq!(result1.len(), result2.len());
        assert_eq!(result2.len(), result3.len());
        assert!(std::ptr::eq(result1, result2), "should return same slice");
        assert!(std::ptr::eq(result2, result3), "should return same slice");
    }

    #[test]
    fn test_native_roots_client_config() {
        // Building client config succeeds if native roots are available
        // (which they should be on most CI/dev systems)
        // On systems without native certs, this returns Err (expected behavior)
        let result = native_roots_client_config();

        // Log the result for debugging in CI
        match &result {
            Ok(_) => tracing::debug!("native_roots_client_config succeeded"),
            Err(e) => {
                tracing::debug!(error = %e, "native_roots_client_config failed (expected on minimal containers)");
            }
        }

        // We don't assert success because CI containers may not have OS certs.
        // The important thing is it doesn't panic.
    }

    /// `webpki_roots_client_config()` must always build successfully — the
    /// trust store comes from a static `webpki-roots::TLS_SERVER_ROOTS` slice
    /// that is non-empty on every supported platform. Catches a silent
    /// regression if the `webpki-roots` crate ever renames its constant or
    /// changes its `extend`-able item type.
    ///
    /// Asserts behavioural properties this crate actually controls:
    ///   1. The config carries the crypto provider returned by
    ///      `get_crypto_provider()` (non-empty cipher-suite list).
    ///   2. The provider's cipher-suite set is non-empty — proves we did
    ///      not accidentally build a config whose negotiation would fail
    ///      with `NoCipherSuitesInCommon` against every peer.
    ///   3. The provider's kx-group list is non-empty — same reasoning.
    ///
    /// (Asserting `alpn_protocols.len() == 0` was a rustls default we do
    /// not control; replaced.)
    #[test]
    fn test_webpki_roots_client_config_builds() {
        let cfg = webpki_roots_client_config().expect("webpki roots must always build");
        let provider = cfg.crypto_provider();
        assert!(
            !provider.cipher_suites.is_empty(),
            "TLS client config must carry a non-empty cipher-suite list"
        );
        assert!(
            !provider.kx_groups.is_empty(),
            "TLS client config must carry a non-empty kx-group list"
        );
    }

    /// When built with `--features fips`, `build_client_config` MUST:
    ///   1. Set `require_ems = true` (NIST SP 800-52 Rev. 2 §3.5)
    ///   2. Make `config.fips()` return true (full FIPS chain)
    ///
    /// Without this, an Apple-corecrypto-backed FIPS build silently advertises
    /// `config.fips() == false` because rustls's stock `require_ems` default
    /// is gated on rustls's *own* `fips` feature — which we deliberately keep
    /// off on macOS to avoid pulling the AWS-LC FIPS module.
    ///
    /// Exercised via the public `webpki_roots_client_config()` (which routes
    /// through `build_client_config`); calling it avoids a hard dependency on
    /// the OS keychain that `native_roots_client_config` carries.
    ///
    /// Run via `cargo test -p cyberware-modkit-http --features fips`.
    /// `build_client_config` auto-installs the platform FIPS provider in
    /// test mode (see `fips_test_provider` module) so this test does not
    /// need its own explicit install.
    #[test]
    #[cfg(feature = "fips")]
    fn fips_client_config_requires_ems_and_advertises_fips() {
        let cfg = webpki_roots_client_config().expect("build under fips");
        assert!(cfg.require_ems, "fips build must set require_ems = true");
        assert!(
            cfg.fips(),
            "fips build must yield ClientConfig::fips() == true (full provider chain)"
        );
    }
}
