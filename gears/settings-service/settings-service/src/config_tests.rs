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

use super::{IN_PROCESS_CONTRACTS, SettingsServiceConfig};

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

#[test]
fn the_audit_retention_defaults_to_twelve_months() {
    let cfg = parse(serde_json::json!({})).expect("parses");
    assert_eq!(cfg.audit_retention_days, 365);
    let cfg = parse(serde_json::json!({ "audit_retention_days": 730 })).expect("parses");
    assert_eq!(cfg.audit_retention_days, 730);
}

#[test]
fn the_step_up_section_is_optional_and_defaults_its_window_to_five_minutes() {
    let cfg = parse(serde_json::json!({})).expect("parses");
    assert!(cfg.step_up.is_none(), "absent means no verifier is bound");
    let cfg = parse(serde_json::json!({
        "step_up": { "jwks_uri": "https://idp.example/keys" }
    }))
    .expect("parses");
    let step_up = cfg.step_up.expect("present");
    assert_eq!(step_up.max_age_seconds, 300);
    assert!(step_up.acr_values.is_empty());
    assert!(
        parse(serde_json::json!({ "step_up": { "max_age_seconds": 60 } })).is_err(),
        "the JWKS endpoint is required"
    );
}

#[test]
fn a_remote_binding_for_either_sdk_trait_is_refused_with_the_contract_named() {
    for contract in IN_PROCESS_CONTRACTS {
        for transport in ["rest", "grpc", "REST"] {
            let config: SettingsServiceConfig = serde_json::from_value(serde_json::json!({
                "client_wiring": {
                    contract: { "transport": transport, "endpoint": "http://elsewhere" }
                }
            }))
            .expect("the section parses");
            let refusal = config
                .check_in_process_bindings()
                .expect_err("a remote binding is refused");
            assert!(refusal.contains(contract), "{refusal}");
            assert!(refusal.contains(transport), "{refusal}");
        }
    }
}

#[test]
fn a_local_binding_another_contract_and_no_section_at_all_are_all_accepted() {
    for section in [
        serde_json::json!({}),
        serde_json::json!({ "client_wiring": {} }),
        serde_json::json!({
            "client_wiring": { "settings_reader_client": { "transport": "local" } }
        }),
        // Another gear's contract is none of this gear's business.
        serde_json::json!({
            "client_wiring": {
                "authz_resolver_api": { "transport": "rest", "endpoint": "http://authz" }
            }
        }),
    ] {
        let config: SettingsServiceConfig =
            serde_json::from_value(section.clone()).expect("parses");
        assert!(
            config.check_in_process_bindings().is_ok(),
            "{section} should be accepted"
        );
    }
}
