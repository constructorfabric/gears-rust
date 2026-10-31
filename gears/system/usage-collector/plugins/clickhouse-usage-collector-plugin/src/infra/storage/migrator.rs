//! Versioned schema migrations, modelled on `sea-orm-migration`.
//!
//! sea-orm itself cannot drive them: neither `sea-query` nor
//! `sea-orm-migration` has a `ClickHouse` backend (there is no sqlx driver,
//! and the table builder has no `ENGINE`, `ORDER BY`, `PARTITION BY`, `TTL`,
//! `SETTINGS` or `ON CLUSTER`). The shape is the same: an ordered list of named
//! steps ([`ChMigration`]), one file each (`mNNNN_*.rs`), with an `up` that
//! talks to a [`SchemaManager`], and a tracking table ([`MIGRATIONS_TRACKER`])
//! recording which steps ran, so a deployment only runs the steps it has not
//! seen yet.
//!
//! **One list for every topology.** A step declares logical tables
//! ([`table::Table`]); the [`SchemaManager`] fills in the rest — plain tables
//! on a single node; on a cluster, replicated shard-local tables behind
//! `Distributed` tables, all `ON CLUSTER` — plus the configured retention. A
//! deployment starts on a single node or on a cluster and stays there.
//!
//! **Every step must be idempotent and safe to run concurrently.** `ClickHouse`
//! has no `pg_advisory_lock`: replicas starting together can each see a step
//! as pending and run it. Steps therefore use `IF NOT EXISTS` DDL and read
//! before they alter; a step is recorded only after it succeeds, and a
//! duplicate record is harmless.
//!
//! **Steps reach the database only through the [`SchemaManager`]**, so the
//! same steps can be recorded instead of run ([`render_ddl`]): that is the
//! DDL an operator runs on a cluster the plugin does not manage.
//!
//! Adding a step: a new `mNNNN_<name>.rs` with the next number, appended to
//! [`migrations`]. Never reorder, rename or remove a step that has shipped.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;

use crate::infra::storage::ddl::{CreateTable, Engine};
use crate::infra::storage::pool::{ensure_insert_dedup_window, exec_statement, timed};
use crate::infra::storage::topology::Topology;

pub mod m0001_init;
mod m0002_insert_dedup_window;
pub mod table;

/// Tracking table of the applied steps; replicated to every host on a cluster.
pub const MIGRATIONS_TRACKER: &str = "usage_collector_migrations";

/// Every step, in order.
fn migrations() -> Vec<Box<dyn ChMigration>> {
    vec![
        Box::new(m0001_init::M0001Init),
        Box::new(m0002_insert_dedup_window::M0002InsertDedupWindow),
    ]
}

/// One versioned schema step.
#[async_trait]
pub(crate) trait ChMigration: Send + Sync {
    /// Unique, ordered name: `m<4 digits>_<snake_case>`.
    fn name(&self) -> &'static str;

    /// Apply the step. Must be idempotent (module docs).
    async fn up(&self, m: &SchemaManager<'_>) -> anyhow::Result<()>;
}

/// Inputs a migration run needs beyond the connection.
#[derive(Debug, Clone, Copy)]
pub struct MigrationOptions {
    /// Configured `usage_records` retention, baked into the TTL of new tables.
    pub retention_period_secs: u64,
}

/// `client` with the server-side timeouts of a cluster migration statement:
/// `send_timeout` / `receive_timeout` for the HTTP exchange, and
/// `distributed_ddl_task_timeout` for how long the initiator waits on every
/// host of an `ON CLUSTER` query (180 s by default, which would otherwise cut
/// the budget short). Used for every migration-time statement on a cluster:
/// [`run_migrations`] and `pool::ensure_retention_ttl`.
#[must_use]
pub fn migration_client(
    client: &clickhouse::Client,
    statement_timeout_secs: u64,
) -> clickhouse::Client {
    let secs = statement_timeout_secs.to_string();
    client
        .clone()
        .with_setting("send_timeout", secs.clone())
        .with_setting("receive_timeout", secs.clone())
        .with_setting("distributed_ddl_task_timeout", secs)
}

/// Where a step's statements go.
enum Sink<'a> {
    /// Executed against the server, each bounded by `deadline`.
    Live {
        client: &'a clickhouse::Client,
        deadline: Duration,
    },
    /// Collected, for [`render_ddl`].
    Record(&'a Mutex<Vec<String>>),
}

/// What a step runs against. Renders tables for the topology and executes or
/// records the result.
pub(crate) struct SchemaManager<'a> {
    sink: Sink<'a>,
    topology: &'a Topology,
    retention_period_secs: u64,
}

impl SchemaManager<'_> {
    /// Create `table` for the topology (`table::Table::to_ddl`).
    pub(crate) async fn create_table(&self, table: &table::Table) -> anyhow::Result<()> {
        for stmt in table.to_ddl(self.topology, self.retention_period_secs)? {
            self.exec(&stmt).await?;
        }
        Ok(())
    }

    /// Execute one statement, already written for the topology.
    pub(crate) async fn exec(&self, sql: &str) -> anyhow::Result<()> {
        match &self.sink {
            Sink::Live { client, deadline } => exec_statement(client, sql, *deadline).await,
            Sink::Record(stmts) => {
                stmts
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(sql.to_owned());
                Ok(())
            }
        }
    }

    /// Reconcile `usage_records`' single-node insert dedup window with the
    /// one `m0001_init` declares (`pool::ensure_insert_dedup_window`).
    ///
    /// Nothing to do on a cluster, where the replicated local tables
    /// deduplicate on their own, or when recording: a fresh table already
    /// carries the window.
    pub(crate) async fn ensure_insert_dedup_window(&self) -> anyhow::Result<()> {
        match (&self.sink, self.topology) {
            (Sink::Live { client, deadline }, Topology::SingleNode) => {
                ensure_insert_dedup_window(client, *deadline).await
            }
            _ => Ok(()),
        }
    }
}

/// The tracking table's DDL for `topology`.
fn tracker_ddl(topology: &Topology) -> String {
    let (cluster, engine) = match topology {
        Topology::SingleNode => (None, Engine::ReplacingMergeTree { version: None }),
        // One Keeper path for the whole cluster and a replica name unique per
        // host, so every host holds the same history.
        Topology::Cluster { name, .. } => (
            Some(name.as_str()),
            Engine::ReplicatedReplacingMergeTree {
                keeper: Some((
                    format!("/clickhouse/tables/{{database}}/{MIGRATIONS_TRACKER}"),
                    "{shard}_{replica}".to_owned(),
                )),
                version: None,
            },
        ),
    };
    CreateTable::new(MIGRATIONS_TRACKER)
        .if_not_exists()
        .on_cluster(cluster)
        .column(
            "name",
            "String",
            "Applied migration step (ChMigration::name)",
        )
        .column_with_default(
            "applied_at",
            "DateTime64(3)",
            "now64(3)",
            "When the step finished",
        )
        .engine(engine)
        .order_by(&["name"])
        .to_sql()
}

/// Names of the steps, in order.
#[must_use]
pub fn migration_names() -> Vec<&'static str> {
    migrations().iter().map(|m| m.name()).collect()
}

/// Every step's statements for `topology`, in order, without touching a
/// server: the DDL an operator runs to provision a cluster the plugin does
/// not manage. Running it again is a no-op.
///
/// Steps that only reconcile live state (`m0002_insert_dedup_window`) record
/// nothing.
///
/// # Errors
///
/// Returns an error naming the step that cannot be rendered for `topology`.
pub async fn render_ddl(
    topology: &Topology,
    retention_period_secs: u64,
) -> anyhow::Result<Vec<String>> {
    let recorded = Mutex::new(Vec::new());
    let m = SchemaManager {
        sink: Sink::Record(&recorded),
        topology,
        retention_period_secs,
    };
    for step in migrations() {
        step.up(&m)
            .await
            .with_context(|| format!("rendering schema migration {}", step.name()))?;
    }
    Ok(recorded
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner))
}

/// Create the tracking table if missing and read the recorded step names.
async fn applied_steps(
    client: &clickhouse::Client,
    topology: &Topology,
    deadline: Duration,
) -> anyhow::Result<Vec<String>> {
    exec_statement(client, &tracker_ddl(topology), deadline)
        .await
        .context("failed to create the migration tracking table")?;
    timed(
        deadline,
        "reading applied migrations",
        client
            .query(&format!("SELECT DISTINCT name FROM {MIGRATIONS_TRACKER}"))
            .fetch_all(),
    )
    .await
}

/// Run one step and record it once it succeeded.
async fn apply_step(
    m: &SchemaManager<'_>,
    client: &clickhouse::Client,
    deadline: Duration,
    step: &dyn ChMigration,
) -> anyhow::Result<()> {
    let name = step.name();
    tracing::info!(migration = name, "applying schema migration");
    step.up(m)
        .await
        .with_context(|| format!("schema migration {name} failed"))?;
    timed(
        deadline,
        "recording an applied migration",
        client
            .query(&format!(
                "INSERT INTO {MIGRATIONS_TRACKER} (name) VALUES (?)"
            ))
            .bind(name)
            .execute(),
    )
    .await
}

/// Run every pending migration step for `topology`, in order.
///
/// Creates the tracking table if missing, skips the steps it records, and
/// records each step after it succeeds. A deployment that predates the
/// tracking table runs every step, which is a no-op on its existing schema
/// since all steps are idempotent. Each statement is bounded by `deadline`
/// on the client side; the server-side timeouts are `client`'s own, so a
/// cluster caller passes a [`migration_client`].
///
/// Returns the names of the steps it ran.
///
/// # Errors
///
/// Returns an error naming the failing step when a step fails, or when the
/// tracking table cannot be created, read or written.
pub async fn run_migrations(
    client: &clickhouse::Client,
    topology: &Topology,
    opts: MigrationOptions,
    deadline: Duration,
) -> anyhow::Result<Vec<&'static str>> {
    let m = SchemaManager {
        sink: Sink::Live { client, deadline },
        topology,
        retention_period_secs: opts.retention_period_secs,
    };

    let applied = applied_steps(client, topology, deadline).await?;
    let mut ran = Vec::new();
    for step in migrations() {
        if !applied.iter().any(|a| a == step.name()) {
            apply_step(&m, client, deadline, step.as_ref()).await?;
            ran.push(step.name());
        }
    }
    if ran.is_empty() {
        tracing::debug!("schema is up to date");
    }
    Ok(ran)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "migrator_tests.rs"]
mod migrator_tests;
