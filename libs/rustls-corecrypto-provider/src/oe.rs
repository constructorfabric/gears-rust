//! Runtime Operational Environment (OE) validation for the macOS FIPS posture.
//!
//! A FIPS 140-3 claim is only valid when the running OS version lies inside
//! the Operational Environment listed on the active Apple corecrypto CMVP
//! certificate. This module reads the macOS product version at startup and
//! compares it against a major-version whitelist synchronised with the
//! "Compliance caveat" table in this crate's README.
//!
//! ## Runtime FIPS witness
//!
//! [`fips_witness_ok`] is the one entry point every `fips()` impl across
//! this crate delegates to. It returns `true` iff (a) the running macOS
//! major is inside [`SUPPORTED_OE_MACOS_MAJOR`], or (b) the override
//! env-var [`OE_OVERRIDE_ENV`] is set. Mirrors `rustls-cng-crypto`'s
//! `crate::fips::enabled()` posture on Windows — there is no startup
//! panic; an OE mismatch produces `fips() == false` everywhere (and a
//! single `tracing::warn!`), so downstream `ClientConfig::fips()` /
//! `ServerConfig::fips()` correctly report the runtime witness rather
//! than the design intent. The witness is cached process-wide via
//! [`std::sync::OnceLock`] so we pay one `sysctlbyname` call per process.
//!
//! See PRD §8.3 "Operational Environment validation at startup" and PRD §10
//! TODO-7 for the long-term automation plan.

use std::fmt;
use std::sync::OnceLock;

/// macOS major versions whose patch releases lie inside an active Apple
/// corecrypto CMVP certificate's Operational Environment.
///
/// Synchronise with the README "Compliance caveat" table. Major-only
/// matching is intentional: Apple's CMVP submissions cover the entire
/// patch family of a given macOS major (`13.x`, `14.x`, `15.x`), so a
/// patch-version bump on the deployment host does not invalidate the
/// claim — only a major-version bump does.
pub const SUPPORTED_OE_MACOS_MAJOR: &[u32] = &[12, 13, 14, 15, 26];

/// Environment variable that, when set to a non-empty value other than
/// `"0"`, forces [`fips_witness_ok`] to return `true` on a macOS major
/// outside [`SUPPORTED_OE_MACOS_MAJOR`]. Intended for CI on pre-release
/// macOS during the window between a major-version release and the
/// publication of Apple's next corecrypto CMVP submission — never for
/// production.
///
/// **Do not set in production.** Setting this in production asserts a
/// FIPS claim on an OS version that has not been validated.
///
/// (Note: prior versions of this crate panicked under `--features fips`
/// on OE mismatch and treated the env-var as a "downgrade to warning"
/// switch. The crate no longer panics — the override now flips the
/// witness from `false` to `true`. See the README "Runtime FIPS witness"
/// section.)
pub const OE_OVERRIDE_ENV: &str = "CYBERWARE_FIPS_OE_OVERRIDE";

/// Outcome of OE validation. Distinct from `rustls::Error` because this
/// is a deployment-environment problem, not a TLS-layer problem.
#[derive(Debug, Clone)]
pub enum OeError {
    /// `kern.osproductversion` reports a major version outside the
    /// supported whitelist.
    UnsupportedVersion {
        detected: (u32, u32),
        supported: &'static [u32],
    },
    /// `sysctlbyname` failed (e.g. EPERM in a sandbox) — we could not
    /// determine the running macOS version.
    SysctlFailed(String),
    /// `kern.osproductversion` returned an output we could not parse
    /// as `MAJOR.MINOR[.PATCH]`.
    ParseFailed(String),
}

impl fmt::Display for OeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OeError::UnsupportedVersion {
                detected,
                supported,
            } => write!(
                f,
                "running macOS {}.{} is not inside the Apple corecrypto CMVP cert OE \
                 (supported majors: {:?}); the runtime FIPS witness will report false \
                 and so will CryptoProvider::fips() / ClientConfig::fips(). \
                 Verify cert coverage at \
                 https://csrc.nist.gov/projects/cryptographic-module-validation-program. \
                 Set {OE_OVERRIDE_ENV}=1 to force the witness to true on an unvalidated \
                 macOS major (CI / pre-release only — never in production).",
                detected.0, detected.1, supported
            ),
            OeError::SysctlFailed(reason) => {
                write!(f, "kern.osproductversion sysctl failed: {reason}")
            }
            OeError::ParseFailed(s) => {
                write!(f, "could not parse macOS version string {s:?}")
            }
        }
    }
}

impl std::error::Error for OeError {}

/// Returns `true` if the user has explicitly opted out of the fail-closed
/// gate via [`OE_OVERRIDE_ENV`]. Treats `""` and `"0"` as not-set.
pub fn override_enabled() -> bool {
    match std::env::var(OE_OVERRIDE_ENV) {
        Ok(v) => !v.is_empty() && v != "0",
        Err(_) => false,
    }
}

/// Read the running macOS product version (e.g. "14.5.1") via
/// `sysctlbyname("kern.osproductversion", ...)` and parse the leading
/// `MAJOR.MINOR`.
///
/// Implementation note: we deliberately use the same syscall surface as
/// every other macOS process that asks the same question (`sw_vers`,
/// `Foundation.NSProcessInfo`, etc.). The string is the OS's
/// authoritative product-version, not the kernel version
/// (`kern.osrelease`, which numbers Darwin releases differently).
pub fn current_macos_version() -> Result<(u32, u32), OeError> {
    let raw = read_sysctl_string("kern.osproductversion")?;
    parse_version(&raw)
}

fn parse_version(s: &str) -> Result<(u32, u32), OeError> {
    let mut parts = s.split('.');
    let major = parts
        .next()
        .and_then(|p| p.parse::<u32>().ok())
        .ok_or_else(|| OeError::ParseFailed(s.to_owned()))?;
    // If a second segment exists it must parse as u32 -- silently
    // accepting non-numeric minor (e.g. "14.beta" -> minor=0) would
    // hide a corrupted sysctl reply.
    let minor = match parts.next() {
        Some(p) => p
            .parse::<u32>()
            .map_err(|_| OeError::ParseFailed(s.to_owned()))?,
        None => 0,
    };
    Ok((major, minor))
}

fn read_sysctl_string(name: &str) -> Result<String, OeError> {
    use std::ffi::CString;
    use std::os::raw::{c_int, c_void};

    // `libc::sysctlbyname` is transitively present in our dependency
    // graph (via `core-foundation`'s libc dep). We declare the extern
    // here rather than adding `libc` as a direct dep — single use site,
    // single sig, no risk of API drift.
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const std::os::raw::c_char,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *mut c_void,
            newlen: usize,
        ) -> c_int;
    }

    let cname = CString::new(name).map_err(|e| OeError::SysctlFailed(e.to_string()))?;

    // First call: query required buffer size.
    let mut len: usize = 0;
    // SAFETY: `cname` outlives the call; `oldp` is NULL so the kernel
    // only writes into `len`. Sole side effect: writes a usize.
    let rc = unsafe {
        sysctlbyname(
            cname.as_ptr(),
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(OeError::SysctlFailed(format!(
            "size query for {name} returned errno={}",
            std::io::Error::last_os_error()
        )));
    }
    if len == 0 {
        return Err(OeError::SysctlFailed(format!(
            "{name} reported zero-length value"
        )));
    }

    let mut buf: Vec<u8> = vec![0; len];
    // SAFETY: same as above; `buf.len() == len` so the kernel will not
    // overrun. After the call we trim the trailing NUL.
    let rc = unsafe {
        sysctlbyname(
            cname.as_ptr(),
            buf.as_mut_ptr() as *mut c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(OeError::SysctlFailed(format!(
            "value fetch for {name} returned errno={}",
            std::io::Error::last_os_error()
        )));
    }

    // `len` after the second call is the byte count actually written,
    // including the trailing NUL terminator that the kernel appends.
    if let Some(&0) = buf.get(len.saturating_sub(1)) {
        buf.truncate(len.saturating_sub(1));
    } else {
        buf.truncate(len);
    }

    String::from_utf8(buf).map_err(|e| OeError::SysctlFailed(format!("non-UTF-8 reply: {e}")))
}

/// Validate that the current macOS major version is inside
/// [`SUPPORTED_OE_MACOS_MAJOR`].
///
/// Returns `Ok(())` on match, `Err(OeError::*)` on mismatch or on any
/// failure to determine the OS version.
pub fn validate_oe() -> Result<(), OeError> {
    let (major, minor) = current_macos_version()?;
    if SUPPORTED_OE_MACOS_MAJOR.contains(&major) {
        Ok(())
    } else {
        Err(OeError::UnsupportedVersion {
            detected: (major, minor),
            supported: SUPPORTED_OE_MACOS_MAJOR,
        })
    }
}

/// Process-wide cached witness for the runtime FIPS posture. Populated on
/// first call to [`fips_witness_ok`] and never mutated afterwards.
///
/// ## Test-isolation hazard
///
/// `cargo test` shares one process across the entire test binary, and many
/// tests in this crate transitively prime this slot — any call into
/// `default_provider()` / `fips_provider()` reaches `fips_witness_ok` and
/// populates `FIPS_WITNESS` for the rest of the process. Once set, the
/// `OnceLock` does not re-evaluate; subsequent env-var manipulations
/// (e.g. via `temp_env` in `override_treats_empty_and_zero_as_unset`)
/// affect `override_enabled()` directly but **not** `fips_witness_ok`.
///
/// Tests that need to verify witness behaviour in non-default states
/// must exercise the pure-function policy
/// [`compute_fips_witness`] directly, not `fips_witness_ok` — see the
/// `compute_fips_witness_*` cases in the module's test section. A
/// `reset_witness_for_tests()` hook was intentionally not added: it
/// would require `unsafe` mutation of this `static OnceLock` and would
/// be unsound under `cargo test --test-threads > 1`.
static FIPS_WITNESS: OnceLock<bool> = OnceLock::new();

/// Pure-function policy: given the outcome of an OE check and whether the
/// override env-var is set, return whether the runtime FIPS witness
/// should report `true`. Extracted from [`fips_witness_ok`] so the policy
/// is unit-testable without touching the global cache.
fn compute_fips_witness(result: &Result<(), OeError>, override_set: bool) -> bool {
    result.is_ok() || override_set
}

/// Runtime FIPS witness — the single entry point every `fips()` impl in
/// this crate delegates to.
///
/// Returns `true` iff (a) the running macOS major is inside
/// [`SUPPORTED_OE_MACOS_MAJOR`], or (b) the override env-var
/// [`OE_OVERRIDE_ENV`] is set (intended for CI on pre-release macOS).
/// Otherwise returns `false` and `ClientConfig::fips()` /
/// `ServerConfig::fips()` correctly report the runtime witness rather
/// than design intent.
///
/// The witness is cached process-wide via [`OnceLock`] — one
/// `sysctlbyname` call per process. On the first call where the OE check
/// fails AND the override is not set, a single `tracing::warn!` is
/// emitted for telemetry.
///
/// Mirrors `rustls-cng-crypto`'s `crate::fips::enabled()` posture: no
/// startup panic; failure surfaces as `fips() == false` everywhere.
pub fn fips_witness_ok() -> bool {
    *FIPS_WITNESS.get_or_init(|| {
        let result = validate_oe();
        let override_set = override_enabled();
        let ok = compute_fips_witness(&result, override_set);
        if !ok {
            if let Err(err) = &result {
                tracing::warn!(
                    error = %err,
                    "FIPS witness: OE-validation failed; CryptoProvider::fips() will report \
                     false on this host (no panic). Set {OE_OVERRIDE_ENV}=1 to force the \
                     witness to true on pre-release macOS in CI only — never in production."
                );
            }
        } else if let Err(err) = &result {
            // OK because override was set — log so operators are not
            // surprised that fips() returns true on an unvalidated OE.
            tracing::warn!(
                error = %err,
                "FIPS witness: OE-validation failed but {OE_OVERRIDE_ENV} is set; \
                 reporting fips() == true on an unvalidated macOS version. \
                 This setting must not be used in production."
            );
        }
        ok
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Major.Minor.Patch parses correctly.
    #[test]
    fn parses_three_component_version() {
        assert_eq!(parse_version("14.5.1").expect("parse"), (14, 5));
    }

    /// Major.Minor (no patch) parses too — sw_vers omits the trailing
    /// `.0` on `.0` releases.
    #[test]
    fn parses_two_component_version() {
        assert_eq!(parse_version("15.0").expect("parse"), (15, 0));
    }

    /// Major-only is acceptable; minor defaults to 0 rather than
    /// failing the whole validation on a benign formatting variation.
    #[test]
    fn parses_major_only_version() {
        assert_eq!(parse_version("14").expect("parse"), (14, 0));
    }

    /// Garbage parses to an error rather than silently passing.
    #[test]
    fn parse_rejects_non_numeric_major() {
        let err = parse_version("Sonoma").expect_err("garbage must fail");
        assert!(matches!(err, OeError::ParseFailed(_)));
    }

    /// Non-numeric minor must also fail rather than silently defaulting
    /// to 0. A corrupted reply like "14.beta" would otherwise pass the
    /// major-whitelist check (major=14) with a fabricated minor.
    #[test]
    fn parse_rejects_non_numeric_minor() {
        let err = parse_version("14.beta").expect_err("non-numeric minor must fail");
        assert!(matches!(err, OeError::ParseFailed(_)));
    }

    /// The whitelist must be non-empty and in strictly ascending order so
    /// (a) a future "earliest supported major" bump cannot accidentally
    /// regress by appending instead of replacing, and (b) the witness
    /// always has at least one OS major to accept. Replaces a prior
    /// tautological "the whitelist contains its own elements" assertion
    /// that could not fail unless `slice::contains` was broken.
    #[test]
    fn whitelist_is_non_empty_and_ascending() {
        assert!(
            !SUPPORTED_OE_MACOS_MAJOR.is_empty(),
            "OE whitelist must contain at least one macOS major"
        );
        assert!(
            SUPPORTED_OE_MACOS_MAJOR.windows(2).all(|w| w[0] < w[1]),
            "OE whitelist must be strictly ascending, got {SUPPORTED_OE_MACOS_MAJOR:?}"
        );
    }

    /// Sanity check the rejection of a clearly-unsupported version.
    /// macOS 10 (Catalina and earlier) is outside every current cert OE.
    /// macOS 11 (Big Sur) is also out — the floor was raised to 12 per
    /// the C-3 review fix.
    #[test]
    fn unsupported_version_is_rejected_by_whitelist_check() {
        assert!(!SUPPORTED_OE_MACOS_MAJOR.contains(&10));
        assert!(!SUPPORTED_OE_MACOS_MAJOR.contains(&11));
        assert!(!SUPPORTED_OE_MACOS_MAJOR.contains(&99));
    }

    /// Override env-var detection: empty / "0" / unset must all read
    /// as not-overridden so that a stray export doesn't silently relax
    /// the gate.
    ///
    /// Uses `temp_env` (workspace dev-dep) for hermetic per-case env
    /// mutation -- safer than direct `std::env::set_var` calls under
    /// parallel `cargo test`, and avoids the edition-2024 unsafe-set_var
    /// noise.
    #[test]
    fn override_treats_empty_and_zero_as_unset() {
        temp_env::with_var_unset(OE_OVERRIDE_ENV, || {
            assert!(!override_enabled(), "unset must read as not-overridden");
        });
        temp_env::with_var(OE_OVERRIDE_ENV, Some(""), || {
            assert!(!override_enabled(), "empty must read as not-overridden");
        });
        temp_env::with_var(OE_OVERRIDE_ENV, Some("0"), || {
            assert!(!override_enabled(), "\"0\" must read as not-overridden");
        });
        temp_env::with_var(OE_OVERRIDE_ENV, Some("1"), || {
            assert!(override_enabled(), "\"1\" must read as overridden");
        });
    }

    /// On any reasonable macOS dev/CI host the sysctl read succeeds
    /// and yields a sensible major version (>= 10, < 100). Anything
    /// else is either a bug in `read_sysctl_string` or a deeply
    /// unusual sandbox.
    #[test]
    fn current_macos_version_returns_plausible_major() {
        let (major, _minor) = current_macos_version().expect("sysctl on macOS host");
        assert!(
            (10..100).contains(&major),
            "implausible macOS major {major}"
        );
    }

    // =========================================================================
    // compute_fips_witness — pure policy fn, exercises C-2 / test-gaps #4 + #10
    // without touching the global `FIPS_WITNESS` OnceLock.
    // =========================================================================

    /// `Ok(())` from `validate_oe` → witness true regardless of override.
    #[test]
    fn compute_fips_witness_returns_true_on_ok() {
        assert!(compute_fips_witness(&Ok(()), false));
        assert!(compute_fips_witness(&Ok(()), true));
    }

    /// Unsupported macOS major + no override → witness false.
    #[test]
    fn compute_fips_witness_returns_false_on_unsupported_no_override() {
        let err = Err(OeError::UnsupportedVersion {
            detected: (99, 0),
            supported: SUPPORTED_OE_MACOS_MAJOR,
        });
        assert!(!compute_fips_witness(&err, false));
    }

    /// Unsupported macOS major + override set → witness true (CI escape hatch).
    #[test]
    fn compute_fips_witness_returns_true_on_unsupported_with_override() {
        let err = Err(OeError::UnsupportedVersion {
            detected: (99, 0),
            supported: SUPPORTED_OE_MACOS_MAJOR,
        });
        assert!(compute_fips_witness(&err, true));
    }

    /// `SysctlFailed` (e.g. EPERM in a sandbox) + no override → witness false.
    /// Same policy as `UnsupportedVersion` — any failure to determine the
    /// OE means we cannot witness FIPS.
    #[test]
    fn compute_fips_witness_returns_false_on_sysctl_failed() {
        let err = Err(OeError::SysctlFailed("sandboxed".to_owned()));
        assert!(!compute_fips_witness(&err, false));
        assert!(compute_fips_witness(&err, true));
    }

    /// `ParseFailed` (corrupted sysctl reply) follows the same policy.
    #[test]
    fn compute_fips_witness_returns_false_on_parse_failed() {
        let err = Err(OeError::ParseFailed("garbage".to_owned()));
        assert!(!compute_fips_witness(&err, false));
        assert!(compute_fips_witness(&err, true));
    }

    /// On a healthy macOS dev/CI host (major ∈ [12, 13, 14, 15] at the time
    /// of writing) the cached witness must report `true`. If this test
    /// flips, either the OE whitelist needs extending for a new macOS
    /// release, or the host running CI has rolled past Apple's currently
    /// published cert OE.
    #[test]
    fn fips_witness_ok_on_supported_host() {
        assert!(
            fips_witness_ok(),
            "fips_witness_ok() returned false on the host — check \
             SUPPORTED_OE_MACOS_MAJOR vs. the running macOS major"
        );
    }
}
