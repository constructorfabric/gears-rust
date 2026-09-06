// Created: 2026-09-06 by Constructor Tech
//! Tests for the registry-backed Type Validator, against a hand-built source.

use serde_json::json;

use super::GtsTypeValidator;
use crate::domain::error::DomainError;
use crate::domain::validation::TypeValidator;
use crate::field;
use crate::test_support::FakeSource;

const PORT_TYPE: &str = "gts.cf.toolkit.settings.type_port.v1~";
const IP_TYPE: &str = "gts.cf.toolkit.settings.type_ipv4.v1~";
const REGEX_TYPE: &str = "gts.cf.toolkit.settings.type_regex.v1~";
const REF_TYPE: &str = "gts.cf.toolkit.settings.type_tenant_ref.v1~";
const SECRET_TYPE: &str = "gts.cf.toolkit.settings.type_api_token.v1~";
const TENANT_TYPE: &str = "gts.cf.core.am.tenant.v1~";

fn catalogue() -> FakeSource {
    FakeSource::default()
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
        .validate_value("gts.cf.toolkit.settings.type_missing.v1~", &json!(1))
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
        "gts.cf.toolkit.settings.type_regex_pair.v1~",
        json!({
            "$id": "gts://gts.cf.toolkit.settings.type_regex_pair.v1~",
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
            "gts.cf.toolkit.settings.type_regex_pair.v1~",
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
        .resolve_traits("gts.cf.toolkit.settings.type_missing.v1~")
        .await
    {
        Err(DomainError::Validation { field, code, .. }) => {
            assert_eq!(field, "value_type_id");
            assert_eq!(code, field::VALUE_TYPE_UNKNOWN);
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}
