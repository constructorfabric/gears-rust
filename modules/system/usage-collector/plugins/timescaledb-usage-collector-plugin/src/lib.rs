//! `TimescaleDB` Usage Collector Plugin
//!
//! A production [`usage_collector_sdk::UsageCollectorPluginClientV1`] implementation
//! backed by `TimescaleDB` for durable high-throughput usage record persistence,
//! aggregation query pushdown via continuous aggregates, and cursor-based raw pagination.
//!
//! ## Configuration
//!
//! `database_url` must include `sslmode=require`, `sslmode=verify-ca`, or
//! `sslmode=verify-full`; plaintext connections are rejected by [`config::TimescaleDbConfig::validate`].
//!
//! The top-level key under `modules` MUST match the `ModKit` module name
//! declared in `#[modkit::module(name = "timescaledb-usage-collector-plugin")]`
//! because `ctx.config()` reads `modules.<module-name>.config` verbatim.
//!
//! ```yaml
//! modules:
//!   timescaledb-usage-collector-plugin:
//!     config:
//!       database_url: "postgres://user:pass@host/db?sslmode=require"
//!       pool_size_min: 2
//!       pool_size_max: 16
//!       retention_default: "365days"
//!       connection_timeout: "10s"
//! ```
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod config;
pub(crate) mod domain;
pub(crate) mod infra;
mod module;

/// Re-exports of crate-internal types for integration tests living in
/// `tests/`. Gated by the `integration` feature so the plugin's public API
/// surface remains the `usage_collector_sdk::UsageCollectorPluginClientV1`
/// trait it registers via the client hub (see MODKIT-CORE-001).
///
/// External crates must not import from `__integration_test_api` outside of
/// the integration test crate of this plugin.
#[cfg(feature = "integration")]
#[doc(hidden)]
pub mod __integration_test_api {
    pub mod domain {
        pub mod client {
            pub use crate::domain::client::TimescaleDbPluginClient;
        }
        pub mod insert_port {
            pub use crate::domain::insert_port::InsertPort;
        }
        pub mod metrics {
            pub use crate::domain::metrics::{NoopMetrics, PluginMetrics};
        }
        pub mod query_port {
            pub use crate::domain::query_port::QueryPort;
        }
    }
    pub mod infra {
        pub mod continuous_aggregate {
            pub use crate::infra::continuous_aggregate::setup_continuous_aggregate;
        }
        pub mod migrations {
            pub use crate::infra::migrations::run_migrations;
        }
        pub mod pg_insert_port {
            pub use crate::infra::pg_insert_port::PgInsertPort;
        }
        pub mod pg_query_port {
            pub use crate::infra::pg_query_port::PgQueryPort;
        }
    }
    pub mod module {
        pub use crate::module::health_check_for_tests;
    }
}
