//! Version resolution for `ReplacingMergeTree` reads.
//!
//! Both plugin tables are `ReplacingMergeTree(version)`, so several physical
//! rows can share one sort key until a background merge collapses them to the
//! highest `version`. Every read must resolve that *before* it filters on a
//! version-dependent column, pages, limits, or aggregates — otherwise a
//! superseded row is returned, counted, or paged as if it were current. This
//! module offers the two tools the reads use:
//!
//! - **Explicit resolution** ([`latest_by`], [`LATEST_ONE`]) for *point* reads
//!   whose candidate set is already tiny and index-pruned — `get`, the
//!   deactivation cascade's read, the create-path dedup lookups, and the
//!   catalog. `ORDER BY version DESC LIMIT 1 BY <key>` over a handful of rows
//!   is cheaper than `FINAL`, which would merge every part the key range
//!   touches to answer a one-row question.
//! - **`FINAL`** for the *range* reads — `list` and `aggregate` — which read
//!   `FROM usage_records FINAL` with the settings in [`FINAL_READ_SETTINGS`].
//!   The engine collapses each sort key to its highest-`version` row before
//!   the `WHERE`, `ORDER BY` / `LIMIT` and `GROUP BY` run, so the whole query
//!   is one level with one `WHERE`: a caller `status` predicate, the keyset
//!   tuple, and the aggregate's own [`ACTIVE_ONLY`] all read the resolved
//!   value, and the look-ahead `LIMIT` counts logical rows.
//!
//! ## Physical duplicates versus API results
//!
//! Nothing in this plugin issues an `UPDATE`. A second physical row for one
//! sort key is one of:
//!
//! - a **deactivation marker** — [`make_inactive_marker`] clones the source
//!   row and changes only `status` (→ `inactive`) and `version` (→ higher,
//!   minted by `mapper::version_higher_than` off the row it supersedes). It
//!   keeps the source row's `id`, `tenant_id` and `created_at`; or
//! - a **duplicate create** — two uncoordinated creates for one dedup key
//!   (concurrent plugin instances, a retry after a timeout, an async-insert
//!   flush whose engine-side dedup token did not take) both write, storing the
//!   same deterministic `id` under different `version`s.
//!
//! Both are allowed on disk. What the API returns is resolved: a logical record
//! appears at most once in a `list` page, is counted once by `aggregate`, and
//! `get` returns its highest version. **Highest version wins** — that is the
//! only rule, on every read. It makes a marker always win over the create it
//! supersedes, and the later of two concurrent creates win over the earlier.
//! It does *not* make the first writer win: two concurrent submissions with
//! conflicting payloads under one idempotency key can both land, and the
//! higher version is what every read then shows. `IdempotencyConflict` is
//! raised only when the earlier row is already visible at pre-read time.
//!
//! ## Why `FINAL` is exact here
//!
//! `ReplacingMergeTree` resolves by the full `ORDER BY` tuple
//! ([`RECORD_DEDUP_KEY`]), and every version of a logical row shares it:
//! markers clone the source row, duplicate creates derive the same `id` from
//! the same dedup tuple. From that one fact follow the two placements `FINAL`
//! needs:
//!
//! - **Same partition.** `created_at` is in the key and the table is
//!   `PARTITION BY toYYYYMM(created_at)`, so all versions share a partition and
//!   the background merge can collapse them. That is what makes
//!   `do_not_merge_across_partitions_select_final = 1` exact: each partition is
//!   resolved on its own, and a partition holding a single already-merged part
//!   is read without the merge step at all. The single-node DDL pins the
//!   partition key, and the cluster startup check refuses a local table whose
//!   partition key deviates (`pool::check_cluster_schema`).
//! - **Same shard.** On a cluster the `Distributed` table shards by
//!   `cityHash64(tenant_id)`, which every version shares, so each shard holds
//!   every version of its rows. `FINAL` is forwarded to the shard-local
//!   `ReplicatedReplacingMergeTree` query, where it is therefore complete; the
//!   initiator only merges resolved per-shard results.
//!
//! `optimize_move_to_prewhere_if_final = 0` (the server default, pinned so a
//! settings profile cannot flip it) keeps every `WHERE` conjunct *above* the
//! merge step. `PREWHERE` filters per part before the collapse, which for
//! `status` would keep a superseded active row whose marker the filter removed.
//! Predicates on sorting-key columns are pushed into the read regardless, and
//! that is safe: they are identical across all versions of a key.
//!
//! Rule for the point reads that resolve with [`latest_by`]: a `status`
//! predicate must **not** be applied to the scan *below* the resolution step —
//! it would retain a superseded active row while discarding its own marker.
//! The deactivation cascade's read applies it above the step.
//!
//! ## What resolution does not do
//!
//! `FINAL` resolves the versions that are present. It does not fill in a row
//! that has not committed yet, nor one that has not reached the replica being
//! read: a pre-read can miss an in-flight write, and read-your-writes holds on
//! the replica that committed the row, not on a lagging one. The engine-side
//! `insert_deduplication_token` every `usage_records` `INSERT` carries is a
//! safeguard against write amplification where the engine honours it; the
//! reads here are correct whether or not it did.
//!
//! [`make_inactive_marker`]: crate::infra::storage::mapper::make_inactive_marker

/// Suffix resolving each sort key to its highest-`version` row: `ORDER BY
/// version DESC LIMIT 1 BY <keys>`.
///
/// `keys` must name the columns identifying one logical row — the table's
/// `ORDER BY` tuple, or the subset of it left unpinned by the query's `WHERE`.
/// Rendered with a leading space so it appends directly to a query body.
///
/// Callers that also need a presentation order put it *outside* this suffix
/// (in an enclosing query), because `ORDER BY version DESC` is what makes
/// `LIMIT 1 BY` pick the resolved row.
#[must_use]
pub fn latest_by(keys: &[&str]) -> String {
    format!(" ORDER BY version DESC LIMIT 1 BY {}", keys.join(", "))
}

/// Suffix resolving a single expected sort key: `ORDER BY version DESC LIMIT 1`.
///
/// Only for reads whose `WHERE` pins the whole sort key (so at most one logical
/// row can match) — `usage_type_catalog` point reads, whose sort key is
/// `(gts_id)` alone. A read that can span sort keys must use [`latest_by`]
/// instead, or it may return a row that is not its own key's resolved version.
pub const LATEST_ONE: &str = " ORDER BY version DESC LIMIT 1";

/// The `usage_records` sort key — the tuple `ReplacingMergeTree` collapses on.
///
/// The schema's `ORDER BY` is this constant (`migrator::m0001_init::RECORDS_ORDER_BY`), so
/// the helpers here and the DDL cannot drift apart. The *set* of columns is
/// the row identity; their order is not (`LIMIT 1 BY` is order-insensitive).
/// The order is chosen for the primary index; see `migrator::m0001_init::RECORDS_ORDER_BY`.
pub const RECORD_DEDUP_KEY: &[&str] = &["gts_id", "tenant_id", "created_at", "id"];

/// The `usage_records` table expression the range reads scan: the logical
/// table name with the `FINAL` modifier.
///
/// The logical name is the same on a single node and on a cluster (where it is
/// the `Distributed` table); `FINAL` is forwarded to the shard-local tables.
pub const RECORDS_FINAL: &str = "usage_records FINAL";

/// The aggregate's liveness predicate over resolved rows: `status = 'active'`.
///
/// Sound only after resolution — below `FINAL` it would keep a superseded
/// active row and drop its marker. A bare literal with no placeholder, so it
/// adds nothing to a caller's bind list.
pub const ACTIVE_ONLY: &str = "status = 'active'";

/// Per-query settings attached to every `FINAL` read, as `(name, value)`.
///
/// * `do_not_merge_across_partitions_select_final = 1` — resolve each
///   partition independently (and skip the merge step for a partition that
///   holds one already-merged part). Exact by the partition invariant in the
///   module docs; a sizeable share of `FINAL`'s cost otherwise.
/// * `optimize_move_to_prewhere_if_final = 0` — the server default, pinned so
///   a settings profile cannot move a `status` conjunct into `PREWHERE`, below
///   the merge step (module docs).
///
/// Attached per query rather than on the shared client: they are meaningless
/// on inserts, DDL and the `LIMIT 1 BY` point reads, and a client-wide
/// `do_not_merge_across_partitions_select_final` would silently apply to any
/// future `FINAL` over the unpartitioned catalog.
pub const FINAL_READ_SETTINGS: &[(&str, &str)] = &[
    ("do_not_merge_across_partitions_select_final", "1"),
    ("optimize_move_to_prewhere_if_final", "0"),
];

/// Attach [`FINAL_READ_SETTINGS`] to a query that reads [`RECORDS_FINAL`].
pub fn with_final_read_settings(mut query: clickhouse::query::Query) -> clickhouse::query::Query {
    for (name, value) in FINAL_READ_SETTINGS {
        query = query.with_setting(*name, *value);
    }
    query
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "dedup_tests.rs"]
mod dedup_tests;
