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

#[test]
fn a_contributed_declaration_carries_only_what_the_module_stated() {
    // Optional fields absent on the wire mean the declaration's defaults —
    // `standard`, `public`, step-up required, not exposable — decided by the
    // service, not invented by the SDK.
    use super::{ContributedDeclaration, ScopeClass};
    let key = crate::SettingKey::contributed(
        "cf",
        "settings_demo",
        "network",
        "proxy_enabled",
        std::num::NonZeroU32::new(1).expect("non-zero"),
    )
    .expect("key");
    let decl = ContributedDeclaration::new(
        key,
        "gts.cf.toolkit.settings.type_bool_flag.v1~",
        serde_json::json!(false),
        ScopeClass::Cascading,
    );
    let wire = serde_json::to_value(&decl).expect("serializes");
    assert_eq!(
        wire,
        serde_json::json!({
            "key": "gts.cf.core.settings.setting_type.v1~cf.settings_demo.network.proxy_enabled.v1~",
            "valueTypeId": "gts.cf.toolkit.settings.type_bool_flag.v1~",
            "defaultValue": false,
            "scopeClass": "cascading"
        })
    );
    let back: ContributedDeclaration = serde_json::from_value(wire).expect("round-trips");
    assert_eq!(back.scope_class, ScopeClass::Cascading);
    assert!(back.requires_step_up.is_none());
}

#[test]
fn a_reconcile_result_reports_refusals_per_key() {
    use super::{ContributionError, ReconcileResult};
    let result = ReconcileResult {
        registered: 2,
        errors: vec![ContributionError {
            key: "gts.cf.core.settings.setting_type.v1~cf.x.y.z.v1~".to_owned(),
            code: "value_type_unknown".to_owned(),
            message: "no such type".to_owned(),
        }],
        ..ReconcileResult::default()
    };
    let wire = serde_json::to_value(&result).expect("serializes");
    assert_eq!(wire["registered"], 2);
    assert_eq!(wire["errors"][0]["code"], "value_type_unknown");
    // A set that changed nothing serializes without an `errors` key at all.
    let quiet = serde_json::to_value(ReconcileResult::default()).expect("serializes");
    assert!(quiet.get("errors").is_none());
}
