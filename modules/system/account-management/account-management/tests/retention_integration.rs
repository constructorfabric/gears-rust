//! Integration tests for the retention pipeline against a real `SQLite` / Postgres
//! database.
//!
//! These tests are **ignored by default** until the AM integration-test scaffold
//! (real DB connection, migration runner, module initialisation) is in place.
//! See `feature-tenant-hierarchy-management.md` retention § for the test
//! requirements and tracking issue for the scaffold.
//!
//! ## What these tests must cover once the scaffold lands
//!
//! 1. **`scan_retention_due` ordering** — seed rows at multiple depths with
//!    `deleted_at` values that straddle the retention window boundary.
//!    Assert the returned Vec is sorted by the canonical contract
//!    `(depth DESC, deleted_at ASC, id ASC)` (the same column sequence
//!    pinned by `apply_retention_leaf_first_order` and the
//!    `retention_scan_orders_leaf_first` snapshot test) and that rows whose
//!    `deleted_at + retention_window > now` are excluded.
//!
//! 2. **`is_due` SQL vs Rust parity** — insert a row whose `deleted_at` is
//!    exactly `now - retention_window`. The SQL predicate and the Rust
//!    `is_due(now, deleted_at, retention)` check must both return `true`.
//!
//! 3. **Claim-lock atomicity** — start two concurrent `scan_retention_due` calls
//!    on the same batch. Assert each row appears in exactly one of the two result
//!    sets (no double-processing).
//!
//! 4. **Default vs per-row retention window** — insert one row with
//!    `retention_window_secs = NULL` (uses module default) and one with an
//!    explicit override. Assert each row becomes due at the correct wall-clock
//!    time.
//!
//! 5. **Leaf-first FK guard (Postgres only)** — insert a parent and a child
//!    both past their retention window. Run `hard_delete_batch`. Assert the
//!    child row is removed first and the parent succeeds in the same tick
//!    without a FK violation. Postgres-only: the `SQLite` migration variant
//!    deliberately omits FK clauses (`modkit-db` does not enable
//!    `PRAGMA foreign_keys`), so on `SQLite` the test should only assert
//!    leaf-first deletion ordering, not an FK rejection.
//!
//! 6. **Parent-starvation regression** — seed N+1 due rows where N parents have
//!    `deleted_at` older than one due leaf. Each parent has at least one
//!    Deleted-state child still present (so `hard_delete_one` defers it via
//!    the child-existence guard). Call `scan_retention_due(limit = N)`.
//!    Assert the leaf appears in the returned batch — the SQL must select
//!    leaf-first under the canonical contract
//!    `(depth DESC, deleted_at ASC, id ASC)`, not
//!    `deleted_at ASC` first, otherwise the older parents fill the LIMIT
//!    window every tick and the leaf starves indefinitely. The unit-side
//!    snapshot at `infra/storage/repo_impl/retention.rs::tests::retention_scan_orders_leaf_first`
//!    pins the ORDER BY column sequence; this integration test is the
//!    end-to-end version that exercises `LIMIT` interaction with
//!    `hard_delete_one`'s defer-on-children semantics.

/// Placeholder — replace with real integration tests once the AM
/// integration-test scaffold is ready.
#[test]
#[ignore = "AM integration-test scaffold not yet in place; see feature-tenant-hierarchy-management.md retention § and tracking issue for scan_retention_due SQL coverage"]
fn scan_retention_due_integration_scaffold_pending() {}
