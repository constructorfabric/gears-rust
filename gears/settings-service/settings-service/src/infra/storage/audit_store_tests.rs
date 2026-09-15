// Created: 2026-09-07 by Constructor Tech
//! The store over an in-memory database: append, history, retention.

use std::sync::Arc;

use serde_json::json;
use time::{Duration, OffsetDateTime};
use toolkit_db::{DBProvider, DbError};
use toolkit_odata::ODataQuery;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::AuditStore;
use crate::audit::{AuditOperation, AuditRecord, AuditSink, AuditValue};
use crate::domain::error::DomainError;
use crate::test_support::sqlite_provider;

const KEY: &str = "gts.cf.core.settings.setting_type.v1~acme.settings.network.enable_proxy.v1~";

async fn db() -> Arc<DBProvider<DbError>> {
    sqlite_provider().await
}

fn record(tenant: Uuid, request: &str) -> AuditRecord {
    AuditRecord::new(KEY, Some(tenant), "admin", AuditOperation::Change, request)
        .with_pre_image(AuditValue::record(json!(false), false))
        .with_post_image(AuditValue::record(json!(true), false))
}

async fn history(
    db: &DBProvider<DbError>,
    tenant: Uuid,
    limit: Option<u64>,
    cursor: Option<toolkit_odata::CursorV1>,
) -> toolkit_odata::Page<crate::audit::StoredAuditRecord> {
    let conn = db.conn().expect("connection");
    AuditStore
        .history(
            &conn,
            &AccessScope::allow_all(),
            KEY,
            tenant,
            &ODataQuery {
                limit,
                cursor,
                ..ODataQuery::default()
            },
        )
        .await
        .expect("history reads")
}

#[tokio::test]
async fn appended_records_come_back_newest_first_for_their_pair_only() {
    let db = db().await;
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let conn = db.conn().expect("connection");
    for request in ["r1", "r2", "r3"] {
        AuditStore
            .append(&conn, &AccessScope::allow_all(), record(a, request))
            .await
            .expect("append");
    }
    AuditStore
        .append(&conn, &AccessScope::allow_all(), record(b, "other-tenant"))
        .await
        .expect("append");

    let page = history(&db, a, None, None).await;
    let requests: Vec<&str> = page.items.iter().map(|r| r.request_id.as_str()).collect();
    assert_eq!(requests, vec!["r3", "r2", "r1"], "newest first");
    assert!(page.items.iter().all(|r| r.tenant_id == Some(a)));
    assert_eq!(
        page.items[0].pre_image,
        Some(AuditValue::Clear(json!(false)))
    );
    assert_eq!(page.items[0].operation, AuditOperation::Change);

    let empty = history(&db, Uuid::new_v4(), None, None).await;
    assert!(
        empty.items.is_empty(),
        "no history is an empty page, not an error"
    );
}

#[tokio::test]
async fn a_second_page_follows_the_cursor_without_duplicates() {
    let db = db().await;
    let tenant = Uuid::new_v4();
    let conn = db.conn().expect("connection");
    for i in 0..5 {
        AuditStore
            .append(
                &conn,
                &AccessScope::allow_all(),
                record(tenant, &format!("r{i}")),
            )
            .await
            .expect("append");
    }
    let first = history(&db, tenant, Some(2), None).await;
    assert_eq!(first.items.len(), 2);
    let cursor = first.page_info.next_cursor.expect("more pages");
    let parsed = toolkit_odata::CursorV1::decode(&cursor).expect("cursor decodes");
    let second = history(&db, tenant, Some(2), Some(parsed)).await;
    assert_eq!(second.items.len(), 2);
    let mut seen: Vec<Uuid> = first
        .items
        .iter()
        .chain(second.items.iter())
        .map(|r| r.id)
        .collect();
    let before = seen.len();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), before, "no record appears on both pages");
}

#[tokio::test]
async fn a_secret_image_is_stored_masked_and_read_back_masked() {
    let db = db().await;
    let tenant = Uuid::new_v4();
    let conn = db.conn().expect("connection");
    let rec = AuditRecord::new(KEY, Some(tenant), "admin", AuditOperation::Change, "r")
        .with_post_image(AuditValue::record(json!("hunter2"), true));
    AuditStore
        .append(&conn, &AccessScope::allow_all(), rec)
        .await
        .expect("append");
    let page = history(&db, tenant, None, None).await;
    assert_eq!(page.items[0].post_image, Some(AuditValue::Masked));
}

#[tokio::test]
async fn pruning_removes_only_records_past_their_horizon() {
    let db = db().await;
    let tenant = Uuid::new_v4();
    let conn = db.conn().expect("connection");
    let now = OffsetDateTime::now_utc();
    // Explicit horizons: one behind us, one ahead.
    AuditStore
        .append(
            &conn,
            &AccessScope::allow_all(),
            record(tenant, "expired").with_retain_until(now - Duration::days(1)),
        )
        .await
        .expect("append");
    AuditStore
        .append(
            &conn,
            &AccessScope::allow_all(),
            record(tenant, "kept").with_retain_until(now + Duration::days(1)),
        )
        .await
        .expect("append");
    // No horizon: the default applies from when it was written, which is now.
    AuditStore
        .append(&conn, &AccessScope::allow_all(), record(tenant, "default"))
        .await
        .expect("append");

    let pruned = AuditStore
        .prune_expired(&conn, &AccessScope::allow_all(), now, Duration::days(365))
        .await
        .expect("prune");
    assert_eq!(pruned, 1);
    let left: Vec<String> = history(&db, tenant, None, None)
        .await
        .items
        .into_iter()
        .map(|r| r.request_id)
        .collect();
    assert_eq!(left.len(), 2);
    assert!(left.contains(&"kept".to_owned()) && left.contains(&"default".to_owned()));

    // Far enough in the future the default horizon has passed too.
    let pruned = AuditStore
        .prune_expired(
            &conn,
            &AccessScope::allow_all(),
            now + Duration::days(400),
            Duration::days(365),
        )
        .await
        .expect("prune");
    assert_eq!(pruned, 2);
}

#[tokio::test]
async fn a_failed_append_is_unavailability() {
    // A scope that denies everything cannot write a row; the sink reports it
    // as unavailability for the caller to roll back on.
    let db = db().await;
    let conn = db.conn().expect("connection");
    let err = AuditStore
        .append(&conn, &AccessScope::deny_all(), record(Uuid::new_v4(), "r"))
        .await
        .expect_err("refused");
    assert!(matches!(err, DomainError::Unavailable { .. }), "{err:?}");
}

#[tokio::test]
async fn records_of_one_change_set_are_retrievable_together() {
    let db = db().await;
    let conn = db.conn().expect("connection");
    let change_set = Uuid::new_v4();
    for tenant in [Uuid::new_v4(), Uuid::new_v4()] {
        AuditStore
            .append(
                &conn,
                &AccessScope::allow_all(),
                record(tenant, "batch").with_change_set(change_set),
            )
            .await
            .expect("append");
    }
    AuditStore
        .append(
            &conn,
            &AccessScope::allow_all(),
            record(Uuid::new_v4(), "alone"),
        )
        .await
        .expect("append");
    let together = AuditStore
        .by_change_set(&conn, &AccessScope::allow_all(), change_set)
        .await
        .expect("by change set");
    assert_eq!(together.len(), 2);
    assert!(together.iter().all(|r| r.change_set_id == Some(change_set)));
}
