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
        .with_pre_image(AuditValue::record(
            json!(false),
            crate::audit::ImageClass::Public,
        ))
        .with_post_image(AuditValue::record(
            json!(true),
            crate::audit::ImageClass::Public,
        ))
}

async fn history(
    db: &DBProvider<DbError>,
    tenant: Uuid,
    limit: Option<u64>,
    cursor: Option<toolkit_odata::CursorV1>,
) -> crate::domain::odata::Listing<crate::audit::StoredAuditRecord> {
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
async fn a_record_stored_under_a_minted_id_carries_its_change_ref_and_detail() {
    // An activation state names the change's record by the id the mutation
    // minted before the append, and an `activation` record answers a change by
    // its `change_ref` with the consumer's detail: both come back as written,
    // and an ordinary record carries neither.
    let db = db().await;
    let tenant = Uuid::new_v4();
    let (minted, change) = (Uuid::new_v4(), Uuid::new_v4());
    let conn = db.conn().expect("connection");
    AuditStore
        .append(&conn, &AccessScope::allow_all(), record(tenant, "plain"))
        .await
        .expect("append");
    AuditStore
        .append(
            &conn,
            &AccessScope::allow_all(),
            record(tenant, "minted").with_record_id(minted),
        )
        .await
        .expect("append");
    AuditStore
        .append(
            &conn,
            &AccessScope::allow_all(),
            AuditRecord::new(
                KEY,
                Some(tenant),
                "gear",
                AuditOperation::Activation,
                "report",
            )
            .by_module()
            .failed()
            .with_change_ref(change)
            .with_detail("could not apply"),
        )
        .await
        .expect("append");

    let page = history(&db, tenant, None, None).await;
    let by_request = |request: &str| {
        page.items
            .iter()
            .find(|r| r.request_id == request)
            .expect("the record is in the history")
    };
    assert_eq!(
        by_request("minted").id,
        minted,
        "stored under the minted id"
    );
    assert_eq!(by_request("plain").change_ref, None);
    assert_eq!(by_request("plain").detail, None);
    let report = by_request("report");
    assert_eq!(report.operation, AuditOperation::Activation);
    assert_eq!(report.change_ref, Some(change));
    assert_eq!(report.detail.as_deref(), Some("could not apply"));
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
    let rec =
        AuditRecord::new(KEY, Some(tenant), "admin", AuditOperation::Change, "r").with_post_image(
            AuditValue::record(json!("hunter2"), crate::audit::ImageClass::Secret),
        );
    AuditStore
        .append(&conn, &AccessScope::allow_all(), rec)
        .await
        .expect("append");
    let page = history(&db, tenant, None, None).await;
    assert_eq!(page.items[0].post_image, Some(AuditValue::Masked));
}

#[tokio::test]
async fn a_pii_image_keeps_its_class_through_the_store() {
    // The class is inside the stored image, next to the `clear` and `masked`
    // kinds the column already held: no column of its own, no migration.
    let db = db().await;
    let tenant = Uuid::new_v4();
    let conn = db.conn().expect("connection");
    let rec = AuditRecord::new(KEY, Some(tenant), "admin", AuditOperation::Change, "r")
        .with_pre_image(AuditValue::record(
            json!("bob@example.com"),
            crate::audit::ImageClass::Pii,
        ))
        .with_post_image(AuditValue::record(
            json!("alice@example.com"),
            crate::audit::ImageClass::Pii,
        ));
    AuditStore
        .append(&conn, &AccessScope::allow_all(), rec)
        .await
        .expect("append");
    let page = history(&db, tenant, None, None).await;
    assert_eq!(
        page.items[0].pre_image,
        Some(AuditValue::Pii(json!("bob@example.com")))
    );
    assert_eq!(
        page.items[0].post_image,
        Some(AuditValue::Pii(json!("alice@example.com")))
    );
    assert!(page.items[0].has_pii_image());
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
        .prune_expired(
            &conn,
            &AccessScope::allow_all(),
            now,
            Duration::days(365),
            1_000,
        )
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
            1_000,
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

/// A stored row as the mapper sees it, with the given images.
fn stored_row(
    pre_value: Option<serde_json::Value>,
) -> crate::infra::storage::entity::audit_record::Model {
    crate::infra::storage::entity::audit_record::Model {
        id: Uuid::new_v4(),
        resource: format!("{KEY}#tenant"),
        declaration_key: KEY.to_owned(),
        tenant_id: Some(Uuid::new_v4()),
        operation: "change".to_owned(),
        actor: "admin".to_owned(),
        actor_classification: "public".to_owned(),
        pre_value,
        post_value: Some(json!({ "kind": "clear", "value": true })),
        outcome: "success".to_owned(),
        request_id: "req".to_owned(),
        change_set_id: None,
        change_ref: None,
        detail: None,
        occurred_at: OffsetDateTime::now_utc(),
        retain_until: None,
    }
}

#[test]
fn an_image_that_does_not_decode_is_an_integrity_error_not_an_absent_image() {
    // The record's enum fields already refuse a value they cannot read; an
    // image is evidence of what changed and gets the same treatment. Reading
    // it as "no image" would erase the trace of a change without a sign.
    let corrupt = super::to_domain(stored_row(Some(json!("garbage"))));
    assert!(
        matches!(corrupt, Err(DomainError::Internal { .. })),
        "{corrupt:?}"
    );

    let intact = super::to_domain(stored_row(Some(json!({ "kind": "clear", "value": false }))))
        .expect("an intact row maps");
    assert_eq!(
        intact.pre_image,
        Some(AuditValue::record(
            json!(false),
            crate::audit::ImageClass::Public
        ))
    );
    let absent = super::to_domain(stored_row(None)).expect("an absent image is absent");
    assert_eq!(absent.pre_image, None);
}

#[tokio::test]
async fn the_store_itself_refuses_to_rewrite_a_record() {
    // Append-only is a property of the table, not of call-site discipline: a
    // future code path — or a misused one — that issues an `UPDATE` is refused
    // by the trigger the migration carries, and the record reads back as it
    // was written.
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;

    use crate::infra::storage::entity::audit_record::{Column, Entity as AuditEntity};

    let db = db().await;
    let tenant = Uuid::new_v4();
    let conn = db.conn().expect("connection");
    AuditStore
        .append(&conn, &AccessScope::allow_all(), record(tenant, "once"))
        .await
        .expect("append");
    let written = history(&db, tenant, None, None).await.items.remove(0);

    let tampered = AuditEntity::update_many()
        .col_expr(Column::Actor, Expr::value("someone else"))
        .filter(Column::Id.eq(written.id))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await;
    assert!(
        tampered.is_err(),
        "the store refuses the rewrite: {tampered:?}"
    );

    let read_back = history(&db, tenant, None, None).await.items.remove(0);
    assert_eq!(read_back.actor, "admin", "the record is as it was written");
}

#[tokio::test]
async fn a_cursor_minted_for_one_history_is_refused_on_another() {
    // A cursor carries a boundary in one result set. Applied to another
    // setting's or another scope's history it would silently skip or repeat
    // rows there, so it is refused as a cursor for a different query.
    let db = db().await;
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    {
        let conn = db.conn().expect("connection");
        for (tenant, request) in [(a, "a1"), (a, "a2"), (b, "b1"), (b, "b2")] {
            AuditStore
                .append(&conn, &AccessScope::allow_all(), record(tenant, request))
                .await
                .expect("append");
        }
    }
    let first = history(&db, a, Some(1), None).await;
    let cursor = toolkit_odata::CursorV1::decode(
        &first
            .page_info
            .next_cursor
            .expect("a second page for tenant a"),
    )
    .expect("cursor decodes");

    // Its own history continues with it.
    let second = history(&db, a, Some(1), Some(cursor.clone())).await;
    assert_eq!(second.items.len(), 1);

    // Another scope's history refuses it.
    let conn = db.conn().expect("connection");
    let foreign = AuditStore
        .history(
            &conn,
            &AccessScope::allow_all(),
            KEY,
            b,
            &ODataQuery {
                limit: Some(1),
                cursor: Some(cursor),
                ..ODataQuery::default()
            },
        )
        .await;
    assert!(
        matches!(foreign, Err(DomainError::Validation { .. })),
        "a cursor for tenant a's history is refused on tenant b's: {foreign:?}"
    );
}

#[tokio::test]
async fn a_prune_deletes_at_most_its_batch_and_only_what_is_expired() {
    // One bounded statement per call: a backlog is worked off over several,
    // each its own commit, and a record still held is never among them.
    let db = db().await;
    let tenant = Uuid::new_v4();
    let conn = db.conn().expect("connection");
    let now = OffsetDateTime::now_utc();
    for i in 0..5 {
        AuditStore
            .append(
                &conn,
                &AccessScope::allow_all(),
                record(tenant, &format!("old-{i}")),
            )
            .await
            .expect("append");
    }
    AuditStore
        .append(
            &conn,
            &AccessScope::allow_all(),
            record(tenant, "held").with_retain_until(now + Duration::days(500)),
        )
        .await
        .expect("append");

    let later = now + Duration::days(400);
    let mut passes = Vec::new();
    loop {
        let pruned = AuditStore
            .prune_expired(
                &conn,
                &AccessScope::allow_all(),
                later,
                Duration::days(365),
                2,
            )
            .await
            .expect("prune");
        passes.push(pruned);
        if pruned == 0 {
            break;
        }
    }
    assert_eq!(passes, vec![2, 2, 1, 0]);
    let left: Vec<String> = history(&db, tenant, None, None)
        .await
        .items
        .into_iter()
        .map(|r| r.request_id)
        .collect();
    assert_eq!(left, vec!["held".to_owned()]);
}

#[tokio::test]
async fn a_record_is_stamped_on_the_shared_microsecond_clock() {
    // Postgres keeps microseconds and SQLite keeps what it is given; every
    // other timestamp column is aligned so both backends hold the same instant,
    // and the history cursor, minted from `occurred_at`, is one of them.
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
    for item in history(&db, tenant, None, None).await.items {
        assert_eq!(
            item.occurred_at.nanosecond() % 1_000,
            0,
            "{}",
            item.occurred_at
        );
    }
}

#[test]
fn an_activation_record_with_an_unknown_outcome_is_an_integrity_error_never_a_guess() {
    // The database's CHECK refuses to store one, so the mapper is the only
    // place the rule can be pinned: a listed report and an attached report
    // both come through it, and a record the service cannot read fails the
    // read rather than showing a guessed outcome or vanishing from the page.
    let mut row = stored_row(None);
    row.operation = "activation".to_owned();
    row.change_ref = Some(Uuid::new_v4());
    row.outcome = "partial".to_owned();
    let refused = super::to_domain(row);
    assert!(
        matches!(refused, Err(DomainError::Internal { .. })),
        "{refused:?}"
    );
}

fn activation(tenant: Uuid, change: Uuid, subscriber: &str) -> AuditRecord {
    AuditRecord::new(
        KEY,
        Some(tenant),
        subscriber,
        AuditOperation::Activation,
        "req",
    )
    .by_module()
    .with_change_ref(change)
}

async fn append(db: &DBProvider<DbError>, record: AuditRecord) {
    let conn = db.conn().expect("connection");
    AuditStore
        .append(&conn, &AccessScope::allow_all(), record)
        .await
        .expect("append");
}

#[tokio::test]
async fn a_report_leaves_the_listing_of_the_history_holding_its_change_and_stays_in_others() {
    let db = db().await;
    let (x, d) = (Uuid::new_v4(), Uuid::new_v4());
    let change = Uuid::new_v4();
    append(&db, record(x, "r1").with_record_id(change)).await;
    append(&db, activation(x, change, "gear-a")).await;
    append(&db, activation(d, change, "gear-b")).await;
    // A report whose change record is not among this history's records.
    append(&db, activation(x, Uuid::new_v4(), "gear-c")).await;

    let at_x = history(&db, x, None, None).await;
    let listed: Vec<&str> = at_x.items.iter().map(|r| r.actor.as_str()).collect();
    assert_eq!(at_x.total_count, 2, "{listed:?}");
    assert!(
        listed.contains(&"admin") && listed.contains(&"gear-c"),
        "{listed:?}"
    );
    assert!(
        !listed.contains(&"gear-a"),
        "attached, never listed: {listed:?}"
    );

    // The descendant's history has no record of the change, so its report is
    // an ordinary record there.
    let at_d = history(&db, d, None, None).await;
    assert_eq!(at_d.total_count, 1);
    assert_eq!(at_d.items[0].actor, "gear-b");
    assert_eq!(at_d.items[0].change_ref, Some(change));
}

#[tokio::test]
async fn the_attachment_lookup_is_one_read_by_change_ref_across_tenants() {
    let db = db().await;
    let (x, d) = (Uuid::new_v4(), Uuid::new_v4());
    let (first, second, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    append(&db, activation(x, first, "gear-a")).await;
    append(&db, activation(d, first, "gear-b")).await;
    append(&db, activation(d, second, "gear-c")).await;
    append(&db, activation(d, other, "gear-d")).await;
    // Another key's report on the same change, and a record that is no report.
    append(
        &db,
        AuditRecord::new(
            "gts.cf.core.settings.setting_type.v1~acme.settings.network.other.v1~",
            Some(d),
            "gear-e",
            AuditOperation::Activation,
            "req",
        )
        .with_change_ref(first),
    )
    .await;
    append(&db, record(d, "r").with_record_id(Uuid::new_v4())).await;

    let conn = db.conn().expect("connection");
    let mut found: Vec<String> = AuditStore
        .reports_for(&conn, &AccessScope::allow_all(), KEY, &[first, second])
        .await
        .expect("lookup")
        .into_iter()
        .map(|r| r.actor)
        .collect();
    found.sort();
    assert_eq!(found, ["gear-a", "gear-b", "gear-c"]);
    assert!(
        AuditStore
            .reports_for(&conn, &AccessScope::allow_all(), KEY, &[])
            .await
            .expect("no ids, no read")
            .is_empty()
    );
}
