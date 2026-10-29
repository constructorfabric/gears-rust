//! Docker-free test support for the startup path: a client over
//! `clickhouse::test::Mock`, and the fixtures the mocked server answers with.
//!
//! The mock serves one queued handler per request, in order, and panics on an
//! unexpected request or on a handler left unused when it drops. A test
//! therefore pins the exact request sequence: not queuing a handler for an
//! `ALTER` is what proves the `ALTER` was not sent.

use clickhouse::test::{Mock, handlers};

use crate::infra::storage::pool::SystemTableRow;
use crate::infra::storage::topology::Topology;

/// A client whose every request goes to `mock`.
pub fn client(mock: &Mock) -> clickhouse::Client {
    clickhouse::Client::default().with_mock(mock)
}

/// Answer the next statement (DDL, `INSERT … VALUES`) with an empty success.
pub fn ok(mock: &Mock) {
    mock.add(handlers::record_ddl());
}

/// Answer the next statement with an empty success and return its SQL text.
pub fn capture(mock: &Mock) -> handlers::RecordDdlControl {
    mock.add(handlers::record_ddl())
}

/// Fail the next request with HTTP 500.
pub fn fail(mock: &Mock) {
    mock.add(handlers::failure(
        clickhouse::test::status::INTERNAL_SERVER_ERROR,
    ));
}

/// Answer the next `fetch_one` / `fetch_all` with `rows`.
pub fn rows<T>(mock: &Mock, rows: impl IntoIterator<Item = T>)
where
    T: serde::Serialize + clickhouse::Row,
{
    mock.add(handlers::provide(rows));
}

/// A `usage_records` `create_table_query` as `system.tables` reports it:
/// `ttl` is the TTL clause (empty for none), `dedup_window` the
/// `non_replicated_deduplication_window` setting (`None` for absent).
pub fn create_sql(ttl: &str, dedup_window: Option<u64>) -> String {
    let ttl = if ttl.is_empty() {
        String::new()
    } else {
        format!(" TTL {ttl}")
    };
    let window = dedup_window
        .map(|w| format!("non_replicated_deduplication_window = {w}, "))
        .unwrap_or_default();
    format!(
        "CREATE TABLE db.usage_records (`id` UUID, `created_at` DateTime64(6, 'UTC')) \
         ENGINE = ReplacingMergeTree(version) PARTITION BY toYYYYMM(created_at) \
         ORDER BY (gts_id, tenant_id, created_at, id){ttl} \
         SETTINGS {window}index_granularity = 8192"
    )
}

/// The rewritten TTL clause `ClickHouse` stores for `secs` of retention.
pub fn ttl(secs: u64) -> String {
    format!("created_at + toIntervalSecond({secs})")
}

// ── Reference cluster ────────────────────────────────────────────────────────

/// The two-table-pair cluster topology the fixtures below describe.
pub fn cluster_topology() -> Topology {
    Topology::Cluster {
        name: "uc".to_owned(),
        records_local: "usage_records_local".to_owned(),
        catalog_local: "usage_type_catalog_local".to_owned(),
        insert_quorum: None,
    }
}

/// One `system.tables` row with no `create_table_query`.
pub fn table_row(
    name: &str,
    engine: &str,
    engine_full: &str,
    sorting: &str,
    partition: &str,
) -> SystemTableRow {
    SystemTableRow {
        name: name.to_owned(),
        engine: engine.to_owned(),
        engine_full: engine_full.to_owned(),
        sorting_key: sorting.to_owned(),
        partition_key: partition.to_owned(),
        create_table_query: String::new(),
    }
}

/// The rows `system.tables` reports for the reference cluster DDL in
/// database `db`; the records local table is first.
pub fn good_cluster_rows() -> Vec<SystemTableRow> {
    vec![
        table_row(
            "usage_records_local",
            "ReplicatedReplacingMergeTree",
            "ReplicatedReplacingMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}', version) \
             PARTITION BY toYYYYMM(created_at) ORDER BY (gts_id, tenant_id, created_at, id)",
            "gts_id, tenant_id, created_at, id",
            "toYYYYMM(created_at)",
        ),
        table_row(
            "usage_type_catalog_local",
            "ReplicatedReplacingMergeTree",
            "ReplicatedReplacingMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}', version) ORDER BY gts_id",
            "gts_id",
            "",
        ),
        table_row(
            "usage_records",
            "Distributed",
            "Distributed('uc', 'db', 'usage_records_local', cityHash64(tenant_id))",
            "",
            "",
        ),
        table_row(
            "usage_type_catalog",
            "Distributed",
            "Distributed('uc', 'db', 'usage_type_catalog_local', cityHash64(gts_id))",
            "",
            "",
        ),
    ]
}

/// Queue the four `validate_cluster_schema` probes for a usable cluster of
/// `replicas` replicas in database `db` whose records TTL is `ttl_secs`.
pub fn valid_cluster_probes(mock: &Mock, replicas: u64, ttl_secs: u64) {
    let mut tables = good_cluster_rows();
    tables[0].create_table_query = create_sql(&ttl(ttl_secs), None);
    rows(mock, ["db".to_owned()]);
    rows(mock, [replicas]);
    rows(mock, tables);
    rows(mock, [replicas * 4]);
}
