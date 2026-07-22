#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Regression test for ADR-0014: with `created_at` folded into the derived
//! `id`, the same `(tenant_id, gts_id, idempotency_key)` at two different
//! `created_at` values persists as two rows with DISTINCT ids, so `get` and
//! `deactivate` each address exactly one. Requires Docker.

mod common;

use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use timescaledb_usage_collector_plugin::domain::ports::{CatalogStore, RecordStore};
use usage_collector_sdk::{UsageRecordStatus, derive_usage_record_id};

const VCPU_GTS: &str = "gts.cf.core.uc.usage_record.v1~cf.compute._.vcpu_hours.v1";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_same_key_different_created_at_are_distinct_and_addressable() {
    let h = common::bring_up()
        .await
        .expect("timescaledb container (Docker required)");
    let catalog = common::catalog_store(&h.pool);
    catalog
        .create(common::fixture_usage_type(VCPU_GTS, "counter", &[]))
        .await
        .expect("register usage type (satisfies the gts_id FK)");
    let store = common::record_store(&h.pool);
    let tenant = Uuid::from_u128(0x00C0_FFEE);

    // Two records sharing the 3-tuple, differing only in created_at. Ids are
    // derived exactly as the SDK/gateway would (from the full 4-tuple).
    let t1 = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("valid instant");
    let t2 = t1 + Duration::seconds(1);

    let mut r1 = common::fixture_usage_record(
        VCPU_GTS,
        tenant,
        "idem-shared",
        rust_decimal::Decimal::ONE,
        1,
    );
    r1.created_at = t1;
    r1.id = derive_usage_record_id(r1.tenant_id, &r1.gts_id, &r1.idempotency_key, r1.created_at);
    let mut r2 = common::fixture_usage_record(
        VCPU_GTS,
        tenant,
        "idem-shared",
        rust_decimal::Decimal::ONE,
        2,
    );
    r2.created_at = t2;
    r2.id = derive_usage_record_id(r2.tenant_id, &r2.gts_id, &r2.idempotency_key, r2.created_at);

    assert_ne!(
        r1.id, r2.id,
        "same 3-tuple + different created_at must derive distinct ids"
    );
    let (id1, id2) = (r1.id, r2.id);

    store.create(r1).await.expect("create row 1");
    store
        .create(r2)
        .await
        .expect("create row 2 (distinct 4-tuple -> fresh insert)");

    // Both rows persist.
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM usage_records WHERE tenant_id = $1")
        .bind(tenant)
        .fetch_one(&h.pool)
        .await
        .expect("count rows");
    assert_eq!(count, 2, "both distinct-4-tuple rows must persist");

    // `get` is surgical.
    let g1 = store.get(id1).await.expect("get row 1");
    assert_eq!(g1.created_at, t1, "get(id1) must return the t1 row");
    let g2 = store.get(id2).await.expect("get row 2");
    assert_eq!(g2.created_at, t2, "get(id2) must return the t2 row");

    // `deactivate` is surgical.
    store.deactivate(id1).await.expect("deactivate row 1");
    assert_eq!(
        store.get(id1).await.expect("re-get row 1").status,
        UsageRecordStatus::Inactive,
        "row 1 must be inactive after deactivate(id1)",
    );
    assert_eq!(
        store.get(id2).await.expect("re-get row 2").status,
        UsageRecordStatus::Active,
        "row 2 must remain active - deactivate(id1) must not touch it",
    );
}
