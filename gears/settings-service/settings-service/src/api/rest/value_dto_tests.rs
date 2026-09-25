// Created: 2026-09-17 by Virtuozzo International GmbH
//! What the write surface puts on the wire: masking that survives both
//! images, the closed rejection vocabulary, and the reports' shapes.

use serde_json::json;
use settings_service_sdk::EffectiveSource;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    BatchChangeRequest, render_batch_item, render_committed, render_impact, render_pending,
    render_validation,
};
use crate::audit::AuditOperation;
use crate::domain::error::DomainError;
use crate::domain::resolution::{EffectiveValue, MASK_TOKEN};
use crate::domain::secrets::PendingSecret;
use crate::domain::validation::FieldViolation;
use crate::domain::writes::service::{Committed, ImpactEntry, ImpactReport, ValidationReport};
use crate::field;

fn at(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(seconds).expect("in range")
}

fn committed(classification: &str) -> Committed {
    Committed {
        key: "gts.cf.core.settings.setting_type.v1~acme.billing.network.proxy.v1~".to_owned(),
        tenant_id: Uuid::nil(),
        scope: "/tenants/00000000-0000-0000-0000-000000000000".to_owned(),
        scope_class: "cascading".to_owned(),
        data_classification: classification.to_owned(),
        old_value: Some(json!("before")),
        new_value: Some(json!("after")),
        etag: "v2".to_owned(),
        operation: AuditOperation::Change,
        change_set_id: Uuid::nil(),
        released_secret: None,
    }
}

fn effective(classification: &str) -> EffectiveValue {
    EffectiveValue {
        key: "k".to_owned(),
        declaration_id: Uuid::nil(),
        scope: "/".to_owned(),
        tenant_id: Uuid::nil(),
        value: json!("current"),
        source: EffectiveSource::SchemaDefault,
        source_scope: None,
        fallback: json!("current"),
        fallback_source: EffectiveSource::SchemaDefault,
        fallback_scope: None,
        traits: json!({}),
        trail: Vec::new(),
        data_classification: classification.to_owned(),
        domain_affinity: None,
        secret_backed: false,
        declaration_last_change_at: at(100),
        resolved_row_last_change_at: None,
        own_row: None,
    }
}

#[test]
fn a_stage_answers_with_the_token_and_its_expiry_and_nothing_else() {
    let pending = PendingSecret {
        id: Uuid::from_u128(7),
        declaration_id: Uuid::nil(),
        tenant_id: Uuid::nil(),
        subject_id: "subject".to_owned(),
        secret_ref: "settings/abc/def".to_owned(),
        created_at: at(0),
        expires_at: at(600),
    };

    let dto = render_pending(&pending);

    assert_eq!(dto.pending_id, Uuid::from_u128(7).to_string());
    assert_eq!(dto.expires_at, "1970-01-01T00:10:00Z");
    // The store reference and the subject are the service's business. Whatever
    // else this type grows, serializing it must never disclose them.
    let wire = serde_json::to_string(&dto).expect("serializes");
    assert!(!wire.contains("settings/abc/def"), "{wire}");
    assert!(!wire.contains("subject"), "{wire}");
}

#[test]
fn a_secrets_images_are_both_masked_and_the_flag_says_so() {
    let dto = render_committed(&committed("secret"), true);

    assert_eq!(dto.old_value, Some(json!(MASK_TOKEN)));
    assert_eq!(dto.new_value, Some(json!(MASK_TOKEN)));
    assert!(dto.masked);
    // The rest of the record is not a value and is not masked with it.
    assert_eq!(dto.etag, "v2");
    assert_eq!(dto.operation, "change");
}

#[test]
fn pii_turns_on_the_mask_only_without_the_entitlement_and_public_never() {
    let unmasked = render_committed(&committed("pii"), true);
    assert_eq!(
        (unmasked.old_value, unmasked.new_value, unmasked.masked),
        (Some(json!("before")), Some(json!("after")), false)
    );

    let masked = render_committed(&committed("pii"), false);
    assert_eq!(
        (masked.old_value, masked.new_value, masked.masked),
        (Some(json!(MASK_TOKEN)), Some(json!(MASK_TOKEN)), true)
    );

    let public = render_committed(&committed("public"), false);
    assert_eq!(
        (public.old_value, public.new_value, public.masked),
        (Some(json!("before")), Some(json!("after")), false)
    );
}

#[test]
fn an_absent_image_stays_absent_rather_than_becoming_the_mask() {
    // A first write has no image before; a removal none after. Rendering must
    // not invent one, or a client cannot tell "nothing was there" from "you
    // may not see what was there".
    let mut first = committed("secret");
    first.old_value = None;
    first.operation = AuditOperation::Create;
    let dto = render_committed(&first, false);
    assert_eq!(dto.old_value, None);
    assert_eq!(dto.new_value, Some(json!(MASK_TOKEN)));
    assert!(dto.masked, "the image that does exist is masked");
    assert_eq!(dto.operation, "create");

    let mut removed = committed("public");
    removed.new_value = None;
    removed.operation = AuditOperation::Revert;
    let dto = render_committed(&removed, false);
    assert_eq!(dto.new_value, None);
    assert!(!dto.masked);
    assert_eq!(dto.operation, "revert");
}

#[test]
fn every_rejection_carries_a_word_from_the_closed_vocabulary() {
    // The list `BatchItemDto::error` documents. A new arm in `rejection_code`
    // that is not here is a word the contract never promised.
    const VOCABULARY: &[&str] = &[
        "invalid",
        "if_match_required",
        "stale",
        "conflict",
        "forbidden",
        "retired",
        "not_found",
        "unavailable",
        "error",
    ];
    let cases: Vec<(DomainError, &str)> = vec![
        (
            DomainError::Validation {
                field: "value".to_owned(),
                code: field::VALIDATION,
                message: "no".to_owned(),
            },
            "invalid",
        ),
        (
            DomainError::PreconditionRequired {
                detail: "no tag".to_owned(),
            },
            "if_match_required",
        ),
        (
            DomainError::PreconditionFailed {
                detail: "stale tag".to_owned(),
            },
            "stale",
        ),
        (
            DomainError::Conflict {
                detail: "global".to_owned(),
            },
            "conflict",
        ),
        (DomainError::Unauthorized { resource: "value" }, "forbidden"),
        (
            DomainError::Retired {
                key: "k".to_owned(),
            },
            "retired",
        ),
        (
            DomainError::NotFound {
                resource: "declaration",
            },
            "not_found",
        ),
        (
            DomainError::Unavailable {
                detail: "down".to_owned(),
            },
            "unavailable",
        ),
        (
            // Step-up never reaches an entry -- a batch refuses as a whole
            // before any change is evaluated -- so it falls to the catch-all
            // rather than growing the vocabulary.
            DomainError::StepUpRequired {
                reason: "stale",
                max_age_seconds: 300,
                acr_values: Vec::new(),
            },
            "error",
        ),
    ];

    for (err, expected) in cases {
        let item = render_batch_item("k", &Err(err), false);
        assert_eq!(item.outcome, "rejected");
        assert_eq!(item.error.as_deref(), Some(expected));
        assert!(VOCABULARY.contains(&expected), "{expected} is not promised");
        assert!(item.change.is_none(), "a rejection carries no change");
        assert!(item.detail.is_some(), "a rejection says why");
    }
}

#[test]
fn a_committed_entry_carries_the_change_and_no_error() {
    let item = render_batch_item("k", &Ok(committed("public")), false);

    assert_eq!(item.outcome, "committed");
    assert_eq!(item.key, "k");
    assert!(item.error.is_none());
    assert!(item.detail.is_none());
    let change = item.change.expect("the change");
    assert_eq!(change.new_value, Some(json!("after")));
    assert_eq!(change.etag, "v2");
}

#[test]
fn a_batch_entry_is_keyed_by_what_was_asked_for_not_by_what_was_stored() {
    // The client matches answers to its own request rows. A key it did not
    // send -- the parsed and re-rendered form, say -- would break that match.
    let item = render_batch_item("as.the.client.sent.it", &Ok(committed("public")), false);
    assert_eq!(item.key, "as.the.client.sent.it");
    assert_ne!(
        item.change.expect("the change").key,
        "as.the.client.sent.it"
    );
}

#[test]
fn an_impact_entry_is_masked_by_the_settings_classification() {
    let report = ImpactReport {
        changed: vec![
            ImpactEntry {
                tenant_id: Uuid::from_u128(1),
                scope: "/tenants/a".to_owned(),
                current: json!("one"),
            },
            ImpactEntry {
                tenant_id: Uuid::from_u128(2),
                scope: "/tenants/b".to_owned(),
                current: json!("two"),
            },
        ],
        total_changed: 9,
        scanned: 40,
        truncated: true,
    };

    let dto = render_impact(&report, "secret", true);
    assert_eq!(
        dto.changed.iter().map(|e| &e.current).collect::<Vec<_>>(),
        vec![&json!(MASK_TOKEN), &json!(MASK_TOKEN)],
        "a reachable value is still the value"
    );
    // The counts describe the walk, not the values, and are never masked.
    assert_eq!(
        (dto.total_changed, dto.scanned, dto.truncated),
        (9, 40, true)
    );
    assert_eq!(dto.changed[1].scope, "/tenants/b");

    let plain = render_impact(&report, "public", false);
    assert_eq!(plain.changed[0].current, json!("one"));
}

#[test]
fn validity_is_the_absence_of_violations_not_a_separate_verdict() {
    let ok = render_validation(
        &ValidationReport {
            violations: Vec::new(),
            effective: std::sync::Arc::new(effective("public")),
            impact: None,
        },
        true,
    );
    assert!(ok.valid);
    assert!(ok.violations.is_empty());
    assert!(ok.impact.is_none(), "a local setting reports no impact");

    let bad = render_validation(
        &ValidationReport {
            violations: vec![FieldViolation {
                field: "value/port".to_owned(),
                code: field::VALIDATION,
                message: "out of range".to_owned(),
            }],
            effective: std::sync::Arc::new(effective("public")),
            impact: Some(ImpactReport::empty()),
        },
        true,
    );
    assert!(!bad.valid);
    assert_eq!(bad.violations.len(), 1);
    assert_eq!(bad.violations[0].field, "value/port");
    assert_eq!(bad.violations[0].code, field::VALIDATION);
    assert!(bad.impact.is_some());
}

#[test]
fn the_report_masks_its_impact_by_the_same_classification_as_its_value() {
    // One decision, taken from the setting being validated: an impact entry
    // that escaped it would disclose through the preview what the effective
    // read refuses.
    let report = ValidationReport {
        violations: Vec::new(),
        effective: std::sync::Arc::new(effective("secret")),
        impact: Some(ImpactReport {
            changed: vec![ImpactEntry {
                tenant_id: Uuid::from_u128(1),
                scope: "/tenants/a".to_owned(),
                current: json!("descendant"),
            }],
            total_changed: 1,
            scanned: 1,
            truncated: false,
        }),
    };

    let dto = render_validation(&report, true);
    assert_eq!(dto.effective.value, json!(MASK_TOKEN));
    assert_eq!(
        dto.impact.expect("impact").changed[0].current,
        json!(MASK_TOKEN)
    );
}

#[test]
fn an_internal_fault_in_a_batch_entry_says_so_and_nothing_more() {
    // The page is a 200, so this detail never meets the top-level mapper that
    // strips diagnostics; the renderer has to hold the same line itself.
    let err = DomainError::Internal {
        diagnostic: "postgres at 10.0.0.5:5432 refused the connection".to_owned(),
    };
    let item = render_batch_item("k", &Err(err), false);
    assert_eq!(item.error.as_deref(), Some("error"));
    assert_eq!(item.detail.as_deref(), Some("internal error"));
}

#[test]
fn a_batch_entry_tells_an_explicit_null_from_an_omitted_value() {
    // The single-item write carries `null` as a value; the batch must not
    // collapse it into "no value" and refuse the `set` for want of one.
    let explicit: BatchChangeRequest =
        serde_json::from_str(r#"{ "key": "k", "value": null }"#).expect("deserializes");
    assert_eq!(
        explicit
            .value
            .as_deref()
            .map(serde_json::value::RawValue::get),
        Some("null"),
        "an explicit null is a value"
    );

    let omitted: BatchChangeRequest =
        serde_json::from_str(r#"{ "key": "k" }"#).expect("deserializes");
    assert!(omitted.value.is_none(), "no field, no value");
}
