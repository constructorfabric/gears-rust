#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! `TimescaleDB`-backed integration tests for `PgCatalogStore`
//! (create / get / delete / list). Requires Docker.

mod common;

use uuid::Uuid;

use toolkit_odata::{CursorV1, ODataQuery};

use usage_collector_sdk::{UsageCollectorPluginError, UsageKind};

use timescaledb_usage_collector_plugin::domain::ports::CatalogStore;

const VCPU_GTS: &str = "gts.cf.core.uc.usage_record.v1~cf.compute._.vcpu_hours.v1";
const RAM_GTS: &str = "gts.cf.core.uc.usage_record.v1~cf.compute._.ram_gb.v1";
const MISSING_GTS: &str = "gts.cf.core.uc.usage_record.v1~cf.compute._.absent.v1";
const DISK_GTS: &str = "gts.cf.core.uc.usage_record.v1~cf.compute._.disk_gb.v1";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_create_then_get_roundtrips() {
    let h = common::bring_up()
        .await
        .expect("timescaledb container (Docker required)");
    let store = common::catalog_store(&h.pool);

    let ut = common::fixture_usage_type(VCPU_GTS, "counter", &["region", "tier"]);
    let created = store.create(ut.clone()).await.expect("create");
    assert_eq!(created, ut, "create returns the stored usage type");

    let fetched = store
        .get(common::fixture_gts_id(VCPU_GTS))
        .await
        .expect("get");
    assert_eq!(fetched.kind, UsageKind::Counter, "kind roundtrips");
    assert_eq!(
        fetched.metadata_fields, ut.metadata_fields,
        "metadata_fields roundtrip"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_create_duplicate_is_already_exists() {
    let h = common::bring_up()
        .await
        .expect("timescaledb container (Docker required)");
    let store = common::catalog_store(&h.pool);

    let ut = common::fixture_usage_type(VCPU_GTS, "gauge", &[]);
    store.create(ut.clone()).await.expect("first create");

    let err = store
        .create(ut)
        .await
        .expect_err("second create must conflict");
    assert!(
        matches!(
            err,
            UsageCollectorPluginError::UsageTypeAlreadyExists { .. }
        ),
        "duplicate create must be UsageTypeAlreadyExists, got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_get_missing_is_not_found() {
    let h = common::bring_up()
        .await
        .expect("timescaledb container (Docker required)");
    let store = common::catalog_store(&h.pool);

    let err = store
        .get(common::fixture_gts_id(MISSING_GTS))
        .await
        .expect_err("absent get must fail");
    assert!(
        matches!(err, UsageCollectorPluginError::UsageTypeNotFound { .. }),
        "absent get must be UsageTypeNotFound, got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_delete_unreferenced_succeeds() {
    let h = common::bring_up()
        .await
        .expect("timescaledb container (Docker required)");
    let store = common::catalog_store(&h.pool);

    let ut = common::fixture_usage_type(VCPU_GTS, "counter", &["region"]);
    store.create(ut).await.expect("create");

    store
        .delete(common::fixture_gts_id(VCPU_GTS))
        .await
        .expect("delete unreferenced");

    let err = store
        .get(common::fixture_gts_id(VCPU_GTS))
        .await
        .expect_err("get after delete must fail");
    assert!(
        matches!(err, UsageCollectorPluginError::UsageTypeNotFound { .. }),
        "get after delete must be UsageTypeNotFound, got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_delete_missing_is_not_found() {
    let h = common::bring_up()
        .await
        .expect("timescaledb container (Docker required)");
    let store = common::catalog_store(&h.pool);

    let err = store
        .delete(common::fixture_gts_id(MISSING_GTS))
        .await
        .expect_err("delete absent must fail");
    assert!(
        matches!(err, UsageCollectorPluginError::UsageTypeNotFound { .. }),
        "delete absent must be UsageTypeNotFound, got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_delete_referenced_is_usage_type_referenced() {
    let h = common::bring_up()
        .await
        .expect("timescaledb container (Docker required)");
    let store = common::catalog_store(&h.pool);

    let ut = common::fixture_usage_type(RAM_GTS, "counter", &[]);
    store.create(ut).await.expect("create");

    let tenant_id = Uuid::from_u128(0xABCD);
    common::insert_raw_usage_record(&h.pool, RAM_GTS, tenant_id)
        .await
        .expect("insert raw usage record");

    let err = store
        .delete(common::fixture_gts_id(RAM_GTS))
        .await
        .expect_err("delete referenced must fail");
    match err {
        UsageCollectorPluginError::UsageTypeReferenced {
            sample_ref_count, ..
        } => assert!(
            sample_ref_count >= 1,
            "sample_ref_count must be >= 1, got {sample_ref_count}"
        ),
        other => panic!("delete referenced must be UsageTypeReferenced, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_list_types_paginates_by_gts_id() {
    let h = common::bring_up()
        .await
        .expect("timescaledb container (Docker required)");
    let store = common::catalog_store(&h.pool);

    // Create three usage types. The list is gts_id-ordered ascending regardless
    // of insert order.
    for gts in [RAM_GTS, VCPU_GTS, DISK_GTS] {
        store
            .create(common::fixture_usage_type(gts, "counter", &[]))
            .await
            .expect("create usage type");
    }

    // Expected gts_id order is lexicographic ascending over the full string.
    let mut expected = [DISK_GTS, RAM_GTS, VCPU_GTS];
    expected.sort_unstable();

    // First page: limit 2 -> first two by gts_id + a next cursor.
    let page1 = store
        .list(&ODataQuery::new().with_limit(2))
        .await
        .expect("list first page");
    assert_eq!(page1.items.len(), 2, "first page capped at limit");
    assert_eq!(
        page1.items[0].gts_id.as_ref(),
        expected[0],
        "first item is the lexicographically-smallest gts_id"
    );
    assert_eq!(
        page1.items[1].gts_id.as_ref(),
        expected[1],
        "second item is the next gts_id"
    );
    let token = page1
        .page_info
        .next_cursor
        .expect("three types over limit 2 yield a next cursor");

    // Follow the cursor: the remaining (third) type.
    let cursor = CursorV1::decode(&token).expect("decode next cursor");
    let page2 = store
        .list(&ODataQuery::new().with_limit(2).with_cursor(cursor))
        .await
        .expect("list second page");
    assert_eq!(page2.items.len(), 1, "second page has the remaining type");
    assert_eq!(
        page2.items[0].gts_id.as_ref(),
        expected[2],
        "second page continues in gts_id order with no overlap"
    );
    assert!(
        page2.page_info.next_cursor.is_none(),
        "the final page has no next cursor"
    );
}
