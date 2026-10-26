//! Deployment topology: single node, or a sharded replicated cluster.
//!
//! Every `SELECT` and `INSERT` in this plugin names the logical tables
//! `usage_records` / `usage_type_catalog`. On a single node those are the
//! `ReplacingMergeTree` tables themselves; on a cluster they are `Distributed`
//! tables over shard-local `ReplicatedReplacingMergeTree` tables, so the query
//! text is the same in both topologies. The only statements that must name the
//! physical table are mutations — `ClickHouse` refuses `ALTER TABLE … DELETE`
//! on a `Distributed` table — and those go through [`Topology::delete_where_sql`].
//!
//! **Shard-colocation invariant.** `usage_records` is sharded by
//! `cityHash64(tenant_id)`. A deactivation marker clones its source row
//! (`mapper::make_inactive_marker`), and a compensation row belongs to the
//! tenant of the row it corrects, so every version of a row — and every row an
//! `id NOT IN (SELECT id … status = 'inactive')` anti-join must see — lives on
//! one shard. That is what makes `distributed_product_mode = 'local'` (see
//! [`Topology::client_settings`]) exact rather than an approximation.

use crate::config::ClickHousePluginConfig;

/// Logical name of the usage-records table (the `Distributed` table on a cluster).
pub const RECORDS_TABLE: &str = "usage_records";
/// Logical name of the usage-type catalog table (the `Distributed` table on a cluster).
pub const CATALOG_TABLE: &str = "usage_type_catalog";

/// Sharding expression the cluster schema must use for `usage_records`.
pub const RECORDS_SHARDING_KEY: &str = "cityHash64(tenant_id)";
/// Sharding expression the cluster schema must use for `usage_type_catalog`.
pub const CATALOG_SHARDING_KEY: &str = "cityHash64(gts_id)";

/// Where the plugin's tables physically live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Topology {
    /// One server (optionally replicated by the operator); the logical tables
    /// are the storage tables.
    SingleNode,
    /// Sharded cluster: logical tables are `Distributed`, data lives in the
    /// shard-local tables named here.
    Cluster {
        /// Cluster name from the server's `remote_servers`.
        name: String,
        /// Local table behind [`RECORDS_TABLE`].
        records_local: String,
        /// Local table behind [`CATALOG_TABLE`].
        catalog_local: String,
        /// `insert_quorum` sent with every insert, when configured.
        insert_quorum: Option<String>,
    },
}

impl Topology {
    /// Derive the topology from a validated config.
    #[must_use]
    pub fn from_config(cfg: &ClickHousePluginConfig) -> Self {
        match &cfg.cluster {
            None => Self::SingleNode,
            Some(c) => Self::Cluster {
                name: c.name.clone(),
                records_local: c.records_local_table.clone(),
                catalog_local: c.catalog_local_table.clone(),
                insert_quorum: c.insert_quorum.clone(),
            },
        }
    }

    /// Settings attached to the shared client — and therefore to every request
    /// — in this topology. Empty on a single node.
    ///
    /// * `distributed_product_mode = 'local'` — rewrites the `IN` subqueries
    ///   over `usage_records` to the shard-local table, so each shard
    ///   anti-joins against its own markers. Exact by the shard-colocation
    ///   invariant (module docs); without it `ClickHouse` rejects the
    ///   subquery (`DISTRIBUTED_IN_JOIN_SUBQUERY_DENIED`).
    /// * `optimize_skip_unused_shards = 1` — a query pinning `tenant_id`
    ///   reads only the owning shard.
    /// * `distributed_foreground_insert = 1` — an `INSERT` into a
    ///   `Distributed` table returns only once the target shard committed it,
    ///   so errors surface to the caller and the `insert_deduplication_token`
    ///   is enforced by the replicated table's dedup window.
    /// * `insert_quorum` — when configured.
    ///
    /// The insert settings ride on `SELECT`s too, where the server ignores
    /// them; one client-wide list keeps every call site topology-agnostic.
    #[must_use]
    pub fn client_settings(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::SingleNode => Vec::new(),
            Self::Cluster { insert_quorum, .. } => {
                let mut settings = vec![
                    ("distributed_product_mode", "local".to_owned()),
                    ("optimize_skip_unused_shards", "1".to_owned()),
                    ("distributed_foreground_insert", "1".to_owned()),
                ];
                if let Some(q) = insert_quorum {
                    settings.push(("insert_quorum", q.clone()));
                }
                settings
            }
        }
    }

    /// `mutations_sync` for `ALTER TABLE … DELETE`: wait for the local
    /// mutation on a single node (`1`), for every replica on a cluster (`2`),
    /// so a follow-up read through the `Distributed` table cannot hit a
    /// replica that still holds the deleted rows.
    #[must_use]
    pub fn mutations_sync(&self) -> &'static str {
        match self {
            Self::SingleNode => "1",
            Self::Cluster { .. } => "2",
        }
    }

    /// `ALTER TABLE … DELETE WHERE gts_id = ?` against the physical table
    /// behind `logical_table` ([`RECORDS_TABLE`] or [`CATALOG_TABLE`]).
    ///
    /// On a cluster this targets the local table `ON CLUSTER`, because a
    /// mutation cannot run on a `Distributed` table and the rows of one
    /// `gts_id` are spread over every shard. Identifiers come from validated
    /// config (`ClickHouseClusterConfig::validate`) or the `'static` logical
    /// names; `gts_id` stays a bind.
    #[must_use]
    pub fn delete_where_sql(&self, logical_table: &str) -> String {
        let (table, on_cluster) = self.alter_target(logical_table);
        format!("ALTER TABLE {table}{on_cluster} DELETE WHERE gts_id = ?")
    }

    /// The shard-local table behind `logical_table` on a cluster; `None` on a
    /// single node or for a table the topology configures no local table for.
    #[must_use]
    pub fn local_table(&self, logical_table: &str) -> Option<&str> {
        match self {
            Self::SingleNode => None,
            Self::Cluster {
                records_local,
                catalog_local,
                ..
            } => match logical_table {
                RECORDS_TABLE => Some(records_local),
                CATALOG_TABLE => Some(catalog_local),
                _ => None,
            },
        }
    }

    /// The table an `ALTER` on `logical_table` ([`RECORDS_TABLE`] or
    /// [`CATALOG_TABLE`]) must name, and the ` ON CLUSTER <name>` clause it
    /// needs (empty on a single node). On a cluster that is the shard-local
    /// table on every host: `ClickHouse` refuses mutations and most `ALTER`s
    /// on a `Distributed` table.
    #[must_use]
    pub fn alter_target<'a>(&'a self, logical_table: &'a str) -> (&'a str, String) {
        match self {
            Self::SingleNode => (logical_table, String::new()),
            Self::Cluster {
                name,
                records_local,
                catalog_local,
                ..
            } => {
                let local = if logical_table == RECORDS_TABLE {
                    records_local
                } else {
                    catalog_local
                };
                (local, format!(" ON CLUSTER {name}"))
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "topology_tests.rs"]
mod topology_tests;
