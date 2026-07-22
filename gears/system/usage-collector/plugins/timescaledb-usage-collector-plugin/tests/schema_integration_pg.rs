#![cfg(feature = "postgres")]
mod common;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_migrations_create_hypertable_and_retention() {
    let h = common::bring_up()
        .await
        .expect("timescaledb container (Docker required)");

    let ht: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM timescaledb_information.hypertables WHERE hypertable_name = 'usage_records'",
    )
    .fetch_one(&h.pool)
    .await
    .expect("hypertable query");
    assert_eq!(ht, 1, "usage_records must be a hypertable");

    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM timescaledb_information.jobs \
         WHERE proc_name = 'policy_retention' AND hypertable_name = 'usage_records'",
    )
    .fetch_one(&h.pool)
    .await
    .expect("jobs query");
    assert!(jobs >= 1, "retention policy must be registered");

    sqlx::query("SELECT gts_id, kind, metadata_fields FROM usage_type_catalog LIMIT 0")
        .fetch_all(&h.pool)
        .await
        .expect("usage_type_catalog must exist");
}

/// Approach A: dedup is the hypertable's own 4-tuple UNIQUE, so there is no
/// separate `usage_dedup` table and no `prune_usage_dedup` cleanup procedure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_dedup_is_the_records_4tuple_unique_not_a_separate_table() {
    let h = common::bring_up()
        .await
        .expect("timescaledb container (Docker required)");

    let dedup_table: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
         WHERE table_name = 'usage_dedup')",
    )
    .fetch_one(&h.pool)
    .await
    .expect("usage_dedup existence query");
    assert!(
        !dedup_table,
        "usage_dedup must not exist (approach A removed the separate dedup table)"
    );

    let prune_proc: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_proc WHERE proname = 'prune_usage_dedup')",
    )
    .fetch_one(&h.pool)
    .await
    .expect("prune_usage_dedup existence query");
    assert!(
        !prune_proc,
        "prune_usage_dedup must not exist (approach A removed the cleanup job)"
    );

    // The dedup authority is the hypertable's own 4-tuple UNIQUE.
    let uniq: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'usage_records_dedup_uniq')",
    )
    .fetch_one(&h.pool)
    .await
    .expect("constraint existence query");
    assert!(
        uniq,
        "usage_records_dedup_uniq (the 4-tuple dedup authority) must exist"
    );
}
