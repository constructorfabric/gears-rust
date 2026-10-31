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

/// An already-cancelled token must abort `init` before any startup I/O.
///
/// `cfg.validate()` runs before the cancel race, so the config must be valid
/// (`https://` keeps `allow_insecure_http` out of it); the backend is never
/// dialed. No crypto provider is installed on purpose: the biased `select!`
/// short-circuits before step A, so `build_client` never runs — had it run,
/// `init` would fail with a crypto-provider error instead of the cancellation.
#[tokio::test]
async fn init_aborts_before_startup_io_when_already_cancelled() {
    let provider = Arc::new(StaticConfig(json!({
        "config": {
            "database_url": "https://user:pass@127.0.0.1:1/usage"
        }
    })));

    let cancel = CancellationToken::new();
    cancel.cancel();

    let ctx = GearCtx::new(
        "clickhouse-usage-collector-plugin",
        Uuid::from_u128(10),
        provider,
        Arc::new(ClientHub::default()),
        cancel,
    );

    let err = ClickHouseUsageCollectorPlugin
        .init(&ctx)
        .await
        .expect_err("a cancelled token must abort init before any startup I/O");

    assert!(
        err.to_string().contains("init cancelled during shutdown"),
        "unexpected error: {err}"
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

// ── The full init sequence against a mocked ClickHouse ──────────────────────
//
// `database_url` points at a `clickhouse::test::Mock`: `build_client` runs
// unchanged, and the mock answers the startup statements in the order `init`
// sends them (an extra or a missing request fails the test). The types
// registry is a stub registered in the `ClientHub`.

mod mocked {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use clickhouse::test::Mock;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;
    use toolkit::client_hub::ClientScope;
    use toolkit::gts::PluginV1;
    use toolkit_canonical_errors::CanonicalError;
    use types_registry_sdk::{
        GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery,
        TypesRegistryClient,
    };
    use usage_collector_sdk::{UsageCollectorPluginSpecV1, UsageCollectorPluginV1};
    use uuid::Uuid;

    use toolkit::{ClientHub, Gear, GearCtx};

    use super::super::ClickHouseUsageCollectorPlugin;
    use super::{StaticConfig, install_test_crypto_provider};
    use crate::config::ClickHousePluginConfig;
    use crate::infra::storage::migrator::migration_names;
    use crate::infra::storage::test_mock::{create_sql, fail, ok, rows, ttl, valid_cluster_probes};

    const RETENTION: u64 = 31_536_000;

    /// Answers `register` with one result per entity; `init` calls nothing
    /// else.
    struct RegistryStub {
        reject: bool,
    }

    #[async_trait]
    impl TypesRegistryClient for RegistryStub {
        async fn register(
            &self,
            entities: Vec<serde_json::Value>,
        ) -> Result<Vec<RegisterResult>, CanonicalError> {
            Ok(entities
                .iter()
                .map(|e| {
                    let gts_id = e["id"].as_str().unwrap_or_default().to_owned();
                    if self.reject {
                        RegisterResult::Err {
                            gts_id: Some(gts_id),
                            error: CanonicalError::internal("rejected by the stub").create(),
                        }
                    } else {
                        RegisterResult::Ok { gts_id }
                    }
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

    /// A hub holding the registry stub, or none at all for `registry: None`.
    fn hub(registry: Option<RegistryStub>) -> Arc<ClientHub> {
        let hub = Arc::new(ClientHub::default());
        if let Some(stub) = registry {
            hub.register::<dyn TypesRegistryClient>(Arc::new(stub));
        }
        hub
    }

    /// A gear context over `database_url` with `extra` merged into the config.
    fn ctx(
        database_url: &str,
        extra: &serde_json::Value,
        hub: Arc<ClientHub>,
        cancel: CancellationToken,
    ) -> GearCtx {
        let mut config = json!({
            "database_url": database_url,
            "allow_insecure_http": true,
            "retention_period_secs": RETENTION,
        });
        if let (Some(config), Some(extra)) = (config.as_object_mut(), extra.as_object()) {
            config.extend(extra.clone());
        }
        GearCtx::new(
            "clickhouse-usage-collector-plugin",
            Uuid::from_u128(20),
            Arc::new(StaticConfig(json!({ "config": config }))),
            hub,
            cancel,
        )
    }

    /// A cluster config `init` accepts; `async_insert` is refused with one.
    fn cluster(manage_schema: bool) -> serde_json::Value {
        json!({
            "async_insert": false,
            "cluster": { "name": "uc", "manage_schema": manage_schema },
        })
    }

    /// The scope `init` registers the adapter under, for the default vendor
    /// and priority.
    fn adapter_scope() -> ClientScope {
        let cfg = ClickHousePluginConfig::default();
        let (instance_id, _) = PluginV1::<UsageCollectorPluginSpecV1>::build_registration(
            "cf.core._.clickhouse_usage_collector.v1",
            cfg.vendor,
            cfg.priority,
        )
        .expect("the registration payload builds");
        ClientScope::gts_id(&instance_id)
    }

    /// An up-to-date single-node schema: tracker DDL, every step recorded,
    /// and a TTL that already matches.
    fn up_to_date_single_node(mock: &Mock) {
        ok(mock);
        rows(mock, migration_names().into_iter().map(str::to_owned));
        rows(mock, [create_sql(&ttl(RETENTION), None)]);
    }

    #[tokio::test]
    async fn init_on_a_single_node_registers_the_adapter() {
        install_test_crypto_provider();
        let mock = Mock::new();
        up_to_date_single_node(&mock);
        let hub = hub(Some(RegistryStub { reject: false }));
        let cancel = CancellationToken::new();

        ClickHouseUsageCollectorPlugin
            .init(&ctx(
                mock.url(),
                &json!({}),
                Arc::clone(&hub),
                cancel.clone(),
            ))
            .await
            .expect("init succeeds on an up-to-date schema");

        assert!(
            hub.try_get_scoped::<dyn UsageCollectorPluginV1>(&adapter_scope())
                .is_some(),
            "the storage adapter is resolvable by its GTS instance id"
        );
        // Shutdown: the readiness watcher clears the gauge and exits.
        cancel.cancel();
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn init_on_an_operator_provisioned_cluster_only_validates_it() {
        install_test_crypto_provider();
        let mock = Mock::new();
        valid_cluster_probes(&mock, 2, RETENTION);

        ClickHouseUsageCollectorPlugin
            .init(&ctx(
                mock.url(),
                &cluster(false),
                hub(Some(RegistryStub { reject: false })),
                CancellationToken::new(),
            ))
            .await
            .expect("init succeeds on a usable cluster schema");
    }

    /// Managed: migrations and the TTL reconcile `ON CLUSTER`, then the same
    /// validation gate.
    #[tokio::test]
    async fn init_on_a_managed_cluster_migrates_then_validates() {
        install_test_crypto_provider();
        let mock = Mock::new();
        ok(&mock);
        rows(&mock, migration_names().into_iter().map(str::to_owned));
        rows(&mock, [create_sql(&ttl(RETENTION), None)]);
        valid_cluster_probes(&mock, 2, RETENTION);

        ClickHouseUsageCollectorPlugin
            .init(&ctx(
                mock.url(),
                &cluster(true),
                hub(Some(RegistryStub { reject: false })),
                CancellationToken::new(),
            ))
            .await
            .expect("init succeeds on a managed cluster");
    }

    #[tokio::test]
    async fn init_fails_when_the_retention_ttl_cannot_be_reconciled() {
        install_test_crypto_provider();
        let mock = Mock::new();
        ok(&mock);
        rows(&mock, migration_names().into_iter().map(str::to_owned));
        fail(&mock);

        let err = ClickHouseUsageCollectorPlugin
            .init(&ctx(
                mock.url(),
                &json!({}),
                hub(Some(RegistryStub { reject: false })),
                CancellationToken::new(),
            ))
            .await
            .expect_err("an unreconciled TTL fails init");

        assert!(format!("{err:#}").contains("create_table_query"), "{err:#}");
    }

    #[tokio::test]
    async fn init_fails_on_an_unusable_cluster_schema() {
        install_test_crypto_provider();
        let mock = Mock::new();
        rows(&mock, ["db".to_owned()]);
        rows(&mock, [0_u64]);

        let err = ClickHouseUsageCollectorPlugin
            .init(&ctx(
                mock.url(),
                &cluster(false),
                hub(Some(RegistryStub { reject: false })),
                CancellationToken::new(),
            ))
            .await
            .expect_err("an undeclared cluster fails init");

        assert!(
            format!("{err:#}").contains("is not declared in system.clusters"),
            "{err:#}"
        );
    }

    /// `types_registry` is the gear's dependency; without it in the hub the
    /// handshake cannot start.
    #[tokio::test]
    async fn init_fails_without_a_types_registry_client() {
        install_test_crypto_provider();
        let mock = Mock::new();
        up_to_date_single_node(&mock);
        let hub = hub(None);

        ClickHouseUsageCollectorPlugin
            .init(&ctx(
                mock.url(),
                &json!({}),
                Arc::clone(&hub),
                CancellationToken::new(),
            ))
            .await
            .expect_err("no registry, no registration");

        assert!(
            hub.try_get_scoped::<dyn UsageCollectorPluginV1>(&adapter_scope())
                .is_none(),
            "an unregistered plugin must not expose its adapter"
        );
    }

    #[tokio::test]
    async fn init_fails_when_the_registry_rejects_the_instance() {
        install_test_crypto_provider();
        let mock = Mock::new();
        up_to_date_single_node(&mock);
        let hub = hub(Some(RegistryStub { reject: true }));

        ClickHouseUsageCollectorPlugin
            .init(&ctx(
                mock.url(),
                &json!({}),
                Arc::clone(&hub),
                CancellationToken::new(),
            ))
            .await
            .expect_err("a rejected registration fails init");

        assert!(
            hub.try_get_scoped::<dyn UsageCollectorPluginV1>(&adapter_scope())
                .is_none(),
            "a rejected plugin must not expose its adapter"
        );
    }

    /// A shutdown while a startup statement hangs aborts `init` promptly
    /// rather than waiting out the statement's client deadline.
    #[tokio::test]
    async fn init_aborts_when_cancelled_during_startup_io() {
        install_test_crypto_provider();
        // Accepts connections and never answers.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a local socket");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let mut accepted = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                accepted.push(stream);
            }
        });
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            trigger.cancel();
        });

        let err = tokio::time::timeout(
            Duration::from_secs(10),
            ClickHouseUsageCollectorPlugin.init(&ctx(
                &format!("http://{addr}"),
                &json!({}),
                hub(Some(RegistryStub { reject: false })),
                cancel,
            )),
        )
        .await
        .expect("cancellation ends init well before the client deadline")
        .expect_err("a cancelled init does not succeed");

        assert!(
            err.to_string().contains("init cancelled during shutdown"),
            "{err:#}"
        );
    }
}
