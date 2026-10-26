//! `m0001_init`: the plugin's two tables, `usage_type_catalog` and
//! `usage_records`.
//!
//! The tables are declared once, by their logical names; the migrator renders
//! them for the deployment's topology (`table` module docs). Startup
//! validation of an operator-provisioned cluster (`pool::check_cluster_schema`)
//! compares the live tables against the keys declared here.
//!
//! Every statement is `CREATE TABLE IF NOT EXISTS`, so a change here does not
//! reach a table that already exists: ship it as a new step (`ALTER TABLE …`).
//!
//! # Constraints `ClickHouse` does not have
//!
//! No `PRIMARY KEY` / `UNIQUE`, no `ON CONFLICT`, no `FOREIGN KEY`:
//! - uniqueness of `usage_type_catalog.gts_id` is the create sequence's
//!   application-level pre-existence check (DESIGN.md §3.6);
//! - `usage_records` dedup is emulated (SELECT then INSERT), the engine's
//!   `insert_deduplication_token` window catches the racing retries the
//!   pre-read misses, and `ReplacingMergeTree` convergence is the backstop
//!   (DESIGN.md §3.6 Ingest Dedup step 8);
//! - `usage_records.gts_id` is a soft reference to the catalog, checked in
//!   application code on both sides: an insert-time existence read of
//!   `usage_type_catalog`, and, on the delete side, `delete_usage_type`'s
//!   capped reference probe plus a post-delete sweep
//!   (`ALTER TABLE … DELETE WHERE gts_id = ?`) for rows that landed inside its
//!   probe→delete window (DESIGN.md §3.6). That emulation is not race-free: an
//!   insert whose existence check passed before the catalog row was removed
//!   can commit after the sweep and orphan a row. The window is bounded and
//!   instrumented (`uc_clickhouse_orphaned_reference_detected_total`), not
//!   closed; deleting a usage type while ingest for it is in flight is an
//!   operational error the plugin cannot prevent.

use async_trait::async_trait;

use super::table::Table;
use super::{ChMigration, SchemaManager};
use crate::infra::storage::ddl::SkipIndex;
use crate::infra::storage::pool::INSERT_DEDUP_WINDOW_BLOCKS;
use crate::infra::storage::query::dedup::RECORD_DEDUP_KEY;
use crate::infra::storage::topology::{
    CATALOG_SHARDING_KEY, CATALOG_TABLE, RECORDS_SHARDING_KEY, RECORDS_TABLE,
};

pub(super) struct M0001Init;

#[async_trait]
impl ChMigration for M0001Init {
    fn name(&self) -> &'static str {
        "m0001_init"
    }

    async fn up(&self, m: &SchemaManager<'_>) -> anyhow::Result<()> {
        m.create_table(&catalog()).await?;
        m.create_table(&records()).await
    }
}

// ── usage_type_catalog ───────────────────────────────────────────────────────

/// `usage_type_catalog`.
///
/// Engine: `ReplacingMergeTree(version)` — the row with the highest `version`
/// wins on merge, and on the read-time resolution every `SELECT` applies. This
/// resolves the create sequence's own race window: two concurrent
/// `create_usage_type` calls for the same `gts_id` may both pass the
/// pre-existence check and insert; convergence collapses the duplicate
/// physical rows, keeping whichever insert's version is higher.
///
/// Rows are deleted by the plugin: `delete_usage_type` issues an
/// `ALTER TABLE … DELETE WHERE gts_id = ?` under `mutations_sync`
/// (DESIGN.md §3.6). A heavyweight mutation rather than a lightweight
/// `DELETE FROM`, and deliberately not a versioned "deleted" marker row: the
/// mutation removes the physical rows, so a re-create of the same `gts_id` has
/// no surviving higher-version copy to outrank. That is why this table needs
/// no tombstone flag. The table is unpartitioned and tiny, so the part rewrite
/// the mutation costs is trivial.
///
/// On a cluster it is sharded by [`CATALOG_SHARDING_KEY`]; it is tiny and
/// read by `gts_id`.
fn catalog() -> Table {
    Table::new(CATALOG_TABLE)
        .column(
            "gts_id",
            "String",
            "GTS usage-type identifier; sorting-key column (closest ClickHouse analog of a primary key)",
        )
        // Discriminants must match `entity::UsageTypeKindCode`.
        .column(
            "kind",
            "Enum8('counter' = 1, 'gauge' = 2)",
            "Counter or gauge classification; stored verbatim",
        )
        .column(
            "metadata_fields",
            "Array(String)",
            "Closed list of allowed metadata key names; stored verbatim",
        )
        .column(
            "version",
            "UInt64",
            "ReplacingMergeTree version column; higher value wins on merge / FINAL resolution",
        )
        .version("version")
        .order_by(CATALOG_ORDER_BY)
        .sharding_key(CATALOG_SHARDING_KEY)
}

/// `usage_type_catalog` sort key: a single column for point lookups on
/// `gts_id`. The table carries no TTL and no time column, so it is not
/// partitioned.
pub const CATALOG_ORDER_BY: &[&str] = &["gts_id"];

// ── usage_records ────────────────────────────────────────────────────────────

/// `usage_records`.
///
/// Engine: `ReplacingMergeTree(version)`, as for the catalog. A new versioned
/// row with `status = inactive`, a higher `version` and the same `id` emulates
/// deactivation (no in-place `UPDATE`; DESIGN.md §3.6 Deactivation Cascade).
/// Read-time handling differs by read shape (`query::dedup`):
/// - point reads (get, the cascade's own read, the create-path dedup lookups)
///   resolve the highest version explicitly with
///   `ORDER BY version DESC LIMIT 1 BY <sort key>` over a bloom-filter-pruned
///   candidate set;
/// - range reads (list, aggregate) do not resolve versions: they scan raw rows
///   and exclude the ids that carry an inactive marker
///   (`id NOT IN (SELECT id … WHERE … AND status = 'inactive')`), which is
///   exact for deactivation before any merge and costs one hash probe per row
///   instead of a sort or hash aggregation over the whole scan.
///
/// The insert dedup window keeps the last [`INSERT_DEDUP_WINDOW_BLOCKS`]
/// inserted blocks' `insert_deduplication_token`s (every insert carries one,
/// see `record_store::insert_dedup_token`), so a racing retry of the same rows
/// is dropped before it becomes a part. `ClickHouse` enforces the token on
/// synchronous inserts; on asynchronous inserts only `Replicated*` engines do,
/// so on a single node with `async_insert` on (the plugin default) a duplicate
/// single-record create that lands in a different flush than its twin is
/// visible to list/aggregate until the merge.
///
/// On a cluster it is sharded by [`RECORDS_SHARDING_KEY`]. This is required,
/// not a tuning choice: a deactivation marker clones its source row and a
/// compensation row belongs to the tenant it corrects, so every row an
/// `id NOT IN (SELECT id … status = 'inactive')` anti-join must see lives on
/// the same shard — which is what lets the plugin run those queries with
/// `distributed_product_mode = 'local'` (`topology` module docs).
///
/// The TTL expires rows after the configured retention, measured on
/// `created_at` directly (`table::ttl_expr`). Expiry is whole-partition only
/// ([`RECORDS_PARTITION_KEY`], `ttl_only_drop_parts = 1`), so the window is a
/// lower bound on a row's lifetime rather than an exact deadline.
fn records() -> Table {
    Table::new(RECORDS_TABLE)
        .column(
            "id",
            "UUID",
            "Deterministic gateway-derived record id (UUIDv5 of the 4-tuple dedup key); ADR-0013 / ADR-0014",
        )
        .column("tenant_id", "UUID", "Owning tenant; second ORDER BY column")
        .column(
            "gts_id",
            "String",
            "Usage type; leading ORDER BY column, pinned by every request-path read; application-enforced reference to usage_type_catalog (no FK in ClickHouse)",
        )
        .column("value", "Decimal128(9)", "Signed delta")
        .column(
            "created_at",
            "DateTime64(6)",
            "Event time; third ORDER BY column for time-range scan locality",
        )
        .column("resource_id", "String", "Resource instance identifier")
        .column("resource_type", "String", "Resource type discriminator")
        .column("subject_id", "Nullable(String)", "Optional subject identifier")
        .column(
            "subject_type",
            "Nullable(String)",
            "Optional subject type discriminator",
        )
        .column("idempotency_key", "String", "Caller idempotency key")
        .column(
            "corrects_id",
            "Nullable(UUID)",
            "Set on a compensation row; references the corrected ordinary-usage row",
        )
        // Discriminants must match `entity::UsageRecordStatusCode`.
        .column(
            "status",
            "Enum8('active' = 1, 'inactive' = 2)",
            "Lifecycle status; transitions are new versioned rows, never in-place UPDATE",
        )
        .column(
            "metadata",
            "Map(String, String)",
            "Caller metadata; Map(String, String) chosen over JSON for efficient metadata[key] push-down at query time",
        )
        .column("ingested_at", "DateTime64(6)", "Server insert timestamp")
        .column(
            "version",
            "UInt64",
            "ReplacingMergeTree version column; higher value wins on merge / FINAL resolution",
        )
        .indexes(&RECORDS_INDEXES)
        .version("version")
        .partition_by(RECORDS_PARTITION_KEY)
        .order_by(RECORDS_ORDER_BY)
        .ttl_delete_after("created_at")
        .setting("ttl_only_drop_parts", &1)
        .insert_dedup_window(INSERT_DEDUP_WINDOW_BLOCKS)
        .sharding_key(RECORDS_SHARDING_KEY)
}

/// `usage_records` sort key: `(gts_id, tenant_id, created_at, id)`.
///
/// `id` is the deterministic `UUIDv5` projection of the canonical dedup tuple
/// (ADR-0013 / ADR-0014), so this sort key is one-to-one with that tuple. The
/// column order is chosen so that:
/// 1. `gts_id` leads, because every request-path read pins it: it is a typed
///    SPI parameter on both list and aggregate, whereas `tenant_id` and
///    `created_at` arrive only through the optional `OData` `$filter`. Behind a
///    leading high-cardinality `tenant_id`, a `gts_id = ?` read falls back to
///    `ClickHouse`'s generic exclusion search, which prunes effectively only
///    when the preceding key column has low cardinality; against a UUID it
///    read every granule. `gts_id` is also the low-cardinality column of the
///    four, so leading with it compresses the primary index better.
/// 2. The dominant read pattern (type + tenant + time-range scans for
///    aggregation / list) is a sort-key-aligned range scan over the
///    `(gts_id, tenant_id, created_at)` prefix.
/// 3. The dedup lookup resolves against that same three-column prefix as a
///    primary-key range rather than a full scan.
///
/// Permuting these four columns does not change what `ReplacingMergeTree`
/// collapses on: the engine's row identity is the sort key as a *set* of
/// columns, the `LIMIT 1 BY` resolution fragment (`query::dedup`) is
/// order-insensitive too, and the marker anti-join keys on `id` alone. Only
/// adding or removing a column would change which rows resolve together.
///
/// The dedup lookup itself keys on the canonical tuple
/// `(tenant_id, gts_id, created_at, idempotency_key)`, not on `id` — see
/// `record_store::DedupKey`. `idempotency_key` is deliberately absent from the
/// sort key: adding it would change what `ReplacingMergeTree` collapses on,
/// and the three-column prefix already prunes to a handful of rows, over which
/// `idempotency_key` applies as a cheap residual filter. Keying the lookup on
/// `id` instead would miss a stored row whose `id` disagrees with its own
/// tuple and re-insert it under an idempotency key already in use.
///
/// Neither `ORDER BY` nor `PARTITION BY` can be altered in place, so a
/// deployment provisioned before either changed keeps what it has until the
/// table is rebuilt (`CREATE` new + `INSERT SELECT` + `EXCHANGE TABLES`).
/// Every variant is correct; the older ones are only slower.
pub const RECORDS_ORDER_BY: &[&str] = RECORD_DEDUP_KEY;

/// `usage_records` partition key: monthly on `created_at`, the TTL column.
///
/// This buys two things on a table written in event-time order and expired by
/// TTL:
/// - Time-range reads prune whole partitions before the primary index is
///   consulted. This composes with, rather than replaces, the sort-key prefix:
///   it prunes a `created_at` range even when the read pins no `gts_id` or
///   `tenant_id`, and it needs the `created_at` predicate to reach the scan,
///   which is what the `$filter` split in `query::dedup` is for.
/// - TTL expiry becomes a metadata-only partition drop rather than a merge
///   that rewrites every column of a part to remove its expired rows.
///   `ttl_only_drop_parts = 1` makes that the only mode: a part is dropped
///   once all of its rows have expired and is never partially rewritten. The
///   cost is that a row outlives the configured window by up to the span of
///   its own partition; `pool::ensure_retention_ttl` warns when
///   `retention_period_secs` is short enough for that overshoot to matter.
///
/// Monthly is chosen against the 1-year default retention: about 13 live
/// partitions, the right order of magnitude (a `MergeTree` starts paying for
/// part count in the hundreds of partitions, and a single partition would give
/// TTL nothing to drop). A much shorter retention would prefer a finer key,
/// but `PARTITION BY` is fixed at `CREATE` while retention is config, so the
/// DDL commits to the default's scale.
///
/// Partitioning weakens `INSERT` atomicity, which the deactivation cascade
/// depends on: `ClickHouse` commits one part per partition an insert touches,
/// so a statement spanning two months is two commits. See the ATOMICITY NOTE
/// on `deactivate` in `record_store` for the window this opens.
pub const RECORDS_PARTITION_KEY: &str = "toYYYYMM(created_at)";

/// `usage_records` data-skipping indexes.
///
/// Two request-path predicates do not lead with the sort-key prefix and would
/// otherwise scan every granule as the table grows:
/// - `get_usage_record` → `WHERE id = ?`;
/// - the deactivation cascade → `WHERE id = ? OR (corrects_id = ? AND status = …)`.
///
/// `id` is the trailing sort-key column and `corrects_id` is not in the sort
/// key at all, so both get a bloom filter. Neither predicate is served by
/// reordering the sort key: skip indexes prune granules for reads that cannot
/// use the key prefix at all, and they never affect which rows resolve
/// together.
///
/// Adding an index here does not reach a table that already exists: that
/// needs an `ALTER TABLE usage_records ADD INDEX …` (plus `MATERIALIZE INDEX`
/// for existing parts) as a migration step.
pub const RECORDS_INDEXES: [SkipIndex; 2] = [
    SkipIndex {
        name: "idx_records_id",
        expr: "id",
        kind: "bloom_filter",
        granularity: 1,
    },
    SkipIndex {
        name: "idx_records_corrects_id",
        expr: "corrects_id",
        kind: "bloom_filter",
        granularity: 1,
    },
];

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "m0001_init_tests.rs"]
mod m0001_init_tests;
