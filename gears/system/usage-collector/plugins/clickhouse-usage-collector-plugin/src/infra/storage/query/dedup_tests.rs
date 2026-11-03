use super::{
    ACTIVE_ONLY, FINAL_READ_SETTINGS, LATEST_ONE, RECORD_DEDUP_KEY, RECORDS_FINAL, latest_by,
    with_final_read_settings,
};

// ── latest_by ────────────────────────────────────────────────────────────────

#[test]
fn latest_by_renders_single_key() {
    assert_eq!(
        latest_by(&["gts_id"]),
        " ORDER BY version DESC LIMIT 1 BY gts_id"
    );
}

#[test]
fn latest_by_renders_full_record_key() {
    assert_eq!(
        latest_by(RECORD_DEDUP_KEY),
        " ORDER BY version DESC LIMIT 1 BY gts_id, tenant_id, created_at, id"
    );
}

#[test]
fn latest_by_starts_with_space_so_it_appends_to_a_query_body() {
    // Call sites concatenate this straight onto `… WHERE x = ?` with no
    // separator of their own.
    assert!(latest_by(&["id"]).starts_with(' '));
    assert!(LATEST_ONE.starts_with(' '));
}

#[test]
fn latest_by_orders_version_descending() {
    // The DESC is the whole point: `LIMIT 1 BY` takes the first row per key in
    // the ordered stream, so ASC would resolve to the *superseded* row.
    assert!(latest_by(&["id"]).contains("version DESC"));
    assert!(LATEST_ONE.contains("version DESC"));
}

// ── RECORD_DEDUP_KEY ─────────────────────────────────────────────────────────

#[test]
fn record_dedup_key_is_pinned() {
    // The schema's ORDER BY is this constant, so changing it changes the sort
    // key of fresh tables — but not of existing ones, which `ORDER BY` cannot
    // be altered on. That tuple is what the engine collapses on, so a column
    // added or dropped here silently resolves the wrong row on old tables.
    assert_eq!(
        RECORD_DEDUP_KEY,
        &["gts_id", "tenant_id", "created_at", "id"]
    );
}

// ── FINAL range reads ────────────────────────────────────────────────────────

/// The range reads scan the logical table under `FINAL`; the name is the same
/// on a single node and through the cluster's `Distributed` table.
#[test]
fn records_final_is_the_logical_table_under_final() {
    assert_eq!(RECORDS_FINAL, "usage_records FINAL");
}

/// The liveness predicate is a bare literal: no `?`, no parentheses, nothing a
/// caller has to bind for.
#[test]
fn active_only_is_a_bare_literal_with_no_placeholder() {
    assert_eq!(ACTIVE_ONLY, "status = 'active'");
    assert!(!ACTIVE_ONLY.contains('?'));
}

/// The two settings a `FINAL` read must carry: partition-local resolution (exact
/// by the partition invariant) and no `PREWHERE` under `FINAL` (which would
/// filter `status` per part, below the merge step).
#[test]
fn final_read_settings_pin_partition_locality_and_no_prewhere() {
    assert_eq!(
        FINAL_READ_SETTINGS,
        &[
            ("do_not_merge_across_partitions_select_final", "1"),
            ("optimize_move_to_prewhere_if_final", "0"),
        ]
    );
}

/// `with_final_read_settings` must not panic on a fresh query and must return
/// a handle the caller can keep binding against (the settings ride on the
/// query, not the client).
#[test]
fn with_final_read_settings_returns_a_bindable_query() {
    let client = clickhouse::Client::default().with_url("http://127.0.0.1:1");
    let q = with_final_read_settings(client.query("SELECT 1 FROM usage_records FINAL WHERE a = ?"));
    drop(q.bind(1));
}
