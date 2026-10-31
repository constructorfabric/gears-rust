use super::{CATALOG_ORDER_BY, RECORDS_ORDER_BY, RECORDS_PARTITION_KEY, catalog, records};
use crate::infra::storage::pool::{DEFAULT_RETENTION_SECS, INSERT_DEDUP_WINDOW_BLOCKS};
use crate::infra::storage::topology::Topology;

fn single(table: &super::Table) -> String {
    let mut ddl = table
        .to_ddl(&Topology::SingleNode, DEFAULT_RETENTION_SECS)
        .unwrap();
    assert_eq!(ddl.len(), 1);
    ddl.remove(0)
}

fn cluster() -> Topology {
    Topology::Cluster {
        name: "uc".to_owned(),
        records_local: "usage_records_local".to_owned(),
        catalog_local: "usage_type_catalog_local".to_owned(),
        insert_quorum: None,
    }
}

#[test]
fn both_tables_are_replacing_merge_trees_on_version() {
    for ddl in [single(&catalog()), single(&records())] {
        assert!(
            ddl.contains("ENGINE = ReplacingMergeTree(version)"),
            "{ddl}"
        );
    }
}

#[test]
fn sort_keys() {
    assert!(single(&catalog()).contains("ORDER BY (gts_id)"));
    assert!(single(&records()).contains("ORDER BY (gts_id, tenant_id, created_at, id)"));
}

/// Every sort-key column must be declared, or `CREATE TABLE` fails on the
/// server.
#[test]
fn sort_key_columns_are_declared() {
    for (ddl, keys) in [
        (single(&catalog()), CATALOG_ORDER_BY),
        (single(&records()), RECORDS_ORDER_BY),
    ] {
        for key in keys {
            assert!(
                ddl.contains(&format!("\n    {key} ")),
                "{key} is not a column:\n{ddl}"
            );
        }
    }
}

/// `PARTITION BY toYYYYMM(created_at)` is what gives TTL whole parts to drop,
/// and `ttl_only_drop_parts = 1` is what stops expiry from rewriting a part to
/// delete rows out of it: one decision, so both halves are pinned together.
/// The catalog carries no TTL and no time column, so it stays unpartitioned.
#[test]
fn records_partition_on_the_ttl_column_and_the_catalog_does_not() {
    let records = single(&records());
    assert_eq!(RECORDS_PARTITION_KEY, "toYYYYMM(created_at)");
    assert!(
        records.contains("PARTITION BY toYYYYMM(created_at)"),
        "{records}"
    );
    assert!(
        records.contains("SETTINGS ttl_only_drop_parts = 1"),
        "{records}"
    );
    let catalog = single(&catalog());
    assert!(!catalog.contains("PARTITION BY"), "{catalog}");
    assert!(!catalog.contains("TTL"), "{catalog}");
}

/// The TTL must use the `DateTime64` column directly: `toDateTime` saturates in
/// 2106 and can expire rows the moment they are written.
#[test]
fn records_ttl_uses_created_at_directly() {
    let records = single(&records());
    assert!(
        records.contains(&format!(
            "TTL created_at + INTERVAL {DEFAULT_RETENTION_SECS} SECOND DELETE"
        )),
        "{records}"
    );
    assert!(!records.contains("toDateTime"), "{records}");
}

/// Fresh single-node deployments get the window from the DDL, so
/// `m0002_insert_dedup_window` is a no-op on them; it joins the existing
/// `SETTINGS` clause rather than adding a second one.
#[test]
fn records_keep_the_insert_dedup_window_on_a_single_node() {
    let records = single(&records());
    assert!(
        records.contains(&format!(
            "SETTINGS ttl_only_drop_parts = 1, \
             non_replicated_deduplication_window = {INSERT_DEDUP_WINDOW_BLOCKS}"
        )),
        "{records}"
    );
}

#[test]
fn records_carry_both_bloom_filter_indexes() {
    let records = single(&records());
    for index in [
        "INDEX idx_records_id id TYPE bloom_filter GRANULARITY 1",
        "INDEX idx_records_corrects_id corrects_id TYPE bloom_filter GRANULARITY 1",
    ] {
        assert!(records.contains(index), "{index} missing:\n{records}");
    }
}

/// `usage_records` must be sharded by tenant: markers and compensation rows
/// must land on the shard of the rows they refer to (`topology` module docs).
#[test]
fn cluster_tables_shard_by_the_colocation_keys() {
    let catalog = catalog().to_ddl(&cluster(), 3600).unwrap();
    assert!(
        catalog[1].ends_with(
            "ENGINE = Distributed(uc, currentDatabase(), usage_type_catalog_local, cityHash64(gts_id))"
        ),
        "{}",
        catalog[1]
    );
    let records = records().to_ddl(&cluster(), 3600).unwrap();
    assert!(
        records[1].ends_with(
            "ENGINE = Distributed(uc, currentDatabase(), usage_records_local, cityHash64(tenant_id))"
        ),
        "{}",
        records[1]
    );
}
