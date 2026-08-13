// Created: 2026-08-12 by Constructor Tech
//! Tests for the bootstrap contract.
//!
//! Acceptance criteria: FEATURE `gear-foundation.md` §6 — *a gear start with a
//! missing required bootstrap value fails at startup rather than falling back to
//! a default* — and CDSL step `inst-gf-init-2`.
//!
//! # No deployment-owned value is required today
//!
//! The gear reads its section with `ctx.config()`, not `config_or_default()`, so
//! the section must exist and every field must deserialize. But the only field
//! left is a design-fixed backstop with a default, so there is currently nothing
//! a deployment can omit and be refused for.
//!
//! That is a statement about scope, not a weakening: the step-up values that
//! were required here traced to no PRD requirement, and one of them could
//! weaken a requirement that has no carve-out. The machinery stays so the first
//! genuinely required field inherits it, and the tests below pin what remains
//! observable: unknown keys and wrong-typed values are refused.
//!
//! The absence of a struct-level `#[serde(default)]` is deliberately *not*
//! asserted here. While every field carries its own default there is no input
//! that distinguishes the two, so any such test would pass for the wrong
//! reason. It becomes testable the day a required field is added — which is
//! also the day it starts to matter.

use super::SettingsServiceConfig;

fn parse(json: serde_json::Value) -> Result<SettingsServiceConfig, serde_json::Error> {
    serde_json::from_value(json)
}

#[test]
fn a_complete_config_parses() {
    let cfg = parse(serde_json::json!({ "cache_ttl_seconds": 15 })).expect("parses");
    assert_eq!(cfg.cache_ttl_seconds, 15);
}

#[test]
fn the_cache_backstop_defaults_because_the_design_fixes_it() {
    // DESIGN.md §4.2 sets 30s and says this cache owns the knob. A default here
    // is a real answer rather than a guess, which is why it is the one field
    // allowed to have one.
    let cfg = parse(serde_json::json!({})).expect("parses without the optional field");
    assert_eq!(cfg.cache_ttl_seconds, 30);
}

#[test]
fn a_mistyped_key_is_refused_rather_than_ignored() {
    // Without `deny_unknown_fields` this would start the service with the
    // default TTL while the operator believed they had set 60.
    let err = parse(serde_json::json!({ "cache_ttl_second": 60 })).expect_err("must not parse");
    assert!(
        err.to_string().contains("cache_ttl_second"),
        "the error must name the unknown key, got `{err}`"
    );
}

#[test]
fn a_wrong_typed_value_is_refused_rather_than_coerced() {
    // A TTL of "thirty" is a deployment error. Coercing or defaulting it would
    // start the service with a staleness bound nobody chose.
    assert!(parse(serde_json::json!({ "cache_ttl_seconds": "thirty" })).is_err());
}
