//! Tests that verify the `GTS_ID_PREFIX` compile-time customization works
//! end-to-end through the `gts_id!` and `gts_uri!` macros.
//!
//! Two layers of tests:
//!
//! 1. **In-process** — structural invariants that pass with any valid prefix
//!    (default or custom). These verify the macros produce correct results
//!    for whatever prefix was compiled in.
//! 2. **Subprocess** — spawn `cargo test` with different `GTS_ID_PREFIX` env
//!    vars and verify the compiled-in prefix changes accordingly. Since the
//!    prefix is a compile-time constant (`option_env!`), changing it requires
//!    a rebuild, which the subprocess triggers. A guard env var
//!    (`GTS_PREFIX_TEST_SPAWNED`) prevents infinite recursion.

use std::process::Command;
use toolkit_gts::{GTS_ID_PREFIX, GTS_ID_URI_PREFIX, gts_id, gts_uri};

const SUFFIX: &str = "test.cf.toolkit_gts.prefix_check.v1~";
const TYPE_ID: &str = gts_id!("test.cf.toolkit_gts.prefix_check.v1~");
const TYPE_URI: &str = gts_uri!("test.cf.toolkit_gts.prefix_check.v1~");

// =====================================================================
//  Layer 1: in-process structural invariants (pass with any prefix)
// =====================================================================

#[test]
fn gts_id_macro_prepends_configured_prefix() {
    assert!(
        TYPE_ID.starts_with(GTS_ID_PREFIX),
        "gts_id! result \"{TYPE_ID}\" does not start with GTS_ID_PREFIX \"{GTS_ID_PREFIX}\""
    );
    assert!(
        TYPE_ID.ends_with(SUFFIX),
        "gts_id! result \"{TYPE_ID}\" does not end with the suffix \"{SUFFIX}\""
    );
    assert_eq!(TYPE_ID, format!("{GTS_ID_PREFIX}{SUFFIX}"));
}

#[test]
fn gts_uri_macro_prepends_uri_and_configured_id_prefix() {
    assert!(
        TYPE_URI.starts_with(GTS_ID_URI_PREFIX),
        "gts_uri! result \"{TYPE_URI}\" does not start with GTS_ID_URI_PREFIX \"{GTS_ID_URI_PREFIX}\""
    );
    let expected = format!("{GTS_ID_URI_PREFIX}{GTS_ID_PREFIX}{SUFFIX}");
    assert_eq!(TYPE_URI, expected);
}

#[test]
fn prefix_constant_is_well_formed() {
    println!("Compiled with GTS_ID_PREFIX = \"{GTS_ID_PREFIX}\"");
    assert!(!GTS_ID_PREFIX.is_empty());
    assert!(GTS_ID_PREFIX.ends_with('.'));
}

// =====================================================================
//  Layer 2: subprocess tests — verify env var actually changes the prefix
// =====================================================================

/// Guard env var — when set, subprocess-spawning tests skip themselves to
/// avoid infinite recursion (the spawned `cargo test` would re-run them).
const SPAWNED_GUARD: &str = "GTS_PREFIX_TEST_SPAWNED";

/// Run `cargo test --test prefix_customization prefix_constant_is_well_formed
/// -- --nocapture` with the given `GTS_ID_PREFIX` env var and return the
/// combined stdout+stderr output.
fn run_with_prefix(prefix: Option<&str>) -> (bool, String) {
    let mut cmd = Command::new("cargo");
    cmd.args([
        "test",
        "-p",
        "cf-gears-toolkit-gts",
        "--test",
        "prefix_customization",
        "prefix_constant_is_well_formed",
        "--",
        "--nocapture",
    ]);
    if let Some(p) = prefix {
        cmd.env("GTS_ID_PREFIX", p);
    }
    cmd.env(SPAWNED_GUARD, "1");

    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => panic!("failed to spawn cargo: {e}"),
    };
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), combined)
}

#[test]
fn default_prefix_is_gts_dot() {
    if std::env::var(SPAWNED_GUARD).is_ok() {
        return;
    }

    let (ok, output) = run_with_prefix(None);
    assert!(ok, "cargo test failed:\n{output}");
    assert!(
        output.contains(r#"Compiled with GTS_ID_PREFIX = "gts.""#),
        "expected default prefix \"gts.\" in output, got:\n{output}"
    );
}

#[test]
fn custom_prefix_acme_dot() {
    if std::env::var(SPAWNED_GUARD).is_ok() {
        return;
    }

    let (ok, output) = run_with_prefix(Some("acme."));
    assert!(ok, "cargo test failed:\n{output}");
    assert!(
        output.contains(r#"Compiled with GTS_ID_PREFIX = "acme.""#),
        "expected custom prefix \"acme.\" in output, got:\n{output}"
    );
}

#[test]
fn custom_prefix_myco_dot() {
    if std::env::var(SPAWNED_GUARD).is_ok() {
        return;
    }

    let (ok, output) = run_with_prefix(Some("myco."));
    assert!(ok, "cargo test failed:\n{output}");
    assert!(
        output.contains(r#"Compiled with GTS_ID_PREFIX = "myco.""#),
        "expected custom prefix \"myco.\" in output, got:\n{output}"
    );
}

#[test]
fn invalid_prefix_fails_to_compile() {
    if std::env::var(SPAWNED_GUARD).is_ok() {
        return;
    }

    // "Acme." is invalid — uppercase is rejected by the const validator.
    let (ok, output) = run_with_prefix(Some("Acme."));
    assert!(
        !ok,
        "expected compilation to fail with invalid prefix 'Acme.', but it succeeded:\n{output}"
    );
    assert!(
        output.contains("GTS_ID_PREFIX") || output.contains("panic"),
        "expected error mentioning GTS_ID_PREFIX or panic, got:\n{output}"
    );
}
