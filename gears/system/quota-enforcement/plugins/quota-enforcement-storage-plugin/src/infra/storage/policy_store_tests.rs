#![allow(clippy::expect_used)]
use super::*;
use crate::QeOutbox;
use crate::test_support::{FailingEnqueuer, bound_outbox, enqueued_messages, test_db};
use quota_enforcement_sdk::PolicySchemaSnapshot;
use quota_enforcement_sdk::testing::test_metric;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use toolkit_db::secure::SecureUpdateExt;

fn context() -> SecurityContext {
    SecurityContext::anonymous()
}
fn draft() -> PolicyDraft {
    PolicyDraft {
        scope: PolicyScope::Metric {
            metric: test_metric(),
        },
        engine_id: "most-restrictive-wins".into(),
        engine_config: serde_json::json!({}),
        timeout_ms: Some(100),
        description: None,
        comment: Some("creation".into()),
        created_by: "ignored".into(),
        schema_snapshot: PolicySchemaSnapshot::default(),
    }
}
fn patch(version: u32) -> PolicyUpdate {
    PolicyUpdate {
        if_match_version: version,
        engine_id: None,
        engine_config: None,
        timeout_ms: None,
        comment: None,
        created_by: "ignored".into(),
        schema_snapshot: None,
    }
}

/// A `policy-changed` event, optionally already naming its policy.
fn policy_changed(policy_id: Option<PolicyId>) -> NotificationEvent {
    NotificationEvent {
        event_id: quota_enforcement_sdk::EventId::generate(),
        kind: quota_enforcement_sdk::NotificationEventKind::PolicyChanged,
        scope: quota_enforcement_sdk::NotificationScope::Platform,
        quota_id: None,
        policy_id,
        subject: None,
        payload: serde_json::json!({ "change_kind": "created" }),
        emitted_at: OffsetDateTime::now_utc(),
    }
}

#[tokio::test]
async fn versions_rollback_recreate_and_noops_preserve_history() {
    let db = test_db().await;
    let (handle, outbox) = bound_outbox(&db).await;
    let store = SqlPolicyStore::new(db, outbox);
    let first = store
        .create_policy(&context(), draft(), &[])
        .await
        .expect("create");
    let id = first.policy_id.clone();
    assert_eq!(first.timeout_ms, Some(100));
    assert_eq!(first.created_by, context().subject_id().to_string());
    assert!(matches!(
        store.create_policy(&context(), draft(), &[]).await,
        Err(StorageError::PolicyScopeOccupied { .. })
    ));
    store
        .update_policy(&context(), id.clone(), patch(1), &[])
        .await
        .expect("v2");
    store
        .update_policy(&context(), id.clone(), patch(2), &[])
        .await
        .expect("v3");
    let back = store
        .rollback_policy(&context(), id.clone(), 1, Some("rollback".into()), &[])
        .await
        .expect("rollback");
    assert!(back.is_applied());
    assert_eq!(back.into_inner().comment.as_deref(), Some("creation"));
    assert!(
        !store
            .rollback_policy(&context(), id.clone(), 1, None, &[])
            .await
            .expect("replay")
            .is_applied(),
        "rolling back onto the already-active target changes nothing"
    );
    assert_eq!(
        store
            .update_policy(&context(), id.clone(), patch(1), &[])
            .await
            .expect("v4")
            .version,
        4
    );
    assert!(
        store
            .delete_policy(&context(), id.clone(), None, &[])
            .await
            .expect("delete")
            .is_applied()
    );
    assert!(
        !store
            .delete_policy(&context(), id.clone(), None, &[])
            .await
            .expect("replay")
            .is_applied(),
        "a repeated delete writes no second audit row or event"
    );
    let replacement = store
        .create_policy(&context(), draft(), &[])
        .await
        .expect("recreate");
    assert_ne!(replacement.policy_id, id);
    assert_eq!(
        store
            .read_policy_version(&id, 4)
            .await
            .expect("read")
            .expect("retained")
            .state,
        PolicyVersionState::Deleted
    );
    assert_eq!(
        store
            .delete_policy(&context(), PolicyId::global(), None, &[])
            .await,
        Err(StorageError::CannotDeleteSeededGlobalPolicy)
    );
    handle.stop().await;
}

#[tokio::test]
async fn enqueue_failure_rolls_back_policy_header_and_version() {
    let db = test_db().await;
    let failed = SqlPolicyStore::new(db.clone(), Arc::new(FailingEnqueuer));
    assert!(
        failed
            .create_policy(&context(), draft(), &[])
            .await
            .is_err()
    );
    let conn = db.conn().expect("connection");
    assert!(
        repo::at_scope(&conn, &scope_key(&draft().scope))
            .await
            .expect("read")
            .is_none()
    );
}

#[tokio::test]
async fn scope_and_id_reads_resolve_the_active_version_and_never_fall_back() {
    let db = test_db().await;
    let (_handle, outbox) = bound_outbox(&db).await;
    let store = SqlPolicyStore::new(db, outbox);
    let metric_scope = PolicyScope::Metric {
        metric: test_metric(),
    };

    // An unoccupied scope is absence, not an error.
    assert!(
        store
            .read_policy(&metric_scope)
            .await
            .expect("empty scope")
            .is_none()
    );
    assert!(
        store
            .read_active_policy_by_id(&PolicyId::new("never-created"))
            .await
            .expect("unknown id")
            .is_none()
    );
    assert!(
        store
            .read_active_policies()
            .await
            .expect("empty")
            .is_empty()
    );

    let global = store
        .create_policy(
            &context(),
            PolicyDraft {
                scope: PolicyScope::Global,
                ..draft()
            },
            &[],
        )
        .await
        .expect("seed global");
    let metric = store
        .create_policy(&context(), draft(), &[])
        .await
        .expect("create metric");

    assert_eq!(
        store
            .read_policy(&metric_scope)
            .await
            .expect("read")
            .map(|v| v.policy_id),
        Some(metric.policy_id.clone())
    );
    assert_eq!(
        store.read_active_policies().await.expect("active").len(),
        2,
        "both the global and the metric policy are active"
    );

    // Deleting the metric policy clears its pointer. The exact-scope read must
    // report absence rather than substituting `global`: the fallback is a
    // selection decision taken inside the evaluation transaction, and callers
    // also use this read to ask whether a scope is occupied.
    assert!(
        store
            .delete_policy(&context(), metric.policy_id.clone(), None, &[])
            .await
            .expect("delete")
            .is_applied()
    );
    assert!(
        store
            .read_policy(&metric_scope)
            .await
            .expect("read")
            .is_none(),
        "an exact-scope read never falls back to the global policy"
    );
    assert!(
        store
            .read_active_policy_by_id(&metric.policy_id)
            .await
            .expect("read")
            .is_none(),
        "a deleted policy has no active version"
    );
    assert_eq!(
        store
            .read_active_policies()
            .await
            .expect("active")
            .into_iter()
            .map(|v| v.policy_id)
            .collect::<Vec<_>>(),
        vec![global.policy_id]
    );
}

#[tokio::test]
async fn history_pages_in_version_order_and_refuses_a_foreign_cursor() {
    let db = test_db().await;
    let (_handle, outbox) = bound_outbox(&db).await;
    let store = SqlPolicyStore::new(db, outbox);
    let created = store
        .create_policy(&context(), draft(), &[])
        .await
        .expect("create");
    let id = created.policy_id.clone();
    for version in 1..4 {
        store
            .update_policy(&context(), id.clone(), patch(version), &[])
            .await
            .expect("update");
    }

    let first = store
        .list_policy_versions(&id, PageRequest::first(2))
        .await
        .expect("page one");
    assert_eq!(
        first.items.iter().map(|m| m.version).collect::<Vec<_>>(),
        vec![1, 2]
    );
    let cursor = first.next_cursor.expect("more versions follow");
    let second = store
        .list_policy_versions(
            &id,
            PageRequest {
                limit: 2,
                cursor: Some(cursor),
            },
        )
        .await
        .expect("page two");
    assert_eq!(
        second.items.iter().map(|m| m.version).collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert!(second.next_cursor.is_none());
    assert_eq!(
        second.items.last().map(|m| m.state),
        Some(PolicyVersionState::Active)
    );

    // A cursor this listing did not issue is a caller-input error, never a
    // silent restart at page one. The quota listing's cursor is a 16-byte
    // UUIDv7, so it fails the width check here.
    assert!(matches!(
        store
            .list_policy_versions(
                &id,
                PageRequest {
                    limit: 2,
                    cursor: Some(cursor::encode(quota_enforcement_sdk::QuotaId::generate())),
                },
            )
            .await,
        Err(StorageError::InvalidCursor)
    ));
    assert!(matches!(
        store
            .list_policy_versions(&PolicyId::new("never-created"), PageRequest::first(10))
            .await,
        Err(StorageError::PolicyNotFound { .. })
    ));
}

#[tokio::test]
async fn a_creation_event_carries_the_generated_policy_id() {
    let db = test_db().await;
    let (_handle, outbox) = bound_outbox(&db).await;
    let store = SqlPolicyStore::new(db.clone(), outbox);
    // The caller composes the event before storage mints the id, so it has
    // nothing to put in `policy_id`.
    let created = store
        .create_policy(&context(), draft(), &[policy_changed(None)])
        .await
        .expect("create");

    let messages = enqueued_messages(&db).await;
    assert_eq!(messages.len(), 1);
    let enqueued: NotificationEvent =
        serde_json::from_slice(&messages[0].payload).expect("event payload");
    assert_eq!(
        enqueued.policy_id.as_ref(),
        Some(&created.policy_id),
        "storage fills the id it generated"
    );

    // A stale caller-supplied ID cannot misattribute a policy creation.
    let other = PolicyId::new("caller-supplied");
    store
        .create_policy(
            &context(),
            PolicyDraft {
                scope: PolicyScope::Global,
                ..draft()
            },
            &[policy_changed(Some(other.clone()))],
        )
        .await
        .expect("create global");
    let messages = enqueued_messages(&db).await;
    let enqueued: NotificationEvent =
        serde_json::from_slice(&messages[1].payload).expect("event payload");
    assert_eq!(enqueued.policy_id, Some(PolicyId::global()));
}

#[tokio::test]
async fn an_active_read_never_reports_a_retired_version() {
    let db = test_db().await;
    let (_handle, outbox) = bound_outbox(&db).await;
    let store = SqlPolicyStore::new(db.clone(), outbox);
    let created = store
        .create_policy(&context(), draft(), &[])
        .await
        .expect("create");
    let id = created.policy_id.clone();
    let scope = PolicyScope::Metric {
        metric: test_metric(),
    };

    store
        .update_policy(&context(), id.clone(), patch(1), &[])
        .await
        .expect("v2");

    // Every active read must agree with the version states, not with a pointer
    // value read in an earlier statement.
    for found in [
        store.read_policy(&scope).await.expect("by scope"),
        store.read_active_policy_by_id(&id).await.expect("by id"),
        store
            .read_active_policies()
            .await
            .expect("scan")
            .into_iter()
            .next(),
    ] {
        let found = found.expect("an active version exists");
        assert_eq!(found.version, 2);
        assert_eq!(found.state, PolicyVersionState::Active);
    }

    // Corrupt the pointer so it names the superseded version. A read that
    // followed the pointer would hand back version 1 as active; a read that
    // asks for `state = 'active'` cannot.
    let conn = db.conn().expect("connection");
    repo::set_pointer(&conn, id.as_str(), Some(1), 2)
        .await
        .expect("move the pointer behind the states");
    assert_eq!(
        store
            .read_active_policy_by_id(&id)
            .await
            .expect("by id")
            .map(|v| v.version),
        Some(2),
        "the version states decide, not the header pointer"
    );
    assert_eq!(
        store
            .read_active_policies()
            .await
            .expect("scan")
            .into_iter()
            .map(|v| v.version)
            .collect::<Vec<_>>(),
        vec![2]
    );
}

#[tokio::test]
async fn an_outage_is_unavailable_and_a_corrupt_payload_is_internal() {
    let db = test_db().await;
    let (_handle, _outbox) = bound_outbox(&db).await;

    // An unbound outbox cannot accept the event, so the transition rolls back.
    // That is a reachability failure the gear lifts to 503, not a 500.
    let unbound = SqlPolicyStore::new(db.clone(), Arc::new(QeOutbox::new()));
    let error = unbound
        .create_policy(&context(), draft(), &[policy_changed(None)])
        .await
        .expect_err("an unbound outbox rolls the transition back");
    assert!(
        matches!(error, StorageError::Unavailable(_)),
        "expected an outage, got {error:?}"
    );

    let failing = SqlPolicyStore::new(db.clone(), Arc::new(FailingEnqueuer));
    let error = failing
        .create_policy(&context(), draft(), &[policy_changed(None)])
        .await
        .expect_err("a payload the outbox cannot serialize");
    assert!(
        matches!(error, StorageError::Internal(_)),
        "a serialization failure will not fix itself, got {error:?}"
    );

    // A payload that does not decode is inconsistent state, never an outage.
    let (_handle, outbox) = bound_outbox(&db).await;
    let store = SqlPolicyStore::new(db.clone(), outbox);
    let created = store
        .create_policy(&context(), draft(), &[])
        .await
        .expect("create");
    let conn = db.conn().expect("connection");
    policy_version::Entity::update_many()
        .col_expr(
            policy_version::Column::Payload,
            sea_orm::sea_query::Expr::value("not json"),
        )
        .filter(policy_version::Column::PolicyId.eq(created.policy_id.as_str()))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("corrupt the payload");
    let error = store
        .read_active_policy_by_id(&created.policy_id)
        .await
        .expect_err("a payload that does not decode");
    assert!(
        matches!(error, StorageError::Internal(_)),
        "expected inconsistent state, got {error:?}"
    );
}

#[tokio::test]
async fn scope_read_resolves_recreated_policy_without_old_history() {
    let db = test_db().await;
    let (handle, outbox) = bound_outbox(&db).await;
    let store = SqlPolicyStore::new(db, outbox);
    let scope = draft().scope;
    let original = store
        .create_policy(&context(), draft(), &[])
        .await
        .expect("create");
    store
        .delete_policy(&context(), original.policy_id.clone(), None, &[])
        .await
        .expect("delete");
    let replacement = store
        .create_policy(&context(), draft(), &[])
        .await
        .expect("recreate");
    let current = store
        .read_policy(&scope)
        .await
        .expect("scope read")
        .expect("present");
    assert_eq!(current.policy_id, replacement.policy_id);
    assert_ne!(current.policy_id, original.policy_id);
    assert_eq!(current.state, PolicyVersionState::Active);
    handle.stop().await;
}
