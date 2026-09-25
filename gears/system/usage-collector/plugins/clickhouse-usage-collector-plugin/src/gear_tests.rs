use std::sync::Arc;

use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use toolkit::{ClientHub, ConfigProvider, Gear, GearCtx};

use super::ClickHouseUsageCollectorPlugin;

/// Minimal [`ConfigProvider`] serving one fixed gear-config JSON.
///
/// `config_expanded_or_default` reads the gear node's `config` sub-object, so
/// the value must be shaped `{ "config": { ... } }`.
struct StaticConfig(serde_json::Value);

impl ConfigProvider for StaticConfig {
    fn get_gear_config(&self, _gear_name: &str) -> Option<&serde_json::Value> {
        Some(&self.0)
    }
}

/// Install a rustls `CryptoProvider` for this test process.
///
/// Only needed by tests that let `init` reach step A: `build_client` fails
/// closed when no provider is installed (see `pool.rs::new_base_client`), and
/// in production `toolkit::bootstrap::init_procedure` installs one before any
/// `Gear::init` runs. Tests that assert on a *config* rejection never get that
/// far and do not need it.
///
/// Must not be hoisted into a shared once-per-process fixture: under
/// `cargo nextest` each test is its own process, so the install has to happen
/// in the test that depends on it rather than being inherited from whichever
/// test happened to run first. That in-process ordering dependency is exactly
/// what made this a latent `cargo test` pass and a `make test-fips` failure.
///
/// Idempotent and race-safe: `install_default` is backed by a process-wide
/// `OnceLock`, so a second call returns `Err` and is deliberately dropped.
/// `drop` rather than `let _ =` satisfies `clippy::let_underscore_must_use`.
fn install_test_crypto_provider() {
    drop(rustls::crypto::aws_lc_rs::default_provider().install_default());
}

#[tokio::test]
async fn init_rejects_empty_database_url() {
    let provider = Arc::new(StaticConfig(json!({
        "config": {}
    })));

    let ctx = GearCtx::new(
        "clickhouse-usage-collector-plugin",
        Uuid::from_u128(1),
        provider,
        Arc::new(ClientHub::default()),
        CancellationToken::new(),
    );

    let err = ClickHouseUsageCollectorPlugin
        .init(&ctx)
        .await
        .expect_err("empty database_url must be rejected before any ClickHouse I/O");

    assert!(
        err.to_string().contains("database_url"),
        "expected database_url validation error, got: {err}"
    );
}

#[tokio::test]
async fn init_rejects_plaintext_http_database_url_without_override() {
    let provider = Arc::new(StaticConfig(json!({
        "config": {
            "database_url": "http://user:pass@ch:8123/usage"
        }
    })));

    let ctx = GearCtx::new(
        "clickhouse-usage-collector-plugin",
        Uuid::from_u128(4),
        provider,
        Arc::new(ClientHub::default()),
        CancellationToken::new(),
    );

    let err = ClickHouseUsageCollectorPlugin
        .init(&ctx)
        .await
        .expect_err("plaintext http:// database_url must be rejected without an explicit override");

    assert!(
        err.to_string().contains("allow_insecure_http"),
        "expected an allow_insecure_http validation error, got: {err}"
    );
}

/// A config that validates but names a backend nothing answers on must fail
/// `init` at the migration step rather than reporting the gear ready.
///
/// `init` publishes the readiness gauge as 0 *before* any startup I/O and
/// flips it to 1 only after the whole sequence, so a plugin that could not
/// migrate must never reach that flip. This pins the failure to step B: the
/// error is the migration's own, not a config rejection, which is what proves
/// validation passed and the startup sequence actually ran.
///
/// `https://` keeps `allow_insecure_http` out of it; port 1 is reserved and
/// never bound, so the connection is refused immediately rather than hanging
/// out the 35s client deadline.
#[tokio::test]
async fn init_fails_at_the_migration_step_when_the_backend_is_unreachable() {
    let provider = Arc::new(StaticConfig(json!({
        "config": {
            "database_url": "https://user:pass@127.0.0.1:1/usage"
        }
    })));

    let ctx = GearCtx::new(
        "clickhouse-usage-collector-plugin",
        Uuid::from_u128(7),
        provider,
        Arc::new(ClientHub::default()),
        CancellationToken::new(),
    );

    // Step A (build_client) needs an installed provider, or `init` fails there
    // and never reaches the migration this test is about.
    install_test_crypto_provider();

    let err = ClickHouseUsageCollectorPlugin
        .init(&ctx)
        .await
        .expect_err("init must not report success when the schema migration cannot run");

    let msg = format!("{err:#}");
    assert!(
        msg.contains("migration DDL statement"),
        "the failure must come from the migration step, not from config validation \
         or the registry handshake, got: {msg}"
    );
}
