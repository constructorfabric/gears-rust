#![cfg(feature = "integration")]
// The fixture and the seed are plain helpers, which `allow-expect-in-tests`
// does not reach; a failed expectation there is the test failing, as in the
// sibling PostgreSQL suites.
#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The activation read's two queries on a real `PostgreSQL`: the history's
//! listing rule — a report whose change record is among the history's records
//! is left out of the page query and its count — and the attachment lookup by
//! `change_ref` through the partial `idx_audit_change_ref`. The rows are the
//! API rows of `activation_surface_tests`, walked here with page size 1 on the
//! backend every stand runs on.
//!
//! Docker required: without it the suite skips, unless
//! `SETTINGS_SERVICE_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI
//! sets it. Run via `make test-integration LANE=settings-service-pg` or:
//!
//! ```sh
//! cargo nextest run -p cf-gears-settings-service --features integration --test pg_activation_read_test
//! ```

mod support;

use std::time::Duration;

use cf_gears_settings_service::audit::{
    ActorTenant, AuditOperation, AuditRecord, AuditSink, StoredAuditRecord,
};
use cf_gears_settings_service::infra::storage::audit_store::{AuditStore, reports_select};
use sea_orm::{ConnectionTrait, QueryTrait, Statement, TransactionTrait};
use toolkit_db::{DBProvider, connect_db};
use toolkit_odata::{CursorV1, ODataQuery};
use toolkit_security::AccessScope;
use uuid::Uuid;

const KEY: &str = "gts.cf.core.settings.setting_type.v1~acme.settings.network.enable_proxy.v1~";

fn change(tenant: Uuid, id: Uuid) -> AuditRecord {
    AuditRecord::new(
        KEY,
        Some(tenant),
        "admin",
        ActorTenant::machine(),
        AuditOperation::Change,
        "req",
    )
    .with_record_id(id)
}

fn report(tenant: Uuid, change: Uuid, subscriber: &str) -> AuditRecord {
    AuditRecord::new(
        KEY,
        Some(tenant),
        subscriber,
        ActorTenant::machine(),
        AuditOperation::Activation,
        "req",
    )
    .by_module()
    .with_change_ref(change)
}

async fn page(
    db: &DBProvider<toolkit_db::DbError>,
    tenant: Uuid,
    cursor: Option<&str>,
) -> cf_gears_settings_service::domain::odata::Listing<StoredAuditRecord> {
    let conn = db.conn().expect("connection");
    AuditStore
        .history(
            &conn,
            &AccessScope::allow_all(),
            KEY,
            tenant,
            &ODataQuery {
                limit: Some(1),
                cursor: cursor.map(|c| CursorV1::decode(c).expect("a cursor")),
                ..ODataQuery::default()
            },
        )
        .await
        .expect("history reads")
}

#[tokio::test]
async fn the_listing_rule_and_the_attachment_lookup_walk_as_the_api_rows_do() {
    let Some(pg) = support::platform::pg_fixture("activation read").await else {
        return;
    };
    let db = DBProvider::new(
        connect_db(&pg.url, support::platform::pool())
            .await
            .expect("the toolkit connects"),
    );
    let (x, d) = (Uuid::from_u128(0xc1), Uuid::from_u128(0xc2));
    let (first, newer) = (Uuid::from_u128(0x100), Uuid::from_u128(0x200));
    let conn = db.conn().expect("connection");
    let all = AccessScope::allow_all();
    // A change at `X` with a report at `X` and one at the descendant `D`, then
    // a newer record of the key at `X`.
    for record in [
        change(x, first),
        report(x, first, "gear-x"),
        report(d, first, "gear-d"),
    ] {
        AuditStore
            .append(&conn, &all, record)
            .await
            .expect("append");
    }
    tokio::time::sleep(Duration::from_millis(5)).await;
    AuditStore
        .append(&conn, &all, change(x, newer))
        .await
        .expect("append");

    // At `X`, page size 1: the newer record alone, then the change; the report
    // at `X` is on neither page, and the total counts the two changes.
    let page_one = page(&db, x, None).await;
    assert_eq!(
        page_one.items.iter().map(|r| r.id).collect::<Vec<_>>(),
        [newer]
    );
    assert_eq!(page_one.total_count, 2);
    let cursor = page_one.page_info.next_cursor.expect("a second page");
    let page_two = page(&db, x, Some(&cursor)).await;
    assert_eq!(
        page_two.items.iter().map(|r| r.id).collect::<Vec<_>>(),
        [first]
    );
    assert_eq!(page_two.total_count, 2);
    assert!(page_two.page_info.next_cursor.is_none());

    // One lookup for the page's changes finds both reports, whatever tenant.
    let mut found: Vec<String> = AuditStore
        .reports_for(&conn, &all, KEY, &[newer, first])
        .await
        .expect("lookup")
        .into_iter()
        .map(|r| r.actor)
        .collect();
    found.sort();
    assert_eq!(found, ["gear-d", "gear-x"]);

    // At `D` the same report is an ordinary record, its `change_ref` carried.
    let at_d = page(&db, d, None).await;
    assert_eq!(at_d.total_count, 1);
    assert_eq!(at_d.items[0].actor, "gear-d");
    assert_eq!(at_d.items[0].change_ref, Some(first));

    // The lookup's predicate is one the partial index serves. On five rows the
    // planner would pick whatever is cheapest, so inside a transaction that is
    // rolled back the table is made to look as a large one does: sequential
    // scans off and the key-leading index gone, leaving the lookup to the
    // partial index on `change_ref`.
    let tx = pg.conn.begin().await.expect("a transaction");
    for setting in [
        "SET LOCAL enable_seqscan = off",
        "DROP INDEX idx_audit_scoped",
    ] {
        tx.execute_unprepared(setting)
            .await
            .expect("planner setting");
    }
    // The statement `reports_for` emits, not a copy of it: the gear's own
    // query builder, rendered for the backend of the transaction.
    let lookup = reports_select(KEY, &[newer, first]).build(tx.get_database_backend());
    let plan = tx
        .query_all_raw(Statement {
            sql: format!("EXPLAIN {}", lookup.sql),
            ..lookup
        })
        .await
        .expect("explain")
        .iter()
        .map(|row| row.try_get_by_index::<String>(0).expect("a plan line"))
        .collect::<Vec<_>>()
        .join("\n");
    tx.rollback().await.expect("rolled back");
    assert!(plan.contains("idx_audit_change_ref"), "{plan}");
}

/// DESIGN-activation §7 Level 2, *Excluded silent instance in the read*, on
/// the backend every stand runs on: two instances A and B, change 1 leaving B
/// silent, change 2 committed. The outer join of the states to their
/// subscriptions and the read of the silent subscriptions both run on
/// `PostgreSQL`.
#[tokio::test]
async fn a_silent_instance_the_change_left_out_is_an_excluded_entry_on_postgres() {
    use cf_gears_settings_service::domain::activation::publisher::{StateRow, StateWrite};
    use cf_gears_settings_service::domain::activation::{
        ActivationRead, ActivationRepository, ActivationStatus, ChangeOutcome, InstanceKey,
        build_items,
    };
    use cf_gears_settings_service::infra::storage::activation_repo::ActivationRepo;

    let Some(pg) = support::platform::pg_fixture("activation read instances").await else {
        return;
    };
    let db: DBProvider<toolkit_db::DbError> = DBProvider::new(
        connect_db(&pg.url, support::platform::pool())
            .await
            .expect("the toolkit connects"),
    );
    let conn = db.conn().expect("connection");
    let tenant = Uuid::from_u128(0xc1).to_string();
    let instance = |n: u128| InstanceKey {
        subscriber: "gear-a".to_owned(),
        instance_id: Uuid::from_u128(n),
        key: KEY.to_owned(),
        tenant: tenant.clone(),
    };
    let mut subscriptions = Vec::new();
    for (n, label) in [(1, "host-a"), (2, "host-b")] {
        subscriptions.push(
            ActivationRepo
                .register(&conn, &instance(n), Some(label))
                .await
                .expect("registered")
                .id,
        );
    }
    let write = |change: u128, ids: &[Uuid]| StateWrite {
        key: KEY.to_owned(),
        change_ref: Uuid::from_u128(change),
        rows: ids
            .iter()
            .map(|id| StateRow {
                subscription_id: *id,
                tenant: tenant.clone(),
                expected_value: "EXPECTED".to_owned(),
            })
            .collect(),
    };
    ActivationRepo
        .write_states(&conn, &write(1, &subscriptions))
        .await
        .expect("change 1");
    let at = time::OffsetDateTime::now_utc();
    assert!(
        ActivationRepo
            .call_silent(&conn, &instance(2), Uuid::from_u128(1), at)
            .await
            .expect("called silent")
    );
    ActivationRepo
        .mark_silent(&conn, &instance(2), at)
        .await
        .expect("marked");
    ActivationRepo
        .write_states(&conn, &write(2, &subscriptions))
        .await
        .expect("change 2");

    let rows = ActivationRepo
        .activation_rows(&conn, KEY, &tenant)
        .await
        .expect("the rows");
    let silent = ActivationRepo
        .silent_subscriptions(&conn, KEY, &tenant)
        .await
        .expect("the silent subscriptions");
    let read = ActivationRead {
        tenant: Uuid::from_u128(0xc1),
        items: build_items(rows, &silent),
    };
    let item = &read.items[0];
    assert_eq!(item.change_ref, Uuid::from_u128(2));
    assert_eq!(
        (item.expected, item.silent_excluded, item.outcome),
        (1, 1, ChangeOutcome::Awaiting)
    );
    assert!(!item.instances[0].excluded);
    assert_eq!(item.instances[0].instance_label.as_deref(), Some("host-a"));
    let excluded = &item.instances[1];
    assert!(excluded.excluded && excluded.status == ActivationStatus::Silent);
    assert!(excluded.silent_since.is_some());
    assert_eq!(excluded.instance_label.as_deref(), Some("host-b"));

    // The platform administrator keeps the identity; any other reader loses
    // it and nothing else.
    let tenant_view = read.clone().disclosed_to(false);
    assert!(
        tenant_view.items[0]
            .instances
            .iter()
            .all(|e| e.instance_id.is_none() && e.instance_label.is_none())
    );
    assert_eq!(read.clone().disclosed_to(true), read);
}
