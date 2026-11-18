//! Tests for the registry-backed Type Validator, against a hand-built source.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery,
    TypesRegistryClient, testing,
};
use uuid::Uuid;

use super::GtsTypeValidator;
use crate::domain::error::DomainError;
use crate::domain::validation::{TypeValidator, ValueTypeRefusal};
use crate::field;
use crate::test_support::{
    BOOL, DECLARED_SETTING, FakeSource, MODULE_TYPE, NOT_TYPE_IDS, REFERRING_TYPE, TEXT,
    referring_type_schema, registry_client, resolution_catalogue,
};
use settings_service_sdk::gts::SETTING_TYPE_BASE;

const PORT_TYPE: &str = "gts.cf.core.settings.type_port.v1~";
const IP_TYPE: &str = "gts.cf.core.settings.type_ipv4.v1~";
const REGEX_TYPE: &str = "gts.cf.core.settings.type_regex.v1~";
const REF_TYPE: &str = "gts.cf.core.settings.type_tenant_ref.v1~";
const SECRET_TYPE: &str = "gts.cf.core.settings.type_api_token.v1~";
const TENANT_TYPE: &str = "gts.cf.core.am.tenant.v1~";
const CRON_TYPE: &str = "gts.cf.core.settings.type_cron.v1~";
const ODD_DIALECT_TYPE: &str = "gts.cf.core.settings.type_quartz_cron.v1~";
const REGION_TYPE: &str = "gts.cf.core.settings.type_region.v1~";
const UNKNOWN_SOURCE_TYPE: &str = "gts.cf.core.settings.type_ghost_enum.v1~";
const REGION_SOURCE: &str = "gts.cf.core.platform.region.v1~";

fn catalogue() -> FakeSource {
    FakeSource::default()
        .with_type(
            CRON_TYPE,
            json!({
                "$id": format!("gts://{CRON_TYPE}"),
                "type": "string",
                "x-gts-traits": { "cron_dialect": "standard" }
            }),
        )
        .with_type(
            ODD_DIALECT_TYPE,
            json!({
                "$id": format!("gts://{ODD_DIALECT_TYPE}"),
                "type": "string",
                "x-gts-traits": { "cron_dialect": "quartz" }
            }),
        )
        .with_type(
            REGION_TYPE,
            json!({
                "$id": format!("gts://{REGION_TYPE}"),
                "type": "string",
                "x-gts-traits": { "dynamic_enum_source": REGION_SOURCE }
            }),
        )
        .with_type(
            UNKNOWN_SOURCE_TYPE,
            json!({
                "$id": format!("gts://{UNKNOWN_SOURCE_TYPE}"),
                "type": "string",
                "x-gts-traits": { "dynamic_enum_source": "gts.cf.core.platform.nowhere.v1~" }
            }),
        )
        .with_enum(REGION_SOURCE, &["eu-west-1", "eu-central-1", "us-east-1"])
        .with_type(
            PORT_TYPE,
            json!({
                "$id": format!("gts://{PORT_TYPE}"),
                "type": "object",
                "properties": { "port": { "type": "integer", "minimum": 1, "maximum": 65535 } },
                "required": ["port"]
            }),
        )
        .with_type(
            IP_TYPE,
            json!({ "$id": format!("gts://{IP_TYPE}"), "type": "string", "format": "ipv4" }),
        )
        .with_type(
            REGEX_TYPE,
            json!({
                "$id": format!("gts://{REGEX_TYPE}"),
                "type": "string",
                "x-gts-traits": { "regex": true }
            }),
        )
        .with_type(
            REF_TYPE,
            json!({
                "$id": format!("gts://{REF_TYPE}"),
                "type": "string",
                "x-gts-traits": { "entity_reference": TENANT_TYPE }
            }),
        )
        .with_type(
            SECRET_TYPE,
            json!({
                "$id": format!("gts://{SECRET_TYPE}"),
                "type": "string",
                "x-gts-traits": { "secret": true, "multiline": false }
            }),
        )
        .with_instance(&format!("{TENANT_TYPE}acme.tenants.root.v1"))
}

fn codes(result: &crate::domain::validation::ValidationResult) -> Vec<&'static str> {
    result.violations.iter().map(|v| v.code).collect()
}

#[tokio::test]
async fn an_unknown_type_is_a_rejection_not_an_acceptance() {
    // Fail closed: a value nobody could check is not a value anybody accepted.
    // The fault is the declaration's, so it is reported on `value_type_id`.
    let v = GtsTypeValidator::new(catalogue());
    let result = v
        .validate_value("gts.cf.core.settings.type_missing.v1~", &json!(1))
        .await
        .expect("a rejection, not an error");
    assert!(!result.is_accepted());
    assert_eq!(result.violations[0].field, "value_type_id");
    assert_eq!(result.violations[0].code, field::VALUE_TYPE_UNKNOWN);
}

#[tokio::test]
async fn an_unreachable_registry_is_unavailable() {
    let v = GtsTypeValidator::new(FakeSource {
        unavailable: true,
        ..FakeSource::default()
    });
    assert!(matches!(
        v.validate_value(PORT_TYPE, &json!({ "port": 80 })).await,
        Err(DomainError::Unavailable { .. })
    ));
    assert!(matches!(
        v.resolve_traits(PORT_TYPE).await,
        Err(DomainError::Unavailable { .. })
    ));
}

#[tokio::test]
async fn a_valid_value_is_accepted() {
    let v = GtsTypeValidator::new(catalogue());
    let result = v
        .validate_value(PORT_TYPE, &json!({ "port": 8080 }))
        .await
        .expect("validates");
    assert!(result.is_accepted(), "{result:?}");
}

#[tokio::test]
async fn a_schema_violation_names_the_position() {
    let v = GtsTypeValidator::new(catalogue());
    let result = v
        .validate_value(PORT_TYPE, &json!({ "port": 70000 }))
        .await
        .expect("validates");
    assert_eq!(codes(&result), vec![field::VALUE_SCHEMA]);
    assert_eq!(result.violations[0].field, "value/port");
}

#[tokio::test]
async fn a_schema_violation_names_the_rule_and_never_quotes_the_value() {
    // The message travels: into a 400, a batch item, a log line. The value may
    // be a credential or personal data, so the rule is named and the position
    // is given by `field`; what was submitted is not repeated back.
    let v = GtsTypeValidator::new(catalogue());
    let result = v
        .validate_value(PORT_TYPE, &json!({ "port": "sk-live-TOPSECRET" }))
        .await
        .expect("validates");
    assert_eq!(codes(&result), vec![field::VALUE_SCHEMA]);
    assert_eq!(result.violations[0].field, "value/port");
    let message = &result.violations[0].message;
    assert!(!message.contains("TOPSECRET"), "{message}");
    assert!(
        message.contains("integer"),
        "the rule is still named: {message}"
    );
}

#[tokio::test]
async fn a_format_keyword_is_asserted_not_annotated() {
    // `format` is advisory to a plain JSON Schema validator; here a value that
    // does not match rejects, exactly as `type` would.
    let v = GtsTypeValidator::new(catalogue());
    let result = v
        .validate_value(IP_TYPE, &json!("999.1.1.1"))
        .await
        .expect("validates");
    assert_eq!(codes(&result), vec![field::VALUE_FORMAT]);
    let ok = v
        .validate_value(IP_TYPE, &json!("10.0.0.1"))
        .await
        .expect("validates");
    assert!(ok.is_accepted());
}

#[tokio::test]
async fn a_guard_fault_is_reported_alone_before_any_schema_check() {
    let v = GtsTypeValidator::new(catalogue());
    let result = v
        .validate_value(PORT_TYPE, &json!({ "port": 9_007_199_254_740_993_u64 }))
        .await
        .expect("validates");
    assert_eq!(codes(&result), vec![field::VALUE_NOT_CANONICAL]);
}

#[tokio::test]
async fn a_guard_fault_is_refused_before_the_registry_is_asked() {
    // Over the byte cap, against a type nobody registered: the cheap guard
    // answers, and the registry is never consulted for a value it would only
    // have to refuse afterwards.
    let v = GtsTypeValidator::new(catalogue());
    let oversized = json!("x".repeat(crate::domain::validation::guards::MAX_SERIALIZED_BYTES));
    let result = v
        .validate_value(
            "gts.cf.core.settings.type_nobody_registered.v1~",
            &oversized,
        )
        .await
        .expect("validates");
    assert_eq!(codes(&result), vec![field::VALUE_TOO_LARGE]);
    assert_eq!(
        v.source()
            .schema_lookups
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the guard decided; the registry was not asked"
    );
}

#[tokio::test]
async fn a_trait_checked_value_holds_at_most_the_leaf_cap_and_is_refused_above_it() {
    // Under the byte cap and still thousands of strings: the trait check would
    // compile or look up each one. The count is bounded like the bytes are.
    let source = catalogue().with_type(
        "gts.cf.core.settings.type_patterns.v1~",
        json!({
            "$id": "gts://gts.cf.core.settings.type_patterns.v1~",
            "type": "array",
            "items": { "type": "string" },
            "x-gts-traits": { "regex": true }
        }),
    );
    let v = GtsTypeValidator::new(source);
    let over: Vec<&str> = std::iter::repeat_n("a", super::MAX_TRAIT_LEAVES + 1).collect();
    let result = v
        .validate_value("gts.cf.core.settings.type_patterns.v1~", &json!(over))
        .await
        .expect("validates");
    assert_eq!(codes(&result), vec![field::VALUE_TOO_MANY_LEAVES]);

    let at: Vec<&str> = std::iter::repeat_n("a", super::MAX_TRAIT_LEAVES).collect();
    let result = v
        .validate_value("gts.cf.core.settings.type_patterns.v1~", &json!(at))
        .await
        .expect("validates");
    assert!(result.is_accepted(), "{result:?}");
}

#[tokio::test]
async fn a_regex_trait_requires_the_value_to_compile() {
    let v = GtsTypeValidator::new(catalogue());
    let bad = v
        .validate_value(REGEX_TYPE, &json!("(unclosed"))
        .await
        .expect("validates");
    assert_eq!(codes(&bad), vec![field::VALUE_REGEX_INVALID]);
    let good = v
        .validate_value(REGEX_TYPE, &json!("^[a-z]+$"))
        .await
        .expect("validates");
    assert!(good.is_accepted());
}

#[tokio::test]
async fn an_entity_reference_must_resolve_to_an_instance_of_its_type() {
    let v = GtsTypeValidator::new(catalogue());
    let known = format!("{TENANT_TYPE}acme.tenants.root.v1");
    let ok = v
        .validate_value(REF_TYPE, &json!(known))
        .await
        .expect("validates");
    assert!(ok.is_accepted(), "{ok:?}");

    let unknown = v
        .validate_value(
            REF_TYPE,
            &json!(format!("{TENANT_TYPE}acme.tenants.ghost.v1")),
        )
        .await
        .expect("validates");
    assert_eq!(codes(&unknown), vec![field::VALUE_REFERENCE_UNRESOLVED]);

    // An id of another type does not count even if such an instance existed.
    let wrong_type = v
        .validate_value(
            REF_TYPE,
            &json!("gts.cf.core.am.user.v1~acme.users.root.v1"),
        )
        .await
        .expect("validates");
    assert_eq!(codes(&wrong_type), vec![field::VALUE_REFERENCE_UNRESOLVED]);
}

#[tokio::test]
async fn every_fault_is_collected_rather_than_the_first() {
    let source = catalogue().with_type(
        "gts.cf.core.settings.type_regex_pair.v1~",
        json!({
            "$id": "gts://gts.cf.core.settings.type_regex_pair.v1~",
            "type": "object",
            "properties": {
                "include": { "type": "string" },
                "exclude": { "type": "string" },
                "limit": { "type": "integer", "maximum": 10 }
            },
            "x-gts-traits": { "regex": true }
        }),
    );
    let v = GtsTypeValidator::new(source);
    let result = v
        .validate_value(
            "gts.cf.core.settings.type_regex_pair.v1~",
            &json!({ "include": "(", "exclude": "[", "limit": 11 }),
        )
        .await
        .expect("validates");
    let mut got = codes(&result);
    got.sort_unstable();
    assert_eq!(
        got,
        vec![
            field::VALUE_REGEX_INVALID,
            field::VALUE_REGEX_INVALID,
            field::VALUE_SCHEMA
        ]
    );
}

#[tokio::test]
async fn traits_are_resolved_with_the_secret_marker() {
    let v = GtsTypeValidator::new(catalogue());
    let traits = v.resolve_traits(SECRET_TYPE).await.expect("resolves");
    assert!(traits.secret);
    assert!(!traits.multiline);
    assert_eq!(traits.raw, json!({ "secret": true, "multiline": false }));
}

#[tokio::test]
async fn trait_resolution_of_an_unknown_type_fails_rather_than_returning_an_empty_set() {
    // An empty set would classify a secret-trait type as public.
    let v = GtsTypeValidator::new(catalogue());
    match v
        .resolve_traits("gts.cf.core.settings.type_missing.v1~")
        .await
    {
        Err(DomainError::Validation { field, code, .. }) => {
            assert_eq!(field, "value_type_id");
            assert_eq!(code, field::VALUE_TYPE_UNKNOWN);
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_cron_value_must_parse_under_the_dialect_its_type_declares() {
    let v = GtsTypeValidator::new(catalogue());
    for accepted in ["0 3 * * *", "*/5 9-17 * * mon-fri"] {
        let result = v
            .validate_value(CRON_TYPE, &json!(accepted))
            .await
            .expect("validates");
        assert!(result.violations.is_empty(), "{accepted}: {result:?}");
    }

    let refused = v
        .validate_value(CRON_TYPE, &json!("0 3 * *"))
        .await
        .expect("validates");
    assert_eq!(refused.violations.len(), 1);
    assert_eq!(refused.violations[0].code, field::VALUE_CRON_INVALID);
    assert_eq!(refused.violations[0].field, "value");
    assert!(
        refused.violations[0].message.contains("five fields"),
        "{:?}",
        refused.violations[0]
    );
}

#[tokio::test]
async fn a_cron_dialect_this_service_cannot_check_refuses_the_value() {
    // Admitting it would turn the rule into an annotation, which is the one
    // thing the trait is specified not to be.
    let v = GtsTypeValidator::new(catalogue());
    let refused = v
        .validate_value(ODD_DIALECT_TYPE, &json!("0 0 12 * * ?"))
        .await
        .expect("validates");
    assert_eq!(refused.violations.len(), 1);
    assert_eq!(
        refused.violations[0].code,
        field::VALUE_CRON_DIALECT_UNKNOWN
    );
    assert!(
        refused.violations[0].message.contains("quartz"),
        "{:?}",
        refused.violations[0]
    );
}

#[tokio::test]
async fn a_dynamic_enum_value_must_be_a_member_of_its_source() {
    // A member is a registered instance derived from the source: its own id
    // is looked up, and the members are never listed.
    let v = GtsTypeValidator::new(catalogue());
    let accepted = v
        .validate_value(REGION_TYPE, &json!(format!("{REGION_SOURCE}eu-west-1")))
        .await
        .expect("validates");
    assert!(accepted.violations.is_empty(), "{accepted:?}");

    let refused = v
        .validate_value(REGION_TYPE, &json!(format!("{REGION_SOURCE}mars-north-2")))
        .await
        .expect("validates");
    assert_eq!(refused.violations.len(), 1);
    assert_eq!(refused.violations[0].code, field::VALUE_NOT_IN_ENUM);
    // The message names the source, so a client knows where to look.
    assert!(
        refused.violations[0].message.contains(REGION_SOURCE),
        "{:?}",
        refused.violations[0]
    );

    // A registered instance of another type is not a member either.
    let elsewhere = v
        .validate_value(
            REGION_TYPE,
            &json!(format!("{TENANT_TYPE}acme.tenants.root.v1")),
        )
        .await
        .expect("validates");
    assert_eq!(codes(&elsewhere), vec![field::VALUE_NOT_IN_ENUM]);
}

#[tokio::test]
async fn entity_references_are_resolved_in_one_lookup_whatever_their_number() {
    let v = GtsTypeValidator::new(catalogue().with_type(
        "gts.cf.core.settings.type_tenant_refs.v1~",
        json!({
            "$id": "gts://gts.cf.core.settings.type_tenant_refs.v1~",
            "type": "array",
            "items": { "type": "string" },
            "x-gts-traits": { "entity_reference": TENANT_TYPE }
        }),
    ));
    let known = format!("{TENANT_TYPE}acme.tenants.root.v1");
    let refs: Vec<&str> = std::iter::repeat_n(known.as_str(), 50).collect();
    let result = v
        .validate_value("gts.cf.core.settings.type_tenant_refs.v1~", &json!(refs))
        .await
        .expect("validates");
    assert!(result.is_accepted(), "{result:?}");
    assert_eq!(
        v.source()
            .instance_lookups
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "fifty leaves, one lookup"
    );
}

#[tokio::test]
async fn a_dynamic_enum_source_this_deployment_does_not_know_refuses_the_value() {
    let v = GtsTypeValidator::new(catalogue());
    let refused = v
        .validate_value(UNKNOWN_SOURCE_TYPE, &json!("anything"))
        .await
        .expect("validates");
    assert_eq!(refused.violations.len(), 1);
    assert_eq!(refused.violations[0].code, field::VALUE_ENUM_SOURCE_UNKNOWN);
}

#[tokio::test]
async fn every_trait_failure_is_collected_rather_than_only_the_first() {
    // A structured value carrying a trait on the whole is checked leaf by leaf,
    // and each bad leaf is its own field-level error.
    let source = catalogue().with_type(
        "gts.cf.core.settings.type_schedules.v1~",
        json!({
            "$id": "gts://gts.cf.core.settings.type_schedules.v1~",
            "type": "array",
            "items": { "type": "string" },
            "x-gts-traits": { "cron_dialect": "standard" }
        }),
    );
    let v = GtsTypeValidator::new(source);
    let result = v
        .validate_value(
            "gts.cf.core.settings.type_schedules.v1~",
            &json!(["0 3 * * *", "not cron", "60 0 * * *"]),
        )
        .await
        .expect("validates");
    assert_eq!(result.violations.len(), 2);
    let fields: Vec<&str> = result
        .violations
        .iter()
        .map(|violation| violation.field.as_str())
        .collect();
    assert_eq!(fields, vec!["value/1", "value/2"]);
}

const MISSPELT_SECRET_TYPE: &str = "gts.cf.core.settings.type_misspelt_secret.v1~";

fn catalogue_with_a_misspelt_secret() -> FakeSource {
    catalogue().with_type(
        MISSPELT_SECRET_TYPE,
        json!({
            "$id": format!("gts://{MISSPELT_SECRET_TYPE}"),
            "type": "string",
            "x-gts-traits": { "secret": "true" }
        }),
    )
}

#[tokio::test]
async fn trait_resolution_of_a_type_with_a_misspelt_trait_fails_rather_than_defaulting() {
    // Read as absent, `"secret": "true"` would classify a credential public.
    let v = GtsTypeValidator::new(catalogue_with_a_misspelt_secret());
    match v.resolve_traits(MISSPELT_SECRET_TYPE).await {
        Err(DomainError::Validation {
            field,
            code,
            message,
        }) => {
            assert_eq!(field, "value_type_id");
            assert_eq!(code, field::VALUE_TYPE_MALFORMED);
            assert!(
                message.contains("`secret` must be a boolean, found a string"),
                "{message}"
            );
        }
        other => panic!("expected a validation fault on value_type_id, got {other:?}"),
    }
}

#[tokio::test]
async fn a_value_of_a_type_with_a_misspelt_trait_is_rejected_not_passed() {
    let v = GtsTypeValidator::new(catalogue_with_a_misspelt_secret());
    let result = v
        .validate_value(MISSPELT_SECRET_TYPE, &json!("hunter2"))
        .await
        .expect("the registry answered");
    assert!(!result.is_accepted());
    assert_eq!(result.violations.len(), 1, "{result:?}");
    assert_eq!(result.violations[0].field, "value_type_id");
    assert_eq!(result.violations[0].code, field::VALUE_TYPE_MALFORMED);
}

/// Types whose trait blocks are present but are not objects. The registry's
/// merge keeps only objects and drops the rest without a sign, so each of
/// these would read as a type with no traits at all — a secret type among
/// them classified public — if its shape were not checked.
fn catalogue_with_shapeless_trait_blocks() -> (FakeSource, Vec<(&'static str, &'static str)>) {
    let cases = [
        (
            "gts.cf.core.settings.type_traits_array.v1~",
            json!({ "x-gts-traits": ["secret"] }),
            "`x-gts-traits` must be an object, found an array",
        ),
        (
            "gts.cf.core.settings.type_traits_string.v1~",
            json!({ "x-gts-traits": "secret" }),
            "`x-gts-traits` must be an object, found a string",
        ),
        (
            "gts.cf.core.settings.type_traits_null.v1~",
            json!({ "x-gts-traits": null }),
            "`x-gts-traits` must be an object, found null",
        ),
        (
            "gts.cf.core.settings.type_trait_schema_bool.v1~",
            json!({ "x-gts-traits-schema": true }),
            "`x-gts-traits-schema` must be an object, found a boolean",
        ),
        (
            "gts.cf.core.settings.type_trait_props_array.v1~",
            json!({ "x-gts-traits-schema": { "properties": [{ "secret": { "default": true } }] } }),
            "`x-gts-traits-schema.properties` must be an object, found an array",
        ),
    ];
    let mut source = catalogue();
    let mut expected = Vec::new();
    for (id, traits, message) in cases {
        let mut schema = json!({ "$id": format!("gts://{id}"), "type": "string" });
        for (k, v) in traits.as_object().expect("an object") {
            schema[k] = v.clone();
        }
        source = source.with_type(id, schema);
        expected.push((id, message));
    }
    (source, expected)
}

#[tokio::test]
async fn a_trait_block_that_is_not_an_object_makes_the_type_malformed_not_traitless() {
    let (source, cases) = catalogue_with_shapeless_trait_blocks();
    let v = GtsTypeValidator::new(source);
    for (id, expected) in cases {
        match v.resolve_traits(id).await {
            Err(DomainError::Validation {
                field,
                code,
                message,
            }) => {
                assert_eq!(field, "value_type_id", "{id}");
                assert_eq!(code, field::VALUE_TYPE_MALFORMED, "{id}");
                assert!(message.contains(expected), "{id}: {message}");
            }
            other => panic!("{id}: expected a malformed type, got {other:?}"),
        }
        let result = v
            .validate_value(id, &json!("hunter2"))
            .await
            .expect("the registry answered");
        assert!(!result.is_accepted(), "{id}: {result:?}");
        assert_eq!(
            result.violations[0].code,
            field::VALUE_TYPE_MALFORMED,
            "{id}: {result:?}"
        );
    }
}

#[tokio::test]
async fn an_instance_of_a_derived_type_is_not_a_reference_to_the_base_type() {
    // A type derived from the target spells the target as its prefix, and so
    // does every instance of it. The prefix is not the boundary: the type the
    // instance is registered under is, and it has to be the target itself.
    let derived_instance = format!("{TENANT_TYPE}cf.core.am.derived.v1~acme.tenants.sub.v1");
    let derived_member = format!("{REGION_SOURCE}cf.core.platform.derived.v1~eu-west-9");
    let v = GtsTypeValidator::new(
        catalogue()
            .with_instance(&derived_instance)
            .with_instance(&derived_member),
    );

    let reference = v
        .validate_value(REF_TYPE, &json!(derived_instance))
        .await
        .expect("validates");
    assert_eq!(codes(&reference), vec![field::VALUE_REFERENCE_UNRESOLVED]);

    // The same boundary for a dynamic enum: a member is an instance of the
    // source, not of a type that merely starts with it.
    let member = v
        .validate_value(REGION_TYPE, &json!(derived_member))
        .await
        .expect("validates");
    assert_eq!(codes(&member), vec![field::VALUE_NOT_IN_ENUM]);
}

const REGION_TENANT_TYPE: &str = "gts.cf.core.settings.type_region_and_tenant.v1~";

#[tokio::test]
async fn both_registry_traits_on_one_type_share_a_lookup_and_neither_admits_the_others_instance() {
    // One lookup answers both traits, each reading it against its own expected
    // type: a region member is no tenant reference, a tenant is no region.
    let v = GtsTypeValidator::new(catalogue().with_type(
        REGION_TENANT_TYPE,
        json!({
            "$id": format!("gts://{REGION_TENANT_TYPE}"),
            "type": "string",
            "x-gts-traits": {
                "dynamic_enum_source": REGION_SOURCE,
                "entity_reference": TENANT_TYPE
            }
        }),
    ));
    let region = format!("{REGION_SOURCE}eu-west-1");
    let tenant = format!("{TENANT_TYPE}acme.tenants.root.v1");

    let as_region = v
        .validate_value(REGION_TENANT_TYPE, &json!(region))
        .await
        .expect("validates");
    assert_eq!(codes(&as_region), vec![field::VALUE_REFERENCE_UNRESOLVED]);

    let as_tenant = v
        .validate_value(REGION_TENANT_TYPE, &json!(tenant))
        .await
        .expect("validates");
    assert_eq!(codes(&as_tenant), vec![field::VALUE_NOT_IN_ENUM]);

    assert_eq!(
        v.source()
            .instance_lookups
            .load(std::sync::atomic::Ordering::SeqCst),
        2,
        "one lookup per value, shared by both traits"
    );
}

#[tokio::test]
async fn dynamic_enum_members_are_resolved_in_one_lookup_whatever_their_number() {
    let regions_type = "gts.cf.core.settings.type_regions.v1~";
    let v = GtsTypeValidator::new(catalogue().with_type(
        regions_type,
        json!({
            "$id": format!("gts://{regions_type}"),
            "type": "array",
            "items": { "type": "string" },
            "x-gts-traits": { "dynamic_enum_source": REGION_SOURCE }
        }),
    ));
    let member = format!("{REGION_SOURCE}eu-central-1");
    let members: Vec<&str> = std::iter::repeat_n(member.as_str(), 50).collect();
    let result = v
        .validate_value(regions_type, &json!(members))
        .await
        .expect("validates");
    assert!(result.is_accepted(), "{result:?}");
    assert_eq!(
        v.source()
            .instance_lookups
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "fifty leaves, one lookup"
    );
}

// ── A type that is not a value type ──────────────────────────────────────────

#[tokio::test]
async fn a_declared_setting_named_as_a_type_does_not_compile_into_a_validator() {
    // What a declaration naming a setting key as its value type ran into
    // before the value-type check stood in front: the setting's own type
    // narrows `payload` by a `gts://` reference to its value type, the
    // effective schema inlines the parent chain and nothing else, and a
    // reference left unresolved does not build. The port reports that as an
    // internal fault — a `500` on the wire.
    let v = GtsTypeValidator::new(resolution_catalogue());
    assert!(
        v.resolve_traits(DECLARED_SETTING).await.is_ok(),
        "the traits resolve: nothing refuses the id before the schema is compiled"
    );
    match v.validate_value(DECLARED_SETTING, &json!(true)).await {
        Err(DomainError::Internal { diagnostic }) => {
            assert!(
                diagnostic.contains("is not a valid JSON Schema"),
                "{diagnostic}"
            );
        }
        other => panic!("expected the internal fault, got {other:?}"),
    }
}

#[tokio::test]
async fn the_abstract_base_named_as_a_type_validates_any_object_with_a_payload() {
    // The other half: the base constrains nothing but the presence of
    // `payload`, so as a value type it accepts a default of that shape, and
    // only the value-type check keeps a declaration from adopting it.
    let v = GtsTypeValidator::new(resolution_catalogue());
    let result = v
        .validate_value(SETTING_TYPE_BASE, &json!({ "payload": 1 }))
        .await
        .expect("validated");
    assert!(result.is_accepted(), "{result:?}");
}

/// What the port answered about a type, a failure of the lookup being the
/// test's to fail on.
async fn judged<S: super::SchemaSource>(
    v: &GtsTypeValidator<S>,
    id: &str,
) -> Result<(), ValueTypeRefusal> {
    v.ensure_value_type(id)
        .await
        .unwrap_or_else(|err| panic!("{id}: the lookup failed: {err:?}"))
}

fn refused_as_not_a_value_type(outcome: &Result<(), ValueTypeRefusal>, what: &str) {
    assert!(
        matches!(outcome, Err(ValueTypeRefusal::NotAValueType(_))),
        "{what}: expected the refusal, got {outcome:?}"
    );
}

#[test]
fn a_refusal_reads_on_the_rest_surface_as_a_fault_of_value_type_id_under_its_own_code() {
    // The one place a refusal becomes the validation fault a client is
    // shown: the field is the same for all three, the code tells them apart.
    for (refusal, code) in [
        (
            ValueTypeRefusal::NotAValueType("not one".to_owned()),
            field::NOT_A_VALUE_TYPE,
        ),
        (
            ValueTypeRefusal::Unknown("not registered".to_owned()),
            field::VALUE_TYPE_UNKNOWN,
        ),
        (
            ValueTypeRefusal::Unsupported("does not build".to_owned()),
            field::VALUE_TYPE_UNSUPPORTED,
        ),
    ] {
        let detail = match &refusal {
            ValueTypeRefusal::NotAValueType(m)
            | ValueTypeRefusal::Unknown(m)
            | ValueTypeRefusal::Unsupported(m) => m.clone(),
        };
        match DomainError::from(refusal) {
            DomainError::Validation {
                field,
                code: got,
                message,
            } => {
                assert_eq!(field, "value_type_id");
                assert_eq!(got, code);
                assert_eq!(message, detail);
            }
            other => panic!("expected a validation fault, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_type_of_the_gears_namespace_outside_the_catalogue_is_not_a_value_type() {
    // The setting base, a declared setting and a resource type of the gear:
    // refused on the identifier, registered or not, without asking the
    // registry.
    let v = GtsTypeValidator::new(resolution_catalogue());
    for id in [
        SETTING_TYPE_BASE,
        DECLARED_SETTING,
        "gts.cf.core.settings.setting_type.v1~acme.settings.network.never_declared.v1~",
        "gts.cf.core.settings.declaration.v1~",
    ] {
        refused_as_not_a_value_type(&judged(&v, id).await, id);
    }
    assert_eq!(
        v.source()
            .schema_lookups
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the identifier decides"
    );
}

#[tokio::test]
async fn an_abstract_type_of_any_namespace_is_not_a_value_type() {
    const ENVELOPE: &str = "gts.acme.demo.values.envelope.v1~";
    const CONCRETE: &str = "gts.acme.demo.values.envelope.v1~acme.demo.values.retry.v1~";
    let source = resolution_catalogue()
        .with_type(
            ENVELOPE,
            json!({
                "$id": format!("gts://{ENVELOPE}"),
                "type": "object",
                "x-gts-abstract": true
            }),
        )
        .with_derived_type(
            CONCRETE,
            json!({
                "$id": format!("gts://{CONCRETE}"),
                "allOf": [{ "$ref": format!("gts://{ENVELOPE}") }],
                "properties": { "attempts": { "type": "integer" } }
            }),
        );
    let v = GtsTypeValidator::new(source);
    refused_as_not_a_value_type(&judged(&v, ENVELOPE).await, ENVELOPE);
    // The keyword marks the type that carries it: a concrete type derived
    // from an abstract one is a value type.
    judged(&v, CONCRETE).await.expect("a concrete derived type");
}

#[tokio::test]
async fn a_catalogue_type_a_type_derived_from_one_and_a_module_type_are_value_types() {
    const SHORT_TEXT: &str =
        "gts.cf.core.settings.type_plain_text.v1~acme.demo.values.short_text.v1~";
    let source = resolution_catalogue().with_derived_type(
        SHORT_TEXT,
        json!({
            "$id": format!("gts://{SHORT_TEXT}"),
            "allOf": [{ "$ref": format!("gts://{TEXT}") }],
            "maxLength": 16
        }),
    );
    let v = GtsTypeValidator::new(source);
    for id in [BOOL, SHORT_TEXT, MODULE_TYPE] {
        judged(&v, id)
            .await
            .unwrap_or_else(|refusal| panic!("{id}: {refusal:?}"));
    }
}

#[tokio::test]
async fn an_unregistered_or_unreachable_type_is_never_taken_for_a_value_type() {
    let v = GtsTypeValidator::new(resolution_catalogue());
    for id in [
        "gts.cf.core.settings.type_missing.v1~",
        "gts.acme.demo.values.missing.v1~",
    ] {
        let outcome = judged(&v, id).await;
        assert!(
            matches!(outcome, Err(ValueTypeRefusal::Unknown(_))),
            "{id}: {outcome:?}"
        );
    }
    let down = GtsTypeValidator::new(FakeSource {
        unavailable: true,
        ..resolution_catalogue()
    });
    assert!(matches!(
        down.ensure_value_type(BOOL).await,
        Err(DomainError::Unavailable { .. })
    ));
}

#[tokio::test]
async fn an_id_that_is_no_type_id_is_an_unknown_type_and_the_registry_is_not_asked() {
    // A bare word, an instance id, a text with the trailing `~` that is no
    // GTS id: nothing is registered under such an id, and the registry would
    // refuse it as an argument. To a declaration that is the same fact as an
    // absent type, so the adapter answers it itself — over a registry that
    // fails every call, the answer is still "unknown", never the failure.
    let refusing: Arc<dyn TypesRegistryClient> = Arc::new(RefusingRegistry {
        error: testing::internal("the registry must not be asked"),
    });
    let v = GtsTypeValidator::new(refusing);
    for id in NOT_TYPE_IDS.into_iter().chain(["not a gts id~"]) {
        let outcome = judged(&v, id).await;
        assert!(
            matches!(outcome, Err(ValueTypeRefusal::Unknown(_))),
            "{id}: {outcome:?}"
        );
    }

    // And over the registry's own test client the three ports agree.
    let v = GtsTypeValidator::new(registry_client());
    for id in NOT_TYPE_IDS {
        let outcome = judged(&v, id).await;
        assert!(
            matches!(outcome, Err(ValueTypeRefusal::Unknown(_))),
            "{id}: {outcome:?}"
        );
        match v.resolve_traits(id).await {
            Err(DomainError::Validation { code, .. }) => {
                assert_eq!(code, field::VALUE_TYPE_UNKNOWN, "{id}");
            }
            other => panic!("{id}: {other:?}"),
        }
        let result = v.validate_value(id, &json!(true)).await.expect("judged");
        assert_eq!(codes(&result), vec![field::VALUE_TYPE_UNKNOWN], "{id}");
    }
    judged(&v, BOOL)
        .await
        .expect("the registered type is still one");
}

/// A types registry whose `get_type_schema` answers every id with one error.
/// The adapter's type lookup calls nothing else.
struct RefusingRegistry {
    error: CanonicalError,
}

#[async_trait::async_trait]
impl TypesRegistryClient for RefusingRegistry {
    async fn get_type_schema(&self, _type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        Err(self.error.clone())
    }

    async fn register(&self, _entities: Vec<Value>) -> Result<Vec<RegisterResult>, CanonicalError> {
        unreachable!("the type lookup registers nothing")
    }
    async fn register_type_schemas(
        &self,
        _type_schemas: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        unreachable!("the type lookup registers nothing")
    }
    async fn get_type_schema_by_uuid(
        &self,
        _type_uuid: Uuid,
    ) -> Result<GtsTypeSchema, CanonicalError> {
        unreachable!("the type lookup reads by id")
    }
    async fn get_type_schemas(
        &self,
        _type_ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        unreachable!("the type lookup reads one type")
    }
    async fn get_type_schemas_by_uuid(
        &self,
        _type_uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        unreachable!("the type lookup reads by id")
    }
    async fn list_type_schemas(
        &self,
        _query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        unreachable!("the type lookup lists nothing")
    }
    async fn register_instances(
        &self,
        _instances: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        unreachable!("the type lookup registers nothing")
    }
    async fn get_instance(&self, _id: &str) -> Result<GtsInstance, CanonicalError> {
        unreachable!("the type lookup reads no instance")
    }
    async fn get_instance_by_uuid(&self, _uuid: Uuid) -> Result<GtsInstance, CanonicalError> {
        unreachable!("the type lookup reads no instance")
    }
    async fn get_instances(
        &self,
        _ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, CanonicalError>> {
        unreachable!("the type lookup reads no instance")
    }
    async fn get_instances_by_uuid(
        &self,
        _uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        unreachable!("the type lookup reads no instance")
    }
    async fn list_instances(
        &self,
        _query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        unreachable!("the type lookup lists nothing")
    }
}

#[tokio::test]
async fn a_registered_type_the_registry_cannot_build_is_a_fault_not_an_unknown_type() {
    // The registry answers a type id with a refusal of the argument when it
    // holds the type and cannot build it: its parent is gone, or the stored
    // schema is one it does not accept. Read as "unknown", that would tell
    // an administrator to register a type that is registered. It is a fault
    // of the registered data, carried with the registry's own words — on
    // every port, and never as a refusal of the declaration.
    let broken: Arc<dyn TypesRegistryClient> = Arc::new(RefusingRegistry {
        error: testing::invalid_gts_id(format!(
            "type-schema {MODULE_TYPE} references missing parent gts.acme.demo.values.base.v1~"
        )),
    });
    let v = GtsTypeValidator::new(broken);
    let faults = [
        v.ensure_value_type(MODULE_TYPE).await.map(|_| ()),
        v.resolve_traits(MODULE_TYPE).await.map(|_| ()),
        v.validate_value(MODULE_TYPE, &json!(3)).await.map(|_| ()),
    ];
    for fault in faults {
        match fault {
            Err(DomainError::Internal { diagnostic }) => {
                assert!(diagnostic.contains(MODULE_TYPE), "{diagnostic}");
                assert!(
                    diagnostic.contains("references missing parent"),
                    "the registry's reason is kept: {diagnostic}"
                );
            }
            other => panic!("expected the internal fault, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_type_whose_schema_refers_outside_its_parent_chain_is_refused_as_unsupported() {
    // A module's concrete type that takes a property's shape from another
    // type by `gts://`. The effective schema keeps the reference, which does
    // not build: the declaration is told so, on the type it named.
    let v = GtsTypeValidator::new(
        resolution_catalogue().with_type(REFERRING_TYPE, referring_type_schema()),
    );
    match judged(&v, REFERRING_TYPE).await {
        Err(ValueTypeRefusal::Unsupported(message)) => {
            assert!(message.contains(REFERRING_TYPE), "{message}");
            // The builder's own reason is in the refusal: here, the
            // reference it could not follow.
            assert!(
                message.contains(&format!("gts://{MODULE_TYPE}")),
                "the refusal names what did not build: {message}"
            );
        }
        other => panic!("expected the refusal, got {other:?}"),
    }
    // A value of a row already stored with such a type is not re-labelled:
    // validating it is the internal fault it was.
    assert!(matches!(
        v.validate_value(REFERRING_TYPE, &json!({ "inner": 3 }))
            .await,
        Err(DomainError::Internal { .. })
    ));
}

#[tokio::test]
async fn an_id_longer_than_any_gts_id_is_refused_without_being_repeated() {
    // A multi-kilobyte id, in the gear's namespace and outside it: both are
    // refused as unknown types in a refusal of fixed length that does not
    // repeat what was sent, and the registry is not asked.
    let v = GtsTypeValidator::new(resolution_catalogue());
    let filler = "z".repeat(8 * 1024);
    for id in [
        format!("gts.cf.core.settings.{filler}.v1~"),
        format!("gts.acme.demo.values.{filler}.v1~"),
    ] {
        match judged(&v, &id).await {
            Err(ValueTypeRefusal::Unknown(message)) => {
                assert!(message.len() < 200, "a bounded refusal: {message}");
                assert!(!message.contains(&filler[..64]), "{message}");
                assert!(message.contains(&id.len().to_string()), "{message}");
            }
            other => panic!("expected an unknown type, got {other:?}"),
        }
    }
    assert_eq!(
        v.source()
            .schema_lookups
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the length decides"
    );

    // At the bound the id is judged, and repeated, as any other.
    let at_bound = format!(
        "gts.acme.demo.values.{}.v1~",
        "z".repeat(gts::GTS_ID_MAX_LENGTH - "gts.acme.demo.values..v1~".len())
    );
    assert_eq!(at_bound.len(), gts::GTS_ID_MAX_LENGTH);
    match judged(&v, &at_bound).await {
        Err(ValueTypeRefusal::Unknown(message)) => {
            assert!(message.contains(&at_bound), "{message}");
        }
        other => panic!("expected an unknown type, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unsupported_refusal_repeats_at_most_the_bound_of_the_builders_reason() {
    // A reference long enough that the builder's reason, which quotes it,
    // runs past the bound: the refusal carries the bound's worth of it and
    // no more.
    const LONG_REF_TYPE: &str = "gts.acme.demo.values.long_wrapper.v1~";
    let target = format!("gts://gts.acme.demo.values.{}.v1~", "z".repeat(2_000));
    let v = GtsTypeValidator::new(resolution_catalogue().with_type(
        LONG_REF_TYPE,
        json!({
            "$id": format!("gts://{LONG_REF_TYPE}"),
            "type": "object",
            "properties": { "inner": { "$ref": target } }
        }),
    ));
    match judged(&v, LONG_REF_TYPE).await {
        Err(ValueTypeRefusal::Unsupported(message)) => {
            let (_, after) = message
                .split_once("could be checked: ")
                .unwrap_or_else(|| panic!("the reason's lead-in: {message}"));
            let (reason, _) = after
                .split_once(" (a schema that refers")
                .unwrap_or_else(|| panic!("the reason's end: {message}"));
            assert_eq!(
                reason.chars().count(),
                super::BUILD_REASON_MAX_CHARS,
                "the reason is cut at the bound: {reason}"
            );
            assert!(!message.contains(&target), "{message}");
        }
        other => panic!("expected the refusal, got {other:?}"),
    }
}
