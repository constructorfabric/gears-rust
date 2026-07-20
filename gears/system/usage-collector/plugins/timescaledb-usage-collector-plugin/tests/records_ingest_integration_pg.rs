#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! `TimescaleDB`-backed integration tests for `PgRecordStore` ingest:
//! single insert with idempotency dedup (insert / absorb / conflict),
//! compensation persistence, and batch per-row outcomes. Requires Docker.

mod common;

use rust_decimal::Decimal;
use uuid::Uuid;

use usage_collector_sdk::{UsageCollectorPluginError, UsageRecordStatus};

use timescaledb_usage_collector_plugin::domain::ports::{CatalogStore, RecordStore};
use timescaledb_usage_collector_plugin::infra::storage::record_store::PgRecordStore;

const VCPU_GTS: &str = "gts.cf.core.uc.usage_record.v1~cf.compute._.vcpu_hours.v1";

/// Bring up a container and register `VCPU_GTS` in the catalog so the
/// `usage_records.gts_id` FK is satisfied for every inserted record.
async fn setup() -> (common::TsHarness, PgRecordStore) {
    let h = common::bring_up()
        .await
        .expect("timescaledb container (Docker required)");
    let catalog = common::catalog_store(&h.pool);
    catalog
        .create(common::fixture_usage_type(VCPU_GTS, "counter", &[]))
        .await
        .expect("register usage type for FK");
    let store = common::record_store(&h.pool);
    (h, store)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_insert_new_record_returns_active() {
    let (_h, store) = setup().await;
    let tenant = Uuid::from_u128(0x1001);

    let record = common::fixture_usage_record(VCPU_GTS, tenant, "idem-new", Decimal::new(5, 0), 1);

    let stored = store
        .create(record.clone())
        .await
        .expect("create new record");

    assert_eq!(stored.id, record.id, "id round-trips");
    assert_eq!(stored.value, record.value, "value round-trips");
    assert_eq!(stored.tenant_id, record.tenant_id, "tenant round-trips");
    assert_eq!(
        stored.idempotency_key, record.idempotency_key,
        "idempotency_key round-trips"
    );
    assert_eq!(
        stored.created_at, record.created_at,
        "created_at round-trips at second precision"
    );
    assert_eq!(
        stored.status,
        UsageRecordStatus::Active,
        "first accept defaults to Active"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_exact_retry_is_absorbed() {
    let (_h, store) = setup().await;
    let tenant = Uuid::from_u128(0x1002);

    let record =
        common::fixture_usage_record(VCPU_GTS, tenant, "idem-retry", Decimal::new(7, 0), 2);

    let first = store.create(record.clone()).await.expect("first create");
    let second = store
        .create(record.clone())
        .await
        .expect("exact retry must be absorbed, not conflict");

    assert_eq!(first.id, second.id, "absorb returns the same stored id");
    assert_eq!(second.id, record.id, "stored id is the original");
    assert_eq!(
        second.value, record.value,
        "absorb returns the stored value"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_insert_with_unregistered_gts_id_is_usage_type_not_found() {
    // The core pre-checks the usage type exists before inserting, but in the
    // narrow TOCTOU window where the type is deleted between that check and the
    // insert the `usage_records.gts_id` -> catalog FK is violated (23503). The
    // plugin must surface that as the typed `UsageTypeNotFound` (which the core
    // lifts to a 404), not a generic Internal (500).
    const UNREGISTERED_GTS: &str =
        "gts.cf.core.uc.usage_record.v1~cf.compute._.unregistered_hours.v1";
    let (_h, store) = setup().await;
    let tenant = Uuid::from_u128(0x1009);

    let record = common::fixture_usage_record(
        UNREGISTERED_GTS,
        tenant,
        "idem-no-type",
        Decimal::new(1, 0),
        9,
    );

    let err = store
        .create(record)
        .await
        .expect_err("insert against an unregistered gts_id must fail the FK");

    match err {
        UsageCollectorPluginError::UsageTypeNotFound { gts_id } => {
            assert_eq!(
                gts_id,
                common::fixture_gts_id(UNREGISTERED_GTS),
                "the typed error carries the missing gts_id"
            );
        }
        other => panic!("expected UsageTypeNotFound, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_same_key_conflicting_value_is_idempotency_conflict() {
    let (_h, store) = setup().await;
    let tenant = Uuid::from_u128(0x1003);

    let first = common::fixture_usage_record(VCPU_GTS, tenant, "idem-dup", Decimal::new(3, 0), 3);
    let stored = store.create(first.clone()).await.expect("first create");

    // Same (tenant, gts, idempotency_key) but a different value AND a distinct
    // `id` — a canonical-field mismatch (both `value` and the record `id` are
    // canonical here), which must surface as a conflict.
    let conflicting =
        common::fixture_usage_record(VCPU_GTS, tenant, "idem-dup", Decimal::new(999, 0), 4);

    let err = store
        .create(conflicting)
        .await
        .expect_err("conflicting value on the same key must fail");

    match err {
        UsageCollectorPluginError::IdempotencyConflict {
            idempotency_key,
            existing_id,
        } => {
            assert_eq!(idempotency_key, "idem-dup", "conflict carries the key");
            assert_eq!(
                existing_id, stored.id,
                "conflict carries the previously stored row's id"
            );
        }
        other => panic!("expected IdempotencyConflict, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_compensation_persists_corrects_id() {
    let (_h, store) = setup().await;
    let tenant = Uuid::from_u128(0x1004);

    let original =
        common::fixture_usage_record(VCPU_GTS, tenant, "idem-orig", Decimal::new(10, 0), 5);
    let original = store.create(original).await.expect("create original");

    // A compensation: negative value, a fresh idempotency key, and corrects_id
    // pointing at the original row.
    let mut compensation =
        common::fixture_usage_record(VCPU_GTS, tenant, "idem-comp", Decimal::new(-10, 0), 6);
    compensation.corrects_id = Some(original.id);

    let stored = store
        .create(compensation.clone())
        .await
        .expect("create compensation");
    assert_eq!(
        stored.corrects_id,
        Some(original.id),
        "create returns the compensation target"
    );

    let fetched = store.get(stored.id).await.expect("get compensation back");
    assert_eq!(
        fetched.corrects_id,
        Some(original.id),
        "corrects_id persists and reads back"
    );
    assert_eq!(
        fetched.value,
        Decimal::new(-10, 0),
        "negative value persists"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_batch_preserves_order_and_isolates_conflict() {
    let (_h, store) = setup().await;
    let tenant = Uuid::from_u128(0x1005);

    // Pre-existing record whose key the batch's #2 will collide with using a
    // conflicting value.
    let existing =
        common::fixture_usage_record(VCPU_GTS, tenant, "batch-dup", Decimal::new(1, 0), 7);
    let existing = store.create(existing).await.expect("seed existing record");

    let row0 = common::fixture_usage_record(VCPU_GTS, tenant, "batch-0", Decimal::new(2, 0), 8);
    // #2 reuses `batch-dup` with a different value -> IdempotencyConflict.
    let row1 = common::fixture_usage_record(VCPU_GTS, tenant, "batch-dup", Decimal::new(42, 0), 9);
    let row2 = common::fixture_usage_record(VCPU_GTS, tenant, "batch-2", Decimal::new(3, 0), 10);

    let results = store
        .create_batch(vec![row0.clone(), row1, row2.clone()])
        .await
        .expect("batch returns per-row outcomes");

    assert_eq!(results.len(), 3, "one result per input row, in order");

    let r0 = results[0].as_ref().expect("row 0 inserted");
    assert_eq!(r0.id, row0.id, "row 0 preserves position");

    match results[1].as_ref() {
        Err(UsageCollectorPluginError::IdempotencyConflict { existing_id, .. }) => {
            assert_eq!(
                *existing_id, existing.id,
                "row 1 conflict points at the seeded row"
            );
        }
        other => panic!("row 1 must be IdempotencyConflict, got {other:?}"),
    }

    let r2 = results[2].as_ref().expect("row 2 inserted");
    assert_eq!(r2.id, row2.id, "row 2 preserves position");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_empty_batch_is_internal_error() {
    let (_h, store) = setup().await;

    let err = store
        .create_batch(Vec::new())
        .await
        .expect_err("empty batch is a host-contract breach");
    assert!(
        matches!(err, UsageCollectorPluginError::Internal(_)),
        "empty batch must surface as Internal, got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_deactivate_flips_target_and_active_compensations() {
    let (_h, store) = setup().await;
    let tenant = Uuid::from_u128(0x1006);

    // Target record R, plus an active compensation C pointing at it.
    let target =
        common::fixture_usage_record(VCPU_GTS, tenant, "deact-target", Decimal::new(20, 0), 11);
    let target = store.create(target).await.expect("create target");

    let mut comp =
        common::fixture_usage_record(VCPU_GTS, tenant, "deact-comp", Decimal::new(-20, 0), 12);
    comp.corrects_id = Some(target.id);
    let comp = store.create(comp).await.expect("create compensation");

    store
        .deactivate(target.id)
        .await
        .expect("deactivate target succeeds");

    let fetched_target = store.get(target.id).await.expect("get target back");
    assert_eq!(
        fetched_target.status,
        UsageRecordStatus::Inactive,
        "target flips to inactive"
    );

    let fetched_comp = store.get(comp.id).await.expect("get compensation back");
    assert_eq!(
        fetched_comp.status,
        UsageRecordStatus::Inactive,
        "depth-1 active compensation flips to inactive"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_deactivate_missing_is_not_found() {
    let (_h, store) = setup().await;

    let missing = Uuid::from_u128(999_999);
    let err = store
        .deactivate(missing)
        .await
        .expect_err("deactivating an unknown id must fail");

    match err {
        UsageCollectorPluginError::UsageRecordNotFound { id } => {
            assert_eq!(id, missing, "not-found carries the requested id");
        }
        other => panic!("expected UsageRecordNotFound, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_deactivate_already_inactive_is_already_inactive() {
    let (_h, store) = setup().await;
    let tenant = Uuid::from_u128(0x1007);

    let record =
        common::fixture_usage_record(VCPU_GTS, tenant, "deact-twice", Decimal::new(30, 0), 13);
    let record = store.create(record).await.expect("create record");

    store
        .deactivate(record.id)
        .await
        .expect("first deactivate succeeds");

    let err = store
        .deactivate(record.id)
        .await
        .expect_err("second deactivate on an inactive row must fail");

    match err {
        UsageCollectorPluginError::UsageRecordAlreadyInactive { id } => {
            assert_eq!(id, record.id, "already-inactive carries the row id");
        }
        other => panic!("expected UsageRecordAlreadyInactive, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_deactivate_leaves_unrelated_records_active() {
    let (_h, store) = setup().await;
    let tenant = Uuid::from_u128(0x1008);

    let r1 = common::fixture_usage_record(VCPU_GTS, tenant, "deact-r1", Decimal::new(40, 0), 14);
    let r1 = store.create(r1).await.expect("create r1");

    // Unrelated record: distinct id, no corrects_id pointing at r1.
    let r2 = common::fixture_usage_record(VCPU_GTS, tenant, "deact-r2", Decimal::new(50, 0), 15);
    let r2 = store.create(r2).await.expect("create r2");

    store.deactivate(r1.id).await.expect("deactivate r1");

    let fetched_r2 = store.get(r2.id).await.expect("get r2 back");
    assert_eq!(
        fetched_r2.status,
        UsageRecordStatus::Active,
        "unrelated record stays active (depth-1 scope guard)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_deactivate_does_not_propagate_past_depth_one() {
    let (_h, store) = setup().await;
    let tenant = Uuid::from_u128(0x1009);

    // Chain A <- B (corrects A) <- C (corrects B). Deactivating A must flip A
    // and its depth-1 compensation B, but leave the depth-2 row C active: the
    // cascade clause (`corrects_id = $1`) is bounded to one level, NOT recursive.
    let a = common::fixture_usage_record(VCPU_GTS, tenant, "deact-d2-a", Decimal::new(20, 0), 16);
    let a = store.create(a).await.expect("create A");

    let mut b =
        common::fixture_usage_record(VCPU_GTS, tenant, "deact-d2-b", Decimal::new(-20, 0), 17);
    b.corrects_id = Some(a.id);
    let b = store.create(b).await.expect("create B (corrects A)");

    let mut c =
        common::fixture_usage_record(VCPU_GTS, tenant, "deact-d2-c", Decimal::new(20, 0), 18);
    c.corrects_id = Some(b.id);
    let c = store.create(c).await.expect("create C (corrects B)");

    store.deactivate(a.id).await.expect("deactivate A");

    assert_eq!(
        store.get(a.id).await.expect("get A").status,
        UsageRecordStatus::Inactive,
        "target A flips to inactive"
    );
    assert_eq!(
        store.get(b.id).await.expect("get B").status,
        UsageRecordStatus::Inactive,
        "depth-1 compensation B (corrects A) flips to inactive"
    );
    assert_eq!(
        store.get(c.id).await.expect("get C").status,
        UsageRecordStatus::Active,
        "depth-2 row C (corrects B, not A) stays active: cascade is one level only"
    );
}

/// Approach A: two submissions sharing an `idempotency_key` but carrying
/// DIFFERENT `created_at` are distinct 4-tuples, so both insert — a silent
/// duplicate, never an `IdempotencyConflict`. This is the intentional divergence
/// from the SPI's 3-tuple contract (DESIGN.md §2.2): only a same-key/SAME-time
/// replay dedups. Run concurrently to also prove `ON CONFLICT` does not serialize
/// distinct 4-tuples into one row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pg_concurrent_same_key_different_created_at_inserts_two_rows() {
    let (h, store) = setup().await;
    let tenant = Uuid::from_u128(0x1A1A);

    let rec_a =
        common::fixture_usage_record(VCPU_GTS, tenant, "idem-a1", Decimal::new(3, 0), 0xA1A);
    let mut rec_b =
        common::fixture_usage_record(VCPU_GTS, tenant, "idem-a1", Decimal::new(4, 0), 0xB1B);
    // Shift B's created_at by +1s: same dedup key, different 4-tuple.
    rec_b.created_at = rec_a.created_at + time::Duration::seconds(1);

    let s1 = store.clone();
    let s2 = store.clone();
    let (r1, r2) = tokio::join!(
        tokio::spawn(async move { s1.create(rec_a).await }),
        tokio::spawn(async move { s2.create(rec_b).await }),
    );
    let r1 = r1.expect("task a join");
    let r2 = r2.expect("task b join");

    assert!(r1.is_ok(), "submission a inserts: {r1:?}");
    assert!(
        r2.is_ok(),
        "submission b (different created_at) inserts too: {r2:?}"
    );

    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM usage_records \
         WHERE tenant_id = $1 AND gts_id = $2 AND idempotency_key = $3",
    )
    .bind(tenant)
    .bind(VCPU_GTS)
    .bind("idem-a1")
    .fetch_one(&h.pool)
    .await
    .expect("count rows for dedup key");
    assert_eq!(
        n, 2,
        "same key with two created_at values -> two distinct records (silent duplicate)"
    );
}

/// A batch mixing a fresh insert, an exact retry (absorb), and a canonical
/// mismatch (conflict) on a pre-seeded key returns one positionally-aligned
/// result per row; a conflict is isolated to its own slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_batch_mixes_insert_absorb_conflict_per_row() {
    let (_h, store) = setup().await;
    let tenant = Uuid::from_u128(0xBA7C);

    let seed =
        common::fixture_usage_record(VCPU_GTS, tenant, "idem-dup", Decimal::new(5, 0), 0xB01);
    store.create(seed.clone()).await.expect("seed the dup key");

    let fresh =
        common::fixture_usage_record(VCPU_GTS, tenant, "idem-fresh", Decimal::new(2, 0), 0xB02);
    let absorb = seed.clone(); // exact retry of the seeded row
    let conflict =
        common::fixture_usage_record(VCPU_GTS, tenant, "idem-dup", Decimal::new(9, 0), 0xB03);

    let results = store
        .create_batch(vec![fresh, absorb, conflict])
        .await
        .expect("batch call succeeds");

    assert_eq!(results.len(), 3, "one result per input row, in order");
    assert!(results[0].is_ok(), "fresh row inserted: {:?}", results[0]);
    assert!(results[1].is_ok(), "exact retry absorbed: {:?}", results[1]);
    assert!(
        matches!(
            results[2],
            Err(UsageCollectorPluginError::IdempotencyConflict { .. })
        ),
        "canonical mismatch on seeded key conflicts: {:?}",
        results[2]
    );
}

/// Intra-batch duplicate of a FRESH (not pre-seeded) key: first occurrence
/// inserts, an exact-duplicate later occurrence absorbs against it, and a
/// canonical-mismatch later occurrence conflicts — all within one batch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_batch_intra_batch_duplicate_fresh_key() {
    let (h, store) = setup().await;
    let tenant = Uuid::from_u128(0x001B_A7C0);

    let first =
        common::fixture_usage_record(VCPU_GTS, tenant, "intra-dup", Decimal::new(5, 0), 0xD01);
    let exact = first.clone(); // exact retry within the same batch -> absorb
    let mismatch =
        common::fixture_usage_record(VCPU_GTS, tenant, "intra-dup", Decimal::new(9, 0), 0xD02);

    let results = store
        .create_batch(vec![first.clone(), exact, mismatch])
        .await
        .expect("batch returns per-row outcomes");

    assert_eq!(results.len(), 3, "one result per input row, in order");

    let r0 = results[0].as_ref().expect("first occurrence inserted");
    assert_eq!(r0.id, first.id, "winner is the first occurrence");

    let r1 = results[1].as_ref().expect("exact duplicate absorbed");
    assert_eq!(r1.id, first.id, "absorb returns the winner's stored row");

    match results[2].as_ref() {
        Err(UsageCollectorPluginError::IdempotencyConflict { existing_id, .. }) => {
            assert_eq!(
                *existing_id, first.id,
                "mismatch conflicts against the winner"
            );
        }
        other => panic!("row 2 must be IdempotencyConflict, got {other:?}"),
    }

    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM usage_records \
         WHERE tenant_id = $1 AND gts_id = $2 AND idempotency_key = $3",
    )
    .bind(tenant)
    .bind(VCPU_GTS)
    .bind("intra-dup")
    .fetch_one(&h.pool)
    .await
    .expect("count");
    assert_eq!(n, 1, "intra-batch dup persists exactly one record");
}

/// A batch where every row conflicts against a pre-seeded key: all slots return
/// `IdempotencyConflict`, none fail the batch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_batch_all_rows_conflict() {
    let (_h, store) = setup().await;
    let tenant = Uuid::from_u128(0x000A_11C0);

    let seed =
        common::fixture_usage_record(VCPU_GTS, tenant, "all-conf", Decimal::new(1, 0), 0xC01);
    let seed = store.create(seed).await.expect("seed");

    let a = common::fixture_usage_record(VCPU_GTS, tenant, "all-conf", Decimal::new(2, 0), 0xC02);
    let b = common::fixture_usage_record(VCPU_GTS, tenant, "all-conf", Decimal::new(3, 0), 0xC03);

    let results = store.create_batch(vec![a, b]).await.expect("batch ok");
    assert_eq!(results.len(), 2);
    for (i, r) in results.iter().enumerate() {
        match r {
            Err(UsageCollectorPluginError::IdempotencyConflict { existing_id, .. }) => {
                assert_eq!(*existing_id, seed.id, "row {i} conflicts against the seed");
            }
            other => panic!("row {i} must be IdempotencyConflict, got {other:?}"),
        }
    }
}

/// Approach A: a batch carrying two rows that share an `idempotency_key` but
/// differ in `created_at` inserts BOTH (distinct 4-tuples → silent duplicate),
/// not one-plus-conflict. Contrast `pg_batch_intra_batch_duplicate_fresh_key`,
/// where the duplicate shares the same `created_at` and so absorbs/conflicts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_batch_same_key_different_created_at_inserts_both() {
    let (h, store) = setup().await;
    let tenant = Uuid::from_u128(0x0057_A1EB);

    let first = common::fixture_usage_record(
        VCPU_GTS,
        tenant,
        "batch-dup-time",
        Decimal::new(7, 0),
        0xF01,
    );
    let mut later = common::fixture_usage_record(
        VCPU_GTS,
        tenant,
        "batch-dup-time",
        Decimal::new(8, 0),
        0xF02,
    );
    later.created_at = first.created_at + time::Duration::seconds(1);

    let results = store
        .create_batch(vec![first, later])
        .await
        .expect("batch ok");
    assert_eq!(results.len(), 2);
    assert!(results[0].is_ok(), "first row inserts: {:?}", results[0]);
    assert!(
        results[1].is_ok(),
        "later row (different created_at) inserts too: {:?}",
        results[1]
    );

    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM usage_records \
         WHERE tenant_id = $1 AND gts_id = $2 AND idempotency_key = $3",
    )
    .bind(tenant)
    .bind(VCPU_GTS)
    .bind("batch-dup-time")
    .fetch_one(&h.pool)
    .await
    .expect("count rows for dedup key");
    assert_eq!(
        n, 2,
        "same key, two created_at values in one batch -> two stored records"
    );
}

/// A 100-row batch of distinct fresh records (the documented cap) round-trips:
/// every row inserts, including subject-less rows and a row carrying metadata.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_batch_hundred_distinct_rows_all_insert() {
    let (h, store) = setup().await;
    let tenant = Uuid::from_u128(0x1000);

    let mut batch = Vec::with_capacity(100);
    for i in 0..100u128 {
        let mut r = common::fixture_usage_record(
            VCPU_GTS,
            tenant,
            &format!("bulk-{i}"),
            Decimal::new(i64::try_from(i).expect("fits i64") + 1, 0),
            0x1_0000 + i,
        );
        if i == 0 {
            r.metadata.insert(
                usage_collector_sdk::MetadataKey::new("region").expect("valid key"),
                "eu-1".to_owned(),
            );
        }
        batch.push(r);
    }

    let results = store.create_batch(batch).await.expect("bulk batch ok");
    assert_eq!(results.len(), 100, "one result per row");
    assert!(
        results.iter().all(Result::is_ok),
        "every distinct fresh row inserts"
    );

    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM usage_records WHERE tenant_id = $1 AND gts_id = $2",
    )
    .bind(tenant)
    .bind(VCPU_GTS)
    .fetch_one(&h.pool)
    .await
    .expect("count");
    assert_eq!(n, 100, "all 100 rows persisted");

    let first = results[0].as_ref().expect("row 0 ok");
    let fetched = store.get(first.id).await.expect("get row 0");
    assert_eq!(
        fetched
            .metadata
            .get(&usage_collector_sdk::MetadataKey::new("region").unwrap()),
        Some(&"eu-1".to_owned()),
        "metadata persisted via batch insert"
    );
}

/// Two concurrent batches sharing the same two fresh keys (submitted in opposite
/// input order) must both complete without a deadlock error: the planner sorts
/// claim keys into one global lock order. Exactly one record persists per key.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pg_concurrent_overlapping_batches_do_not_deadlock() {
    let (h, store) = setup().await;
    let tenant = Uuid::from_u128(0x0DEA_D10C);

    // Identical canonical content per key so the loser absorbs (not conflicts).
    let mk = |idem: &str, seq: u128| {
        common::fixture_usage_record(VCPU_GTS, tenant, idem, Decimal::new(1, 0), seq)
    };
    let b1 = vec![mk("ov-a", 0xA1), mk("ov-b", 0xB1)];
    let b2 = vec![mk("ov-b", 0xB1), mk("ov-a", 0xA1)]; // opposite input order

    let s1 = store.clone();
    let s2 = store.clone();
    let (r1, r2) = tokio::join!(
        tokio::spawn(async move { s1.create_batch(b1).await }),
        tokio::spawn(async move { s2.create_batch(b2).await }),
    );
    let r1 = r1.expect("join b1").expect("b1 batch ok (no deadlock)");
    let r2 = r2.expect("join b2").expect("b2 batch ok (no deadlock)");
    assert_eq!(r1.len(), 2);
    assert_eq!(r2.len(), 2);
    // Every slot is a success (insert or absorb) — never a deadlock-driven error.
    assert!(
        r1.iter().chain(r2.iter()).all(Result::is_ok),
        "r1={r1:?} r2={r2:?}"
    );

    for idem in ["ov-a", "ov-b"] {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM usage_records \
             WHERE tenant_id = $1 AND gts_id = $2 AND idempotency_key = $3",
        )
        .bind(tenant)
        .bind(VCPU_GTS)
        .bind(idem)
        .fetch_one(&h.pool)
        .await
        .expect("count");
        assert_eq!(n, 1, "key {idem} maps to exactly one stored record");
    }
}
