//! # Postgres cluster plugin
//!
//! `postgres_cluster_plugin` is the Postgres backend plugin for the cluster
//! gear (DESIGN.md §1). It provides a native `ClusterCacheBackend` over a
//! `sqlx::PgPool` and a native `DistributedLockBackend` over a `cluster_lock`
//! lease row, with one per-instance advisory lock kept purely as a liveness
//! beacon (DESIGN.md §5). Leader election is derived from the SDK default
//! backend over the Postgres cache — no additional tables or connections are
//! required for that primitive (DESIGN.md §6).
//!
//! This is the recommended deployment for **multi-instance, no-K8s**
//! environments (DESIGN.md §1): Postgres is already deployed in every Gears
//! environment, zero new infrastructure is required, and a conditional upsert
//! under `synchronous_commit = on` gives ACID-correct mutual exclusion without
//! a distributed lock service.
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
//! See DESIGN.md §3.1 — this plugin needs an owned, long-lived connection for
//! the liveness beacon (and a `pg_locks` join inside the acquire predicate that
//! reads it), `LISTEN`/`NOTIFY` streaming, and `PgPoolOptions` connect/acquire
//! hooks that have no Sea-ORM equivalent. This is a documented, deliberate architectural
//! exception, not a convenience shortcut — hence the crate-level
//! `#![allow(de0706_no_direct_sqlx)]` below. It used to be a lint-sanctioned
//! exclusion baked into this repo's own `tools/dylint_lints` (via
//! `lint_utils::is_in_postgres_cluster_plugin_path`); that local lint crate was
//! removed in favor of the published `cargo-gears-lints`, whose `DE0706`
//! implementation carries no equivalent per-repo path allowlist, so the
//! exception now lives here instead.
//!
//! ## Status
//!
//! `PostgresCache`, `PostgresLock`, and both builders' `build_and_start` are
//! implemented per DESIGN.md §3.1's crate structure — this is not scaffolding.
//! The plugin-local `cluster_postgres_lock_active_names` gauge /
//! `cluster_postgres_reaper_sweep_duration_seconds` histogram (DESIGN.md §8) are
//! now implemented (see `docs/GAP-SOLUTIONS.md` §5/§6). Layer 4 fault-injection
//! tests (`docs/TESTING.md` §5) against a real Postgres container are not yet
//! written.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
// See "Why `sqlx` directly, not `libs/toolkit-db`" above. `unknown_lints` is
// needed alongside: `de0706_no_direct_sqlx` only exists as a registered lint
// under the `cargo gears lint --dylint` driver, not plain `cargo
// build`/`clippy`, so without it this `allow` itself would warn.
#![allow(unknown_lints)]
#![allow(de0706_no_direct_sqlx)]

mod cache;
mod config;
mod limits;
mod lock;
mod pg_error;
mod pg_setup;
mod plugin;
mod provider;
mod shutdown;

pub use cache::PostgresCache;
pub use config::{PostgresClusterConfig, PostgresLockConfig, ReplicationMode};
pub use lock::{PostgresLock, PostgresLockBuilder, PostgresLockHandle, PostgresLockPlugin};
pub use plugin::{PostgresClusterBuilder, PostgresClusterHandle, PostgresClusterPlugin};
pub use provider::{PROVIDER_NAME, PostgresCacheProvider, PostgresLockProvider};
