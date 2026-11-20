//! The store over an in-memory database: append, history, retention.

use std::sync::Arc;

use serde_json::json;
use time::{Duration, OffsetDateTime};
use toolkit_db::{DBProvider, DbError};
use toolkit_odata::ODataQuery;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::AuditStore;
use crate::audit::{ActorTenant, AuditOperation, AuditRecord, AuditSink, AuditValue};
use crate::domain::error::DomainError;
use crate::test_support::sqlite_provider;

const KEY: &str = "gts.cf.core.settings.setting_type.v1~acme.settings.network.enable_proxy.v1~";

async fn db() -> Arc<DBProvider<DbError>> {
    sqlite_provider().await
}

fn record(tenant: Uuid, request: &str) -> AuditRecord {
    AuditRecord::new(
        KEY,
        Some(tenant),
        "admin",
        ActorTenant::machine(),
        AuditOperation::Change,
        request,
    )
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
    AuditStore::history(
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
        AuditStore::default()
            .append(&conn, &AccessScope::allow_all(), record(a, request))
            .await
            .expect("append");
    }
    AuditStore::default()
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
        AuditStore::default()
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
    let rec = AuditRecord::new(
        KEY,
        Some(tenant),
        "admin",
        ActorTenant::machine(),
        AuditOperation::Change,
        "r",
    )
    .with_post_image(AuditValue::record(
        json!("hunter2"),
        crate::audit::ImageClass::Secret,
    ));
    AuditStore::default()
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
    let rec = AuditRecord::new(
        KEY,
        Some(tenant),
        "admin",
        ActorTenant::machine(),
        AuditOperation::Change,
        "r",
    )
    .with_pre_image(AuditValue::record(
        json!("bob@example.com"),
        crate::audit::ImageClass::Pii,
    ))
    .with_post_image(AuditValue::record(
        json!("alice@example.com"),
        crate::audit::ImageClass::Pii,
    ));
    AuditStore::default()
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
    AuditStore::default()
        .append(
            &conn,
            &AccessScope::allow_all(),
            record(tenant, "expired").with_retain_until(now - Duration::days(1)),
        )
        .await
        .expect("append");
    AuditStore::default()
        .append(
            &conn,
            &AccessScope::allow_all(),
            record(tenant, "kept").with_retain_until(now + Duration::days(1)),
        )
        .await
        .expect("append");
    // No horizon: the default applies from when it was written, which is now.
    AuditStore::default()
        .append(&conn, &AccessScope::allow_all(), record(tenant, "default"))
        .await
        .expect("append");

    let pruned = AuditStore::prune_expired(
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
    let pruned = AuditStore::prune_expired(
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
    let err = AuditStore::default()
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
        AuditStore::default()
            .append(
                &conn,
                &AccessScope::allow_all(),
                record(tenant, "batch").with_change_set(change_set),
            )
            .await
            .expect("append");
    }
    AuditStore::default()
        .append(
            &conn,
            &AccessScope::allow_all(),
            record(Uuid::new_v4(), "alone"),
        )
        .await
        .expect("append");
    let together = AuditStore::default()
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
        actor_tenant_id: None,
        pre_value,
        post_value: Some(json!({ "kind": "clear", "value": true })),
        outcome: "success".to_owned(),
        request_id: "req".to_owned(),
        change_set_id: None,
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
    AuditStore::default()
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
            AuditStore::default()
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
    let foreign = AuditStore::history(
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
        AuditStore::default()
            .append(
                &conn,
                &AccessScope::allow_all(),
                record(tenant, &format!("old-{i}")),
            )
            .await
            .expect("append");
    }
    AuditStore::default()
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
        let pruned = AuditStore::prune_expired(
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
        AuditStore::default()
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

// ---- `settings_audit_events_total`: counted at the sink's append.

fn counting_store() -> (
    AuditStore,
    Arc<crate::test_support::RecordingSecurityMetrics>,
) {
    let metrics = Arc::new(crate::test_support::RecordingSecurityMetrics::default());
    (AuditStore::new(Arc::clone(&metrics) as _), metrics)
}

#[tokio::test]
async fn a_mutation_record_counts_one_mutation_event() {
    let db = db().await;
    let conn = db.conn().expect("connection");
    let (store, metrics) = counting_store();
    for operation in [
        AuditOperation::Create,
        AuditOperation::Change,
        AuditOperation::Revert,
        AuditOperation::Remove,
        AuditOperation::Clone,
    ] {
        store
            .append(
                &conn,
                &AccessScope::allow_all(),
                AuditRecord::new(
                    KEY,
                    Some(Uuid::new_v4()),
                    "admin",
                    ActorTenant::machine(),
                    operation,
                    "req",
                ),
            )
            .await
            .expect("append");
    }
    assert_eq!(metrics.audit_events(), vec!["mutation"; 5]);
}

#[tokio::test]
async fn a_staged_record_counts_as_a_mutation() {
    let db = db().await;
    let conn = db.conn().expect("connection");
    let (store, metrics) = counting_store();
    store
        .append(
            &conn,
            &AccessScope::allow_all(),
            AuditRecord::new(
                KEY,
                Some(Uuid::new_v4()),
                "admin",
                ActorTenant::machine(),
                AuditOperation::Stage,
                "req",
            )
            .with_post_image(AuditValue::Masked),
        )
        .await
        .expect("append");
    assert_eq!(metrics.audit_events(), vec!["mutation"]);
}

#[tokio::test]
async fn a_secret_use_record_counts_one_secret_use_event() {
    let db = db().await;
    let conn = db.conn().expect("connection");
    let (store, metrics) = counting_store();
    store
        .append(
            &conn,
            &AccessScope::allow_all(),
            AuditRecord::new(
                KEY,
                Some(Uuid::new_v4()),
                "service",
                ActorTenant::machine(),
                AuditOperation::SecretUse,
                "req",
            ),
        )
        .await
        .expect("append");
    assert_eq!(metrics.audit_events(), vec!["secret_use"]);
}

#[tokio::test]
async fn a_failed_append_counts_nothing() {
    // A database without the audit table: the insert fails.
    let broken = crate::test_support::schemaless_provider().await;
    let conn = broken.conn().expect("connection");
    let (store, metrics) = counting_store();
    let err = store
        .append(
            &conn,
            &AccessScope::allow_all(),
            record(Uuid::new_v4(), "req"),
        )
        .await
        .expect_err("the insert fails");
    assert!(matches!(err, DomainError::Unavailable { .. }), "{err:?}");
    assert!(metrics.audit_events().is_empty());
}

// ── The history of a subtree ─────────────────────────────────────────────────

/// Append one record and let the shared microsecond clock move on.
///
/// Records written back to back can land in the same microsecond, and the id
/// then decides their order, which a test about "newest first" must not depend
/// on: the pause keeps every record of a test on an instant of its own. The
/// tie itself is covered by `records_of_one_instant_are_ordered_and_paged_by_id`.
async fn append(db: &DBProvider<DbError>, record: AuditRecord) {
    let conn = db.conn().expect("connection");
    AuditStore::default()
        .append(&conn, &AccessScope::allow_all(), record)
        .await
        .expect("append");
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
}

async fn try_history_subtree(
    db: &DBProvider<DbError>,
    target: Uuid,
    descendants: &[Uuid],
    limit: Option<u64>,
    cursor: Option<toolkit_odata::CursorV1>,
) -> Result<toolkit_odata::Page<crate::audit::StoredAuditRecord>, DomainError> {
    let conn = db.conn().expect("connection");
    AuditStore::history_subtree(
        &conn,
        &AccessScope::allow_all(),
        KEY,
        target,
        descendants,
        &ODataQuery {
            limit,
            cursor,
            ..ODataQuery::default()
        },
    )
    .await
}

async fn history_subtree(
    db: &DBProvider<DbError>,
    target: Uuid,
    descendants: &[Uuid],
    limit: Option<u64>,
    cursor: Option<toolkit_odata::CursorV1>,
) -> toolkit_odata::Page<crate::audit::StoredAuditRecord> {
    try_history_subtree(db, target, descendants, limit, cursor)
        .await
        .expect("the subtree history reads")
}

fn requests(page: &toolkit_odata::Page<crate::audit::StoredAuditRecord>) -> Vec<&str> {
    page.items.iter().map(|r| r.request_id.as_str()).collect()
}

fn next_of(
    page: &toolkit_odata::Page<crate::audit::StoredAuditRecord>,
) -> Option<toolkit_odata::CursorV1> {
    page.page_info
        .next_cursor
        .as_deref()
        .map(|c| toolkit_odata::CursorV1::decode(c).expect("cursor decodes"))
}

fn prev_of(
    page: &toolkit_odata::Page<crate::audit::StoredAuditRecord>,
) -> Option<toolkit_odata::CursorV1> {
    page.page_info
        .prev_cursor
        .as_deref()
        .map(|c| toolkit_odata::CursorV1::decode(c).expect("cursor decodes"))
}

#[tokio::test]
async fn a_subtree_history_holds_the_listed_tenants_and_the_definition_newest_first() {
    let db = db().await;
    let (target, child, grandchild, outside) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    // A definition record: no tenant, in every history of the setting.
    append(
        &db,
        AuditRecord::new(
            KEY,
            None,
            "module",
            ActorTenant::machine(),
            AuditOperation::Create,
            "definition",
        ),
    )
    .await;
    for (tenant, request) in [
        (target, "t1"),
        (child, "c1"),
        (outside, "o1"),
        (grandchild, "g1"),
        (target, "t2"),
        (outside, "o2"),
        (child, "c2"),
    ] {
        append(&db, record(tenant, request)).await;
    }
    // Another setting's record at a listed tenant is another history.
    append(
        &db,
        AuditRecord::new(
            "gts.cf.core.settings.setting_type.v1~acme.settings.other.v1~",
            Some(child),
            "admin",
            ActorTenant::machine(),
            AuditOperation::Change,
            "other-setting",
        ),
    )
    .await;

    let page = history_subtree(&db, target, &[child, grandchild], None, None).await;
    assert_eq!(
        requests(&page),
        vec!["c2", "t2", "g1", "c1", "t1", "definition"],
        "the target, the listed descendants and the definition, newest first across them"
    );

    // The target alone is still read when the walk found nothing below it.
    let alone = history_subtree(&db, target, &[], None, None).await;
    assert_eq!(requests(&alone), vec!["t2", "t1", "definition"]);
}

#[tokio::test]
async fn a_subtree_history_pages_by_keyset_through_every_record_once() {
    let db = db().await;
    let (target, child) = (Uuid::new_v4(), Uuid::new_v4());
    for i in 0..7 {
        let tenant = if i % 2 == 0 { target } else { child };
        append(&db, record(tenant, &format!("r{i}"))).await;
    }
    let whole = history_subtree(&db, target, &[child], None, None).await;
    assert_eq!(
        requests(&whole),
        vec!["r6", "r5", "r4", "r3", "r2", "r1", "r0"]
    );
    assert!(whole.page_info.next_cursor.is_none(), "one page holds it");

    let mut walked = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let page = history_subtree(&db, target, &[child], Some(3), cursor).await;
        pages += 1;
        walked.extend(page.items.iter().map(|r| r.id));
        cursor = next_of(&page);
        if cursor.is_none() {
            break;
        }
        assert!(pages < 10, "the walk ends");
    }
    assert_eq!(pages, 3);
    let expected: Vec<Uuid> = whole.items.iter().map(|r| r.id).collect();
    assert_eq!(walked, expected, "the pages are the list, in order, once");
}

/// Store a copy of the record `request` names: its instant exactly, a new id
/// and `tenant`. The table refuses an update, so a tie on the instant is made
/// of such copies.
async fn copy_at_its_instant(probe: &crate::test_support::Probe, request: &str, tenant: Uuid) {
    use sea_orm::ConnectionTrait as _;

    let columns = probe
        .raw
        .query_all_raw(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Sqlite,
            "SELECT name FROM pragma_table_info('audit_records')".to_owned(),
        ))
        .await
        .expect("the table is readable");
    let copied = columns
        .iter()
        .map(|c| c.try_get::<String>("", "name").expect("a name"))
        .filter(|n| n != "id" && n != "tenant_id")
        .collect::<Vec<_>>()
        .join(", ");
    probe
        .raw
        .execute_unprepared(&format!(
            "INSERT INTO audit_records (id, tenant_id, {copied}) \
             SELECT X'{}', X'{}', {copied} FROM audit_records WHERE request_id = '{request}' \
             LIMIT 1",
            Uuid::new_v4().simple(),
            tenant.simple()
        ))
        .await
        .expect("the copy is inserted");
}

#[tokio::test]
async fn records_of_one_instant_are_ordered_and_paged_by_id() {
    // Records of two tenants written in the same microsecond tie on
    // `occurred_at`; the id decides, the same way on every read, so a page
    // boundary that falls inside the tie neither repeats nor skips a record.
    // The table refuses an update, so the tie is made of copies of one stored
    // record: its instant exactly, a new id, and the tenant alternating.
    let (db, probe) = crate::test_support::sqlite_provider_probed().await;
    let (target, child) = (Uuid::new_v4(), Uuid::new_v4());
    append(&db, record(target, "seed")).await;
    for i in 0..5 {
        let tenant = if i % 2 == 0 { child } else { target };
        copy_at_its_instant(&probe, "seed", tenant).await;
    }

    let whole = history_subtree(&db, target, &[child], None, None).await;
    assert_eq!(whole.items.len(), 6, "the seed and its five copies");
    assert!(
        whole
            .items
            .iter()
            .all(|r| r.occurred_at == whole.items[0].occurred_at),
        "one instant"
    );
    let listed: Vec<Uuid> = whole.items.iter().map(|r| r.id).collect();
    let mut by_id = listed.clone();
    by_id.sort();
    by_id.reverse();
    assert_eq!(
        listed, by_id,
        "a tie on the instant is broken by id, descending"
    );
    assert!(
        whole.items.iter().any(|r| r.tenant_id == Some(target))
            && whole.items.iter().any(|r| r.tenant_id == Some(child)),
        "the tie spans both tenants"
    );

    let mut walked = Vec::new();
    let mut cursor = None;
    for _ in 0..3 {
        let page = history_subtree(&db, target, &[child], Some(2), cursor).await;
        walked.extend(page.items.iter().map(|r| r.id));
        cursor = next_of(&page);
    }
    assert!(cursor.is_none(), "three pages of two hold the six");
    assert_eq!(walked, listed, "a boundary inside the tie loses nothing");
}

#[tokio::test]
async fn a_tie_between_other_instants_pages_the_same_forward_and_backward() {
    // An older record, a tie of four on one instant, and a newer record,
    // walked in pages of two: one boundary falls before the tie, one inside
    // it and one after it. The cursor's instant is also a bound of its own on
    // the read, which must change nothing a keyset walk returns, in either
    // direction.
    let (db, probe) = crate::test_support::sqlite_provider_probed().await;
    let (target, child) = (Uuid::new_v4(), Uuid::new_v4());
    append(&db, record(child, "older")).await;
    append(&db, record(target, "tied")).await;
    for tenant in [child, target, child] {
        copy_at_its_instant(&probe, "tied", tenant).await;
    }
    append(&db, record(child, "newer")).await;

    let whole = history_subtree(&db, target, &[child], None, None).await;
    let listed: Vec<Uuid> = whole.items.iter().map(|r| r.id).collect();
    assert_eq!(
        requests(&whole),
        vec!["newer", "tied", "tied", "tied", "tied", "older"]
    );

    let mut pages = Vec::new();
    let mut cursor = None;
    loop {
        let page = history_subtree(&db, target, &[child], Some(2), cursor).await;
        cursor = next_of(&page);
        pages.push(page);
        if cursor.is_none() {
            break;
        }
        assert!(pages.len() < 10, "the walk ends");
    }
    let ids = |page: &toolkit_odata::Page<crate::audit::StoredAuditRecord>| {
        page.items.iter().map(|r| r.id).collect::<Vec<_>>()
    };
    let forward: Vec<Uuid> = pages.iter().flat_map(ids).collect();
    assert_eq!(forward, listed, "forward: the list, in order, once");

    // Back from the last page, page by page, to the first.
    let mut backward = ids(pages.last().expect("a last page"));
    let mut cursor = pages.last().and_then(prev_of);
    while let Some(at) = cursor {
        let page = history_subtree(&db, target, &[child], Some(2), Some(at)).await;
        let mut earlier = ids(&page);
        earlier.extend(backward);
        backward = earlier;
        cursor = prev_of(&page);
        assert!(backward.len() <= listed.len(), "the walk back ends");
    }
    assert_eq!(backward, listed, "backward: the same list");
}

#[tokio::test]
async fn a_cursor_of_one_mode_or_target_is_refused_by_the_other() {
    let db = db().await;
    let (target, child) = (Uuid::new_v4(), Uuid::new_v4());
    for (tenant, request) in [(target, "t1"), (child, "c1"), (target, "t2"), (child, "c2")] {
        append(&db, record(tenant, request)).await;
    }
    let scope_cursor = toolkit_odata::CursorV1::decode(
        &history(&db, target, Some(1), None)
            .await
            .page_info
            .next_cursor
            .expect("a second page of the scope history"),
    )
    .expect("cursor decodes");
    let subtree_first = history_subtree(&db, target, &[child], Some(1), None).await;
    let subtree_cursor = next_of(&subtree_first).expect("a second page of the subtree history");

    // Each continues its own.
    assert_eq!(
        history(&db, target, Some(1), Some(scope_cursor.clone()))
            .await
            .items
            .len(),
        1
    );
    assert_eq!(
        history_subtree(&db, target, &[child], Some(1), Some(subtree_cursor.clone()))
            .await
            .items
            .len(),
        1
    );

    // A single-scope cursor in the subtree history of the same pair.
    let crossed =
        try_history_subtree(&db, target, &[child], Some(1), Some(scope_cursor.clone())).await;
    assert!(
        matches!(&crossed, Err(DomainError::Validation { code, .. }) if *code == crate::field::ODATA_QUERY),
        "{crossed:?}"
    );
    // A subtree cursor in the single-scope history of the same pair.
    let conn = db.conn().expect("connection");
    let crossed = AuditStore::history(
        &conn,
        &AccessScope::allow_all(),
        KEY,
        target,
        &ODataQuery {
            limit: Some(1),
            cursor: Some(subtree_cursor.clone()),
            ..ODataQuery::default()
        },
    )
    .await;
    assert!(
        matches!(&crossed, Err(DomainError::Validation { code, .. }) if *code == crate::field::ODATA_QUERY),
        "{crossed:?}"
    );
    // A subtree cursor of one target in another target's subtree history.
    let foreign = try_history_subtree(&db, child, &[], Some(1), Some(subtree_cursor)).await;
    assert!(
        matches!(foreign, Err(DomainError::Validation { .. })),
        "{foreign:?}"
    );
}

#[tokio::test]
async fn a_subtree_cursor_with_rewritten_keys_is_refused_not_ignored() {
    // The store repeats the cursor's instant as a plain bound on the leading
    // order column, and skips the bound when the first key is no instant. The
    // skipped bound must not be the end of it: a cursor a client rewrote is
    // still refused, by the pager, as the caller's to correct — never read as
    // a page from the start, and never as a failure of the store.
    let db = db().await;
    let (target, child) = (Uuid::new_v4(), Uuid::new_v4());
    for (tenant, request) in [(target, "t1"), (child, "c1"), (target, "t2")] {
        append(&db, record(tenant, request)).await;
    }
    let first = history_subtree(&db, target, &[child], Some(1), None).await;
    let minted = next_of(&first).expect("a second page of the subtree history");
    assert_eq!(minted.k.len(), 2, "the instant, then the id");

    let no_instant = toolkit_odata::CursorV1 {
        k: vec!["not-an-instant".to_owned(), minted.k[1].clone()],
        ..minted.clone()
    };
    let no_keys = toolkit_odata::CursorV1 {
        k: Vec::new(),
        ..minted.clone()
    };
    let one_key = toolkit_odata::CursorV1 {
        k: vec![minted.k[0].clone()],
        ..minted.clone()
    };
    for (what, cursor) in [
        ("a first key that is no instant", no_instant),
        ("no keys", no_keys),
        ("the instant without the id", one_key),
    ] {
        let refused = try_history_subtree(&db, target, &[child], Some(1), Some(cursor)).await;
        assert!(
            matches!(
                &refused,
                Err(DomainError::Validation { code, .. }) if *code == crate::field::ODATA_QUERY
            ),
            "{what}: {refused:?}"
        );
    }

    // The cursor as minted still continues the history.
    let second = history_subtree(&db, target, &[child], Some(1), Some(minted)).await;
    assert_eq!(second.items.len(), 1);
}

#[test]
fn the_single_scope_binding_is_the_pair_alone_and_the_subtree_binding_differs() {
    // A cursor minted before the subtree mode existed carries the hash of the
    // key and the tenant, in that order, and nothing else: the single-scope
    // binding must stay that hash, or every such cursor would be refused.
    use super::{HistoryMode, history_binding};
    use std::hash::{DefaultHasher, Hash, Hasher};

    let tenant = Uuid::from_u128(0x7e4a);
    let mut hasher = DefaultHasher::new();
    KEY.hash(&mut hasher);
    tenant.hash(&mut hasher);
    let pair_alone = format!("{:016x}", hasher.finish());

    assert_eq!(history_binding(KEY, tenant, HistoryMode::Scope), pair_alone);
    assert_ne!(
        history_binding(KEY, tenant, HistoryMode::Subtree),
        pair_alone,
        "the two modes of one pair are two result sets"
    );
}

#[test]
fn a_failed_page_read_is_the_callers_only_when_the_request_is_at_fault() {
    use toolkit_odata::Error;

    // The database failing is an internal fault, as the platform maps a
    // database error; the driver's text stays in the diagnostic and is not
    // what the caller reads.
    let driver = "error returned from database: connection to 10.0.0.7:5432 refused";
    let mapped = super::page_error(&Error::Db(driver.to_owned()));
    match &mapped {
        DomainError::Internal { diagnostic } => {
            assert!(diagnostic.contains(driver), "{diagnostic}");
        }
        other => panic!("expected an internal fault, got {other:?}"),
    }
    assert!(!mapped.wire_message().contains("10.0.0.7"));
    assert!(matches!(
        super::page_error(&Error::ParsingUnavailable("no parser in this build")),
        DomainError::Internal { .. }
    ));

    // What the request carries is the caller's to correct.
    for err in [
        Error::InvalidFilter("f".to_owned()),
        Error::InvalidOrderByField("o".to_owned()),
        Error::OrderMismatch,
        Error::FilterMismatch,
        Error::InvalidCursor,
        Error::InvalidLimit,
        Error::OrderWithCursor,
        Error::CursorInvalidBase64,
        Error::CursorInvalidJson,
        Error::CursorInvalidVersion,
        Error::CursorInvalidKeys,
        Error::CursorInvalidFields,
        Error::CursorInvalidDirection,
    ] {
        let mapped = super::page_error(&err);
        assert!(
            matches!(
                &mapped,
                DomainError::Validation { field, code, .. }
                    if field == "query" && *code == crate::field::ODATA_QUERY
            ),
            "{err:?}: {mapped:?}"
        );
    }
}

#[tokio::test]
async fn a_history_read_the_database_fails_is_an_internal_fault_not_a_bad_request() {
    // The table gone from under the store stands in for any failure of the
    // database during the page read: both histories answer with an internal
    // fault, and neither hands the driver's text to the caller.
    use sea_orm::ConnectionTrait as _;

    let (db, probe) = crate::test_support::sqlite_provider_probed().await;
    let target = Uuid::new_v4();
    append(&db, record(target, "t1")).await;
    probe
        .raw
        .execute_unprepared("DROP TABLE audit_records")
        .await
        .expect("the table is dropped");

    let conn = db.conn().expect("connection");
    let scope = AuditStore::history(
        &conn,
        &AccessScope::allow_all(),
        KEY,
        target,
        &ODataQuery::default(),
    )
    .await
    .map(|_| ());
    let subtree = try_history_subtree(&db, target, &[], None, None)
        .await
        .map(|_| ());
    for (what, outcome) in [
        ("the scope history", scope),
        ("the subtree history", subtree),
    ] {
        match outcome {
            Err(err @ DomainError::Internal { .. }) => {
                let wire = err.wire_message();
                assert!(!wire.contains("audit_records"), "{what}: {wire}");
            }
            other => panic!("{what}: expected an internal fault, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_subtree_at_the_budget_fits_one_statement() {
    // The whole budget of descendants and the target go in one `IN` list;
    // `SQLite` takes 32 766 bind parameters a statement, so the list is not
    // chunked. Run for real, at the size the handler can pass at most.
    let db = db().await;
    let target = Uuid::new_v4();
    let descendants: Vec<Uuid> = (0..crate::domain::resolution::SUBTREE_BUDGET)
        .map(|_| Uuid::new_v4())
        .collect();
    append(&db, record(target, "at-the-target")).await;
    append(&db, record(descendants[0], "first")).await;
    append(&db, record(descendants[descendants.len() - 1], "last")).await;
    append(&db, record(Uuid::new_v4(), "outside")).await;

    let first = history_subtree(&db, target, &descendants, Some(2), None).await;
    assert_eq!(requests(&first), vec!["last", "first"]);
    // With a cursor the statement carries its keyset values too.
    let second = history_subtree(&db, target, &descendants, Some(2), next_of(&first)).await;
    assert_eq!(requests(&second), vec!["at-the-target"]);
}
