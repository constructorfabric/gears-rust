//! # Postgres cluster plugin
//!
//! `postgres_cluster_plugin` is the Postgres backend plugin for the cluster
//! gear (DESIGN.md §1). It provides a native `ClusterCacheBackend` over a
//! `sqlx::PgPool` and a native `DistributedLockBackend` over `PostgreSQL`
//! session-level advisory locks. Leader election and service discovery are
//! derived from the SDK default backends over the Postgres cache — no
//! additional tables or connections are required for those two primitives
//! (DESIGN.md §6).
//!
//! This is the recommended deployment for **multi-instance, no-K8s**
//! environments (DESIGN.md §1): Postgres is already deployed in every Gears
//! environment, zero new infrastructure is required, and the native
//! `pg_advisory_lock` gives ACID-correct mutual exclusion without a
//! distributed lock service.
//!
//! ## Lifecycle (outbox-style builder/handle, ADR-006)
//!
//! Like `standalone_cluster_plugin`, this plugin is **not** registered as a
//! `RunnableCapability`. It exposes a builder/handle pair owned by the cluster
//! wiring crate:
//!
//! ```no_run
//! # async fn doc(config: postgres_cluster_plugin::PostgresClusterConfig) -> Result<(), cluster_sdk::ClusterError> {
//! use postgres_cluster_plugin::PostgresClusterPlugin;
//!
//! let handle = PostgresClusterPlugin::builder(config).build_and_start().await?;
//! let _cache = handle.cache();
//! let _lock = handle.lock();
//! // On graceful shutdown:
//! handle.stop().await;
//! # Ok(())
//! # }
//! ```
//!
//! The lock primitive is also independently reachable via the standalone
//! [`PostgresLockPlugin`] (DESIGN.md §3.5), so an operator can route `lock` to
//! Postgres without binding `cache` to it in the same profile.
//!
//! ## Why `sqlx` directly, not `libs/toolkit-db`
//!
//! See DESIGN.md §3.1 — this plugin needs session-pinned advisory locks,
//! `LISTEN`/`NOTIFY` streaming, and `PgPoolOptions` connect/acquire hooks that
//! have no Sea-ORM equivalent. This is a documented, lint-sanctioned exception
//! (`tools/dylint_lints/lint_utils::is_in_postgres_cluster_plugin_path`), not a
//! convenience shortcut.
//!
//! ## Status
//!
//! `PostgresCache`, `PostgresLock`, and both builders' `build_and_start` are
//! implemented per DESIGN.md §3.1's crate structure — this is not scaffolding.
//! The `synchronous_commit = on` re-assertion on pinned lock connections (on
//! each guard task's own interval, §3.4) and the plugin-local
//! `cluster_postgres_lock_active_names` gauge /
//! `cluster_postgres_reaper_sweep_duration_seconds` histogram (DESIGN.md §8) are
//! now implemented (see `docs/GAP-SOLUTIONS.md` §5/§6). Layer 4 fault-injection
//! tests (`docs/TESTING.md` §5) against a real Postgres container are not yet
//! written.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

mod cache;
mod config;
mod lock;
mod pg_error;
mod pg_setup;
mod plugin;
mod provider;

pub use cache::PostgresCache;
pub use config::{PostgresClusterConfig, PostgresLockConfig, ReplicationMode};
pub use lock::{PostgresLock, PostgresLockBuilder, PostgresLockHandle, PostgresLockPlugin};
pub use plugin::{PostgresClusterBuilder, PostgresClusterHandle, PostgresClusterPlugin};
pub use provider::{PROVIDER_NAME, PostgresCacheProvider, PostgresLockProvider};
