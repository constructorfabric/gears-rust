#![cfg(feature = "clickhouse")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! `ClickHouse`-backed integration tests for [`ChRecordStore`] keyset
//! pagination and pushed-down aggregation:
//! - keyset pagination (first page + cursor follow, no overlap/gap),
//! - `$filter` by `tenant_id`,
//! - metadata side-channel filtering,
//! - SUM nets compensation, COUNT active-only, GROUP BY `resource_id`,
//! - GROUP BY metadata key combined with `$filter` (SELECT/WHERE bind order),
//! - full `Decimal128(9)` precision through the `JSONEachRow` result decode.
//!
//! `list` and `aggregate` read `FROM usage_records FINAL`, so the engine
//! resolves every `ReplacingMergeTree` sort key to its highest `version`
//! before the `WHERE`, the keyset `LIMIT` and the `GROUP BY` run (see
//! `query/dedup.rs`). The deactivation and duplicate-row tests below therefore
//! hold before any merge runs, and stop merges to prove it. Tests insert with
//! distinct `created_at` values so the `(created_at, id)` order is fully
//! observable.
//! Requires Docker.

mod common;

use std::collections::BTreeMap;

use bigdecimal::BigDecimal;
use rust_decimal::Decimal;
use uuid::Uuid;

use toolkit_odata::ast::{CompareOperator, Expr, Value};
use toolkit_odata::{CursorV1, ODataOrderBy, ODataQuery, OrderKey, SortDir};

use usage_collector_sdk::{
    AggregationDimension, AggregationOp, AggregationSpec, MetadataFilter, MetadataKey, UsageRecord,
};

use clickhouse_usage_collector_plugin::domain::ports::{CatalogStore, RecordStore};

const VCPU_GTS: &str = "gts.cf.core.uc.usage_record.v1~cf.compute._.vcpu_hours.v1";

/// Bring up containers and register `VCPU_GTS`.
///
/// Returns `None` when Docker is unavailable, so the caller can skip its own
/// test body with an early `return` instead of terminating the whole test
/// binary (see [`common::bring_up_or_skip`]).
async fn setup_with_type(
    gts: &str,
    fields: &[&str],
) -> Option<(common::ChHarness, impl RecordStore + Clone)> {
    let h = common::bring_up_or_skip().await?;
    let catalog = common::catalog_store(&h);
    catalog
        .create(common::fixture_usage_type(gts, "counter", fields))
        .await
        .expect("register usage type for referential integrity");
    let store = common::record_store(&h);
    Some((h, store))
}

/// A record `i` seconds after the fixture base instant, keyed by `seq` so each
/// seeded row has its own idempotency key (and therefore its own derived id).
fn record_at(gts: &str, tenant: Uuid, seq: u128, i: i64) -> UsageRecord {
    common::fixture_usage_record_at(
        gts,
        tenant,
        &format!("idem-{seq}"),
        Decimal::new(i + 1, 0),
        common::fixture_created_at_offset(i),
    )
}

fn created_at_id_asc() -> ODataOrderBy {
    ODataOrderBy(vec![
        OrderKey {
            field: "created_at".to_owned(),
            dir: SortDir::Asc,
        },
        OrderKey {
            field: "id".to_owned(),
            dir: SortDir::Asc,
        },
    ])
}

/// Keyset pagination: first page respects `$top` and yields a `next_cursor`.
/// Following the cursor yields remaining records with no overlap or gap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_list_keyset_first_page_and_cursor_follow() {
    let Some((_h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    let tenant = Uuid::from_u128(0x2001);

    let mut expected: Vec<Uuid> = Vec::new();
    for i in 0..5 {
        let seq = 0x2001_0000 + u128::try_from(i).unwrap();
        let rec = record_at(VCPU_GTS, tenant, seq, i);
        expected.push(rec.id);
        store.create(rec).await.expect("create record");
    }

    let order = created_at_id_asc();
    let mut seen: Vec<Uuid> = Vec::new();
    let mut cursor: Option<CursorV1> = None;

    loop {
        let mut query = ODataQuery::new().with_limit(2).with_order(order.clone());
        if let Some(c) = cursor.take() {
            query = query.with_cursor(c);
        }
        let page = store
            .list(common::fixture_gts_id(VCPU_GTS), &query, &[])
            .await
            .expect("list page");

        for item in &page.items {
            assert!(
                !seen.contains(&item.id),
                "no record appears on two pages (overlap)"
            );
            seen.push(item.id);
        }

        match page.page_info.next_cursor {
            Some(token) => {
                cursor = Some(CursorV1::decode(&token).expect("decode next cursor"));
            }
            None => break,
        }
    }

    let mut seen_sorted = seen.clone();
    seen_sorted.sort();
    let mut expected_sorted = expected.clone();
    expected_sorted.sort();
    assert_eq!(
        seen_sorted, expected_sorted,
        "walking all pages yields every record exactly once (no gap, no overlap)"
    );
    assert_eq!(seen.len(), 5, "exactly the five inserted records");
}

/// `$filter` by `tenant_id` narrows list results to the specified tenant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_list_keyset_respects_filter() {
    let Some((_h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    let tenant_a = Uuid::from_u128(0x2002_000A);
    let tenant_b = Uuid::from_u128(0x2002_000B);

    store
        .create(record_at(VCPU_GTS, tenant_a, 0x2002_0001, 0))
        .await
        .expect("create A1");
    store
        .create(record_at(VCPU_GTS, tenant_a, 0x2002_0002, 1))
        .await
        .expect("create A2");
    store
        .create(record_at(VCPU_GTS, tenant_b, 0x2002_0003, 2))
        .await
        .expect("create B1");

    let filter = Expr::Compare(
        Box::new(Expr::Identifier("tenant_id".to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(Value::Uuid(tenant_a))),
    );
    let query = ODataQuery::new()
        .with_order(created_at_id_asc())
        .with_filter(filter);

    let page = store
        .list(common::fixture_gts_id(VCPU_GTS), &query, &[])
        .await
        .expect("list filtered by tenant");

    assert_eq!(page.items.len(), 2, "only tenant A's two records match");
    for item in &page.items {
        assert_eq!(
            item.tenant_id, tenant_a,
            "every returned record is tenant A"
        );
    }
}

/// Metadata side-channel filter narrows results to the matching `region` value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_list_metadata_filter_excludes_non_matching_rows() {
    let Some((_h, store)) = setup_with_type(VCPU_GTS, &["region"]).await else {
        return;
    };
    let tenant = Uuid::from_u128(0x2003);

    let regions = ["us-east-1", "us-east-1", "eu-west-1"];
    for (i, region) in regions.iter().enumerate() {
        let idx = i64::try_from(i).unwrap();
        let seq = 0x2003_0000 + u128::try_from(i).unwrap();
        let mut rec = record_at(VCPU_GTS, tenant, seq, idx);
        let mut meta = BTreeMap::new();
        meta.insert(
            MetadataKey::new("region").expect("valid metadata key"),
            (*region).to_owned(),
        );
        rec.metadata = meta;
        store.create(rec).await.expect("create record");
    }

    let filter = MetadataFilter::new("region", ["us-east-1"]).expect("valid metadata filter");
    let query = ODataQuery::new().with_order(created_at_id_asc());

    let page = store
        .list(
            common::fixture_gts_id(VCPU_GTS),
            &query,
            std::slice::from_ref(&filter),
        )
        .await
        .expect("list with metadata filter");

    assert_eq!(
        page.items.len(),
        2,
        "only the two us-east-1 records match the metadata filter"
    );
    for item in &page.items {
        assert_eq!(
            item.metadata
                .get(&MetadataKey::new("region").unwrap())
                .map(String::as_str),
            Some("us-east-1"),
            "every returned record carries the filtered metadata value"
        );
    }
}

/// SUM aggregation nets the compensation row: `10 + (-3) = 7`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_aggregate_sum_nets_compensation() {
    let Some((_h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    let tenant = Uuid::from_u128(0x3001);

    let mut original = record_at(VCPU_GTS, tenant, 0x3001_0001, 0);
    original.value = Decimal::new(10, 0);
    let original_id = original.id;
    store.create(original).await.expect("create original");

    let mut compensation = record_at(VCPU_GTS, tenant, 0x3001_0002, 1);
    compensation.value = Decimal::new(-3, 0);
    compensation.corrects_id = Some(original_id);
    store
        .create(compensation)
        .await
        .expect("create compensation");

    let spec = AggregationSpec {
        op: AggregationOp::Sum,
        group_by: Vec::new(),
    };
    let result = store
        .aggregate(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new(),
            &[],
            spec,
        )
        .await
        .expect("aggregate sum");

    assert_eq!(
        result.buckets.len(),
        1,
        "empty group_by yields exactly one bucket"
    );
    let bucket = &result.buckets[0];
    assert!(bucket.key.is_empty(), "no grouping -> empty bucket key");
    assert_eq!(
        bucket.value,
        Some(BigDecimal::from(7_i64)),
        "SUM nets the active compensation: 10 + (-3) = 7"
    );
}

/// COUNT excludes inactive (deactivated) rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_aggregate_count_active_only() {
    let Some((_h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    let tenant = Uuid::from_u128(0x3002);

    // Insert three active rows, then deactivate one.
    let mut first_id = None;
    for i in 0..3 {
        let seq = 0x3002_0000 + u128::try_from(i).unwrap();
        let stored = store
            .create(record_at(VCPU_GTS, tenant, seq, i))
            .await
            .expect("create record");
        first_id = first_id.or(Some(stored.id));
    }
    let first_id = first_id.expect("three rows were seeded");
    store.deactivate(first_id).await.expect("deactivate first");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let spec = AggregationSpec {
        op: AggregationOp::Count,
        group_by: Vec::new(),
    };
    let result = store
        .aggregate(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new(),
            &[],
            spec,
        )
        .await
        .expect("aggregate count active only");

    assert_eq!(result.buckets.len(), 1, "empty group_by -> one bucket");
    assert_eq!(
        result.buckets[0].value,
        Some(BigDecimal::from(2_i64)),
        "COUNT excludes the deactivated row; two active rows remain"
    );
}

/// GROUP BY `resource_id` aggregation yields one bucket per distinct resource.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_aggregate_group_by_resource_id() {
    let Some((_h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    let tenant = Uuid::from_u128(0x3003);

    let rows = [
        ("idem-3003-1", 4_i64, "res-a", 0_i64),
        ("idem-3003-2", 6, "res-a", 1),
        ("idem-3003-3", 5, "res-b", 2),
    ];
    for (idem, value, resource_id, ts) in rows {
        let rec = common::fixture_usage_record_with_resource_at(
            VCPU_GTS,
            tenant,
            idem,
            Decimal::new(value, 0),
            common::fixture_created_at_offset(ts),
            resource_id,
        );
        store.create(rec).await.expect("create record");
    }

    let spec = AggregationSpec {
        op: AggregationOp::Sum,
        group_by: vec![AggregationDimension::ResourceId],
    };
    let result = store
        .aggregate(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new(),
            &[],
            spec,
        )
        .await
        .expect("aggregate group by resource_id");

    assert_eq!(
        result.buckets.len(),
        2,
        "one bucket per distinct resource_id"
    );
    let mut got: Vec<(String, Option<BigDecimal>)> = result
        .buckets
        .iter()
        .map(|b| {
            assert_eq!(b.key.len(), 1, "single grouped dimension -> one key entry");
            (b.key[0].clone(), b.value.clone())
        })
        .collect();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        got,
        vec![
            ("res-a".to_owned(), Some(BigDecimal::from(10_i64))),
            ("res-b".to_owned(), Some(BigDecimal::from(5_i64))),
        ],
        "each resource_id bucket carries its summed value"
    );
}

/// GROUP BY metadata key with a concurrent `$filter` — exercises SELECT-list
/// bind ordering: the metadata key `?` appears before `gts_id` and the filter
/// binds in the assembled SQL. Wrong order would mis-assign the filter UUID
/// (or `gts_id`) into `metadata[?]` and yield empty/wrong buckets.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_aggregate_group_by_metadata_with_filter() {
    let Some((_h, store)) = setup_with_type(VCPU_GTS, &["region"]).await else {
        return;
    };
    let tenant_a = Uuid::from_u128(0x3005);
    let tenant_b = Uuid::from_u128(0x3006);

    // tenant_a: us-east-1 → 2+3=5, eu-west-1 → 7; tenant_b must be filtered out.
    let rows = [
        (tenant_a, "us-east-1", 2_i64, 0_i64),
        (tenant_a, "us-east-1", 3, 1),
        (tenant_a, "eu-west-1", 7, 2),
        (tenant_b, "us-east-1", 100, 3),
    ];
    for (i, (tenant, region, value, ts)) in rows.iter().enumerate() {
        let seq = 0x3005_0000 + u128::try_from(i).unwrap();
        let mut rec = record_at(VCPU_GTS, *tenant, seq, *ts);
        rec.value = Decimal::new(*value, 0);
        let mut meta = BTreeMap::new();
        meta.insert(
            MetadataKey::new("region").expect("valid metadata key"),
            (*region).to_owned(),
        );
        rec.metadata = meta;
        store.create(rec).await.expect("create record");
    }

    let filter = Expr::Compare(
        Box::new(Expr::Identifier("tenant_id".to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(Value::Uuid(tenant_a))),
    );
    let query = ODataQuery::new().with_filter(filter);
    let spec = AggregationSpec {
        op: AggregationOp::Sum,
        group_by: vec![AggregationDimension::Metadata(
            MetadataKey::new("region").expect("valid metadata key"),
        )],
    };
    let result = store
        .aggregate(common::fixture_gts_id(VCPU_GTS), &query, &[], spec)
        .await
        .expect("aggregate group by metadata with filter");

    assert_eq!(
        result.buckets.len(),
        2,
        "one bucket per distinct region in tenant_a"
    );
    let mut got: Vec<(String, Option<BigDecimal>)> = result
        .buckets
        .iter()
        .map(|b| {
            assert_eq!(b.key.len(), 1, "single grouped dimension -> one key entry");
            (b.key[0].clone(), b.value.clone())
        })
        .collect();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        got,
        vec![
            ("eu-west-1".to_owned(), Some(BigDecimal::from(7_i64))),
            ("us-east-1".to_owned(), Some(BigDecimal::from(5_i64))),
        ],
        "tenant_a regions only; tenant_b's us-east-1=100 must not leak in"
    );
}

/// Paginating on a nullable sort key fails loudly when the boundary row's key
/// is `NULL`, instead of minting a cursor that cannot address that row.
///
/// `subject_id` is orderable and keyset-eligible, but a `NULL` has no cursor
/// key. Encoding the page boundary as an empty string would make the follow-up
/// page skip or repeat rows, so the store refuses to build the cursor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_list_refuses_a_cursor_when_the_boundary_sort_key_is_null() {
    use usage_collector_sdk::UsageCollectorPluginError;

    let Some((_h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    let tenant = Uuid::from_u128(0x2004);

    // The fixture leaves `subject_ref` unset, so every row has subject_id NULL.
    for i in 0..3 {
        let seq = 0x2004_0000 + u128::try_from(i).unwrap();
        store
            .create(record_at(VCPU_GTS, tenant, seq, i))
            .await
            .expect("create record");
    }

    let query = ODataQuery::new()
        .with_limit(2)
        .with_order(ODataOrderBy(vec![OrderKey {
            field: "subject_id".to_owned(),
            dir: SortDir::Asc,
        }]));

    let err = store
        .list(common::fixture_gts_id(VCPU_GTS), &query, &[])
        .await
        .expect_err("a NULL boundary sort key must not yield a cursor");
    match err {
        UsageCollectorPluginError::Internal(msg) => assert!(
            msg.contains("subject_id") && msg.contains("no cursor key"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected Internal, got {other:?}"),
    }
}

// ── Filter validation ────────────────────────────────────────────────────────

/// A `$filter` naming a column outside the record allowlist is rejected by both
/// query paths. Ignoring it instead would widen the result set (or the summed
/// set) beyond what the caller asked for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_list_and_aggregate_reject_a_non_allowlisted_filter_field() {
    use usage_collector_sdk::UsageCollectorPluginError;

    let Some((_h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };

    // `version` is a physical column but not an SPI-filterable field.
    let filter = Expr::Compare(
        Box::new(Expr::Identifier("version".to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(Value::String("1".to_owned()))),
    );
    let query = ODataQuery::new().with_filter(filter);

    let err = store
        .list(common::fixture_gts_id(VCPU_GTS), &query, &[])
        .await
        .expect_err("list must reject a non-allowlisted filter field");
    assert!(
        matches!(err, UsageCollectorPluginError::Internal(_)),
        "expected Internal from list, got {err:?}"
    );

    let err = store
        .aggregate(
            common::fixture_gts_id(VCPU_GTS),
            &query,
            &[],
            AggregationSpec {
                op: AggregationOp::Sum,
                group_by: Vec::new(),
            },
        )
        .await
        .expect_err("aggregate must reject a non-allowlisted filter field");
    assert!(
        matches!(err, UsageCollectorPluginError::Internal(_)),
        "expected Internal from aggregate, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Deactivation-before-merge regressions
//
// Deactivation does not UPDATE — it inserts a marker row carrying the same
// sort key (and the same `id`) with `status = 'inactive'` and a higher
// `version`. `status` is therefore the one column whose value differs between
// physical rows of one logical record, and it is reachable from caller input
// as both a `$filter` field and a keyset sort key.
//
// `list` and `aggregate` read under `FINAL`, so a `status` predicate — the
// caller's, the keyset tuple's, or the aggregate's own `status = 'active'` —
// reads the resolved row. If `FINAL` were missing, or a `status` predicate
// were pushed below it (`PREWHERE`), a superseded active row would survive
// next to (or instead of) its marker. These tests pin the observable
// consequence: a deactivated record is never reported as active, and never
// twice. The two "pre-merge" tests stop merges outright so the guarantee is
// tested, not the merge scheduler's timing.
// ---------------------------------------------------------------------------

/// `$filter=status eq 'active'` must not return a deactivated record.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_list_status_filter_excludes_deactivated_record() {
    let Some((_h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    let tenant = Uuid::from_u128(0x2011);

    // Two records; only the first is deactivated.
    let kept = record_at(VCPU_GTS, tenant, 0x2011_0000, 0);
    let dropped = record_at(VCPU_GTS, tenant, 0x2011_0001, 1);
    let kept_id = kept.id;
    let dropped_id = dropped.id;
    store.create(kept).await.expect("create kept record");
    store.create(dropped).await.expect("create dropped record");
    store
        .deactivate(dropped_id)
        .await
        .expect("deactivate the second record");

    let active_filter = Expr::Compare(
        Box::new(Expr::Identifier("status".to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(Value::String("active".to_owned()))),
    );
    let query = ODataQuery::new()
        .with_order(created_at_id_asc())
        .with_filter(active_filter);

    let page = store
        .list(common::fixture_gts_id(VCPU_GTS), &query, &[])
        .await
        .expect("list filtered by status");

    let ids: Vec<Uuid> = page.items.iter().map(|r| r.id).collect();
    assert_eq!(
        ids,
        vec![kept_id],
        "a deactivated record must not survive `status eq 'active'`; returning it would mean \
         the status predicate was applied below the FINAL resolution and matched the superseded raw row"
    );

    // The complementary direction: the marker is what `inactive` matches, and
    // the still-active record must not appear.
    let inactive_filter = Expr::Compare(
        Box::new(Expr::Identifier("status".to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(Value::String("inactive".to_owned()))),
    );
    let inactive_page = store
        .list(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new()
                .with_order(created_at_id_asc())
                .with_filter(inactive_filter),
            &[],
        )
        .await
        .expect("list filtered by inactive status");
    let inactive_ids: Vec<Uuid> = inactive_page.items.iter().map(|r| r.id).collect();
    assert_eq!(
        inactive_ids,
        vec![dropped_id],
        "exactly the deactivated record matches `status eq 'inactive'`"
    );
}

/// A keyset page ordered by `status` sees each record exactly once, at its
/// resolved status — `status` is keyset-safe, so it can reach the keyset
/// predicate as well as the `$filter`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_list_keyset_ordered_by_status_sees_resolved_rows_once() {
    let Some((h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    common::stop_merges(&h).await;
    let tenant = Uuid::from_u128(0x2012);

    let mut all_ids: Vec<Uuid> = Vec::new();
    for i in 0..4 {
        let seq = 0x2012_0000 + u128::try_from(i).unwrap();
        let rec = record_at(VCPU_GTS, tenant, seq, i);
        all_ids.push(rec.id);
        store.create(rec).await.expect("create record");
    }
    // Deactivate half of them, so both statuses are represented and every
    // deactivated key has two physical rows.
    let deactivated: Vec<Uuid> = all_ids.iter().copied().take(2).collect();
    for id in &deactivated {
        store.deactivate(*id).await.expect("deactivate record");
    }

    let order = ODataOrderBy(vec![
        OrderKey {
            field: "status".to_owned(),
            dir: SortDir::Asc,
        },
        OrderKey {
            field: "id".to_owned(),
            dir: SortDir::Asc,
        },
    ]);

    // Page through with a page size smaller than the row count so the keyset
    // predicate is actually exercised.
    let mut seen: Vec<Uuid> = Vec::new();
    let mut cursor: Option<CursorV1> = None;
    loop {
        let mut query = ODataQuery::new().with_limit(2).with_order(order.clone());
        if let Some(c) = cursor.take() {
            query = query.with_cursor(c);
        }
        let page = store
            .list(common::fixture_gts_id(VCPU_GTS), &query, &[])
            .await
            .expect("keyset page ordered by status");
        for item in &page.items {
            let expected_active = !deactivated.contains(&item.id);
            assert_eq!(
                item.status == usage_collector_sdk::UsageRecordStatus::Active,
                expected_active,
                "record {} reported the wrong resolved status",
                item.id
            );
            seen.push(item.id);
        }
        match page.page_info.next_cursor {
            Some(token) => {
                cursor = Some(CursorV1::decode(&token).expect("decodable cursor"));
            }
            None => break,
        }
    }

    seen.sort_unstable();
    let mut expected = all_ids;
    expected.sort_unstable();
    assert_eq!(
        seen, expected,
        "every record appears exactly once across the pages; a duplicate would mean a \
         superseded active row survived next to its marker"
    );
}

/// `aggregate` must not count a deactivated record, before any merge runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_aggregate_excludes_deactivated_record_pre_merge() {
    let Some((h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    common::stop_merges(&h).await;
    let tenant = Uuid::from_u128(0x2013);

    // values are i+1, so 1 + 2 + 3 = 6 before deactivation.
    let mut ids: Vec<Uuid> = Vec::new();
    for i in 0..3 {
        let seq = 0x2013_0000 + u128::try_from(i).unwrap();
        let rec = record_at(VCPU_GTS, tenant, seq, i);
        ids.push(rec.id);
        store.create(rec).await.expect("create record");
    }
    // Drop the value-3 row.
    store
        .deactivate(ids[2])
        .await
        .expect("deactivate the third record");

    let result = store
        .aggregate(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new(),
            &[],
            AggregationSpec {
                op: AggregationOp::Sum,
                group_by: vec![],
            },
        )
        .await
        .expect("aggregate sum");

    let total = result
        .buckets
        .first()
        .expect("one ungrouped bucket")
        .value
        .clone();
    assert_eq!(
        total,
        Some(BigDecimal::from(3)),
        "SUM must net to 1 + 2 and exclude the deactivated value-3 row; counting it (total 6) \
         would mean the deactivation was not resolved before summing, and counting it twice \
         (total 9) would mean \
         the marker itself was summed too"
    );
}

/// `list` with a metadata filter, a `$filter` and a cursor all at once — the
/// bind-order regression guard.
///
/// `list`'s single `WHERE` binds, in text order, the metadata side-channel,
/// every `$filter` conjunct and the keyset tuple. Exercising only one source
/// at a time cannot catch a swap; this test uses all three, with values chosen
/// so a mis-ordered bind returns the wrong rows rather than an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_list_combines_metadata_filter_dollar_filter_and_cursor() {
    let Some((_h, store)) = setup_with_type(VCPU_GTS, &["region"]).await else {
        return;
    };
    let tenant_a = Uuid::from_u128(0x2014_000A);
    let tenant_b = Uuid::from_u128(0x2014_000B);

    // Six rows across two tenants and two regions. Only tenant A + us-east-1
    // matches, and there are three of those so a page size of 2 forces a
    // cursor round-trip with every predicate still applied.
    let mut expected: Vec<Uuid> = Vec::new();
    for (i, (tenant, region)) in [
        (tenant_a, "us-east-1"),
        (tenant_b, "us-east-1"),
        (tenant_a, "eu-west-1"),
        (tenant_a, "us-east-1"),
        (tenant_b, "eu-west-1"),
        (tenant_a, "us-east-1"),
    ]
    .into_iter()
    .enumerate()
    {
        let seq = 0x2014_0000u128 + u128::try_from(i).unwrap();
        let mut rec = record_at(VCPU_GTS, tenant, seq, i64::try_from(i).unwrap());
        let mut meta = BTreeMap::new();
        meta.insert(
            MetadataKey::new("region").expect("valid metadata key"),
            region.to_owned(),
        );
        rec.metadata = meta;
        if tenant == tenant_a && region == "us-east-1" {
            expected.push(rec.id);
        }
        store.create(rec).await.expect("create record");
    }
    assert_eq!(expected.len(), 3, "fixture must seed three matching rows");

    let metadata_filter =
        MetadataFilter::new("region", ["us-east-1"]).expect("valid metadata filter");
    let tenant_filter = Expr::Compare(
        Box::new(Expr::Identifier("tenant_id".to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(Value::Uuid(tenant_a))),
    );

    let mut seen: Vec<Uuid> = Vec::new();
    let mut cursor: Option<CursorV1> = None;
    loop {
        let mut query = ODataQuery::new()
            .with_limit(2)
            .with_order(created_at_id_asc())
            .with_filter(tenant_filter.clone());
        if let Some(c) = cursor.take() {
            query = query.with_cursor(c);
        }
        let page = store
            .list(
                common::fixture_gts_id(VCPU_GTS),
                &query,
                std::slice::from_ref(&metadata_filter),
            )
            .await
            .expect("list with metadata filter, $filter and cursor");

        for item in &page.items {
            assert_eq!(item.tenant_id, tenant_a, "$filter bind landed on tenant_id");
            assert_eq!(
                item.metadata
                    .get(&MetadataKey::new("region").unwrap())
                    .map(String::as_str),
                Some("us-east-1"),
                "metadata bind landed on the region filter"
            );
            seen.push(item.id);
        }
        match page.page_info.next_cursor {
            Some(token) => cursor = Some(CursorV1::decode(&token).expect("decodable cursor")),
            None => break,
        }
    }

    assert_eq!(
        seen, expected,
        "all three matching rows, in created_at order, across the cursor boundary"
    );
}

// ---------------------------------------------------------------------------
// The aggregate's liveness predicate
//
// `aggregate` applies `status = 'active'` (`dedup::ACTIVE_ONLY`) to rows the
// engine has already resolved under `FINAL`. The tests below are the
// correctness guard for that order: `status = 'active'` on the raw rows would
// keep a superseded active row next to its own inactive marker, and `FINAL`
// without the predicate would sum the markers.
//
// `ch_aggregate_group_by_metadata_with_filter` above is the pre-existing
// bind-order guard and must keep passing unchanged — the liveness predicate
// binds nothing of its own.
// ---------------------------------------------------------------------------

/// Every aggregation op must exclude a deactivated record.
///
/// The pre-existing coverage is `SUM` and `COUNT` only. `MIN`/`MAX`/`AVG` also
/// carry the `corrects_id IS NULL` partition clause ahead of the liveness
/// predicate, and `MIN`/`MAX` are the ops that would keep quietly passing if
/// resolution were dropped and only `status = 'active'` remained.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_aggregate_every_op_excludes_a_deactivated_record() {
    let Some((_h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    let tenant = Uuid::from_u128(0x3101);

    // `record_at` sets value = i + 1, so values are 1, 2, 3 at distinct
    // `created_at`s. The value-3 row is the one deactivated.
    let mut last_id = None;
    for i in 0..3 {
        let seq = 0x3101_0000 + u128::try_from(i).unwrap();
        let stored = store
            .create(record_at(VCPU_GTS, tenant, seq, i))
            .await
            .expect("create record");
        last_id = Some(stored.id);
    }
    let deactivated = last_id.expect("three rows were seeded");
    store
        .deactivate(deactivated)
        .await
        .expect("deactivate the value-3 row");

    // Expected over the surviving values {1, 2}.
    let cases = [
        (AggregationOp::Sum, "3"),
        (AggregationOp::Count, "2"),
        (AggregationOp::Min, "1"),
        (AggregationOp::Max, "2"),
        (AggregationOp::Avg, "1.5"),
    ];

    for (op, expected) in cases {
        let spec = AggregationSpec {
            op,
            group_by: Vec::new(),
        };
        let result = store
            .aggregate(
                common::fixture_gts_id(VCPU_GTS),
                &ODataQuery::new(),
                &[],
                spec,
            )
            .await
            .unwrap_or_else(|e| panic!("aggregate {op:?} failed: {e:?}"));

        assert_eq!(
            result.buckets.len(),
            1,
            "empty group_by -> exactly one bucket for {op:?}"
        );
        let actual = result.buckets[0]
            .value
            .as_ref()
            .map_or_else(|| panic!("{op:?} produced no value"), ToString::to_string);
        let actual_num: BigDecimal = actual.parse().expect("numeric aggregate");
        let expected_num: BigDecimal = expected.parse().expect("numeric literal");
        assert_eq!(
            actual_num, expected_num,
            "{op:?} must aggregate only the two active rows (got {actual})"
        );
    }
}

/// Every op must carry full `Decimal128(9)` precision all the way to the caller.
///
/// Two independent hops used to destroy it, and this test covers both:
///
/// 1. **Transport.** `aggregate` reads its result as `JSONEachRow`, and
///    `ClickHouse` writes a Decimal as an *unquoted* JSON number unless
///    `output_format_json_quote_decimals = 1` is set. `serde_json` is built
///    without `arbitrary_precision`, so an unquoted fractional number is stored
///    as an `f64` and `SUM`/`MIN`/`MAX` come back carrying digits that were
///    never in the data — measured without the setting, the `SUM` below loses
///    its last five digits.
/// 2. **Computation.** `ClickHouse`'s `avg()` returns `Float64`, so an `AVG`
///    built on it is already bounded to ~15 significant digits before the
///    result leaves the server, where no output setting can recover it.
///    `agg_select_expr` divides the two exact Decimal aggregates instead.
///
/// Every other aggregate assertion in this file is a whole number (or `1.5`),
/// all of which round-trip through `f64` intact — so this is the only test that
/// can observe either hop. All four non-`COUNT` expectations here need 19
/// significant digits and so discriminate individually.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_aggregate_preserves_full_decimal_precision() {
    let Some((_h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    let tenant = Uuid::from_u128(0x3102);

    // Both values carry 6 fractional digits under a 13-digit integer part — 19
    // significant digits, well beyond what an `f64` holds exactly. The integer
    // part has to be this wide for the `AVG` case to bite: `ROUND(…, 6)` caps
    // the mean's scale, so only the digits left of the point can push it past
    // `f64`'s ~16-digit reach.
    let mut low = record_at(VCPU_GTS, tenant, 0x3102_0001, 0);
    low.value = Decimal::new(1_234_567_890_123_456_789, 6); // 1234567890123.456789
    store.create(low).await.expect("create the low record");

    let mut high = record_at(VCPU_GTS, tenant, 0x3102_0002, 1);
    high.value = Decimal::new(7_654_321_098_765_432_101, 6); // 7654321098765.432101
    store.create(high).await.expect("create the high record");

    let cases = [
        (AggregationOp::Sum, "8888888988888.888890"),
        (AggregationOp::Count, "2"),
        (AggregationOp::Min, "1234567890123.456789"),
        (AggregationOp::Max, "7654321098765.432101"),
        // The mean is exactly 4444444494444.444445 — the sum halves without a
        // remainder, so `ROUND(…, 6)` has nothing to round and the expectation
        // is rounding-mode independent. Measured, an `avg()`-based AVG returns
        // 4444444494444.444 here: the last three digits are gone before the
        // result leaves the server.
        (AggregationOp::Avg, "4444444494444.444445"),
    ];

    for (op, expected) in cases {
        let spec = AggregationSpec {
            op,
            group_by: Vec::new(),
        };
        let result = store
            .aggregate(
                common::fixture_gts_id(VCPU_GTS),
                &ODataQuery::new(),
                &[],
                spec,
            )
            .await
            .unwrap_or_else(|e| panic!("aggregate {op:?} failed: {e:?}"));

        assert_eq!(
            result.buckets.len(),
            1,
            "empty group_by -> exactly one bucket for {op:?}"
        );
        let actual = result.buckets[0]
            .value
            .as_ref()
            .map_or_else(|| panic!("{op:?} produced no value"), ToString::to_string);
        // `BigDecimal` equality is scale-normalized, so a quoted `Decimal128(9)`
        // rendering its trailing zeros (`2.000000000`) still equals `2`.
        let actual_num: BigDecimal = actual.parse().expect("numeric aggregate");
        let expected_num: BigDecimal = expected.parse().expect("numeric literal");
        assert_eq!(
            actual_num, expected_num,
            "{op:?} must keep every significant digit (got {actual})"
        );
    }
}

/// A group whose only record is deactivated must disappear entirely, not
/// survive as a zero-valued bucket.
///
/// The survivor predicate removes the deactivated row before the caller's
/// `GROUP BY` runs, so its group is never formed — as opposed to being formed
/// and then emptied.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_aggregate_grouped_drops_a_group_whose_only_record_is_deactivated() {
    let Some((_h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    let tenant = Uuid::from_u128(0x3102);

    // Two records with distinct `resource_id`s, so each is its own group.
    let kept = common::fixture_usage_record_at(
        VCPU_GTS,
        tenant,
        "idem-kept",
        Decimal::new(7, 0),
        common::fixture_created_at_offset(0),
    );
    let mut dropped = common::fixture_usage_record_at(
        VCPU_GTS,
        tenant,
        "idem-dropped",
        Decimal::new(9, 0),
        common::fixture_created_at_offset(1),
    );
    dropped.resource_ref =
        usage_collector_sdk::ResourceRef::new("res-dropped".to_owned(), "vm".to_owned())
            .expect("valid resource ref");
    let kept_resource = kept.resource_ref.resource_id().to_owned();
    let dropped_id = dropped.id;

    store.create(kept).await.expect("create kept record");
    store.create(dropped).await.expect("create dropped record");
    store
        .deactivate(dropped_id)
        .await
        .expect("deactivate the dropped record");

    let spec = AggregationSpec {
        op: AggregationOp::Sum,
        group_by: vec![AggregationDimension::ResourceId],
    };
    let result = store
        .aggregate(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new(),
            &[],
            spec,
        )
        .await
        .expect("aggregate grouped by resource_id");

    assert_eq!(
        result.buckets.len(),
        1,
        "the deactivated record's group must vanish, not appear with value 0: {:?}",
        result.buckets
    );
    assert_eq!(result.buckets[0].key, vec![kept_resource]);
    assert_eq!(result.buckets[0].value, Some(BigDecimal::from(7_i64)));
}

/// A caller `status` conjunct in the aggregate, end to end.
///
/// It lands in the single `WHERE` after the liveness predicate. Combining it
/// with a metadata side-channel filter puts the side-channel binds ahead of
/// its own, so a bind-order regression surfaces here.
///
/// `status eq 'inactive'` conflicts with the liveness predicate's
/// `status = 'active'` by construction, so the correct answer is "no active row
/// matched".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_aggregate_with_a_status_filter_exercises_the_trailing_conjunct() {
    let Some((_h, store)) = setup_with_type(VCPU_GTS, &["region"]).await else {
        return;
    };
    let tenant = Uuid::from_u128(0x3103);

    let mut record = common::fixture_usage_record_at(
        VCPU_GTS,
        tenant,
        "idem-outer-where",
        Decimal::new(5, 0),
        common::fixture_created_at_offset(0),
    );
    record.metadata = BTreeMap::from([(
        MetadataKey::new("region".to_owned()).expect("valid metadata key"),
        "us-east-1".to_owned(),
    )]);
    store.create(record).await.expect("create record");

    let inactive_filter = Expr::Compare(
        Box::new(Expr::Identifier("status".to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(Value::String("inactive".to_owned()))),
    );
    let metadata_filter =
        vec![MetadataFilter::new("region", ["us-east-1"]).expect("valid metadata filter")];

    let spec = AggregationSpec {
        op: AggregationOp::Sum,
        group_by: Vec::new(),
    };
    let result = store
        .aggregate(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new().with_filter(inactive_filter),
            &metadata_filter,
            spec,
        )
        .await
        .expect("aggregate with a status filter and a metadata filter");

    // Ungrouped, so still exactly one bucket — with nothing summed into it.
    assert_eq!(result.buckets.len(), 1, "empty group_by -> one bucket");
    let summed = result.buckets[0]
        .value
        .as_ref()
        .map_or_else(|| "0".to_owned(), ToString::to_string);
    let summed: BigDecimal = summed.parse().expect("numeric aggregate");
    assert_eq!(
        summed,
        BigDecimal::from(0_i64),
        "`status eq 'inactive'` cannot match a row the survivor predicate already \
         restricted to active; a non-zero sum here means the trailing conjunct or \
         its bind landed in the wrong place"
    );
}

/// An ungrouped aggregate must always produce exactly one bucket, including
/// when no resolved row is active.
///
/// The gateway depends on this shape: an aggregate over an empty survivor set
/// must still yield its single (null-valued) bucket.
///
/// Every op is exercised because this is the one scope where `AVG`'s expression
/// can fail outright rather than merely return the wrong number: it divides
/// `SUM(value)` by `COUNT(*)`, and an empty survivor set makes that denominator
/// zero. `nullIf` is what turns the resulting `ILLEGAL_DIVISION` into the absent
/// value the bucket is contracted to carry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_aggregate_over_only_deactivated_records_yields_one_ungrouped_bucket() {
    let Some((_h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    let tenant = Uuid::from_u128(0x3104);

    for i in 0..2 {
        let seq = 0x3104_0000 + u128::try_from(i).unwrap();
        let stored = store
            .create(record_at(VCPU_GTS, tenant, seq, i))
            .await
            .expect("create record");
        store
            .deactivate(stored.id)
            .await
            .expect("deactivate every seeded record");
    }

    for op in [
        AggregationOp::Sum,
        AggregationOp::Count,
        AggregationOp::Min,
        AggregationOp::Max,
        AggregationOp::Avg,
    ] {
        let spec = AggregationSpec {
            op,
            group_by: Vec::new(),
        };
        let result = store
            .aggregate(
                common::fixture_gts_id(VCPU_GTS),
                &ODataQuery::new(),
                &[],
                spec,
            )
            .await
            .unwrap_or_else(|e| {
                panic!("an aggregate over no surviving group must still succeed for {op:?}: {e:?}")
            });

        assert_eq!(
            result.buckets.len(),
            1,
            "an ungrouped aggregate always emits exactly one bucket for {op:?}, even when no \
             resolved row is active: {:?}",
            result.buckets
        );
    }
}

/// An unfiltered `list` shows a deactivated record exactly once, as inactive,
/// before any merge runs.
///
/// This is `list`'s `FINAL` resolution end to end: the marker is the resolved
/// row of the deactivated record, its superseded active twin is collapsed into
/// it, and the two untouched records resolve to themselves. The raw table
/// meanwhile holds both physical rows for the deactivated id, which is what
/// makes the assertion meaningful.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_list_shows_a_deactivated_row_once_as_inactive_before_merge() {
    let Some((h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    common::stop_merges(&h).await;
    let tenant = Uuid::from_u128(0x3105);

    let mut ids: Vec<Uuid> = Vec::new();
    for i in 0..3 {
        let seq = 0x3105_0000 + u128::try_from(i).unwrap();
        let rec = record_at(VCPU_GTS, tenant, seq, i);
        ids.push(rec.id);
        store.create(rec).await.expect("create record");
    }
    let deactivated = ids[1];
    store
        .deactivate(deactivated)
        .await
        .expect("deactivate the middle record");
    assert_eq!(
        common::raw_rows_for_id(&h, deactivated).await,
        2,
        "precondition: the marker and its source row both exist physically"
    );

    let page = store
        .list(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new().with_order(created_at_id_asc()),
            &[],
        )
        .await
        .expect("unfiltered list");

    let listed: Vec<Uuid> = page.items.iter().map(|r| r.id).collect();
    assert_eq!(
        listed, ids,
        "every logical record appears exactly once, in order; a fourth row would be the \
         superseded active twin leaking past the FINAL resolution"
    );
    for item in &page.items {
        let expected = if item.id == deactivated {
            usage_collector_sdk::UsageRecordStatus::Inactive
        } else {
            usage_collector_sdk::UsageRecordStatus::Active
        };
        assert_eq!(item.status, expected, "record {} status", item.id);
    }
}

/// `EXPLAIN PIPELINE` of the SQL the store actually sent, pulled back out of
/// `system.query_log`.
///
/// The `clickhouse` crate substitutes binds client-side, so the logged text is
/// executable; only the trailing `FORMAT …` the crate appends is stripped,
/// because `EXPLAIN` takes its own.
async fn explain_pipeline_of_last(
    h: &common::ChHarness,
    must_contain: &str,
    must_not_contain: &str,
) -> Vec<String> {
    // Narrowed to this test's database: the server is shared, so the log
    // carries every sibling test's statements too.
    let sql = "SELECT query FROM system.query_log \
               WHERE type = 'QueryFinish' AND query_kind = 'Select' \
                 AND current_database = currentDatabase() \
                 AND positionCaseInsensitive(query, 'usage_records') > 0 \
                 AND position(query, ?) > 0 AND position(query, ?) = 0 \
               ORDER BY event_time_microseconds DESC LIMIT 1";
    let mut logged: Option<String> = None;
    for _ in 0..20 {
        h.client
            .query("SYSTEM FLUSH LOGS")
            .execute()
            .await
            .expect("flushing system logs must succeed");
        if let Ok(Some(q)) = h
            .client
            .query(sql)
            .bind(must_contain)
            .bind(must_not_contain)
            .fetch_optional::<String>()
            .await
        {
            logged = Some(q);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let logged = logged.expect("the store's SELECT must appear in system.query_log");
    let body = logged
        .rfind(" FORMAT ")
        .map_or(logged.as_str(), |i| &logged[..i]);
    h.client
        .query(&format!("EXPLAIN PIPELINE {body}"))
        .fetch_all::<String>()
        .await
        .unwrap_or_else(|e| panic!("EXPLAIN PIPELINE failed for {body}: {e:?}"))
}

/// `(query, do_not_merge_across_partitions_select_final, optimize_move_to_prewhere_if_final)`
/// of the SQL the store last sent, from `system.query_log`: the text proves
/// `FINAL` is in the statement, the `Settings` map that the per-query settings
/// reached the server (it holds only values that differ from the server
/// default, so `optimize_move_to_prewhere_if_final = 0` — the default — may
/// come back empty).
async fn logged_select_of_last(
    h: &common::ChHarness,
    must_contain: &str,
    must_not_contain: &str,
) -> (String, String, String) {
    let sql = "SELECT query, Settings['do_not_merge_across_partitions_select_final'], \
                      Settings['optimize_move_to_prewhere_if_final'] \
               FROM system.query_log \
               WHERE type = 'QueryFinish' AND query_kind = 'Select' \
                 AND current_database = currentDatabase() \
                 AND positionCaseInsensitive(query, 'usage_records') > 0 \
                 AND position(query, ?) > 0 AND position(query, ?) = 0 \
               ORDER BY event_time_microseconds DESC LIMIT 1";
    for _ in 0..20 {
        h.client
            .query("SYSTEM FLUSH LOGS")
            .execute()
            .await
            .expect("flushing system logs must succeed");
        if let Ok(Some(row)) = h
            .client
            .query(sql)
            .bind(must_contain)
            .bind(must_not_contain)
            .fetch_optional::<(String, String, String)>()
            .await
        {
            return row;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the store's SELECT must appear in system.query_log");
}

/// Both range reads resolve versions in the engine, pinned at the plan level:
/// `EXPLAIN PIPELINE` shows the `ReplacingSorted` step `FINAL` adds, and no
/// resolution construct of the store's own — no `LimitBy`, no marker-subquery
/// set (`CreatingSets`), no second `Aggregating`, no full sort. The statement
/// text carries `FINAL` and the per-query `FINAL` read settings reach the
/// server.
///
/// `ClickHouse` 25.6 reads parts whose primary-key ranges do not intersect
/// without the merge step at all (that is `FINAL` costing nothing on
/// non-overlapping inserts), so the table is seeded with a raw twin — two
/// parts sharing one sort key, merges stopped — to make the step appear.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_read_pipelines_resolve_versions_with_final() {
    let Some((h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    common::stop_merges(&h).await;
    let tenant = Uuid::from_u128(0x3106);
    let mut first: Option<UsageRecord> = None;
    for i in 0..3 {
        let seq = 0x3106_0000 + u128::try_from(i).unwrap();
        let rec = record_at(VCPU_GTS, tenant, seq, i);
        if i == 0 {
            first = Some(rec.clone());
        }
        store.create(rec).await.expect("create record");
    }
    let first = first.expect("seeded");
    let twin = common::raw_row(&first, common::raw_version_for_id(&h, first.id).await + 1);
    common::seed_raw_row(&h, &twin).await;
    assert_eq!(
        common::raw_rows_for_id(&h, first.id).await,
        2,
        "precondition"
    );
    // A tenant filter makes the logged text unique to this test and exercises
    // the pushed-down key-prefix predicate under `FINAL`.
    let tenant_filter = Expr::Compare(
        Box::new(Expr::Identifier("tenant_id".to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(Value::Uuid(tenant))),
    );

    store
        .aggregate(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new().with_filter(tenant_filter.clone()),
            &[],
            AggregationSpec {
                op: AggregationOp::Sum,
                group_by: vec![AggregationDimension::ResourceId],
            },
        )
        .await
        .expect("aggregate");
    let agg = explain_pipeline_of_last(&h, &tenant.to_string(), "ORDER BY").await;
    let agg_text = agg.join("\n");
    assert!(
        agg_text.contains("ReplacingSorted"),
        "aggregate must resolve versions in the engine (FINAL):\n{agg_text}"
    );
    assert!(
        !agg_text.contains("LimitBy") && !agg_text.contains("CreatingSets"),
        "aggregate must carry no resolution construct of its own:\n{agg_text}"
    );
    assert!(
        !agg_text.contains("MergeSortingTransform"),
        "aggregate must not sort the scanned rows:\n{agg_text}"
    );
    assert!(
        agg.iter().filter(|l| l.trim() == "(Aggregating)").count() <= 1,
        "only the caller's own GROUP BY may aggregate:\n{agg_text}"
    );
    let (agg_sql, agg_dnm, agg_prewhere) =
        logged_select_of_last(&h, &tenant.to_string(), "ORDER BY").await;
    assert!(agg_sql.contains("FROM usage_records FINAL"), "{agg_sql}");
    assert_eq!(agg_dnm, "1", "partition-local FINAL must reach the server");
    assert!(
        agg_prewhere.is_empty() || agg_prewhere == "0",
        "{agg_prewhere:?}"
    );

    store
        .list(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new()
                .with_order(created_at_id_asc())
                .with_filter(tenant_filter),
            &[],
        )
        .await
        .expect("list");
    let list = explain_pipeline_of_last(&h, "ORDER BY", "AS agg").await;
    let list_text = list.join("\n");
    assert!(
        list_text.contains("ReplacingSorted"),
        "list must resolve versions in the engine (FINAL):\n{list_text}"
    );
    assert!(
        !list_text.contains("LimitBy") && !list_text.contains("CreatingSets"),
        "list must carry no resolution construct of its own:\n{list_text}"
    );
    assert!(
        !list_text.contains("Aggregating"),
        "list must not aggregate:\n{list_text}"
    );
    // With `gts_id` and `tenant_id` pinned by equalities and the order on the
    // remaining key prefix `(created_at, id)`, ClickHouse reads in key order
    // under `FINAL` too and feeds the resolved stream straight into the LIMIT
    // instead of sorting the tenant. A one-element `tenant_id IN (?)` would
    // defeat this, which is why the translator renders it as an equality.
    assert!(
        list_text.contains("algorithm: InOrder"),
        "the list must read in sorting-key order:\n{list_text}"
    );
    assert!(
        !list_text.contains("MergeSortingTransform"),
        "no full sort of the scanned rows may remain:\n{list_text}"
    );
    let (list_sql, list_dnm, list_prewhere) = logged_select_of_last(&h, "ORDER BY", "AS agg").await;
    assert!(list_sql.contains("FROM usage_records FINAL"), "{list_sql}");
    assert_eq!(list_dnm, "1", "partition-local FINAL must reach the server");
    assert!(
        list_prewhere.is_empty() || list_prewhere == "0",
        "{list_prewhere:?}"
    );
}

// ---------------------------------------------------------------------------
// Physical duplicates versus API results
//
// Concurrent instances, a retry after a timeout, or an async flush whose
// engine-side dedup token did not take can store the same logical record
// twice, as two physical rows under one sort key that differ only in
// `version`. The tests below seed such rows directly (no token, merges
// stopped) and pin what the API promises regardless: `get` returns the
// highest version, `list` shows the record once — on one page — and
// `aggregate` counts it once. Highest version wins, on every read.
// ---------------------------------------------------------------------------

/// Two physical rows for one record, in two parts: every read resolves them
/// to the higher version before it pages, limits or aggregates.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_duplicate_physical_rows_are_collapsed_by_every_read() {
    let Some((h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    common::stop_merges(&h).await;
    let tenant = Uuid::from_u128(0x3201);
    let a = record_at(VCPU_GTS, tenant, 0x3201_0000, 0); // value 1
    let b = record_at(VCPU_GTS, tenant, 0x3201_0001, 1); // value 2
    let (a_id, b_id) = (a.id, b.id);
    store.create(a.clone()).await.expect("create a");
    store.create(b).await.expect("create b");

    // A's twin: the same identity, a higher version, and a value only the
    // twin has, so every assertion can tell which row a read returned.
    let mut twin = common::raw_row(&a, common::raw_version_for_id(&h, a_id).await + 1);
    twin.value = Decimal::new(10, 0);
    common::seed_raw_row(&h, &twin).await;
    assert_eq!(
        common::raw_rows_for_id(&h, a_id).await,
        2,
        "precondition: both physical rows exist"
    );

    assert_eq!(
        store.get(a_id).await.expect("get").value,
        Decimal::new(10, 0),
        "get resolves to the highest version"
    );

    let page = store
        .list(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new().with_order(created_at_id_asc()),
            &[],
        )
        .await
        .expect("list");
    let listed: Vec<Uuid> = page.items.iter().map(|r| r.id).collect();
    assert_eq!(
        listed,
        vec![a_id, b_id],
        "each logical record once, in order"
    );
    assert_eq!(page.items[0].value, Decimal::new(10, 0), "the resolved row");

    // Paging: the look-ahead LIMIT counts logical rows, so the twin cannot
    // shorten a page, repeat A on the next one, or fake a third page.
    let first = store
        .list(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new()
                .with_limit(1)
                .with_order(created_at_id_asc()),
            &[],
        )
        .await
        .expect("first page");
    assert_eq!(
        first.items.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![a_id]
    );
    let token = first.page_info.next_cursor.expect("a second page exists");
    let second = store
        .list(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new()
                .with_limit(1)
                .with_order(created_at_id_asc())
                .with_cursor(CursorV1::decode(&token).expect("decode cursor")),
            &[],
        )
        .await
        .expect("second page");
    assert_eq!(
        second.items.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![b_id]
    );
    assert!(
        second.page_info.next_cursor.is_none(),
        "no phantom third page from the twin"
    );

    for (op, want) in [
        (AggregationOp::Count, 2_i64),
        (AggregationOp::Sum, 12),
        (AggregationOp::Max, 10),
        (AggregationOp::Min, 2),
    ] {
        let result = store
            .aggregate(
                common::fixture_gts_id(VCPU_GTS),
                &ODataQuery::new(),
                &[],
                AggregationSpec {
                    op,
                    group_by: Vec::new(),
                },
            )
            .await
            .expect("aggregate");
        assert_eq!(
            result.buckets[0].value,
            Some(BigDecimal::from(want)),
            "{op:?} must see the resolved row once"
        );
    }
}

/// Both versions of one record inside a single level-0 part — the shape a
/// block stored with `optimize_on_insert = 0` has — in a partition holding no
/// other part. `do_not_merge_across_partitions_select_final` must still run
/// the resolution over that lone part rather than pass its rows through.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_duplicates_inside_a_lone_part_are_collapsed() {
    let Some((h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    common::stop_merges(&h).await;
    let tenant = Uuid::from_u128(0x3202);
    let a = record_at(VCPU_GTS, tenant, 0x3202_0000, 0); // value 1 at the base instant
    // B lives 40 days later, i.e. in another month, so A's partition holds
    // exactly the one raw part seeded below.
    let b = record_at(VCPU_GTS, tenant, 0x3202_0001, 40 * 24 * 3600);
    let (a_id, b_id) = (a.id, b.id);
    store.create(b).await.expect("create b");

    let base = common::raw_version_for_id(&h, b_id).await;
    let older = common::raw_row(&a, base + 1);
    let mut newer = common::raw_row(&a, base + 2);
    newer.value = Decimal::new(10, 0);
    common::seed_raw_rows(&h, &[older, newer], false).await;
    assert_eq!(common::raw_rows_for_id(&h, a_id).await, 2, "precondition");
    let parts_in_a: u64 = h
        .client
        .query(
            "SELECT count() FROM system.parts WHERE database = currentDatabase() \
             AND table = 'usage_records' AND active AND rows = 2",
        )
        .fetch_one()
        .await
        .expect("system.parts");
    assert_eq!(parts_in_a, 1, "precondition: both versions share one part");

    assert_eq!(
        store.get(a_id).await.expect("get").value,
        Decimal::new(10, 0)
    );
    let page = store
        .list(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new().with_order(created_at_id_asc()),
            &[],
        )
        .await
        .expect("list");
    let listed: Vec<Uuid> = page.items.iter().map(|r| r.id).collect();
    assert_eq!(listed, vec![a_id, b_id]);
    assert_eq!(page.items[0].value, Decimal::new(10, 0));
    let result = store
        .aggregate(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new(),
            &[],
            AggregationSpec {
                op: AggregationOp::Count,
                group_by: Vec::new(),
            },
        )
        .await
        .expect("aggregate");
    assert_eq!(result.buckets[0].value, Some(BigDecimal::from(2)));
}

/// A deactivation marker outranks an older active twin that lands late — and,
/// because the one rule is highest-version-wins, a *newer* active twin would
/// outrank the marker. The write path never mints such a row for a record it
/// has seen deactivated (`version_higher_than`), so the second half pins the
/// documented residual: a concurrent create whose version exceeds the marker's
/// resurrects the record.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_marker_outranks_an_older_active_twin_and_highest_version_wins() {
    let Some((h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    common::stop_merges(&h).await;
    let tenant = Uuid::from_u128(0x3203);
    let a = record_at(VCPU_GTS, tenant, 0x3203_0000, 0); // value 1
    let a_id = a.id;
    store.create(a.clone()).await.expect("create a");
    store.deactivate(a_id).await.expect("deactivate a");
    let marker_version = common::raw_version_for_id(&h, a_id).await;

    let mut older = common::raw_row(&a, marker_version - 1);
    older.value = Decimal::new(7, 0);
    common::seed_raw_row(&h, &older).await;
    assert_eq!(common::raw_rows_for_id(&h, a_id).await, 3, "precondition");

    let active_filter = Expr::Compare(
        Box::new(Expr::Identifier("status".to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(Value::String("active".to_owned()))),
    );
    let page = store
        .list(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new().with_order(created_at_id_asc()),
            &[],
        )
        .await
        .expect("list");
    assert_eq!(page.items.len(), 1, "once, not three times");
    assert_eq!(
        page.items[0].status,
        usage_collector_sdk::UsageRecordStatus::Inactive,
        "the marker outranks the older twin"
    );
    let active = store
        .list(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new()
                .with_order(created_at_id_asc())
                .with_filter(active_filter.clone()),
            &[],
        )
        .await
        .expect("list active");
    assert!(
        active.items.is_empty(),
        "the older active twin must not resurface"
    );
    let count = store
        .aggregate(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new(),
            &[],
            AggregationSpec {
                op: AggregationOp::Count,
                group_by: Vec::new(),
            },
        )
        .await
        .expect("count");
    assert_eq!(count.buckets[0].value, Some(BigDecimal::from(0)));

    // The documented residual: a newer active version wins over the marker.
    let mut newer = common::raw_row(&a, marker_version + 1);
    newer.value = Decimal::new(9, 0);
    common::seed_raw_row(&h, &newer).await;
    assert_eq!(
        store.get(a_id).await.expect("get").status,
        usage_collector_sdk::UsageRecordStatus::Active
    );
    let active = store
        .list(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new()
                .with_order(created_at_id_asc())
                .with_filter(active_filter),
            &[],
        )
        .await
        .expect("list active");
    assert_eq!(active.items.len(), 1);
    assert_eq!(active.items[0].value, Decimal::new(9, 0));
    let sum = store
        .aggregate(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new(),
            &[],
            AggregationSpec {
                op: AggregationOp::Sum,
                group_by: Vec::new(),
            },
        )
        .await
        .expect("sum");
    assert_eq!(sum.buckets[0].value, Some(BigDecimal::from(9)));
}

/// Tenant isolation under `FINAL`: a duplicate for one tenant changes nothing
/// another tenant's filtered reads see, and the duplicated tenant's reads
/// resolve it once. `tenant_id` arrives only through `$filter`, so this is the
/// authorization scope the gateway sends, end to end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_tenant_isolation_survives_final() {
    let Some((h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    common::stop_merges(&h).await;
    let (t1, t2) = (Uuid::from_u128(0x3204), Uuid::from_u128(0x3205));
    // The same idempotency keys and instants for both tenants: the ids differ
    // only through `tenant_id`.
    let mut t1_ids = Vec::new();
    let mut t2_ids = Vec::new();
    for i in 0..2 {
        let r1 = record_at(VCPU_GTS, t1, 0x3204_0000 + u128::try_from(i).unwrap(), i);
        let r2 = record_at(VCPU_GTS, t2, 0x3204_0000 + u128::try_from(i).unwrap(), i);
        t1_ids.push(r1.id);
        t2_ids.push(r2.id);
        if i == 0 {
            let mut twin = common::raw_row(&r1, 0);
            store.create(r1).await.expect("create t1");
            twin.version = common::raw_version_for_id(&h, t1_ids[0]).await + 1;
            twin.value = Decimal::new(50, 0);
            common::seed_raw_row(&h, &twin).await;
        } else {
            store.create(r1).await.expect("create t1");
        }
        store.create(r2).await.expect("create t2");
    }
    assert_eq!(
        common::raw_rows_for_id(&h, t1_ids[0]).await,
        2,
        "precondition"
    );

    let tenant_filter = |t: Uuid| {
        Expr::Compare(
            Box::new(Expr::Identifier("tenant_id".to_owned())),
            CompareOperator::Eq,
            Box::new(Expr::Value(Value::Uuid(t))),
        )
    };
    for (tenant, ids, sum) in [(t1, &t1_ids, 52_i64), (t2, &t2_ids, 3)] {
        let page = store
            .list(
                common::fixture_gts_id(VCPU_GTS),
                &ODataQuery::new()
                    .with_order(created_at_id_asc())
                    .with_filter(tenant_filter(tenant)),
                &[],
            )
            .await
            .expect("list");
        assert_eq!(&page.items.iter().map(|r| r.id).collect::<Vec<_>>(), ids);
        assert!(page.items.iter().all(|r| r.tenant_id == tenant));
        let result = store
            .aggregate(
                common::fixture_gts_id(VCPU_GTS),
                &ODataQuery::new().with_filter(tenant_filter(tenant)),
                &[],
                AggregationSpec {
                    op: AggregationOp::Sum,
                    group_by: Vec::new(),
                },
            )
            .await
            .expect("sum");
        assert_eq!(result.buckets[0].value, Some(BigDecimal::from(sum)));
    }
}

/// Every shape a caller can give a `status` predicate — equality, an `OR`
/// with another column, `NOT`, `IN` — sits in the single `WHERE` and reads
/// the resolved status under `FINAL`, before any merge. The aggregate's own
/// liveness predicate composes with the caller's conjunct the same way.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_status_filter_shapes_read_the_resolved_status_under_final() {
    let Some((h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        return;
    };
    common::stop_merges(&h).await;
    let tenant = Uuid::from_u128(0x3206);
    let mut ids: Vec<Uuid> = Vec::new();
    for i in 0..3 {
        let rec = record_at(
            VCPU_GTS,
            tenant,
            0x3206_0000 + u128::try_from(i).unwrap(),
            i,
        );
        ids.push(rec.id);
        store.create(rec).await.expect("create record");
    }
    store
        .deactivate(ids[1])
        .await
        .expect("deactivate the middle record");
    assert_eq!(common::raw_rows_for_id(&h, ids[1]).await, 2, "precondition");

    let status = |s: &str| {
        Expr::Compare(
            Box::new(Expr::Identifier("status".to_owned())),
            CompareOperator::Eq,
            Box::new(Expr::Value(Value::String(s.to_owned()))),
        )
    };
    let id_eq = |id: Uuid| {
        Expr::Compare(
            Box::new(Expr::Identifier("id".to_owned())),
            CompareOperator::Eq,
            Box::new(Expr::Value(Value::Uuid(id))),
        )
    };
    let cases: Vec<(&str, Expr, Vec<Uuid>)> = vec![
        ("status eq 'active'", status("active"), vec![ids[0], ids[2]]),
        ("status eq 'inactive'", status("inactive"), vec![ids[1]]),
        (
            "status eq 'inactive' or id eq <third>",
            Expr::Or(Box::new(status("inactive")), Box::new(id_eq(ids[2]))),
            vec![ids[1], ids[2]],
        ),
        (
            "not (status eq 'active')",
            Expr::Not(Box::new(status("active"))),
            vec![ids[1]],
        ),
        (
            "status in ('active', 'inactive')",
            Expr::In(
                Box::new(Expr::Identifier("status".to_owned())),
                vec![
                    Expr::Value(Value::String("active".to_owned())),
                    Expr::Value(Value::String("inactive".to_owned())),
                ],
            ),
            ids.clone(),
        ),
    ];
    for (name, filter, want) in cases {
        let page = store
            .list(
                common::fixture_gts_id(VCPU_GTS),
                &ODataQuery::new()
                    .with_order(created_at_id_asc())
                    .with_filter(filter),
                &[],
            )
            .await
            .unwrap_or_else(|e| panic!("list with `{name}`: {e:?}"));
        let got: Vec<Uuid> = page.items.iter().map(|r| r.id).collect();
        assert_eq!(
            got, want,
            "`{name}`: each logical record once, at its resolved status"
        );
    }

    for (name, filter, want) in [
        ("status eq 'active'", status("active"), 2_i64),
        ("status eq 'inactive'", status("inactive"), 0),
        (
            "not (status eq 'inactive')",
            Expr::Not(Box::new(status("inactive"))),
            2,
        ),
    ] {
        let result = store
            .aggregate(
                common::fixture_gts_id(VCPU_GTS),
                &ODataQuery::new().with_filter(filter),
                &[],
                AggregationSpec {
                    op: AggregationOp::Count,
                    group_by: Vec::new(),
                },
            )
            .await
            .unwrap_or_else(|e| panic!("count with `{name}`: {e:?}"));
        assert_eq!(
            result.buckets[0].value,
            Some(BigDecimal::from(want)),
            "`{name}`: the liveness predicate and the caller conjunct both read the resolved row"
        );
    }
}

/// The empty-set projection must select no rows while retaining the engine's
/// normal empty-input aggregate result, and must not suppress a live OR branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_empty_authorization_set_preserves_list_and_aggregate_semantics() {
    let Some((_h, store)) = setup_with_type(VCPU_GTS, &[]).await else {
        panic!("Docker is required for this explicitly selected integration test");
    };
    let allowed = Uuid::from_u128(0x5852_000A);
    let excluded = Uuid::from_u128(0x5852_000B);
    let mut expected_ids = Vec::new();
    for (i, tenant, value) in [(0, allowed, 4), (1, allowed, 6), (2, excluded, 100)] {
        let mut record = record_at(
            VCPU_GTS,
            tenant,
            0x5852_0000 + u128::try_from(i).unwrap(),
            i,
        );
        record.value = Decimal::new(value, 0);
        if tenant == allowed {
            expected_ids.push(record.id);
        }
        store.create(record).await.expect("seed both tenants");
    }
    let tenant_eq = |id| {
        Expr::Compare(
            Box::new(Expr::Identifier("tenant_id".into())),
            CompareOperator::Eq,
            Box::new(Expr::Value(Value::Uuid(id))),
        )
    };
    let eq = tenant_eq(Uuid::nil());
    let empty = tenant_eq(excluded).and(eq.clone().and(Expr::Not(Box::new(eq))));
    for with_alternative in [false, true] {
        let filter = if with_alternative {
            empty.clone().or(tenant_eq(allowed))
        } else {
            empty.clone()
        };
        let query = ODataQuery::new()
            .with_filter(filter)
            .with_order(created_at_id_asc());
        let page = store
            .list(common::fixture_gts_id(VCPU_GTS), &query, &[])
            .await
            .expect("list with an empty authorization branch");
        let ids: Vec<_> = page.items.iter().map(|record| record.id).collect();
        assert_eq!(
            ids,
            if with_alternative {
                expected_ids.clone()
            } else {
                vec![]
            }
        );
        assert!(page.page_info.next_cursor.is_none());

        // Compare against an ordinary tenant scope with the same survivor
        // set, so the assertion preserves each backend's aggregate semantics.
        let reference = ODataQuery::new().with_filter(tenant_eq(if with_alternative {
            allowed
        } else {
            Uuid::from_u128(0x5852_000C)
        }));
        for op in [
            AggregationOp::Sum,
            AggregationOp::Count,
            AggregationOp::Min,
            AggregationOp::Max,
            AggregationOp::Avg,
        ] {
            for group_by in [vec![], vec![AggregationDimension::TenantId]] {
                let spec = AggregationSpec { op, group_by };
                let expected = store
                    .aggregate(
                        common::fixture_gts_id(VCPU_GTS),
                        &reference,
                        &[],
                        spec.clone(),
                    )
                    .await
                    .expect("reference aggregate");
                let actual = store
                    .aggregate(common::fixture_gts_id(VCPU_GTS), &query, &[], spec)
                    .await
                    .expect("aggregate with an empty authorization branch");
                assert_eq!(
                    actual, expected,
                    "OR alternative={with_alternative}, op={op:?}"
                );
                if with_alternative && matches!(op, AggregationOp::Sum) {
                    assert_eq!(actual.buckets[0].value, Some(BigDecimal::from(10)));
                }
            }
        }
    }
}
