// Created: 2026-09-07 by Constructor Tech
//! Rendering rules: masking, recency, the review pair and the state tag.

use serde_json::json;
use settings_service_sdk::EffectiveSource;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{ABSENT_STATE_TAG, mask, render};
use crate::domain::resolution::{EffectiveValue, MASK_TOKEN, OwnRow, TrailEntry};

fn at(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(seconds).expect("in range")
}

fn effective(classification: &str) -> EffectiveValue {
    let tenant = Uuid::new_v4();
    EffectiveValue {
        key: "k".to_owned(),
        declaration_id: Uuid::nil(),
        scope: format!("/tenants/{tenant}"),
        tenant_id: tenant,
        value: json!("hello"),
        source: EffectiveSource::Inherited,
        source_scope: Some("/".to_owned()),
        traits: json!({}),
        trail: vec![TrailEntry {
            tenant_id: Uuid::nil(),
            scope: "/".to_owned(),
            has_override: true,
            provided_value: true,
            needs_review: false,
            set_by: Some("root-admin".to_owned()),
            last_change_at: Some(at(200)),
        }],
        data_classification: classification.to_owned(),
        domain_affinity: None,
        secret_backed: false,
        declaration_last_change_at: at(100),
        resolved_row_last_change_at: Some(at(200)),
        own_row: None,
    }
}

#[test]
fn secret_is_always_masked_pii_only_without_the_entitlement_public_never() {
    assert_eq!(mask(&json!("x"), "secret", true), (json!(MASK_TOKEN), true));
    assert_eq!(mask(&json!("x"), "pii", false), (json!(MASK_TOKEN), true));
    assert_eq!(mask(&json!("x"), "pii", true), (json!("x"), false));
    assert_eq!(mask(&json!("x"), "public", false), (json!("x"), false));
}

#[test]
fn recency_is_the_later_of_the_declaration_and_the_resolved_row() {
    let dto = render(&effective("public"), false);
    assert_eq!(dto.last_change_at, "1970-01-01T00:03:20Z");

    let mut older_row = effective("public");
    older_row.resolved_row_last_change_at = Some(at(50));
    assert_eq!(
        render(&older_row, false).last_change_at,
        "1970-01-01T00:01:40Z"
    );

    let mut default = effective("public");
    default.resolved_row_last_change_at = None;
    assert_eq!(
        render(&default, false).last_change_at,
        "1970-01-01T00:01:40Z"
    );
}

#[test]
fn the_review_pair_appears_only_when_the_own_row_is_flagged_and_the_tag_follows_the_row() {
    let plain = render(&effective("public"), false);
    assert_eq!(
        (plain.needs_review, plain.needs_review_detail),
        (None, None)
    );
    assert_eq!(
        plain.etag, ABSENT_STATE_TAG,
        "no own row: the absent-state tag"
    );

    let mut flagged = effective("public");
    flagged.own_row = Some(OwnRow {
        needs_review: true,
        needs_review_detail: Some("no longer a boolean".to_owned()),
        last_change_at: at(300),
        updated_at: at(300),
    });
    let dto = render(&flagged, false);
    assert_eq!(dto.needs_review, Some(true));
    assert_eq!(
        dto.needs_review_detail.as_deref(),
        Some("no longer a boolean")
    );
    assert_eq!(dto.etag, at(300).unix_timestamp_nanos().to_string());
    assert_eq!(
        dto.value,
        json!("hello"),
        "the fallthrough value is served beside the flag"
    );
}

#[test]
fn the_administrative_trail_keeps_setter_identity_and_time() {
    let dto = render(&effective("public"), false);
    assert_eq!(
        dto.inheritance_trail[0].set_by.as_deref(),
        Some("root-admin")
    );
    assert_eq!(
        dto.inheritance_trail[0].last_change_at.as_deref(),
        Some("1970-01-01T00:03:20Z")
    );
    assert_eq!(dto.source, "inherited");
}

mod history {
    use serde_json::json;
    use time::OffsetDateTime;
    use uuid::Uuid;

    use crate::api::rest::setting_dto::render_record;
    use crate::audit::{
        ActorClassification, AuditOperation, AuditOutcome, AuditValue, StoredAuditRecord,
    };
    use crate::domain::resolution::MASK_TOKEN;

    fn record(actor: ActorClassification) -> StoredAuditRecord {
        StoredAuditRecord {
            id: Uuid::nil(),
            declaration_key: "k".to_owned(),
            tenant_id: Some(Uuid::nil()),
            operation: AuditOperation::Change,
            actor: "admin@acme".to_owned(),
            actor_classification: actor,
            pre_image: Some(AuditValue::Clear(json!("old"))),
            post_image: Some(AuditValue::Masked),
            outcome: AuditOutcome::Success,
            request_id: "r".to_owned(),
            change_set_id: None,
            occurred_at: OffsetDateTime::UNIX_EPOCH,
            retain_until: None,
        }
    }

    #[test]
    fn a_pii_actor_is_masked_without_the_entitlement_and_shown_with_it() {
        let hidden = render_record(&record(ActorClassification::Pii), false, false);
        assert_eq!(
            (hidden.actor.as_str(), hidden.actor_masked),
            (MASK_TOKEN, true)
        );
        let shown = render_record(&record(ActorClassification::Pii), false, true);
        assert_eq!(
            (shown.actor.as_str(), shown.actor_masked),
            ("admin@acme", false)
        );
        let module = render_record(&record(ActorClassification::Public), false, false);
        assert_eq!(module.actor, "admin@acme");
    }

    #[test]
    fn recorded_values_follow_the_setting_classification_and_secrets_stay_masked() {
        let public = render_record(&record(ActorClassification::Public), false, false);
        assert_eq!(public.pre_value, Some(json!("old")));
        assert_eq!(
            public.post_value,
            Some(json!(MASK_TOKEN)),
            "recorded masked, shown masked"
        );
        assert!(!public.values_masked);

        let pii = render_record(&record(ActorClassification::Public), true, false);
        assert_eq!(pii.pre_value, Some(json!(MASK_TOKEN)));
        assert!(pii.values_masked);
        let entitled = render_record(&record(ActorClassification::Public), true, true);
        assert_eq!(entitled.pre_value, Some(json!("old")));
    }
}
