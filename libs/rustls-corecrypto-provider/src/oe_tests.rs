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

/// On a healthy macOS dev/CI host (major ∈ [`SUPPORTED_OE_MACOS_MAJOR`])
/// the cached witness must report `true`. If this test
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
