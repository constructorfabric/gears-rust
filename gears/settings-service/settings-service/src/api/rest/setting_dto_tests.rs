// Created: 2026-09-07 by Virtuozzo International GmbH
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
        fallback: json!("hello"),
        fallback_source: EffectiveSource::Inherited,
        fallback_scope: Some("/".to_owned()),
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
fn the_fallback_is_masked_by_the_one_decision_made_for_the_value() {
    // A row with its own override: the value is its own, the fallback the
    // parent's. Whatever the rule says for the value, it says for both.
    let with_own = |classification: &str| {
        let mut e = effective(classification);
        e.value = json!("own");
        e.source = EffectiveSource::OwnOverride;
        e.fallback = json!("parent");
        e
    };
    let secret = render(&with_own("secret"), true);
    assert_eq!(
        (secret.value, secret.fallback),
        (json!(MASK_TOKEN), json!(MASK_TOKEN))
    );
    assert!(secret.masked);

    let pii_hidden = render(&with_own("pii"), false);
    assert_eq!(
        (pii_hidden.value, pii_hidden.fallback),
        (json!(MASK_TOKEN), json!(MASK_TOKEN))
    );
    assert!(pii_hidden.masked);

    let pii_shown = render(&with_own("pii"), true);
    assert_eq!(
        (pii_shown.value, pii_shown.fallback),
        (json!("own"), json!("parent"))
    );
    assert!(!pii_shown.masked);

    let public = render(&with_own("public"), false);
    assert_eq!(
        (public.value, public.fallback),
        (json!("own"), json!("parent"))
    );
    assert_eq!(public.fallback_source, "inherited");
    assert_eq!(public.fallback_scope.as_deref(), Some("/"));
    assert!(!public.masked);
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

mod browse_entry {
    use serde_json::json;
    use time::OffsetDateTime;
    use uuid::Uuid;

    use crate::api::rest::setting_dto::{SettingItemDto, render_flagged};
    use crate::domain::error::DomainError;
    use crate::domain::resolution::MASK_TOKEN;
    use crate::domain::value::StoredValue;

    fn at(seconds: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(seconds).expect("in range")
    }

    fn stored(classification: &str, tenant: Uuid) -> StoredValue {
        StoredValue {
            id: Uuid::nil(),
            declaration_id: Uuid::nil(),
            tenant_id: tenant,
            value: Some(json!("kept")),
            secret_ref: None,
            data_classification: classification.to_owned(),
            needs_review: true,
            needs_review_detail: Some("no longer a port".to_owned()),
            last_change_at: at(200),
            updated_at: at(300),
            set_by: "tenant-admin".to_owned(),
        }
    }

    #[test]
    fn a_key_that_could_not_be_resolved_carries_its_own_outcome() {
        // The browse page answers per key: one unreadable setting must not
        // fail the page, or a single retired declaration empties the screen.
        let cases = [
            (
                DomainError::NotFound {
                    resource: "declaration",
                },
                "not_found",
            ),
            (
                DomainError::Retired {
                    key: "k".to_owned(),
                },
                "retired",
            ),
            (
                DomainError::Unavailable {
                    detail: "registry down".to_owned(),
                },
                "unavailable",
            ),
            (
                DomainError::Conflict {
                    detail: "unexpected".to_owned(),
                },
                "error",
            ),
        ];

        for (err, expected) in cases {
            let item = SettingItemDto::failed("k", &err);
            assert_eq!(item.outcome, expected);
            assert_eq!(item.key, "k");
            assert!(item.effective.is_none());
            assert!(item.flagged.is_none());
            assert!(item.detail.is_some(), "the entry says why");
        }
    }

    #[test]
    fn an_internal_fault_in_a_browse_entry_says_so_and_nothing_more() {
        let err = DomainError::Internal {
            diagnostic: "postgres at 10.0.0.5:5432 refused the connection".to_owned(),
        };
        let item = SettingItemDto::failed("k", &err);
        assert_eq!(item.outcome, "error");
        assert_eq!(item.detail.as_deref(), Some("internal error"));
    }

    #[test]
    fn a_requested_key_with_no_declaration_reads_as_not_found() {
        let item = SettingItemDto::not_found("absent.v1~");
        assert_eq!(item.outcome, "not_found");
        assert_eq!(item.key, "absent.v1~");
    }

    #[test]
    fn mode_is_a_tag_added_to_an_entry_not_a_field_of_its_own_shape() {
        // Every page carries every setting; the client groups by this tag. An
        // entry that never got one omits it rather than guessing a default.
        let untagged = SettingItemDto::not_found("k");
        assert_eq!(untagged.mode, None);
        let wire = serde_json::to_value(&untagged).expect("serializes");
        assert!(wire.get("mode").is_none(), "{wire}");

        let tagged = SettingItemDto::not_found("k").with_mode("advanced");
        assert_eq!(tagged.mode.as_deref(), Some("advanced"));
        assert_eq!(tagged.outcome, "not_found", "the tag changes nothing else");
    }

    #[test]
    fn a_flagged_override_is_masked_and_keeps_why_it_was_flagged() {
        let tenant = Uuid::from_u128(4);
        let root = Uuid::from_u128(1);

        let shown = render_flagged("k", &stored("public", tenant), root, false);
        assert_eq!(shown.value, json!("kept"));
        assert!(!shown.masked);
        assert_eq!(
            shown.needs_review_detail.as_deref(),
            Some("no longer a port")
        );
        assert_eq!(shown.set_by, "tenant-admin");
        assert_eq!(shown.last_change_at, "1970-01-01T00:03:20Z");
        // The value state tag (`last_change_at`), not `updated_at`: a flag moves
        // the latter alone, and a correction must pass the write's check.
        assert_eq!(shown.etag, at(200).unix_timestamp_nanos().to_string());

        // A flagged secret is still a secret: the review listing shows that
        // one exists and needs attention, never what it holds.
        let secret = render_flagged("k", &stored("secret", tenant), root, true);
        assert_eq!(secret.value, json!(MASK_TOKEN));
        assert!(secret.masked);
        assert_eq!(
            secret.needs_review_detail.as_deref(),
            Some("no longer a port")
        );
    }

    #[test]
    fn a_secret_row_holds_no_inline_value_and_still_renders_masked() {
        // A `secret` row keeps its plaintext in the Credential Store, so the
        // inline column is empty. Rendering must not produce a null value a
        // client would show as "unset".
        let tenant = Uuid::from_u128(4);
        let mut row = stored("secret", tenant);
        row.value = None;
        row.secret_ref = Some("settings/abc/def".to_owned());

        let dto = render_flagged("k", &row, Uuid::from_u128(1), true);
        assert_eq!(dto.value, json!(MASK_TOKEN));
        assert!(dto.masked);
        let wire = serde_json::to_string(&dto).expect("serializes");
        assert!(!wire.contains("settings/abc/def"), "{wire}");
    }
}
