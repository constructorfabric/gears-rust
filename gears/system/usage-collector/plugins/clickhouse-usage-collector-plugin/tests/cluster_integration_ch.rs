#![cfg(feature = "clickhouse")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Sharded-cluster integration tests: the plugin's stores over `Distributed`
//! tables on a real two-shard `ClickHouse` cluster.
//!
//! Each test boots its own cluster — two `clickhouse-server` nodes, one shard
//! each, on a private Docker network, with `ClickHouse` Keeper embedded in the
//! first node — and provisions it from the operator DDL
//! (`migrator::render_ddl`, which startup validation prints to operators), so
//! the DDL operators run is the DDL under test, or leaves it bare for the
//! plugin-managed migrations.
//! Requires Docker; skips (or panics under `CH_REQUIRE_DOCKER=1`) like the
//! single-node suites.
//!
//! What only a cluster can show:
//! - rows of different tenants land on different shards;
//! - the survivor anti-join (`id NOT IN (SELECT id … 'inactive')`) is exact
//!   with `distributed_product_mode = 'local'`, i.e. markers are colocated
//!   with the rows they mark;
//! - the insert dedup token is honoured through a `Distributed` insert;
//! - catalog deletes reach every shard (`ON CLUSTER` mutation);
//! - startup validation rejects a deviating operator schema;
//! - the plugin-managed cluster migrations provision a bare cluster.

mod common;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bigdecimal::BigDecimal;
use rust_decimal::Decimal;
use testcontainers::core::WaitFor;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, SortDir};
use usage_collector_sdk::{AggregationOp, AggregationSpec, UsageCollectorPluginError};

use clickhouse_usage_collector_plugin::config::ClickHousePluginConfig;
use clickhouse_usage_collector_plugin::domain::ports::{CatalogStore, RecordStore};
use clickhouse_usage_collector_plugin::infra::storage::catalog_store::ChCatalogStore;
use clickhouse_usage_collector_plugin::infra::storage::migrator::{
    MIGRATIONS_TRACKER, MigrationOptions, migration_client, render_ddl, run_migrations,
};
use clickhouse_usage_collector_plugin::infra::storage::pool::{
    build_client, validate_cluster_schema,
};
use clickhouse_usage_collector_plugin::infra::storage::record_store::ChRecordStore;
use clickhouse_usage_collector_plugin::infra::storage::topology::Topology;

const CLUSTER: &str = "uc";
const DATABASE: &str = "usage";
const PASSWORD: &str = common::CH_TEST_PASSWORD;
const RETENTION_SECS: u64 = 31_536_000;
const VCPU_GTS: &str = "gts.cf.core.uc.usage_record.v1~cf.compute._.vcpu_hours.v1";

/// Ceiling on booting both nodes and Keeper and provisioning the schema.
/// Below the 300s nextest per-test kill, so this error is what a wedged
/// bring-up reports.
const BOOT_BUDGET: Duration = Duration::from_secs(240);

/// A running two-shard cluster. Dropping it removes both containers.
struct Cluster {
    _nodes: Vec<ContainerAsync<GenericImage>>,
    /// Host port of node 1's HTTP interface.
    port: u16,
}

/// `config.d` override for node `i` (1-based) of the cluster `hosts`.
fn node_config(i: usize, hosts: &[String]) -> String {
    let shards = hosts
        .iter()
        .map(|h| {
            format!(
                "<shard><replica><host>{h}</host><port>9000</port>\
                 <user>default</user><password>{PASSWORD}</password></replica></shard>"
            )
        })
        .collect::<Vec<_>>()
        .concat();
    let keeper_host = &hosts[0];
    let keeper_server = if i == 1 {
        format!(
            "<keeper_server><tcp_port>9181</tcp_port><server_id>1</server_id>\
             <log_storage_path>/var/lib/clickhouse/coordination/log</log_storage_path>\
             <snapshot_storage_path>/var/lib/clickhouse/coordination/snapshots</snapshot_storage_path>\
             <raft_configuration><server><id>1</id><hostname>{keeper_host}</hostname>\
             <port>9234</port></server></raft_configuration></keeper_server>"
        )
    } else {
        String::new()
    };
    format!(
        "<clickhouse>\
         <remote_servers><{CLUSTER}>{shards}</{CLUSTER}></remote_servers>\
         <macros><shard>s{i}</shard><replica>r1</replica></macros>\
         <zookeeper><node><host>{keeper_host}</host><port>9181</port></node></zookeeper>\
         <distributed_ddl><path>/clickhouse/task_queue/ddl</path></distributed_ddl>\
         {keeper_server}</clickhouse>"
    )
}

/// The operator DDL for this cluster, one statement per entry.
async fn ddl_statements() -> Vec<String> {
    let topology = Topology::Cluster {
        name: CLUSTER.to_owned(),
        records_local: "usage_records_local".to_owned(),
        catalog_local: "usage_type_catalog_local".to_owned(),
        insert_quorum: None,
    };
    render_ddl(&topology, RETENTION_SECS)
        .await
        .expect("cluster topology")
}

/// Boot the cluster and create the test database on every node; with
/// `provision`, also apply the reference DDL.
async fn boot(provision: bool) -> anyhow::Result<Cluster> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let suffix = format!("{}-{nanos}", std::process::id());
    let network = format!("uc-ch-cluster-{suffix}");
    let hosts: Vec<String> = (1..=2).map(|i| format!("uc-ch{i}-{suffix}")).collect();

    let mut nodes = Vec::new();
    for (idx, host) in hosts.iter().enumerate() {
        let node = test_containers::clickhouse()
            .with_wait_for(WaitFor::Nothing)
            .with_network(network.clone())
            .with_container_name(host.clone())
            .with_hostname(host.clone())
            .with_env_var("CLICKHOUSE_USER", "default")
            .with_env_var("CLICKHOUSE_PASSWORD", PASSWORD)
            .with_copy_to(
                "/etc/clickhouse-server/config.d/cluster.xml",
                node_config(idx + 1, &hosts).into_bytes(),
            )
            .start()
            .await?;
        nodes.push(node);
    }
    let port = nodes[0].get_host_port_ipv4(8123).await?;

    // Ready = both nodes answer through the cluster and Keeper is up, which is
    // what `ON CLUSTER` DDL needs.
    let admin = clickhouse::Client::default()
        .with_url(format!("http://127.0.0.1:{port}"))
        .with_user("default")
        .with_password(PASSWORD);
    loop {
        let replicas = admin
            .query(&format!(
                "SELECT count() FROM clusterAllReplicas('{CLUSTER}', system.one)"
            ))
            .fetch_one::<u64>()
            .await;
        let keeper = admin
            .query("SELECT count() FROM system.zookeeper WHERE path = '/'")
            .fetch_one::<u64>()
            .await;
        if matches!(replicas, Ok(2)) && keeper.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    admin
        .query(&format!(
            "CREATE DATABASE IF NOT EXISTS {DATABASE} ON CLUSTER {CLUSTER}"
        ))
        .execute()
        .await?;
    let db = admin.clone().with_database(DATABASE);
    for stmt in ddl_statements().await.into_iter().filter(|_| provision) {
        db.query(&stmt)
            .execute()
            .await
            .map_err(|e| anyhow::anyhow!("reference cluster DDL failed: {e}\n{stmt}"))?;
    }
    Ok(Cluster {
        _nodes: nodes,
        port,
    })
}

/// Boot a cluster provisioned from the reference DDL, or skip (return
/// `None`) when Docker is unavailable.
async fn boot_or_skip() -> Option<Cluster> {
    boot_with_or_skip(true).await
}

/// Boot a cluster with an empty test database, or skip.
async fn boot_bare_or_skip() -> Option<Cluster> {
    boot_with_or_skip(false).await
}

async fn boot_with_or_skip(provision: bool) -> Option<Cluster> {
    match tokio::time::timeout(BOOT_BUDGET, boot(provision)).await {
        Ok(Ok(c)) => Some(c),
        Ok(Err(e)) => skip(&e.to_string()),
        Err(_elapsed) => skip(&format!("cluster bring-up exceeded {BOOT_BUDGET:?}")),
    }
}

fn skip(why: &str) -> Option<Cluster> {
    assert!(
        !std::env::var("CH_REQUIRE_DOCKER").is_ok_and(|v| v == "1"),
        "CH_REQUIRE_DOCKER=1 but the ClickHouse cluster failed to start: {why}"
    );
    eprintln!("DOCKER UNAVAILABLE — skipping cluster test: {why}");
    None
}

fn cluster_config(c: &Cluster) -> ClickHousePluginConfig {
    config(c, &format!(r#", "cluster": {{ "name": "{CLUSTER}" }}"#))
}

/// Cluster config with the plugin managing the schema.
fn managed_config(c: &Cluster) -> ClickHousePluginConfig {
    config(
        c,
        &format!(
            r#", "cluster": {{ "name": "{CLUSTER}", "manage_schema": true,
                 "migration_statement_timeout_secs": 120 }}"#
        ),
    )
}

fn config(c: &Cluster, extra: &str) -> ClickHousePluginConfig {
    let cfg: ClickHousePluginConfig = serde_json::from_str(&format!(
        r#"{{ "database_url": "http://default:{PASSWORD}@127.0.0.1:{port}/{DATABASE}",
              "allow_insecure_http": true, "async_insert": false{extra} }}"#,
        port = c.port
    ))
    .expect("valid test config");
    cfg.validate().expect("test config validates");
    cfg
}

/// Run the migrations `Gear::init` runs for `cfg`: on a cluster, through the
/// migration client and under the migration budget.
async fn migrate(
    client: &clickhouse::Client,
    cfg: &ClickHousePluginConfig,
) -> anyhow::Result<Vec<&'static str>> {
    let opts = MigrationOptions {
        retention_period_secs: cfg.retention_period_secs,
    };
    let topology = Topology::from_config(cfg);
    match cfg.cluster.as_ref() {
        Some(cluster) => {
            let client = migration_client(client, cluster.migration_statement_timeout_secs);
            run_migrations(&client, &topology, opts, cluster.migration_deadline()).await
        }
        None => run_migrations(client, &topology, opts, common::TEST_REQUEST_TIMEOUT).await,
    }
}

/// The plugin's own client for `cfg`, as `Gear::init` builds it.
fn plugin_client(cfg: &ClickHousePluginConfig) -> clickhouse::Client {
    drop(rustls::crypto::aws_lc_rs::default_provider().install_default());
    build_client(cfg).expect("build the plugin client")
}

/// The plugin's own client (cluster settings included) plus both stores,
/// wired exactly as `Gear::init` wires them.
fn stores(cfg: &ClickHousePluginConfig) -> (clickhouse::Client, ChRecordStore, ChCatalogStore) {
    let client = plugin_client(cfg);
    let records = ChRecordStore::new(
        client.clone(),
        common::metrics(),
        common::TEST_REQUEST_TIMEOUT,
        cfg.async_insert,
    );
    let catalog = ChCatalogStore::new(
        client.clone(),
        CancellationToken::new(),
        common::metrics(),
        common::TEST_REQUEST_TIMEOUT,
        Topology::from_config(cfg),
    );
    (client, records, catalog)
}

/// Rows per shard (`hostName()` → count) in a local table, across the cluster.
async fn rows_per_shard(client: &clickhouse::Client, local_table: &str) -> Vec<(String, u64)> {
    client
        .query(&format!(
            "SELECT hostName() AS h, count() FROM clusterAllReplicas('{CLUSTER}', {DATABASE}.{local_table}) \
             GROUP BY h ORDER BY h"
        ))
        .fetch_all::<(String, u64)>()
        .await
        .expect("per-shard counts")
}

async fn sum_and_count(store: &ChRecordStore) -> (Option<BigDecimal>, Option<BigDecimal>) {
    let mut out = Vec::new();
    for op in [AggregationOp::Sum, AggregationOp::Count] {
        let result = store
            .aggregate(
                common::fixture_gts_id(VCPU_GTS),
                &ODataQuery::new(),
                &[],
                AggregationSpec {
                    op,
                    group_by: Vec::new(),
                },
            )
            .await
            .expect("aggregate across shards");
        out.push(result.buckets.into_iter().next().and_then(|b| b.value));
    }
    let count = out.pop().flatten();
    (out.pop().flatten(), count)
}

/// 16 tenants, one record each (value 1..=16), split between the
/// single-record and the batch insert path.
async fn ingest_sixteen_tenants(records: &ChRecordStore) -> Vec<usage_collector_sdk::UsageRecord> {
    let recs: Vec<_> = (0..16_u128)
        .map(|i| {
            common::fixture_usage_record_at(
                VCPU_GTS,
                Uuid::from_u128(0xC100 + i),
                &format!("idem-{i}"),
                Decimal::from(i64::try_from(i).unwrap() + 1),
                common::fixture_created_at_offset(i64::try_from(i).unwrap()),
            )
        })
        .collect();
    for r in &recs[..8] {
        records.create(r.clone()).await.expect("single create");
    }
    records
        .create_batch(recs[8..].to_vec())
        .await
        .expect("batch create");
    recs
}

/// Ingest, dedup, deactivate and aggregate across two shards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_cluster_records_round_trip_across_shards() {
    let Some(cluster) = boot_or_skip().await else {
        return;
    };
    let cfg = cluster_config(&cluster);
    let (client, records, catalog) = stores(&cfg);

    validate_cluster_schema(
        &client,
        &Topology::from_config(&cfg),
        cfg.retention_period_secs,
        common::TEST_REQUEST_TIMEOUT,
    )
    .await
    .expect("the reference DDL passes startup validation");

    catalog
        .create(common::fixture_usage_type(VCPU_GTS, "counter", &[]))
        .await
        .expect("register usage type");

    let recs = ingest_sixteen_tenants(&records).await;

    let shards = rows_per_shard(&client, "usage_records_local").await;
    assert_eq!(shards.len(), 2, "both shards hold rows: {shards:?}");
    assert!(shards.iter().all(|(_, n)| *n > 0), "{shards:?}");
    assert_eq!(shards.iter().map(|(_, n)| n).sum::<u64>(), 16);

    // An exact retry is answered from the stored row, and the dedup token
    // keeps the physical row count at one.
    records
        .create(recs[0].clone())
        .await
        .expect("retry is idempotent");
    assert_eq!(common_raw_rows(&client, recs[0].id).await, 1);

    // Point read through the Distributed table finds a row on either shard.
    for r in [&recs[0], &recs[15]] {
        assert_eq!(records.get(r.id).await.expect("get").id, r.id);
    }

    // Deactivate four rows spread over the tenants. Each marker must land on
    // its row's shard for the shard-local anti-join to exclude the row.
    for r in recs.iter().step_by(4) {
        records.deactivate(r.id).await.expect("deactivate");
    }
    // Deactivated values: 1 + 5 + 9 + 13 = 28; total 136 → 108 over 12 rows.
    let (sum, count) = sum_and_count(&records).await;
    assert_eq!(sum, Some(BigDecimal::from(108_i64)));
    assert_eq!(count, Some(BigDecimal::from(12_i64)));

    let page = records
        .list(
            common::fixture_gts_id(VCPU_GTS),
            &ODataQuery::new()
                .with_limit(100)
                .with_order(ODataOrderBy(vec![
                    OrderKey {
                        field: "created_at".to_owned(),
                        dir: SortDir::Asc,
                    },
                    OrderKey {
                        field: "id".to_owned(),
                        dir: SortDir::Asc,
                    },
                ])),
            &[],
        )
        .await
        .expect("list across shards");
    assert_eq!(
        page.items.len(),
        16,
        "list returns one row per logical record"
    );
    assert_eq!(
        page.items
            .iter()
            .filter(|r| r.status == usage_collector_sdk::UsageRecordStatus::Inactive)
            .count(),
        4
    );

    // Referenced types are refused, exactly as on a single node.
    assert!(matches!(
        catalog.delete(common::fixture_gts_id(VCPU_GTS)).await,
        Err(UsageCollectorPluginError::UsageTypeReferenced { .. })
    ));
}

async fn common_raw_rows(client: &clickhouse::Client, id: Uuid) -> u64 {
    client
        .query("SELECT count() FROM usage_records WHERE id = ?")
        .bind(id.to_string())
        .fetch_one::<u64>()
        .await
        .expect("raw count")
}

/// A catalog delete is an `ON CLUSTER` mutation on the local table: it must
/// remove the type from every shard, not only the one the client talks to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_cluster_catalog_delete_reaches_every_shard() {
    let Some(cluster) = boot_or_skip().await else {
        return;
    };
    let cfg = cluster_config(&cluster);
    let (client, _records, catalog) = stores(&cfg);

    // Enough types that `cityHash64(gts_id)` puts some on each shard.
    let gts: Vec<String> = (0..8)
        .map(|i| format!("gts.cf.core.uc.usage_record.v1~cf.compute._.metric_{i}.v1"))
        .collect();
    for g in &gts {
        catalog
            .create(common::fixture_usage_type(g, "counter", &[]))
            .await
            .expect("create type");
    }
    let before = rows_per_shard(&client, "usage_type_catalog_local").await;
    assert_eq!(
        before.len(),
        2,
        "types are spread over both shards: {before:?}"
    );

    for g in &gts {
        catalog
            .delete(common::fixture_gts_id(g))
            .await
            .expect("delete unreferenced type");
        assert!(matches!(
            catalog.get(common::fixture_gts_id(g)).await,
            Err(UsageCollectorPluginError::UsageTypeNotFound { .. })
        ));
    }
    assert!(
        rows_per_shard(&client, "usage_type_catalog_local")
            .await
            .is_empty(),
        "no shard keeps a deleted type"
    );
}

/// Startup validation names the deviation instead of letting the plugin run
/// on a schema that breaks its invariants.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_cluster_validation_rejects_a_deviating_schema() {
    let Some(cluster) = boot_or_skip().await else {
        return;
    };
    let cfg = cluster_config(&cluster);
    let (client, _records, _catalog) = stores(&cfg);
    let topology = Topology::from_config(&cfg);

    // Re-shard usage_records by rand(): breaks marker colocation.
    for stmt in [
        format!("DROP TABLE usage_records ON CLUSTER {CLUSTER} SYNC"),
        format!(
            "CREATE TABLE usage_records ON CLUSTER {CLUSTER} AS usage_records_local \
             ENGINE = Distributed({CLUSTER}, currentDatabase(), usage_records_local, rand())"
        ),
    ] {
        client.query(&stmt).execute().await.expect(&stmt);
    }
    let err = validate_cluster_schema(
        &client,
        &topology,
        RETENTION_SECS,
        common::TEST_REQUEST_TIMEOUT,
    )
    .await
    .expect_err("a rand()-sharded usage_records must be refused")
    .to_string();
    // The problem line names the live engine and the expected sharding key;
    // the operator DDL appended below it repeats the key, so match the line.
    assert!(
        err.lines().any(|l| l.contains("rand()")
            && l.contains("expected Distributed(")
            && l.contains("cityHash64(tenant_id)")),
        "{err}"
    );
    // With the reference SQL files gone, the error itself carries the DDL.
    for stmt in ddl_statements().await {
        assert!(
            err.contains(&stmt),
            "missing operator DDL:\n{stmt}\n---\n{err}"
        );
    }

    // Drop the local catalog table on one shard only.
    client
        .query("DROP TABLE usage_type_catalog_local SYNC")
        .execute()
        .await
        .expect("drop local catalog table on node 1");
    let err = validate_cluster_schema(
        &client,
        &topology,
        RETENTION_SECS,
        common::TEST_REQUEST_TIMEOUT,
    )
    .await
    .expect_err("a missing local table must be refused")
    .to_string();
    assert!(err.contains("usage_type_catalog_local"), "{err}");
    assert!(err.contains("expected tables exist across"), "{err}");

    // An undeclared cluster name fails before any table probe.
    let mut wrong = cfg.clone();
    if let Some(c) = wrong.cluster.as_mut() {
        c.name = "nope".to_owned();
    }
    let err = validate_cluster_schema(
        &client,
        &Topology::from_config(&wrong),
        RETENTION_SECS,
        common::TEST_REQUEST_TIMEOUT,
    )
    .await
    .expect_err("an undeclared cluster must be refused")
    .to_string();
    assert!(err.contains("not declared in system.clusters"), "{err}");
}

/// Rows of the cluster tracking table across every host, waiting for
/// replication to deliver `want` of them.
async fn tracker_rows_everywhere(client: &clickhouse::Client, want: u64) -> u64 {
    let sql = format!(
        "SELECT count() FROM clusterAllReplicas('{CLUSTER}', {DATABASE}.{MIGRATIONS_TRACKER})"
    );
    let mut seen = 0;
    for _ in 0..60 {
        seen = client
            .query(&sql)
            .fetch_one::<u64>()
            .await
            .expect("tracker rows");
        if seen == want {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    seen
}

/// With `manage_schema`, the plugin provisions a bare cluster itself, the
/// result passes the same validation as the operator path, the history is
/// replicated to every host, and a restart runs nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_cluster_managed_schema_provisions_a_bare_cluster() {
    let Some(cluster) = boot_bare_or_skip().await else {
        return;
    };
    let cfg = managed_config(&cluster);
    let (client, records, catalog) = stores(&cfg);

    let ran = migrate(&client, &cfg)
        .await
        .expect("managed cluster migrations");
    assert_eq!(ran, ["m0001_init", "m0002_insert_dedup_window"]);
    validate_cluster_schema(
        &client,
        &Topology::from_config(&cfg),
        cfg.retention_period_secs,
        common::TEST_REQUEST_TIMEOUT,
    )
    .await
    .expect("the managed schema passes startup validation");
    // Two steps on each of two hosts.
    assert_eq!(tracker_rows_everywhere(&client, 4).await, 4);

    let again = migrate(&client, &cfg).await.expect("restart");
    assert!(again.is_empty(), "nothing runs on restart: {again:?}");

    catalog
        .create(common::fixture_usage_type(VCPU_GTS, "counter", &[]))
        .await
        .expect("register usage type");
    ingest_sixteen_tenants(&records).await;
    let shards = rows_per_shard(&client, "usage_records_local").await;
    assert_eq!(shards.len(), 2, "both shards hold rows: {shards:?}");
}

// ── `Gear::init` on a real cluster ───────────────────────────────────────────

mod gear_init {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;
    use toolkit::client_hub::ClientScope;
    use toolkit::gts::PluginV1;
    use toolkit::{ClientHub, ConfigProvider, Gear, GearCtx};
    use toolkit_canonical_errors::CanonicalError;
    use types_registry_sdk::{
        GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery,
        TypesRegistryClient,
    };
    use usage_collector_sdk::{UsageCollectorPluginSpecV1, UsageCollectorPluginV1};
    use uuid::Uuid;

    use clickhouse_usage_collector_plugin::ClickHouseUsageCollectorPlugin;
    use clickhouse_usage_collector_plugin::config::ClickHousePluginConfig;

    use super::{CLUSTER, Cluster, DATABASE, PASSWORD, boot_bare_or_skip, tracker_rows_everywhere};

    struct StaticConfig(serde_json::Value);

    impl ConfigProvider for StaticConfig {
        fn get_gear_config(&self, _gear_name: &str) -> Option<&serde_json::Value> {
            Some(&self.0)
        }
    }

    /// Accepts every registration; `init` calls nothing else.
    struct RegistryStub;

    #[async_trait]
    impl TypesRegistryClient for RegistryStub {
        async fn register(
            &self,
            entities: Vec<serde_json::Value>,
        ) -> Result<Vec<RegisterResult>, CanonicalError> {
            Ok(entities
                .iter()
                .map(|e| RegisterResult::Ok {
                    gts_id: e["id"].as_str().unwrap_or_default().to_owned(),
                })
                .collect())
        }
        async fn register_type_schemas(
            &self,
            _type_schemas: Vec<serde_json::Value>,
        ) -> Result<Vec<RegisterResult>, CanonicalError> {
            unreachable!()
        }
        async fn get_type_schema(&self, _type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
            unreachable!()
        }
        async fn get_type_schema_by_uuid(
            &self,
            _type_uuid: Uuid,
        ) -> Result<GtsTypeSchema, CanonicalError> {
            unreachable!()
        }
        async fn get_type_schemas(
            &self,
            _type_ids: Vec<String>,
        ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
            unreachable!()
        }
        async fn get_type_schemas_by_uuid(
            &self,
            _type_uuids: Vec<Uuid>,
        ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
            unreachable!()
        }
        async fn list_type_schemas(
            &self,
            _query: TypeSchemaQuery,
        ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
            unreachable!()
        }
        async fn register_instances(
            &self,
            _instances: Vec<serde_json::Value>,
        ) -> Result<Vec<RegisterResult>, CanonicalError> {
            unreachable!()
        }
        async fn get_instance(&self, _id: &str) -> Result<GtsInstance, CanonicalError> {
            unreachable!()
        }
        async fn get_instance_by_uuid(&self, _uuid: Uuid) -> Result<GtsInstance, CanonicalError> {
            unreachable!()
        }
        async fn get_instances(
            &self,
            _ids: Vec<String>,
        ) -> HashMap<String, Result<GtsInstance, CanonicalError>> {
            unreachable!()
        }
        async fn get_instances_by_uuid(
            &self,
            _uuids: Vec<Uuid>,
        ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
            unreachable!()
        }
        async fn list_instances(
            &self,
            _query: InstanceQuery,
        ) -> Result<Vec<GtsInstance>, CanonicalError> {
            unreachable!()
        }
    }

    /// Run `Gear::init` against `cluster` with the plugin managing the schema
    /// under `retention_secs`; returns the hub it registered into.
    async fn init(cluster: &Cluster, retention_secs: u64) -> anyhow::Result<Arc<ClientHub>> {
        let hub = Arc::new(ClientHub::default());
        hub.register::<dyn TypesRegistryClient>(Arc::new(RegistryStub));
        let config = json!({ "config": {
            "database_url": format!(
                "http://default:{PASSWORD}@127.0.0.1:{}/{DATABASE}", cluster.port
            ),
            "allow_insecure_http": true,
            "async_insert": false,
            "retention_period_secs": retention_secs,
            "cluster": {
                "name": CLUSTER,
                "manage_schema": true,
                "migration_statement_timeout_secs": 120,
            },
        }});
        let ctx = GearCtx::new(
            "clickhouse-usage-collector-plugin",
            Uuid::from_u128(30),
            Arc::new(StaticConfig(config)),
            Arc::clone(&hub),
            CancellationToken::new(),
        );
        drop(rustls::crypto::aws_lc_rs::default_provider().install_default());
        ClickHouseUsageCollectorPlugin.init(&ctx).await?;
        Ok(hub)
    }

    /// The `usage_records_local` TTL clause of every host, polled until each
    /// names `secs` (or the budget runs out).
    async fn local_ttls_everywhere(client: &clickhouse::Client, secs: u64) -> Vec<String> {
        let sql = format!(
            "SELECT create_table_query FROM clusterAllReplicas('{CLUSTER}', system.tables) \
             WHERE database = '{DATABASE}' AND name = 'usage_records_local'"
        );
        let names = |q: &String| {
            q.contains(&format!("toIntervalSecond({secs})"))
                || q.contains(&format!("INTERVAL {secs} SECOND"))
        };
        let mut seen = Vec::new();
        for _ in 0..60 {
            seen = client
                .query(&sql)
                .fetch_all::<String>()
                .await
                .expect("local tables across the cluster");
            if seen.len() == 2 && seen.iter().all(names) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        seen
    }

    /// The whole startup sequence on a bare cluster: `init` provisions it,
    /// passes its own validation gate and registers the adapter; a restart
    /// with another retention reconciles the TTL `ON CLUSTER` on every host.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Docker (testcontainers)"]
    async fn ch_cluster_gear_init_provisions_and_reconciles_a_bare_cluster() {
        let Some(cluster) = boot_bare_or_skip().await else {
            return;
        };
        let admin = clickhouse::Client::default()
            .with_url(format!("http://127.0.0.1:{}", cluster.port))
            .with_user("default")
            .with_password(PASSWORD);

        let one_year = 31_536_000;
        let hub = init(&cluster, one_year)
            .await
            .expect("init provisions a bare managed cluster");

        let cfg = ClickHousePluginConfig::default();
        let (instance_id, _) = PluginV1::<UsageCollectorPluginSpecV1>::build_registration(
            "cf.core._.clickhouse_usage_collector.v1",
            cfg.vendor,
            cfg.priority,
        )
        .expect("registration payload");
        assert!(
            hub.try_get_scoped::<dyn UsageCollectorPluginV1>(&ClientScope::gts_id(&instance_id))
                .is_some(),
            "init registers the storage adapter"
        );
        // Two steps on each of two hosts.
        assert_eq!(tracker_rows_everywhere(&admin, 4).await, 4);

        let two_years = 2 * one_year;
        init(&cluster, two_years)
            .await
            .expect("a restart with another retention succeeds");
        let ttls = local_ttls_everywhere(&admin, two_years).await;
        assert_eq!(ttls.len(), 2, "both hosts hold the local table: {ttls:?}");
        assert!(
            ttls.iter()
                .all(|q| q.contains(&format!("toIntervalSecond({two_years})"))
                    || q.contains(&format!("INTERVAL {two_years} SECOND"))),
            "the TTL is reconciled on every host: {ttls:?}"
        );
        // No step re-ran on restart.
        assert_eq!(tracker_rows_everywhere(&admin, 4).await, 4);
    }
}
