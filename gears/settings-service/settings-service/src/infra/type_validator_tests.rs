// Created: 2026-09-06 by Virtuozzo International GmbH
//! Tests for the registry-backed Type Validator, against a hand-built source.

use serde_json::json;

use super::GtsTypeValidator;
use crate::domain::error::DomainError;
use crate::domain::validation::TypeValidator;
use crate::field;
use crate::test_support::FakeSource;

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
