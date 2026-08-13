// Created: 2026-08-11 by Constructor Tech
//! Tests for the public SDK models.
//!
//! Acceptance criteria: FEATURE `gear-foundation.md` §6 — models serialize
//! stably, because consuming gears depend on that wire shape.

use super::{EffectiveSource, SecretHandle};

#[test]
fn effective_source_serializes_in_snake_case() {
    // Consuming gears match on this wire vocabulary; it is part of the contract.
    for (variant, expected) in [
        (EffectiveSource::OwnOverride, "\"own_override\""),
        (EffectiveSource::Inherited, "\"inherited\""),
        (EffectiveSource::SchemaDefault, "\"schema_default\""),
    ] {
        let json = serde_json::to_string(&variant).expect("serializes");
        assert_eq!(json, expected);
    }
}

#[test]
fn effective_source_round_trips() {
    for variant in [
        EffectiveSource::OwnOverride,
        EffectiveSource::Inherited,
        EffectiveSource::SchemaDefault,
    ] {
        let json = serde_json::to_string(&variant).expect("serializes");
        let back: EffectiveSource = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, variant);
    }
}

#[test]
fn only_schema_default_means_unconfigured() {
    assert!(EffectiveSource::SchemaDefault.is_unconfigured());
    assert!(!EffectiveSource::OwnOverride.is_unconfigured());
    assert!(!EffectiveSource::Inherited.is_unconfigured());
}

#[test]
fn secret_handle_serializes_transparently() {
    let handle = SecretHandle::new("opaque-token-1");
    let json = serde_json::to_string(&handle).expect("serializes");
    assert_eq!(
        json, "\"opaque-token-1\"",
        "the handle is a bare opaque string on the wire, not a wrapper object"
    );
}

#[test]
fn secret_handle_round_trips() {
    let handle = SecretHandle::new("opaque-token-2");
    let json = serde_json::to_string(&handle).expect("serializes");
    let back: SecretHandle = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(back, handle);
}

#[test]
fn secret_handle_debug_redacts_the_token() {
    // A handle must never turn a log line into a disclosure path.
    let handle = SecretHandle::new("super-secret-coordinates");
    let rendered = format!("{handle:?}");
    assert!(
        !rendered.contains("super-secret-coordinates"),
        "Debug must not print the token, got `{rendered}`"
    );
    assert_eq!(rendered, "SecretHandle(<redacted>)");
}
