use std::sync::Arc;

use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use toolkit::{ClientHub, ConfigProvider, Gear, GearCtx};

use super::TimescaleDbUsageCollectorPlugin;

/// Minimal [`ConfigProvider`] serving one fixed gear-config JSON.
///
/// `gear_config_or_default` reads the gear node's `config` sub-object, so the
/// value must be shaped `{ "config": { ... } }`.
struct StaticConfig(serde_json::Value);

impl ConfigProvider for StaticConfig {
    fn get_gear_config(&self, _gear_name: &str) -> Option<&serde_json::Value> {
        Some(&self.0)
    }
}

#[tokio::test]
async fn init_aborts_before_startup_io_when_already_cancelled() {
    // `cfg.validate()` runs before the cancel race and requires a non-empty
    // `database_url`; the bogus DSN is never dialed because the cancelled token
    // short-circuits before `build_pool`.
    let provider = Arc::new(StaticConfig(json!({
        "config": { "database_url": "postgres://127.0.0.1:1/unused?sslmode=disable" }
    })));

    let cancel = CancellationToken::new();
    cancel.cancel();

    let ctx = GearCtx::new(
        "timescaledb-usage-collector-plugin",
        Uuid::from_u128(1),
        provider,
        Arc::new(ClientHub::default()),
        cancel,
    );

    let err = TimescaleDbUsageCollectorPlugin
        .init(&ctx)
        .await
        .expect_err("a cancelled token must abort init before any startup I/O");

    assert!(
        err.to_string().contains("init cancelled during shutdown"),
        "unexpected error: {err}"
    );
}
