use super::*;

// --- Filter / scope tests ---

#[test]
fn build_query_empty_filter_pg() {
    let filter = DeadLetterFilter::default();
    let (sql, values) = build_select_query(DbBackend::Postgres, &filter);
    assert!(sql.contains("d.status = $1"));
    assert_eq!(values.len(), 1);
}

#[test]
fn build_query_partition_filter_pg() {
    let filter = DeadLetterFilter::default().partition(42);
    let (sql, values) = build_select_query(DbBackend::Postgres, &filter);
    assert!(sql.contains("partition_id = $1"));
    assert!(sql.contains("d.status = $2"));
    assert_eq!(values.len(), 2);
}

#[test]
fn build_query_all_fields_pg() {
    let filter = DeadLetterFilter::default()
        .partition(1)
        .queue("orders")
        .payload_type("order.created")
        .limit(10);
    let (sql, values) = build_select_query(DbBackend::Postgres, &filter);
    assert!(sql.contains("$1")); // partition_id
    assert!(sql.contains("$2")); // queue
    assert!(sql.contains("$3")); // payload_type
    assert!(sql.contains("$4")); // status
    assert!(sql.contains("LIMIT 10"));
    assert_eq!(values.len(), 4);
}

#[test]
fn build_query_mysql_uses_question_marks() {
    let filter = DeadLetterFilter::default().partition(1).queue("q");
    let (sql, values) = build_select_query(DbBackend::MySql, &filter);
    assert!(sql.contains('?'));
    assert!(!sql.contains('$'));
    assert_eq!(values.len(), 3); // partition_id, queue, status
}

#[test]
fn scope_payload_type_filter() {
    let scope = DeadLetterScope::default().payload_type("order.created");
    let mut qb = QueryBuilder::new("SELECT 1 FROM t d", DbBackend::Postgres);
    apply_scope(&mut qb, &scope);
    let (sql, values) = qb.finish_no_order(None);
    assert!(sql.contains("d.payload_type = $1"));
    assert_eq!(values.len(), 1);
}

#[test]
fn filter_by_resolved_status() {
    let filter = DeadLetterFilter::default().status(DeadLetterStatus::Resolved);
    let (sql, _) = build_select_query(DbBackend::Postgres, &filter);
    assert!(sql.contains("d.status = $1"));
}

#[test]
fn filter_by_reprocessing_status() {
    let filter = DeadLetterFilter::default().status(DeadLetterStatus::Reprocessing);
    let (sql, values) = build_select_query(DbBackend::Postgres, &filter);
    assert!(sql.contains("d.status = $1"));
    assert_eq!(values.len(), 1);
}

#[test]
fn filter_no_status() {
    let filter = DeadLetterFilter::default().any_status();
    let (sql, values) = build_select_query(DbBackend::Postgres, &filter);
    // Column list contains d.status, but WHERE clause should not filter on it
    assert!(!sql.contains("d.status = $"));
    assert!(values.is_empty());
}

// --- Count ---

#[test]
fn count_query_has_no_order_by() {
    let filter = DeadLetterFilter::default();
    let (sql, _) = build_count_query(DbBackend::Postgres, &filter);
    assert!(sql.contains("COUNT(*)"));
    assert!(!sql.contains("ORDER BY"));
}

// --- Replay ---

#[test]
fn replay_query_includes_orphan_recovery() {
    let scope = DeadLetterScope::default();
    let (sql, _) = build_replay_select(DbBackend::Postgres, &scope);
    assert!(sql.contains("d.status = 'pending'"));
    assert!(sql.contains("d.status = 'reprocessing'"));
    assert!(sql.contains("d.deadline < now()"));
}

#[test]
fn replay_query_pg_has_for_update() {
    let scope = DeadLetterScope::default();
    let (sql, _) = build_replay_select(DbBackend::Postgres, &scope);
    assert!(sql.contains("FOR UPDATE SKIP LOCKED"));
}

#[test]
fn replay_query_mysql_has_for_update() {
    let scope = DeadLetterScope::default();
    let (sql, _) = build_replay_select(DbBackend::MySql, &scope);
    assert!(sql.contains("FOR UPDATE SKIP LOCKED"));
}

#[test]
fn replay_query_sqlite_no_for_update() {
    let scope = DeadLetterScope::default();
    let (sql, _) = build_replay_select(DbBackend::Sqlite, &scope);
    assert!(!sql.contains("FOR UPDATE"));
}

#[test]
fn replay_claim_sets_deadline() {
    let sql = build_batch_claim(DbBackend::Postgres, 2);
    assert!(sql.contains("status = 'reprocessing'"));
    assert!(sql.contains("deadline = now()"));
    assert!(sql.contains("$1 * INTERVAL '1 second'"));
    assert!(sql.contains("$2"));
    assert!(sql.contains("$3"));
}

#[test]
fn replay_claim_mysql() {
    let sql = build_batch_claim(DbBackend::MySql, 1);
    assert!(sql.contains("DATE_ADD(CURRENT_TIMESTAMP(6), INTERVAL ? SECOND)"));
}

#[test]
fn replay_claim_sqlite() {
    let sql = build_batch_claim(DbBackend::Sqlite, 1);
    assert!(sql.contains("datetime('now', '+' || $1 || ' seconds')"));
}

// --- Resolve / Reject ---

#[test]
fn resolve_sql_per_backend() {
    for backend in [DbBackend::Postgres, DbBackend::MySql, DbBackend::Sqlite] {
        let sql = build_batch_resolve(backend, 2);
        assert!(sql.contains("status = 'resolved'"));
        assert!(sql.contains("AND status = 'reprocessing'"));
        assert!(sql.contains("deadline = NULL"));
    }
}

#[test]
fn resolve_uses_db_now() {
    let sql = build_batch_resolve(DbBackend::Postgres, 1);
    assert!(sql.contains("completed_at = now()"));
    let sql = build_batch_resolve(DbBackend::MySql, 1);
    assert!(sql.contains("completed_at = CURRENT_TIMESTAMP(6)"));
    let sql = build_batch_resolve(DbBackend::Sqlite, 1);
    assert!(sql.contains("completed_at = datetime('now')"));
}

#[test]
fn reject_sql_per_backend() {
    for backend in [DbBackend::Postgres, DbBackend::MySql, DbBackend::Sqlite] {
        let sql = build_batch_reject(backend, 2);
        assert!(sql.contains("status = 'pending'"));
        assert!(sql.contains("attempts = attempts + 1"));
        assert!(sql.contains("AND status = 'reprocessing'"));
        assert!(sql.contains("deadline = NULL"));
    }
}

#[test]
fn reject_uses_db_now() {
    let sql = build_batch_reject(DbBackend::Postgres, 1);
    assert!(sql.contains("failed_at = now()"));
    let sql = build_batch_reject(DbBackend::MySql, 1);
    assert!(sql.contains("failed_at = CURRENT_TIMESTAMP(6)"));
    let sql = build_batch_reject(DbBackend::Sqlite, 1);
    assert!(sql.contains("failed_at = datetime('now')"));
}

// --- Discard ---

#[test]
fn discard_query_has_for_update() {
    for backend in [DbBackend::Postgres, DbBackend::MySql] {
        let scope = DeadLetterScope::default();
        let (sql, _) = build_discard_select(backend, &scope);
        assert!(sql.contains("FOR UPDATE SKIP LOCKED"));
        assert!(sql.contains("d.status = 'pending'"));
    }
}

#[test]
fn discard_query_sqlite_no_for_update() {
    let scope = DeadLetterScope::default();
    let (sql, _) = build_discard_select(DbBackend::Sqlite, &scope);
    assert!(!sql.contains("FOR UPDATE"));
}

// --- Cleanup ---

#[test]
fn cleanup_deletes_terminal_only() {
    let scope = DeadLetterScope::default();
    let (sql, _) = build_delete_query(DbBackend::Postgres, &scope);
    assert!(sql.contains("d.status IN ('resolved', 'discarded')"));
    assert!(!sql.contains("'pending'"));
}

// --- List ---

#[test]
fn list_query_never_locks() {
    let filter = DeadLetterFilter::default();
    for backend in [DbBackend::Postgres, DbBackend::MySql, DbBackend::Sqlite] {
        let (sql, _) = build_select_query(backend, &filter);
        assert!(!sql.contains("FOR UPDATE"));
    }
}

// --- Status enum ---

#[test]
fn status_display_and_parse() {
    for status in [
        DeadLetterStatus::Pending,
        DeadLetterStatus::Reprocessing,
        DeadLetterStatus::Resolved,
        DeadLetterStatus::Discarded,
    ] {
        let s = status.to_string();
        let parsed: DeadLetterStatus = s.parse().unwrap();
        assert_eq!(parsed, status);
    }
}

#[test]
fn status_invalid_parse() {
    assert!("unknown".parse::<DeadLetterStatus>().is_err());
}

// --- Default limit ---

#[test]
fn build_select_query_applies_default_limit() {
    let filter = DeadLetterFilter::default(); // no explicit .limit()
    let (sql, _) = build_select_query(DbBackend::Postgres, &filter);
    assert!(
        sql.contains("LIMIT 100"),
        "default limit should be applied, got: {sql}"
    );
}

#[test]
fn build_select_query_respects_explicit_limit() {
    let filter = DeadLetterFilter::default().limit(50);
    let (sql, _) = build_select_query(DbBackend::Postgres, &filter);
    assert!(
        sql.contains("LIMIT 50"),
        "explicit limit should override default, got: {sql}"
    );
    assert!(
        !sql.contains("LIMIT 100"),
        "default limit should not appear"
    );
}

// --- Column list ---

#[test]
fn select_includes_new_columns() {
    let filter = DeadLetterFilter::default().any_status();
    let (sql, _) = build_select_query(DbBackend::Postgres, &filter);
    assert!(sql.contains("d.status"));
    assert!(sql.contains("d.completed_at"));
    assert!(sql.contains("d.deadline"));
    assert!(!sql.contains("d.replayed_at"));
}
