// Created: 2026-09-06 by Virtuozzo International GmbH
//! Tests for the trait-set vocabulary and the result type.

use serde_json::json;

use super::{FieldViolation, MalformedTrait, TraitSet, ValidationResult};
use crate::domain::error::DomainError;
use crate::field;

#[test]
fn the_interpreted_traits_are_read_and_the_rest_kept_raw() {
    let raw = json!({
        "secret": true,
        "multiline": false,
        "cron_dialect": "quartz",
        "dynamic_enum_source": "gts.cf.core.am.tenant_type.v1~",
        "entity_reference": "gts.cf.core.am.tenant.v1~",
        "regex": true,
        "unit": "ms"
    });
    let traits = TraitSet::from_traits(raw.clone()).expect("well-formed");
    assert!(traits.secret);
    assert!(!traits.multiline);
    assert_eq!(traits.cron_dialect.as_deref(), Some("quartz"));
    assert_eq!(
        traits.dynamic_enum_source.as_deref(),
        Some("gts.cf.core.am.tenant_type.v1~")
    );
    assert_eq!(
        traits.entity_reference.as_deref(),
        Some("gts.cf.core.am.tenant.v1~")
    );
    assert!(traits.regex);
    // A trait this gear does not interpret still reaches the client.
    assert_eq!(traits.raw, raw);
}

#[test]
fn an_empty_trait_object_means_no_traits_not_a_failure() {
    // Distinct from a type that could not be resolved, which is an error at
    // the port: an empty object is a real answer.
    let traits = TraitSet::from_traits(json!({})).expect("an empty object is well-formed");
    assert_eq!(
        traits,
        TraitSet {
            raw: json!({}),
            ..TraitSet::default()
        }
    );
}

#[test]
fn a_result_with_no_violations_is_accepted() {
    assert!(ValidationResult::accepted().is_accepted());
    assert!(ValidationResult::accepted().into_result().is_ok());
}

#[test]
fn a_rejected_result_surfaces_its_first_violation_as_the_error() {
    let result = ValidationResult {
        violations: vec![
            FieldViolation {
                field: "value/a".to_owned(),
                code: field::VALUE_SCHEMA,
                message: "first".to_owned(),
            },
            FieldViolation {
                field: "value/b".to_owned(),
                code: field::VALUE_FORMAT,
                message: "second".to_owned(),
            },
        ],
    };
    assert!(!result.is_accepted());
    match result.into_result() {
        Err(DomainError::Validation { field, code, .. }) => {
            assert_eq!(field, "value/a");
            assert_eq!(code, field::VALUE_SCHEMA);
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn a_trait_present_with_the_wrong_type_is_a_fault_not_a_default() {
    // `"secret": "true"` is the case that matters: read as absent it would
    // classify a credential as public. The rest follow the same rule.
    for (raw, name, expected, found) in [
        (
            json!({ "secret": "true" }),
            "secret",
            "a boolean",
            "a string",
        ),
        (json!({ "secret": 1 }), "secret", "a boolean", "a number"),
        (json!({ "regex": null }), "regex", "a boolean", "null"),
        (
            json!({ "multiline": [true] }),
            "multiline",
            "a boolean",
            "an array",
        ),
        (
            json!({ "cron_dialect": 5 }),
            "cron_dialect",
            "a string",
            "a number",
        ),
        (
            json!({ "entity_reference": true }),
            "entity_reference",
            "a string",
            "a boolean",
        ),
        (
            json!({ "dynamic_enum_source": {} }),
            "dynamic_enum_source",
            "a string",
            "an object",
        ),
    ] {
        let err = TraitSet::from_traits(raw.clone()).expect_err(&raw.to_string());
        assert_eq!(
            err,
            MalformedTrait {
                name,
                expected,
                found
            },
            "{raw}"
        );
        assert_eq!(
            err.to_string(),
            format!("trait `{name}` must be {expected}, found {found}")
        );
    }
}

#[test]
fn a_well_formed_trait_beside_an_absent_one_reads_as_before() {
    let traits = TraitSet::from_traits(json!({ "secret": false, "cron_dialect": "standard" }))
        .expect("well-formed");
    assert!(!traits.secret);
    assert!(!traits.regex, "absent is false");
    assert_eq!(traits.cron_dialect.as_deref(), Some("standard"));
    assert_eq!(traits.entity_reference, None, "absent is None");
}
