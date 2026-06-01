use std::sync::OnceLock;

/// Error returned when the crypto provider cannot be installed.
// `Clone` required by `OnceLock<Result<_>>` cache in `init_crypto_provider` --
// the cached result is cloned on every call.
// `PartialEq`/`Eq` used by tests asserting the cached result is stable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CryptoProviderError {
    /// Another crypto provider was already installed (FIPS mode).
    #[error("failed to install FIPS crypto provider - another provider is already installed")]
    FipsProviderConflict,
    /// Windows is not in FIPS mode — the system-wide Group Policy
    /// `FipsAlgorithmPolicy` is not enabled, so CNG would not enforce the
    /// FIPS-Approved algorithm subset at runtime. Fail-closed: refuse to
    /// start rather than silently degrade the FIPS claim.
    #[cfg(target_os = "windows")]
    #[error(
        "Windows is not in FIPS mode (HKLM\\System\\CurrentControlSet\\Control\\Lsa\\FipsAlgorithmPolicy != 1). \
         Enable system-wide FIPS via Group Policy (Computer Configuration > Windows Settings > \
         Security Settings > Local Policies > Security Options > 'System cryptography: Use FIPS \
         compliant algorithms') and reboot before launching this service."
    )]
    SystemFipsModeNotEnabled,
}

static INIT_RESULT: OnceLock<Result<(), CryptoProviderError>> = OnceLock::new();

/// Install the process-wide default rustls [`CryptoProvider`](rustls::crypto::CryptoProvider).
///
/// Dispatch:
///
/// - **`fips` feature + macOS**: installs the Apple corecrypto-backed provider
///   from `rustls-corecrypto-provider`, **restricted to TLS 1.3 cipher
///   suites** via `fips_provider()`. corecrypto is shipped inside macOS and
///   validated by Apple under FIPS 140-3 per OS release; see
///   <https://csrc.nist.gov/projects/cryptographic-module-validation-program>
///   for the cert matching the running macOS version. TLS 1.2 cipher suites
///   are excluded under `fips` because Apple does not expose a separately
///   CAVS-validated TLS PRF primitive (unlike aws-lc-fips on Linux); see
///   the `cyberware-rustls-corecrypto-provider` README "FIPS claim boundaries"
///   section. On macOS the `rustls/fips` feature is not activated (see
///   `rustls-fips-shim`), so the AWS-LC FIPS dylib is not linked.
/// - **`fips` feature + Windows**: installs the Windows CNG-backed provider
///   from `rustls-cng-crypto`'s `fips_provider()`. CNG is shipped inside
///   Windows and validated by Microsoft under FIPS 140-3 per OS release.
///   Requires Windows to be in system-wide FIPS mode
///   (`HKLM\System\CurrentControlSet\Control\Lsa\FipsAlgorithmPolicy = 1`,
///   set via Group Policy); fails closed with
///   [`CryptoProviderError::SystemFipsModeNotEnabled`] otherwise. As with
///   macOS, `rustls/fips` is not activated on this target, so the AWS-LC
///   FIPS dylib is not linked.
/// - **`fips` feature + other** (Linux, etc.): installs the FIPS-validated
///   AWS-LC provider (`aws-lc-fips-sys`, NIST Certificate #4816). The cert's
///   OE covers Linux but not Darwin/Windows, which is why those targets use
///   different providers.
/// - **Standard mode** (no `fips` feature): installs the `aws-lc-rs` provider
///   explicitly. This is required because both `ring` and `aws-lc-rs` are
///   compiled into the binary (ring via `pingora-rustls`), and rustls
///   0.23 panics when it cannot auto-detect a single provider. Conflicts here
///   are non-fatal: if another provider was installed first, it stays active,
///   the conflict is logged at `warn!`, and `Ok(())` is returned.
///
/// This **must** be called before any TLS configuration, HTTP client, database
/// connection, or JWT operation is created.
///
/// Safe to call multiple times -- only the first invocation has an effect;
/// subsequent calls return the cached first-call result.
///
/// # FIPS-claim caveats
///
/// On the resulting provider, `provider.fips() == true` is a **runtime
/// witness** under the witness-pattern rework — it is `true` only when both
/// (a) every primitive routes through a CMVP-validated module *and* (b)
/// the runtime OE check agrees. It is no longer an unconditional design-
/// intent claim.
///
/// **macOS**: the corecrypto crate runs an OE check at first provider
/// construction (`cyberware_rustls_corecrypto_provider::oe::fips_witness_ok`).
/// On a macOS major outside the active corecrypto CMVP cert OE, **every**
/// `fips()` impl in the provider returns `false` and a single
/// `tracing::warn!` is emitted. The post-install witness `assert!` then
/// **panics** — a misconfigured FIPS build must never silently proceed.
/// Use `CYBERWARE_FIPS_OE_OVERRIDE=1` to force the witness to `true`
/// for CI on pre-release macOS — never for production. See the
/// `cyberware-rustls-corecrypto-provider` README "Runtime FIPS witness" section
/// and FIPS PRD §8.3.
///
/// Downstream TLS configuration code (`modkit_http::tls::apply_fips_hardening`)
/// performs its own `config.fips()` check and returns `Err` rather than
/// panicking — `ClientConfig::fips()` depends on per-config settings
/// (`require_ems`, protocol versions) beyond the provider itself, so the
/// TLS-layer check is a defence-in-depth complement to the bootstrap
/// assertion here.
///
/// **Linux / Windows**: runtime OE-validation is not yet implemented; OE
/// coverage is verified via the release checklist (manual CMVP cert search,
/// PRD §9.3). Tracked as a follow-up in PRD §10.
///
/// # Errors
///
/// - [`CryptoProviderError::FipsProviderConflict`] if the `fips` feature is
///   enabled and another rustls provider was installed first.
/// - [`CryptoProviderError::SystemFipsModeNotEnabled`] on Windows+`fips` when
///   the OS is not in system-wide FIPS mode.
///
/// # Panics
///
/// Panics (via `assert!`) under `--features fips` if the installed crypto
/// provider does not report `fips() == true`. This is an intentional
/// fail-closed guard against misconfigured builds (wrong feature flags,
/// wrong OS, or macOS OE mismatch without the override env-var).
pub fn init_crypto_provider() -> Result<(), CryptoProviderError> {
    INIT_RESULT
        .get_or_init(|| {
            #[cfg(all(feature = "fips", target_os = "macos"))]
            {
                // Under modkit's `fips` feature the dependency tree
                // activates `rustls-corecrypto-provider/fips`, which
                // routes `default_provider()` to the TLS-1.3-only FIPS-
                // claim variant — same pattern as `rustls-cng-crypto`'s
                // feature flag. We therefore install the unified entry
                // point (no need to remember which factory under which
                // build profile).
                if let Err(prev) = rustls_corecrypto_provider::default_provider().install_default()
                {
                    tracing::error!(
                        previous_provider = ?prev,
                        "FIPS crypto provider conflict: another rustls provider was already installed"
                    );
                    return Err(CryptoProviderError::FipsProviderConflict);
                }
                tracing::info!("FIPS-140-3 crypto provider installed (Apple corecrypto, macOS, TLS 1.3-only)");
            }

            #[cfg(all(feature = "fips", target_os = "windows"))]
            {
                // Fail-closed: when Windows is not in system-wide FIPS mode,
                // `rustls_cng_crypto::fips_provider()` returns a CryptoProvider
                // with empty `cipher_suites` / `kx_groups` (rustls's per-suite
                // `.fips()` flag is the gate; in non-FIPS-mode Windows none
                // qualify). The upstream crate's `fips::enabled()` helper is
                // `pub(crate)`, so we detect the same condition via the
                // documented empty-provider shape rather than poking
                // `BCryptGetFipsAlgorithmMode` ourselves. Refuse to install
                // rather than degrade the FIPS claim with a non-handshakeable
                // provider.
                let provider = rustls_cng_crypto::fips_provider();
                if provider.cipher_suites.is_empty() {
                    tracing::error!(
                        "Windows FIPS mode not enabled (FipsAlgorithmPolicy != 1); \
                         rustls-cng-crypto returned an empty FIPS provider"
                    );
                    return Err(CryptoProviderError::SystemFipsModeNotEnabled);
                }
                if let Err(prev) = provider.install_default() {
                    tracing::error!(
                        previous_provider = ?prev,
                        "FIPS crypto provider conflict: another rustls provider was already installed"
                    );
                    return Err(CryptoProviderError::FipsProviderConflict);
                }
                tracing::info!("FIPS-140-3 crypto provider installed (Windows CNG)");
            }

            #[cfg(all(feature = "fips", not(any(target_os = "macos", target_os = "windows"))))]
            {
                if let Err(prev) = rustls::crypto::default_fips_provider().install_default() {
                    tracing::error!(
                        previous_provider = ?prev,
                        "FIPS crypto provider conflict: another rustls provider was already installed"
                    );
                    return Err(CryptoProviderError::FipsProviderConflict);
                }
                tracing::info!("FIPS-140-3 crypto provider installed (AWS-LC FIPS module)");
            }

            #[cfg(feature = "fips")]
            {
                let Some(provider) = rustls::crypto::CryptoProvider::get_default() else {
                    unreachable!("provider must be installed at this point");
                };
                assert!(
                    provider.fips(),
                    "FIPS post-install witness failed: installed provider does not report fips()==true"
                );
                tracing::info!("FIPS post-install witness: OK");
            }

            #[cfg(not(feature = "fips"))]
            {
                if let Err(prev) = rustls::crypto::aws_lc_rs::default_provider().install_default() {
                    // Non-fatal: another provider is already active, TLS still works.
                    tracing::warn!(
                        previous_provider = ?prev,
                        "aws-lc-rs crypto provider not installed: another default provider was already set"
                    );
                } else {
                    tracing::info!("aws-lc-rs crypto provider installed");
                }
            }

            Ok(())
        })
        .clone()
}
