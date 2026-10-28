use super::{MIGRATIONS_TRACKER, migration_names, render_ddl, tracker_ddl};
use crate::infra::storage::topology::Topology;

fn cluster(records_local: &str, catalog_local: &str) -> Topology {
    Topology::Cluster {
        name: "uc".to_owned(),
        records_local: records_local.to_owned(),
        catalog_local: catalog_local.to_owned(),
        insert_quorum: None,
    }
}

// ── Step list ────────────────────────────────────────────────────────────────

/// Shipped steps are matched by name, so names must be unique, ordered and
/// shaped `mNNNN_snake_case`: a rename or reorder would re-run a step.
#[test]
fn migration_names_are_unique_ordered_and_well_formed() {
    let names = migration_names();
    assert!(!names.is_empty());
    for (i, name) in names.iter().enumerate() {
        let (prefix, rest) = name.split_at(5);
        assert_eq!(
            prefix,
            format!("m{:04}", i + 1),
            "step {i} must be numbered in order: {name}"
        );
        assert!(
            rest.starts_with('_')
                && rest.len() > 1
                && rest[1..]
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "malformed step name {name}"
        );
    }
}

#[test]
fn shipped_steps_are_pinned() {
    assert_eq!(
        migration_names(),
        ["m0001_init", "m0002_insert_dedup_window"]
    );
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// `m0002_insert_dedup_window` only reconciles live state, so the rendered DDL
/// is exactly `m0001_init`'s tables: two on a single node, a local and a
/// `Distributed` table per logical table on a cluster.
#[tokio::test]
async fn render_ddl_records_only_the_create_statements() {
    let single = render_ddl(&Topology::SingleNode, 3600).await.unwrap();
    assert_eq!(single.len(), 2, "{single:#?}");
    let clustered = render_ddl(
        &cluster("usage_records_local", "usage_type_catalog_local"),
        3600,
    )
    .await
    .unwrap();
    assert_eq!(clustered.len(), 4, "{clustered:#?}");
    for stmt in single.iter().chain(&clustered) {
        assert!(stmt.starts_with("CREATE TABLE IF NOT EXISTS "), "{stmt}");
    }
}

// ── Tracking table ───────────────────────────────────────────────────────────

#[test]
fn single_node_tracker_is_a_plain_replacing_merge_tree() {
    let ddl = tracker_ddl(&Topology::SingleNode);
    assert!(
        ddl.starts_with(&format!(
            "CREATE TABLE IF NOT EXISTS {MIGRATIONS_TRACKER}\n"
        )),
        "{ddl}"
    );
    assert!(
        ddl.contains("ENGINE = ReplacingMergeTree\nORDER BY (name)"),
        "{ddl}"
    );
    assert!(!ddl.contains("ON CLUSTER"), "{ddl}");
}

/// One Keeper path for the cluster and a per-host replica name, so every
/// host holds the same history.
#[test]
fn cluster_tracker_is_replicated_to_every_host() {
    let ddl = tracker_ddl(&cluster("usage_records_local", "usage_type_catalog_local"));
    assert!(
        ddl.contains(&format!("{MIGRATIONS_TRACKER} ON CLUSTER uc\n")),
        "{ddl}"
    );
    assert!(
        ddl.contains(&format!(
            "ReplicatedReplacingMergeTree('/clickhouse/tables/{{database}}/{MIGRATIONS_TRACKER}', \
             '{{shard}}_{{replica}}')"
        )),
        "{ddl}"
    );
}

// ── Live single-node runs ────────────────────────────────────────────────────

// Run with: cargo test -p cf-gears-clickhouse-usage-collector-plugin --features clickhouse
#[cfg(feature = "clickhouse")]
mod live {
    use std::time::Duration;

    use super::super::{
        MIGRATIONS_TRACKER, MigrationOptions, migration_client, render_ddl, run_migrations,
    };
    use crate::infra::storage::pool::exec_statement;
    use crate::infra::storage::test_ch_server;
    use crate::infra::storage::topology::Topology;

    const DEADLINE: Duration = Duration::from_secs(30);
    const START_BUDGET: Duration = Duration::from_secs(240);
    const OPTS: MigrationOptions = MigrationOptions {
        retention_period_secs: 31_536_000,
    };
    const ALL: [&str; 2] = ["m0001_init", "m0002_insert_dedup_window"];

    async fn fresh_client() -> clickhouse::Client {
        tokio::time::timeout(START_BUDGET, async {
            let port = test_ch_server::server_port()
                .await
                .unwrap_or_else(|e| panic!("the shared ClickHouse test server must come up: {e}"));
            let database = test_ch_server::fresh_database(port)
                .await
                .unwrap_or_else(|e| panic!("the per-test database must be created: {e}"));
            clickhouse::Client::default()
                .with_url(format!("http://127.0.0.1:{port}/"))
                .with_user("default")
                .with_password(test_ch_server::PASSWORD)
                .with_database(database)
        })
        .await
        .unwrap_or_else(|_elapsed| panic!("live bring-up must finish within {START_BUDGET:?}"))
    }

    async fn recorded(client: &clickhouse::Client) -> Vec<String> {
        client
            .query(&format!(
                "SELECT DISTINCT name FROM {MIGRATIONS_TRACKER} ORDER BY name"
            ))
            .fetch_all()
            .await
            .expect("tracking table is readable")
    }

    /// Every timeout the migration budget must cover reaches the server; a
    /// misspelt setting name would be rejected here rather than silently
    /// leave `ON CLUSTER` DDL on the 180 s default.
    #[tokio::test]
    #[ignore = "requires Docker (testcontainers)"]
    async fn migration_client_raises_every_server_timeout() {
        let client = migration_client(&fresh_client().await, 1234);
        let got: (String, String, String) = client
            .query(
                "SELECT toString(getSetting('send_timeout')), \
                        toString(getSetting('receive_timeout')), \
                        toString(getSetting('distributed_ddl_task_timeout'))",
            )
            .fetch_one()
            .await
            .expect("reading the effective settings");
        assert_eq!(
            got,
            ("1234".to_owned(), "1234".to_owned(), "1234".to_owned())
        );
    }

    #[tokio::test]
    #[ignore = "requires Docker (testcontainers)"]
    async fn a_fresh_database_runs_and_records_every_step_once() {
        let client = fresh_client().await;
        let ran = run_migrations(&client, &Topology::SingleNode, OPTS, DEADLINE)
            .await
            .expect("first run");
        assert_eq!(ran, ALL);
        assert_eq!(recorded(&client).await, ALL);

        let again = run_migrations(&client, &Topology::SingleNode, OPTS, DEADLINE)
            .await
            .expect("second run");
        assert!(
            again.is_empty(),
            "a recorded step must not run again: {again:?}"
        );
    }

    /// A deployment from before the tracking table: its schema exists, its
    /// history does not. Every step runs as a no-op and is recorded.
    #[tokio::test]
    #[ignore = "requires Docker (testcontainers)"]
    async fn a_pre_runner_deployment_is_adopted() {
        let client = fresh_client().await;
        for stmt in render_ddl(&Topology::SingleNode, OPTS.retention_period_secs)
            .await
            .expect("render")
        {
            exec_statement(&client, &stmt, DEADLINE)
                .await
                .expect("pre-runner schema");
        }
        client
            .query("INSERT INTO usage_type_catalog (gts_id, kind, metadata_fields, version) VALUES ('g', 'counter', [], 1)")
            .execute()
            .await
            .expect("seed row");

        let ran = run_migrations(&client, &Topology::SingleNode, OPTS, DEADLINE)
            .await
            .expect("adopting run");
        assert_eq!(ran, ALL);
        let rows: u64 = client
            .query("SELECT count() FROM usage_type_catalog")
            .fetch_one()
            .await
            .unwrap();
        assert_eq!(rows, 1, "existing data must survive adoption");
    }

    /// Replicas starting together: no lock, so both may run every step.
    #[tokio::test]
    #[ignore = "requires Docker (testcontainers)"]
    async fn concurrent_runs_both_succeed() {
        let client = fresh_client().await;
        let (a, b) = tokio::join!(
            run_migrations(&client, &Topology::SingleNode, OPTS, DEADLINE),
            run_migrations(&client, &Topology::SingleNode, OPTS, DEADLINE),
        );
        a.expect("first concurrent run");
        b.expect("second concurrent run");
        assert_eq!(recorded(&client).await, ALL);
    }
}
