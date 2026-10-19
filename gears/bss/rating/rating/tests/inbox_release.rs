//! Feature 01 §2.2 and §6: `POST /bss-rating/v1/inbox/{id}/release` and V23 for inbox entries.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use axum::http::StatusCode;
use bss_rating::api::rest::RELEASE_PATH;
use bss_rating::authz::{actions, labels};
use bss_rating::domain::digest::Sha256Hex;
use bss_rating::domain::exception::SubjectKind;
use bss_rating::domain::inbox::{
    AcceptOutcome, InboxEntry, InboxSource, InboxState, InboxTransition,
};
use bss_rating::domain::ports::{Dependency, FailureCode};
use bss_rating::infra::storage::repo::{audit_repo, exception_repo, inbox_repo};
use bss_rating::test_providers::MetricSample;
use common::{
    Fixture, exception_key, fact, fixture, post, router, router_with, unsupported_fact_of, user,
};
use serde_json::json;
use toolkit::api::OpenApiRegistryImpl;
use toolkit_db::Db;
use toolkit_security::AccessScope;
use uuid::Uuid;

const TENANT: Uuid = Uuid::from_u128(0xA);

/// An applied fact and a quarantined digest conflict of it.
async fn quarantined(f: &Fixture) -> (InboxEntry, InboxEntry) {
    let applied = f
        .inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "1"})))
        .await
        .unwrap();
    let conflict = f
        .inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "2"})))
        .await
        .unwrap();
    let AcceptOutcome::Quarantined(conflict) = conflict else {
        panic!("expected a quarantine");
    };
    (applied.entry().clone(), conflict)
}

fn path(entry_id: Uuid) -> String {
    format!("/bss-rating/v1/inbox/{entry_id}/release")
}

/// What a failed release must leave as it found: the entry still quarantined, its exception still
/// open and no audit row.
async fn assert_untouched(db: &Db, entry: &InboxEntry) {
    let conn = db.conn().unwrap();
    let scope = AccessScope::for_tenant(entry.key().tenant_id());
    let stored = inbox_repo::find_by_id(&conn, &scope, entry.entry_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.state(), InboxState::Quarantined);
    let reason = entry.quarantine().unwrap().reason().as_str();
    let key = exception_key(
        entry.key().tenant_id(),
        SubjectKind::Inbox,
        entry.entry_id().to_string(),
        reason,
    );
    assert!(
        exception_repo::find_open(&conn, &scope, &key)
            .await
            .unwrap()
            .is_some(),
        "the exception stays open"
    );
    let audit =
        audit_repo::list_for_target(&conn, &scope, "inbox_entry", &entry.entry_id().to_string())
            .await
            .unwrap();
    assert!(audit.is_empty(), "nothing is audited: {audit:?}");
}

#[tokio::test]
async fn release_with_apply_applies_and_audits() {
    let f = fixture().await;
    let (_, conflict) = quarantined(&f).await;
    let operator = user(TENANT);
    let (status, body) = post(
        &router(&f),
        &path(conflict.entry_id()),
        Some(&operator),
        &json!({"reason": "producer confirmed the corrected payload", "resolution": "apply"}),
        Some("req-42"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "applied");
    assert_eq!(body["entry_id"], conflict.entry_id().to_string());
    assert_eq!(body["quarantine_reason"], "digest_conflict");
    assert!(
        body.get("payload").is_none(),
        "the view never carries the payload"
    );
    assert!(f.applier.applied().contains(&conflict.entry_id()));

    let conn = f.db().conn().unwrap();
    let scope = AccessScope::for_tenant(TENANT);
    let audit = audit_repo::list_for_target(
        &conn,
        &scope,
        "inbox_entry",
        &conflict.entry_id().to_string(),
    )
    .await
    .unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].actor, operator.subject_id());
    assert_eq!(audit[0].operation_id, "bss_rating.inbox.release");
    assert_eq!(audit[0].request_id.as_deref(), Some("req-42"));
    assert_eq!(audit[0].reason, "producer confirmed the corrected payload");

    // The entry's exception is resolved by the release.
    let key = exception_key(
        TENANT,
        SubjectKind::Inbox,
        conflict.entry_id().to_string(),
        "digest_conflict",
    );
    assert!(
        exception_repo::find_open(&conn, &scope, &key)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn the_published_path_template_is_the_path_the_router_accepts() {
    let f = fixture().await;
    let (_, conflict) = quarantined(&f).await;
    let openapi = OpenApiRegistryImpl::new();
    let router = router_with(&f, &openapi);
    let templates: Vec<String> = openapi
        .operation_specs
        .iter()
        .map(|spec| spec.value().path.clone())
        .collect();
    assert_eq!(templates, vec![RELEASE_PATH.to_owned()]);
    // A generated client fills `{id}` with the entry id and sends nothing else.
    let concrete = RELEASE_PATH.replace("{id}", &conflict.entry_id().to_string());
    let (status, body) = post(
        &router,
        &concrete,
        Some(&user(TENANT)),
        &json!({"reason": "checked upstream", "resolution": "discard"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "discarded");
}

#[tokio::test]
async fn release_with_discard_keeps_the_entry_unapplied() {
    let f = fixture().await;
    let (_, conflict) = quarantined(&f).await;
    let (status, body) = post(
        &router(&f),
        &path(conflict.entry_id()),
        Some(&user(TENANT)),
        &json!({"reason": "stale replay", "resolution": "discard"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "discarded");
    assert!(body.get("applied_at").is_none());
    assert!(!f.applier.applied().contains(&conflict.entry_id()));
    let conn = f.db().conn().unwrap();
    let stored =
        inbox_repo::find_by_id(&conn, &AccessScope::for_tenant(TENANT), conflict.entry_id())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(stored.state(), InboxState::Discarded);
    assert_eq!(*stored.payload(), json!({"q": "2"}), "the payload is kept");
}

#[tokio::test]
async fn releasing_an_entry_that_is_not_quarantined_is_failed_precondition() {
    let f = fixture().await;
    let (applied, _) = quarantined(&f).await;
    let (status, body) = post(
        &router(&f),
        &path(applied.entry_id()),
        Some(&user(TENANT)),
        &json!({"reason": "r", "resolution": "discard"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("INBOX_ENTRY_NOT_QUARANTINED"),
        "{body}"
    );
}

#[tokio::test]
async fn applying_an_entry_that_was_never_quarantined_is_refused() {
    // The entry is already in the state an `apply` ends in, but it was never quarantined: the
    // replay shortcut must not answer 200 for it.
    let f = fixture().await;
    let (applied, _) = quarantined(&f).await;
    let (status, body) = post(
        &router(&f),
        &path(applied.entry_id()),
        Some(&user(TENANT)),
        &json!({"reason": "r", "resolution": "apply"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("INBOX_ENTRY_NOT_QUARANTINED"),
        "{body}"
    );
    let conn = f.db().conn().unwrap();
    let audit = audit_repo::list_for_target(
        &conn,
        &AccessScope::for_tenant(TENANT),
        "inbox_entry",
        &applied.entry_id().to_string(),
    )
    .await
    .unwrap();
    assert!(audit.is_empty());
}

#[tokio::test]
async fn applying_twice_applies_once_and_audits_once() {
    let f = fixture().await;
    let (applied, conflict) = quarantined(&f).await;
    let router = router(&f);
    let body = json!({"reason": "producer confirmed", "resolution": "apply"});
    for _ in 0..2 {
        let (status, view) = post(
            &router,
            &path(conflict.entry_id()),
            Some(&user(TENANT)),
            &body,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["state"], "applied");
    }
    assert_eq!(
        f.applier.applied(),
        vec![applied.entry_id(), conflict.entry_id()],
        "the conflict is applied once"
    );
    let conn = f.db().conn().unwrap();
    let audit = audit_repo::list_for_target(
        &conn,
        &AccessScope::for_tenant(TENANT),
        "inbox_entry",
        &conflict.entry_id().to_string(),
    )
    .await
    .unwrap();
    assert_eq!(audit.len(), 1);
}

#[tokio::test]
async fn a_reason_is_one_to_a_thousand_characters_without_control_characters() {
    let f = fixture().await;
    let (_, conflict) = quarantined(&f).await;
    let router = router(&f);
    for reason in [
        "   ".to_owned(),
        "x".repeat(1001),
        "line one\nline two".to_owned(),
        "nul\u{0}byte".to_owned(),
    ] {
        let (status, body) = post(
            &router,
            &path(conflict.entry_id()),
            Some(&user(TENANT)),
            &json!({"reason": reason, "resolution": "discard"}),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{reason:?}: {body}");
        assert_eq!(
            body["context"]["field_violations"][0]["field"], "reason",
            "{body}"
        );
    }
    // 1000 characters, multibyte ones included, are accepted, and the reason is stored trimmed.
    let longest = format!("  {}  ", "\u{e9}".repeat(1000));
    let (status, body) = post(
        &router,
        &path(conflict.entry_id()),
        Some(&user(TENANT)),
        &json!({"reason": longest, "resolution": "discard"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let conn = f.db().conn().unwrap();
    let audit = audit_repo::list_for_target(
        &conn,
        &AccessScope::for_tenant(TENANT),
        "inbox_entry",
        &conflict.entry_id().to_string(),
    )
    .await
    .unwrap();
    assert_eq!(audit[0].reason, "\u{e9}".repeat(1000));
}

#[tokio::test]
async fn a_release_is_naturally_idempotent_on_its_target_state() {
    let f = fixture().await;
    let (_, conflict) = quarantined(&f).await;
    let router = router(&f);
    let body = json!({"reason": "r", "resolution": "discard"});
    let (first, _) = post(
        &router,
        &path(conflict.entry_id()),
        Some(&user(TENANT)),
        &body,
        None,
    )
    .await;
    let (again, view) = post(
        &router,
        &path(conflict.entry_id()),
        Some(&user(TENANT)),
        &body,
        None,
    )
    .await;
    assert_eq!(first, StatusCode::OK);
    assert_eq!(again, StatusCode::OK);
    assert_eq!(view["state"], "discarded");
    let conn = f.db().conn().unwrap();
    let audit = audit_repo::list_for_target(
        &conn,
        &AccessScope::for_tenant(TENANT),
        "inbox_entry",
        &conflict.entry_id().to_string(),
    )
    .await
    .unwrap();
    assert_eq!(audit.len(), 1, "a repeated release changes nothing");

    // The other resolution is refused: the entry is no longer quarantined.
    let (other, _) = post(
        &router,
        &path(conflict.entry_id()),
        Some(&user(TENANT)),
        &json!({"reason": "r", "resolution": "apply"}),
        None,
    )
    .await;
    assert_eq!(other, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn another_tenants_entry_is_not_found() {
    // V23 for inbox entries: tenant B's id requested by tenant A is indistinguishable from absent.
    let f = fixture().await;
    let (_, conflict) = quarantined(&f).await;
    let other_tenant = Uuid::from_u128(0xB);
    let (status, body) = post(
        &router(&f),
        &path(conflict.entry_id()),
        Some(&user(other_tenant)),
        &json!({"reason": "r", "resolution": "apply"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (absent, absent_body) = post(
        &router(&f),
        &path(Uuid::now_v7()),
        Some(&user(other_tenant)),
        &json!({"reason": "r", "resolution": "apply"}),
        None,
    )
    .await;
    assert_eq!(absent, StatusCode::NOT_FOUND);
    assert_eq!(body["detail"], absent_body["detail"]);
    assert!(!f.applier.applied().contains(&conflict.entry_id()));
}

#[tokio::test]
async fn release_needs_usage_release() {
    let f = fixture().await;
    let (_, conflict) = quarantined(&f).await;
    f.pdp.deny(labels::USAGE, actions::RELEASE);
    let (status, _) = post(
        &router(&f),
        &path(conflict.entry_id()),
        Some(&user(TENANT)),
        &json!({"reason": "r", "resolution": "apply"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_pdp_outage_is_service_unavailable() {
    let f = fixture().await;
    let (_, conflict) = quarantined(&f).await;
    f.pdp.set_unavailable(true);
    let (status, _) = post(
        &router(&f),
        &path(conflict.entry_id()),
        Some(&user(TENANT)),
        &json!({"reason": "r", "resolution": "apply"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        f.metrics.samples().contains(&MetricSample::DependencyError(
            Dependency::Pdp,
            FailureCode::EvaluationFailed
        )),
        "the outage is counted"
    );
}

#[tokio::test]
async fn malformed_requests_are_invalid_argument() {
    let f = fixture().await;
    let (_, conflict) = quarantined(&f).await;
    let router = router(&f);
    let ctx = user(TENANT);
    for (path, body) in [
        (
            "/bss-rating/v1/inbox/not-a-uuid/release".to_owned(),
            json!({"reason": "r", "resolution": "apply"}),
        ),
        (
            path(conflict.entry_id()),
            json!({"reason": "", "resolution": "apply"}),
        ),
        (
            path(conflict.entry_id()),
            json!({"reason": "r", "resolution": "merge"}),
        ),
        (path(conflict.entry_id()), json!({"reason": "r"})),
        (
            path(conflict.entry_id()),
            json!({"reason": "r", "resolution": "apply", "extra": 1}),
        ),
    ] {
        // A body that is JSON but not the DTO is the platform extractor's 422; every refusal is
        // the canonical InvalidArgument.
        let (status, response) = post(&router, &path, Some(&ctx), &body, None).await;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
            "{path} {body}: {response}"
        );
        assert!(
            response["type"]
                .as_str()
                .is_some_and(|t| t.contains("invalid_argument")),
            "{response}"
        );
    }
}

#[tokio::test]
async fn a_request_without_a_security_context_is_refused_by_the_extractor() {
    // The gateway authenticates `.authenticated()` routes and injects the context; a request that
    // reaches the handler without one is a wiring fault the extractor refuses, and nothing moves.
    let f = fixture().await;
    let (_, conflict) = quarantined(&f).await;
    let (status, _) = post(
        &router(&f),
        &path(conflict.entry_id()),
        None,
        &json!({"reason": "r", "resolution": "discard"}),
        None,
    )
    .await;
    assert!(!status.is_success(), "{status}");
    assert_untouched(f.db(), &conflict).await;
}

#[tokio::test]
async fn applying_an_entry_whose_source_no_feature_applies_changes_nothing() {
    let f = fixture().await;
    // No feature applies `segment` yet; an unsupported schema quarantines one all the same.
    let outcome = f
        .inbox
        .accept_one(unsupported_fact_of(
            InboxSource::Segment,
            TENANT,
            "seg-1",
            json!({"schema": "v99"}),
        ))
        .await
        .unwrap();
    let AcceptOutcome::Quarantined(entry) = outcome else {
        panic!("an unsupported schema is quarantined: {outcome:?}");
    };
    let (status, body) = post(
        &router(&f),
        &path(entry.entry_id()),
        Some(&user(TENANT)),
        &json!({"reason": "r", "resolution": "apply"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["context"]["violations"][0]["type"], "INBOX_SOURCE_NOT_APPLICABLE",
        "{body}"
    );
    assert_untouched(f.db(), &entry).await;
}

#[tokio::test]
async fn an_apply_the_owning_feature_refuses_rolls_the_release_back() {
    let f = fixture().await;
    let (_, conflict) = quarantined(&f).await;
    f.applier.fail();
    let (status, body) = post(
        &router(&f),
        &path(conflict.entry_id()),
        Some(&user(TENANT)),
        &json!({"reason": "r", "resolution": "apply"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_untouched(f.db(), &conflict).await;
}

#[tokio::test]
async fn a_release_that_loses_the_state_cas_is_concurrent_and_rolls_everything_back() {
    // Between the release's read and its conditional write the entry leaves `quarantined` (here,
    // inside the applier, as a concurrent release would): the write matches nothing, and the
    // applier's effect and the audit row roll back with it.
    let f = fixture().await;
    let (_, conflict) = quarantined(&f).await;
    f.applier.discard_first();
    let (status, body) = post(
        &router(&f),
        &path(conflict.entry_id()),
        Some(&user(TENANT)),
        &json!({"reason": "r", "resolution": "apply"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["context"]["reason"], "CONCURRENT_MODIFICATION",
        "{body}"
    );
    assert_untouched(f.db(), &conflict).await;
}

#[tokio::test]
async fn the_audit_row_digests_the_entry_the_trimmed_reason_and_the_resolution() {
    let f = fixture().await;
    let (_, conflict) = quarantined(&f).await;
    let (status, body) = post(
        &router(&f),
        &path(conflict.entry_id()),
        Some(&user(TENANT)),
        &json!({"reason": "  checked upstream  ", "resolution": "discard"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let conn = f.db().conn().unwrap();
    let audit = audit_repo::list_for_target(
        &conn,
        &AccessScope::for_tenant(TENANT),
        "inbox_entry",
        &conflict.entry_id().to_string(),
    )
    .await
    .unwrap();
    let expected = Sha256Hex::of_json(&json!({
        "entry_id": conflict.entry_id(),
        "reason": "checked upstream",
        "resolution": "discard",
    }));
    assert_eq!(audit[0].request_digest, expected.as_str());
    // Another resolution is another request.
    let apply = Sha256Hex::of_json(&json!({
        "entry_id": conflict.entry_id(),
        "reason": "checked upstream",
        "resolution": "apply",
    }));
    assert_ne!(audit[0].request_digest, apply.as_str());
}

#[tokio::test]
async fn a_stale_writer_loses_the_state_cas() {
    // Two operators read the quarantined entry; the first release wins, and the second writer's
    // conditional update matches nothing, so it cannot apply or audit a second time.
    let f = fixture().await;
    let (_, conflict) = quarantined(&f).await;
    let scope = AccessScope::for_tenant(TENANT);
    let conn = f.db().conn().unwrap();
    let stale = inbox_repo::find_by_id(&conn, &scope, conflict.entry_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stale.state(), InboxState::Quarantined);

    let (status, _) = post(
        &router(&f),
        &path(conflict.entry_id()),
        Some(&user(TENANT)),
        &json!({"reason": "r", "resolution": "discard"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let moved = inbox_repo::transition(
        &conn,
        &scope,
        stale.entry_id(),
        InboxTransition::ApplyQuarantined {
            at: stale.received_at(),
        },
    )
    .await
    .unwrap();
    assert!(!moved, "the stale writer's CAS must fail");
    let stored = inbox_repo::find_by_id(&conn, &scope, conflict.entry_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.state(), InboxState::Discarded);
}
