use super::{Table, cluster_local_engine, ttl_expr};
use crate::infra::storage::topology::{CATALOG_TABLE, RECORDS_TABLE, Topology};

fn cluster(records_local: &str, catalog_local: &str) -> Topology {
    Topology::Cluster {
        name: "uc".to_owned(),
        records_local: records_local.to_owned(),
        catalog_local: catalog_local.to_owned(),
        insert_quorum: None,
    }
}

/// A small table exercising every topology-dependent part.
fn table(name: &'static str) -> Table {
    Table::new(name)
        .column("id", "UUID", "Row id")
        .column("at", "DateTime64(6)", "")
        .version("v")
        .order_by(&["id"])
        .ttl_delete_after("at")
        .setting("ttl_only_drop_parts", &1)
        .insert_dedup_window(10)
        .sharding_key("cityHash64(id)")
}

#[test]
fn single_node_is_one_plain_table_under_the_logical_name() {
    let ddl = table(RECORDS_TABLE)
        .to_ddl(&Topology::SingleNode, 60)
        .unwrap();
    assert_eq!(
        ddl,
        ["CREATE TABLE IF NOT EXISTS usage_records\n\
             (\n    id UUID COMMENT 'Row id',\n    at DateTime64(6)\n)\n\
             ENGINE = ReplacingMergeTree(v)\n\
             ORDER BY (id)\n\
             TTL at + INTERVAL 60 SECOND DELETE\n\
             SETTINGS ttl_only_drop_parts = 1, non_replicated_deduplication_window = 10"]
    );
}

/// A replicated local table under the configured name, then a `Distributed`
/// table under the logical name; the dedup window is left to replication.
#[test]
fn cluster_is_a_replicated_local_table_behind_a_distributed_one() {
    let ddl = table(RECORDS_TABLE)
        .to_ddl(&cluster("rec_l", "cat_l"), 60)
        .unwrap();
    assert_eq!(
        ddl,
        [
            "CREATE TABLE IF NOT EXISTS rec_l ON CLUSTER uc\n\
             (\n    id UUID COMMENT 'Row id',\n    at DateTime64(6)\n)\n\
             ENGINE = ReplicatedReplacingMergeTree(v)\n\
             ORDER BY (id)\n\
             TTL at + INTERVAL 60 SECOND DELETE\n\
             SETTINGS ttl_only_drop_parts = 1",
            "CREATE TABLE IF NOT EXISTS usage_records ON CLUSTER uc\n\
             AS rec_l\n\
             ENGINE = Distributed(uc, currentDatabase(), rec_l, cityHash64(id))",
        ]
    );
}

/// Swapped default names land where configured.
#[test]
fn cluster_uses_the_local_table_configured_for_the_logical_one() {
    let topology = cluster("usage_type_catalog_local", "usage_records_local");
    let records = table(RECORDS_TABLE).to_ddl(&topology, 60).unwrap();
    assert!(
        records[1].contains("AS usage_type_catalog_local\n"),
        "{}",
        records[1]
    );
    let catalog = table(CATALOG_TABLE).to_ddl(&topology, 60).unwrap();
    assert!(
        catalog[1].contains("AS usage_records_local\n"),
        "{}",
        catalog[1]
    );
}

#[test]
fn cluster_refuses_a_table_without_sharding_key_or_local_table() {
    let err = Table::new(RECORDS_TABLE)
        .column("id", "UUID", "")
        .to_ddl(&cluster("r", "c"), 60)
        .unwrap_err();
    assert!(format!("{err}").contains("has no sharding key"), "{err}");

    let err = table("other").to_ddl(&cluster("r", "c"), 60).unwrap_err();
    assert!(format!("{err}").contains("no local table"), "{err}");
}

#[test]
fn ttl_uses_the_column_directly() {
    assert_eq!(
        ttl_expr("created_at", 5),
        "created_at + INTERVAL 5 SECOND DELETE"
    );
    assert_eq!(
        cluster_local_engine(None).name(),
        "ReplicatedReplacingMergeTree"
    );
}
