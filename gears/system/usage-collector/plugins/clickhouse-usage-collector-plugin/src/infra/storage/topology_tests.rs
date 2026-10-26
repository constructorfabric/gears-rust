use super::*;

fn cluster() -> Topology {
    Topology::Cluster {
        name: "uc".to_owned(),
        records_local: "usage_records_local".to_owned(),
        catalog_local: "usage_type_catalog_local".to_owned(),
        insert_quorum: None,
    }
}

#[test]
fn single_node_adds_no_client_settings() {
    assert!(Topology::SingleNode.client_settings().is_empty());
}

/// `distributed_product_mode = 'local'` is load-bearing: without it the
/// survivor anti-joins in `query::dedup` are rejected by a `Distributed` table.
#[test]
fn cluster_client_settings_cover_reads_and_inserts() {
    let settings = cluster().client_settings();
    let get = |k: &str| {
        settings
            .iter()
            .find(|(name, _)| *name == k)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(get("distributed_product_mode"), Some("local"));
    assert_eq!(get("optimize_skip_unused_shards"), Some("1"));
    assert_eq!(get("distributed_foreground_insert"), Some("1"));
    assert_eq!(get("insert_quorum"), None);
}

#[test]
fn cluster_insert_quorum_is_forwarded_when_configured() {
    let t = Topology::Cluster {
        name: "uc".to_owned(),
        records_local: "usage_records_local".to_owned(),
        catalog_local: "usage_type_catalog_local".to_owned(),
        insert_quorum: Some("auto".to_owned()),
    };
    assert!(
        t.client_settings()
            .contains(&("insert_quorum", "auto".to_owned()))
    );
}

#[test]
fn single_node_mutation_targets_the_logical_table() {
    assert_eq!(
        Topology::SingleNode.delete_where_sql(RECORDS_TABLE),
        "ALTER TABLE usage_records DELETE WHERE gts_id = ?"
    );
    assert_eq!(Topology::SingleNode.mutations_sync(), "1");
}

/// A mutation on a `Distributed` table is refused, and one `gts_id` spans every
/// shard, so the cluster form must hit the local table `ON CLUSTER` and wait
/// for all replicas.
#[test]
fn cluster_mutation_targets_local_table_on_cluster() {
    let t = cluster();
    assert_eq!(
        t.delete_where_sql(RECORDS_TABLE),
        "ALTER TABLE usage_records_local ON CLUSTER uc DELETE WHERE gts_id = ?"
    );
    assert_eq!(
        t.delete_where_sql(CATALOG_TABLE),
        "ALTER TABLE usage_type_catalog_local ON CLUSTER uc DELETE WHERE gts_id = ?"
    );
    assert_eq!(t.mutations_sync(), "2");
}

#[test]
fn from_config_maps_the_cluster_block() {
    let json = r#"{ "database_url": "https://u:p@h/db", "async_insert": false,
                    "cluster": { "name": "uc", "insert_quorum": "2" } }"#;
    let cfg: ClickHousePluginConfig = serde_json::from_str(json).unwrap();
    assert_eq!(
        Topology::from_config(&cfg),
        Topology::Cluster {
            name: "uc".to_owned(),
            records_local: "usage_records_local".to_owned(),
            catalog_local: "usage_type_catalog_local".to_owned(),
            insert_quorum: Some("2".to_owned()),
        }
    );
    let single: ClickHousePluginConfig =
        serde_json::from_str(r#"{ "database_url": "https://u:p@h/db" }"#).unwrap();
    assert_eq!(Topology::from_config(&single), Topology::SingleNode);
}
