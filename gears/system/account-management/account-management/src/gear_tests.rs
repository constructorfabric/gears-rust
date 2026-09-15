//! Gear-level lifecycle tests. These are deliberately narrow —
//! the full DB wiring is exercised via integration tests; here we
//! verify the cooperative cancellation contract.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "test helpers"
)]

use std::sync::Arc;
use toolkit_gts::gts_id;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use account_management_sdk::IdpPluginClient;

use crate::config::{AccountManagementConfig, ReaperConfig, RetentionConfig};
use crate::domain::bootstrap::BootstrapConfig;
use crate::domain::root_type::RootTypeConfig;
use crate::domain::tenant::model::TenantStatus;
use crate::domain::tenant::resource_checker::InertResourceOwnershipChecker;
use crate::domain::tenant::service::TenantService;
use crate::domain::tenant::test_support::{
    FakeIdpProvisioner, FakeOutcome, FakeTenantRepo, mock_enforcer,
};

use crate::domain::bootstrap::BootstrapService;
use crate::domain::tenant::TenantRepo;
use crate::gear::validate_existing_root_binding;

const TENANT_SCHEMA: &str = gts_id!("cf.core.am.tenant.v1~");
const TENANT_TYPE_SCHEMA: &str = gts_id!("cf.core.am.tenant_type.v1~");

/// Test-only helper that combines bootstrap config validation + saga
/// execution in one call, mirroring the split init/serve production
/// flow. Generic over `R: TenantRepo` so tests can pass
/// `Arc<FakeTenantRepo>` directly.
#[allow(
    clippy::cognitive_complexity,
    reason = "flat dispatch over the FEATURE-pinned strict/non-strict matrix"
)]
async fn run_bootstrap_phase<R: TenantRepo + 'static>(
    bootstrap: Option<BootstrapConfig>,
    idp_required: bool,
    repo: Arc<R>,
    idp: Arc<dyn IdpPluginClient>,
    types_registry: Arc<dyn types_registry_sdk::TypesRegistryClient>,
) -> anyhow::Result<()> {
    let Some(boot_cfg) = bootstrap else {
        return Ok(());
    };
    if let Err(err) = boot_cfg.validate() {
        if boot_cfg.strict {
            return Err(anyhow::anyhow!(
                "bootstrap configuration invalid (strict mode): {err}"
            ));
        }
        tracing::warn!(
            error = %err,
            "bootstrap configuration invalid (non-strict); skipping bootstrap"
        );
        return Ok(());
    }
    let strict = boot_cfg.strict;
    let mut bootstrap_svc = BootstrapService::new(repo, idp, boot_cfg, root_type_cfg());
    bootstrap_svc = bootstrap_svc
        .with_types_registry(types_registry)
        .with_idp_required(idp_required);
    match bootstrap_svc.run().await {
        Ok(root) => {
            tracing::info!(root_id = %root.id, "platform bootstrap saga completed");
            Ok(())
        }
        Err(err) if strict => Err(anyhow::anyhow!(
            "platform bootstrap saga failed (strict mode): {err}"
        )),
        Err(err) => {
            tracing::warn!(
                error = %err,
                "platform bootstrap saga failed (non-strict); proceeding without root"
            );
            Ok(())
        }
    }
}

#[tokio::test(start_paused = true)]
async fn stateful_task_shuts_down_on_cancel() {
    // Run the equivalent of `serve` (retention + reaper as two
    // independent `tokio::spawn` tasks under child tokens) and
    // prove that cancelling the root token shuts down both
    // children promptly.
    //
    // `start_paused = true` switches `tokio::time` to a virtual
    // clock: `interval.tick()`, `sleep(...)`, and `timeout(...)`
    // below all advance against virtual time. The runtime
    // auto-advances when no task is runnable, so the "wait a
    // couple of ticks" step is deterministic and does not consume
    // real wall-clock time. Without the pause, the previous shape
    // burned ~80ms of real time per test run and made the test
    // sensitive to a slow CI runner under load.
    let root = uuid::Uuid::from_u128(0x100);
    let repo = Arc::new(FakeTenantRepo::with_root(root));
    let svc = Arc::new(TenantService::new(
        repo,
        Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok)),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        mock_enforcer(),
        AccountManagementConfig {
            retention: RetentionConfig {
                tick_secs: 1,
                ..RetentionConfig::default()
            },
            reaper: ReaperConfig {
                tick_secs: 1,
                ..ReaperConfig::default()
            },
            ..AccountManagementConfig::default()
        },
    ));

    let cancel = CancellationToken::new();
    let retention_cancel = cancel.child_token();
    let reaper_cancel = cancel.child_token();
    let retention_svc = svc.clone();
    let reaper_svc = svc;

    let retention_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(20));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            // `biased;` ensures cancellation is checked before
            // `interval.tick()` when both are ready. Without it,
            // tokio's random branch selection can let the tick win
            // after a cancel signal is already pending, firing one
            // extra `hard_delete_batch` after shutdown.
            tokio::select! {
                biased;
                () = retention_cancel.cancelled() => break,
                _tick = interval.tick() => {
                    let _ = retention_svc.hard_delete_batch(8).await;
                }
            }
        }
    });
    let reaper_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(20));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                () = reaper_cancel.cancelled() => break,
                _tick = interval.tick() => {
                    let _ = reaper_svc
                        .reap_stuck_provisioning(std::time::Duration::from_secs(1))
                        .await;
                }
            }
        }
    });

    // Let the children run a couple of ticks.
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    cancel.cancel();
    // Both child tasks must exit within the timeout window AND
    // return `Ok(())` from their `JoinHandle`. A `tokio::time::timeout`
    // alone only proves they finished; if either task had panicked,
    // the join would still resolve (with `Err(JoinError)`), and an
    // `is_ok()` check on the outer timeout result would silently
    // pass over the panic.
    let join = tokio::time::timeout(std::time::Duration::from_millis(200), async move {
        tokio::join!(retention_handle, reaper_handle)
    })
    .await
    .expect("retention + reaper tasks must both exit within 200ms of cancel");
    let (retention_res, reaper_res) = join;
    retention_res.expect("retention task must exit without panic on cooperative cancel");
    reaper_res.expect("reaper task must exit without panic on cooperative cancel");
}

// ---------------------------------------------------------------------
// run_bootstrap_phase strict-mode matrix
//
// Pins the four lifecycle outcomes documented on `run_bootstrap_phase`:
//
//   bootstrap = None                                  → Ok (skip)
//   bootstrap = Some(invalid, strict=true)            → Err (init-fatal)
//   bootstrap = Some(invalid, strict=false)           → Ok (logged)
//   bootstrap = Some(valid)  + run() Err + strict=true  → Err (init-fatal)
//   bootstrap = Some(valid)  + run() Err + strict=false → Ok (logged)
//   bootstrap = Some(valid)  + run() Ok                 → Ok (success)
//
// Saga `run()` Ok / Err are driven through the existing `FakeOutcome`
// + `FakeTenantRepo` infra; no `BootstrapService` mocking required.
// ---------------------------------------------------------------------

const ROOT_TENANT_TYPE: &str = gts_id!("cf.core.am.tenant_type.v1~cf.core.am.platform.v1~");

fn root_type_cfg() -> RootTypeConfig {
    RootTypeConfig {
        gts_id: gts::GtsTypeId::new(ROOT_TENANT_TYPE),
        idp_provisioning: false,
    }
}

fn root_id() -> Uuid {
    Uuid::from_u128(0x100)
}

#[test]
fn existing_root_type_binding_must_match_configured_gts_id() {
    let cfg = root_type_cfg();
    let expected = gts::GtsId::try_new(ROOT_TENANT_TYPE)
        .expect("valid root type")
        .to_uuid();
    validate_existing_root_binding(root_id(), expected, None, &cfg)
        .expect("matching durable binding");

    let error = validate_existing_root_binding(root_id(), Uuid::nil(), None, &cfg)
        .expect_err("type drift must be lifecycle-fatal");
    assert!(error.to_string().contains("explicit root/schema migration"));
}

#[test]
fn existing_root_id_must_match_bootstrap_config() {
    let cfg = root_type_cfg();
    let expected = gts::GtsId::try_new(ROOT_TENANT_TYPE)
        .expect("valid root type")
        .to_uuid();
    let bootstrap = valid_bootstrap_cfg(false);

    let error = validate_existing_root_binding(Uuid::nil(), expected, Some(&bootstrap), &cfg)
        .expect_err("root id drift must be lifecycle-fatal");
    assert!(error.to_string().contains("explicit root migration"));
}

fn valid_bootstrap_cfg(strict: bool) -> BootstrapConfig {
    BootstrapConfig {
        root_id: root_id(),
        root_name: "platform-root".into(),
        root_tenant_type: Some(gts::GtsTypeId::new(ROOT_TENANT_TYPE)),
        root_tenant_type_idp_provisioning: Some(false),
        root_tenant_metadata: None,
        idp_wait_timeout: std::time::Duration::from_secs(1),
        idp_retry_backoff_initial: std::time::Duration::from_secs(1),
        idp_retry_backoff_max: std::time::Duration::from_secs(1),
        strict,
    }
}

fn invalid_bootstrap_cfg(strict: bool) -> BootstrapConfig {
    // Default carries `Uuid::nil()` which fails `validate()`. We then
    // flip `strict` to drive the validate-failure branch.
    BootstrapConfig {
        strict,
        ..BootstrapConfig::default()
    }
}

#[tokio::test]
async fn run_bootstrap_phase_none_skips_saga() {
    // The `None` slot is the "deployment bootstraps out of band"
    // contract (multi-region splits, CI smoke tests, unit-test
    // harnesses). The phase MUST NOT touch the IdP.
    let repo = Arc::new(FakeTenantRepo::new());
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::CleanFailure));
    let registry = stub_types_registry();
    run_bootstrap_phase(
        None,
        false,
        repo,
        idp.clone() as Arc<dyn IdpPluginClient>,
        registry,
    )
    .await
    .expect("bootstrap=None must succeed silently");
    assert_eq!(
        idp.provision_call_count(),
        0,
        "bootstrap=None must not invoke the IdP plugin"
    );
}

#[tokio::test]
async fn run_bootstrap_phase_strict_invalid_config_returns_error() {
    let repo = Arc::new(FakeTenantRepo::new());
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    let registry = stub_types_registry();
    let err = run_bootstrap_phase(
        Some(invalid_bootstrap_cfg(true)),
        false,
        repo,
        idp.clone() as Arc<dyn IdpPluginClient>,
        registry,
    )
    .await
    .expect_err("strict + invalid config must surface as init-fatal");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("invalid (strict mode)"),
        "expected 'invalid (strict mode)' in error message, got: {msg}"
    );
    assert_eq!(
        idp.provision_call_count(),
        0,
        "strict-invalid path must not reach the IdP plugin"
    );
}

#[tokio::test]
async fn run_bootstrap_phase_nonstrict_invalid_config_proceeds() {
    let repo = Arc::new(FakeTenantRepo::new());
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    let registry = stub_types_registry();
    run_bootstrap_phase(
        Some(invalid_bootstrap_cfg(false)),
        false,
        repo,
        idp.clone() as Arc<dyn IdpPluginClient>,
        registry,
    )
    .await
    .expect("non-strict + invalid config must proceed (logged + skipped)");
    assert_eq!(
        idp.provision_call_count(),
        0,
        "non-strict-invalid path must not reach the IdP plugin"
    );
}

#[tokio::test]
async fn run_bootstrap_phase_valid_with_active_root_succeeds() {
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root_at_status(&repo, TenantStatus::Active);
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));
    let registry = stub_types_registry();
    run_bootstrap_phase(
        Some(valid_bootstrap_cfg(true)),
        false,
        repo,
        idp.clone() as Arc<dyn IdpPluginClient>,
        registry,
    )
    .await
    .expect("active root + valid cfg must skip idempotently");
    assert_eq!(
        idp.provision_call_count(),
        0,
        "idempotent skip must not call provision_tenant"
    );
}

#[tokio::test(start_paused = true)]
async fn run_bootstrap_phase_strict_run_failure_returns_error() {
    // saga `run()` returns Err → strict=true → init-fatal.
    // CleanFailure perpetually + 1s deadline = IdpUnavailable surfaces
    // after the first retry sleep advances virtual time past deadline.
    let repo = Arc::new(FakeTenantRepo::new());
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::CleanFailure));
    let registry = stub_types_registry();
    let err = run_bootstrap_phase(
        Some(valid_bootstrap_cfg(true)),
        false,
        repo,
        idp.clone() as Arc<dyn IdpPluginClient>,
        registry,
    )
    .await
    .expect_err("strict + saga failure must propagate as init-fatal");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("failed (strict mode)"),
        "expected 'failed (strict mode)' in error message, got: {msg}"
    );
}

#[tokio::test(start_paused = true)]
async fn run_bootstrap_phase_nonstrict_run_failure_proceeds() {
    // saga `run()` returns Err → strict=false → logged + Ok.
    let repo = Arc::new(FakeTenantRepo::new());
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::CleanFailure));
    let registry = stub_types_registry();
    run_bootstrap_phase(
        Some(valid_bootstrap_cfg(false)),
        false,
        repo,
        idp.clone() as Arc<dyn IdpPluginClient>,
        registry,
    )
    .await
    .expect("non-strict + saga failure must proceed (logged)");
}

fn seed_root_at_status(repo: &FakeTenantRepo, status: TenantStatus) {
    use crate::domain::tenant::model::TenantModel;
    use time::OffsetDateTime;
    let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch");
    repo.insert_tenant_raw(TenantModel {
        id: root_id(),
        parent_id: None,
        name: "platform-root".into(),
        status,
        self_managed: false,
        tenant_type_uuid: gts::GtsId::try_new(ROOT_TENANT_TYPE)
            .expect("valid root type")
            .to_uuid(),
        depth: 0,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    });
}

/// Minimal `TypesRegistryClient` that returns a canned root-level AM
/// `tenant_type` schema from `get_type_schema`. Mirrors the stub in
/// `domain/bootstrap/service_tests.rs::StubTypesRegistry`. Inlined
/// here so gear-level tests do not depend on test-mod visibility
/// from a sibling gear.
fn stub_types_registry() -> Arc<dyn types_registry_sdk::TypesRegistryClient> {
    use async_trait::async_trait;
    use std::collections::HashMap;
    use toolkit_canonical_errors::CanonicalError;
    use types_registry_sdk::{
        GtsInstance, GtsTypeId, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery,
        TypesRegistryClient,
    };

    struct Stub;

    #[async_trait]
    impl TypesRegistryClient for Stub {
        async fn register(
            &self,
            _entities: Vec<serde_json::Value>,
        ) -> Result<Vec<RegisterResult>, CanonicalError> {
            unreachable!()
        }
        async fn register_type_schemas(
            &self,
            _type_schemas: Vec<serde_json::Value>,
        ) -> Result<Vec<RegisterResult>, CanonicalError> {
            unreachable!()
        }
        async fn get_type_schema(&self, type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
            // Route per-id so the bootstrap-time GTS validation seam
            // does not short-circuit before the IdP retry-loop branch
            // these tests are designed to exercise. The AM tenant
            // schema MUST advertise a `name` property; without it,
            // `validate_tenant_name_via_gts` rejects `root_name` and
            // the "valid config + CleanFailure" path never reaches
            // `FakeOutcome::CleanFailure`.
            let (id, body) = if type_id == TENANT_SCHEMA {
                (
                    TENANT_SCHEMA,
                    serde_json::json!({
                        "type": "object",
                        "properties": {
                            "name": { "type": "string", "minLength": 1, "maxLength": 255 }
                        }
                    }),
                )
            } else {
                (TENANT_TYPE_SCHEMA, serde_json::json!({}))
            };
            Ok(GtsTypeSchema::try_new(GtsTypeId::new(id), body, None, None)
                .expect("canned root schema must construct"))
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

    Arc::new(Stub)
}

mod no_seed_startup_tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use authz_resolver_sdk::{
        AuthZResolverApi,
        models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
    };
    use sea_orm_migration::{MigrationTrait, MigratorTrait};
    use serde_json::{Value, json};
    use toolkit::{ClientHub, ConfigProvider, Gear, GearCtx};
    use toolkit_canonical_errors::CanonicalError;
    use toolkit_db::migration_runner::run_migrations_for_testing;
    use toolkit_db::{ConnectOpts, DBProvider, Db, connect_db};
    use toolkit_security::PlatformSecurityContext;
    use types_registry_sdk::{TypesRegistryClient, TypesRegistryEntities};

    use crate::gear::AccountManagementGear;
    use crate::infra::types_registry::root_type::validate_persisted_root;

    use super::{ROOT_TENANT_TYPE, root_type_cfg};

    struct StartupConfig {
        account_management: Value,
    }

    impl ConfigProvider for StartupConfig {
        fn get_gear_config(&self, gear_name: &str) -> Option<&Value> {
            (gear_name == "account-management").then_some(&self.account_management)
        }
    }

    struct PermitAllAuthz;

    #[async_trait]
    impl AuthZResolverApi for PermitAllAuthz {
        async fn evaluate(
            &self,
            _ctx: PlatformSecurityContext,
            _request: EvaluationRequest,
        ) -> Result<EvaluationResponse, CanonicalError> {
            Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext::default(),
            })
        }
    }

    async fn migrated_sqlite(migrations: Vec<Box<dyn MigrationTrait>>) -> Db {
        let db = connect_db(
            "sqlite::memory:",
            ConnectOpts {
                max_conns: Some(1),
                min_conns: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("connect in-memory SQLite");
        run_migrations_for_testing(&db, migrations)
            .await
            .expect("apply gear migrations");
        db
    }

    fn context(
        gear_name: &'static str,
        config: Arc<dyn ConfigProvider>,
        hub: Arc<ClientHub>,
        db: Db,
    ) -> GearCtx {
        GearCtx::new(
            gear_name,
            uuid::Uuid::new_v4(),
            config,
            hub,
            tokio_util::sync::CancellationToken::new(),
        )
        .with_db(DBProvider::new(db))
    }

    #[tokio::test]
    async fn full_init_reconciles_and_reads_root_without_a_legacy_seed() {
        let config: Arc<dyn ConfigProvider> = Arc::new(StartupConfig {
            account_management: json!({
                "config": {
                    "root_tenant_type": {
                        "gts_id": ROOT_TENANT_TYPE,
                        "idp_provisioning": false
                    },
                    "bootstrap": {
                        "root_id": "00000000-df51-5b42-9538-d2b56b7ee953",
                        "root_name": "platform-root",
                        "strict": true
                    }
                }
            }),
        });
        let hub = Arc::new(ClientHub::new());
        let authz: Arc<dyn AuthZResolverApi> = Arc::new(PermitAllAuthz);
        hub.register::<dyn AuthZResolverApi>(authz);

        // Initialize the actual Types Registry gear with its default, seedless
        // configuration. This publishes both the legacy process-local client
        // and the authoritative persistent client in production order.
        let types_registry_gear = types_registry::gear::TypesRegistryGear::default();
        let types_registry_ctx = context(
            "types-registry",
            Arc::clone(&config),
            Arc::clone(&hub),
            migrated_sqlite(types_registry::infra::storage::Migrator::migrations()).await,
        );
        types_registry_gear
            .init(&types_registry_ctx)
            .await
            .expect("Types Registry init");

        let legacy = hub
            .get::<dyn TypesRegistryClient>()
            .expect("legacy Types Registry client");
        let persistent = hub
            .get::<dyn TypesRegistryEntities>()
            .expect("persistent Types Registry client");
        assert!(
            legacy.get_type_schema(ROOT_TENANT_TYPE).await.is_err(),
            "the test must not accidentally provide a legacy root seed"
        );
        assert!(
            persistent
                .get_entity(ROOT_TENANT_TYPE)
                .await
                .expect("initial persistent read")
                .is_none(),
            "the test must begin without a persistent root"
        );

        // Initialize the actual Resource Group gear so AM resolves its normal
        // production dependencies and performs user-group type registration.
        let resource_group_gear = resource_group::gear::ResourceGroup::default();
        let resource_group_ctx = context(
            "resource-group",
            Arc::clone(&config),
            Arc::clone(&hub),
            migrated_sqlite(resource_group::infra::storage::migrations::Migrator::migrations())
                .await,
        );
        resource_group_gear
            .init(&resource_group_ctx)
            .await
            .expect("Resource Group init");

        let account_management_gear = AccountManagementGear::default();
        let account_management_ctx = context(
            "account-management",
            config,
            Arc::clone(&hub),
            migrated_sqlite(crate::Migrator::migrations()).await,
        );
        let loaded: crate::config::AccountManagementConfig = account_management_ctx
            .config_or_default()
            .expect("load AM startup config");
        assert_eq!(
            loaded
                .root_tenant_type
                .as_ref()
                .map(|cfg| cfg.gts_id.as_ref()),
            Some(ROOT_TENANT_TYPE)
        );
        account_management_gear
            .init(&account_management_ctx)
            .await
            .expect("AM init without legacy root seed");

        let stored = persistent
            .get_entity(ROOT_TENANT_TYPE)
            .await
            .expect("authoritative root read")
            .expect("AM persisted root");
        validate_persisted_root(&stored, &root_type_cfg()).expect("persisted AM root contract");

        // Bootstrap receives the root-aware overlay, proving the ordinary AM
        // runtime read succeeds even though the globally registered legacy
        // client still has no platform-root entry.
        let root_aware = account_management_gear
            .bootstrap_params
            .lock()
            .as_ref()
            .expect("validated bootstrap parameters")
            .types_registry
            .clone();
        let runtime_root = root_aware
            .get_type_schema(ROOT_TENANT_TYPE)
            .await
            .expect("AM authoritative root read");
        assert_eq!(runtime_root.type_id.as_ref(), ROOT_TENANT_TYPE);
        assert!(legacy.get_type_schema(ROOT_TENANT_TYPE).await.is_err());
    }
}
