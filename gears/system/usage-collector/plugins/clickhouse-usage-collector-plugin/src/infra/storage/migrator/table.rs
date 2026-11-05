//! A logical table as a migration step declares it, and its rendering per
//! topology.
//!
//! A step names the logical table (`usage_records`) and describes its
//! columns, keys, TTL column and settings once. [`Table::to_ddl`] fills in
//! everything that depends on where the plugin runs:
//! - single node: one `ReplacingMergeTree` table under the logical name, with
//!   the single-node insert dedup window;
//! - cluster: a shard-local [`cluster_local_engine`] table under the
//!   configured local name, plus a `Distributed` table under the logical name
//!   over it, sharded by the table's sharding key; both `ON CLUSTER`;
//! - the TTL interval, from the configured retention.

use anyhow::Context as _;

use crate::infra::storage::ddl::{CreateTable, Engine, SkipIndex};
use crate::infra::storage::topology::Topology;

/// Engine of every shard-local table on a cluster. Replication is what makes
/// the insert dedup token effective on async inserts (through
/// `replicated_deduplication_window`) and the data highly available.
///
/// No Keeper arguments: the server's `default_replica_path` /
/// `default_replica_name` apply (`/clickhouse/tables/{uuid}/{shard}`,
/// `{replica}`), which is why cluster DDL must run `ON CLUSTER`: that hands
/// every host the same table UUID and therefore the same Keeper path.
#[must_use]
pub fn cluster_local_engine(version: Option<&'static str>) -> Engine {
    Engine::ReplicatedReplacingMergeTree {
        keeper: None,
        version,
    }
}

/// `<column> + INTERVAL <secs> SECOND DELETE`, the TTL of a table expired
/// after `secs`.
///
/// Uses the column directly so a `DateTime64` column keeps its range. Do not
/// wrap it in `toDateTime`: that casts to 32-bit `DateTime` (saturating in
/// 2106) and can expire rows the moment they are written.
#[must_use]
pub fn ttl_expr(column: &str, secs: u64) -> String {
    format!("{column} + INTERVAL {secs} SECOND DELETE")
}

/// A table independent of topology. Built by a migration step, rendered by
/// the migrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    name: &'static str,
    columns: Vec<(&'static str, &'static str, &'static str)>,
    indexes: Vec<SkipIndex>,
    version: Option<&'static str>,
    partition_by: Option<&'static str>,
    order_by: &'static [&'static str],
    ttl_column: Option<&'static str>,
    settings: Vec<(&'static str, String)>,
    insert_dedup_window: Option<u64>,
    sharding_key: Option<&'static str>,
}

impl Table {
    /// A table with the logical `name` every query uses.
    #[must_use]
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            columns: Vec::new(),
            indexes: Vec::new(),
            version: None,
            partition_by: None,
            order_by: &[],
            ttl_column: None,
            settings: Vec::new(),
            insert_dedup_window: None,
            sharding_key: None,
        }
    }

    /// Append a column `name ty COMMENT 'comment'`.
    #[must_use]
    pub fn column(mut self, name: &'static str, ty: &'static str, comment: &'static str) -> Self {
        self.columns.push((name, ty, comment));
        self
    }

    /// Append data-skipping indexes.
    #[must_use]
    pub fn indexes(mut self, indexes: &[SkipIndex]) -> Self {
        self.indexes.extend_from_slice(indexes);
        self
    }

    /// The `ReplacingMergeTree` version column.
    #[must_use]
    pub fn version(mut self, column: &'static str) -> Self {
        self.version = Some(column);
        self
    }

    #[must_use]
    pub fn partition_by(mut self, expr: &'static str) -> Self {
        self.partition_by = Some(expr);
        self
    }

    #[must_use]
    pub fn order_by(mut self, keys: &'static [&'static str]) -> Self {
        self.order_by = keys;
        self
    }

    /// Expire rows once `column` is older than the configured retention
    /// ([`ttl_expr`]).
    #[must_use]
    pub fn ttl_delete_after(mut self, column: &'static str) -> Self {
        self.ttl_column = Some(column);
        self
    }

    /// Append one `SETTINGS` entry, rendered in every topology.
    #[must_use]
    pub fn setting(mut self, key: &'static str, value: &impl ToString) -> Self {
        self.settings.push((key, value.to_string()));
        self
    }

    /// Keep the last `blocks` inserted blocks' `insert_deduplication_token`s.
    ///
    /// Rendered as `non_replicated_deduplication_window` on a single node
    /// only: on a cluster the replicated local table deduplicates through its
    /// own `replicated_deduplication_window`.
    #[must_use]
    pub fn insert_dedup_window(mut self, blocks: u64) -> Self {
        self.insert_dedup_window = Some(blocks);
        self
    }

    /// The `Distributed` sharding expression on a cluster; required there.
    #[must_use]
    pub fn sharding_key(mut self, expr: &'static str) -> Self {
        self.sharding_key = Some(expr);
        self
    }

    /// The `CREATE TABLE IF NOT EXISTS` statements for `topology`, in
    /// creation order.
    ///
    /// # Errors
    ///
    /// On a cluster, when the table has no sharding key or the topology
    /// configures no local table for it.
    pub fn to_ddl(&self, topology: &Topology, retention_secs: u64) -> anyhow::Result<Vec<String>> {
        match topology {
            Topology::SingleNode => {
                let mut table = self.storage_table(
                    self.name,
                    None,
                    Engine::ReplacingMergeTree {
                        version: self.version,
                    },
                    retention_secs,
                );
                if let Some(blocks) = self.insert_dedup_window {
                    table = table.setting("non_replicated_deduplication_window", &blocks);
                }
                Ok(vec![table.to_sql()])
            }
            Topology::Cluster { name: cluster, .. } => {
                let local = topology
                    .local_table(self.name)
                    .with_context(|| format!("no local table is configured for `{}`", self.name))?;
                let sharding_key = self
                    .sharding_key
                    .with_context(|| format!("`{}` has no sharding key", self.name))?;
                let local_table = self.storage_table(
                    local,
                    Some(cluster),
                    cluster_local_engine(self.version),
                    retention_secs,
                );
                let distributed = CreateTable::new(self.name)
                    .if_not_exists()
                    .on_cluster(Some(cluster))
                    .copy_columns_of(local)
                    .engine(Engine::Distributed {
                        cluster: cluster.clone(),
                        table: local.to_owned(),
                        sharding_key,
                    });
                Ok(vec![local_table.to_sql(), distributed.to_sql()])
            }
        }
    }

    /// The table that holds the rows, under `name`.
    fn storage_table(
        &self,
        name: &str,
        on_cluster: Option<&str>,
        engine: Engine,
        retention_secs: u64,
    ) -> CreateTable {
        let mut table = CreateTable::new(name)
            .if_not_exists()
            .on_cluster(on_cluster);
        for &(column, ty, comment) in &self.columns {
            table = table.column(column, ty, comment);
        }
        table = table
            .indexes(&self.indexes)
            .engine(engine)
            .order_by(self.order_by);
        if let Some(expr) = self.partition_by {
            table = table.partition_by(expr);
        }
        if let Some(column) = self.ttl_column {
            table = table.ttl(ttl_expr(column, retention_secs));
        }
        for (key, value) in &self.settings {
            table = table.setting(key, value);
        }
        table
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "table_tests.rs"]
mod table_tests;
