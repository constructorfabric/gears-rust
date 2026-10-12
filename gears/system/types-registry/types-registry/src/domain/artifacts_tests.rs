//! The resolution fingerprint. Pure.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::resolution_fingerprint;

/// Identical artifacts, digested twice, byte-identical. This is the property the
/// whole "recomputation that changes nothing changes no read" claim rests on.
#[test]
fn the_resolution_fingerprint_is_stable_across_two_computations() {
    let a = resolution_fingerprint(r#"{"a":1}"#, "{}", "{}");
    let b = resolution_fingerprint(r#"{"a":1}"#, "{}", "{}");
    assert_eq!(a.len(), 32, "the persisted equality identity is SHA-256");
    assert_eq!(a, b);
}

/// Each of the three artifacts moves it, so a change confined to the effective
/// traits is not invisible.
#[test]
fn each_artifact_moves_the_fingerprint() {
    let base = resolution_fingerprint("{}", "{}", "{}");
    assert_ne!(
        base,
        resolution_fingerprint(r#"{"a":1}"#, "{}", "{}"),
        "schema"
    );
    assert_ne!(
        base,
        resolution_fingerprint("{}", r#"{"a":1}"#, "{}"),
        "traits"
    );
    assert_ne!(
        base,
        resolution_fingerprint("{}", "{}", r#"{"a":1}"#),
        "traits schema"
    );
}

/// Fields are length-prefixed, so no two splits of adjacent artifacts collide.
#[test]
fn artifact_boundaries_cannot_be_confused() {
    assert_ne!(
        resolution_fingerprint("ab", "c", "{}"),
        resolution_fingerprint("a", "bc", "{}"),
    );
}
