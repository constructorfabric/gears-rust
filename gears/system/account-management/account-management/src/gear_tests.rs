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
use crate::gear::handle_bootstrap_failure;

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
    types_registry: Arc<dyn types_registry_sdk::PlatformTypesRegistryApi>,
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
        Err(err) => handle_bootstrap_failure(err, strict),
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
    .expect("non-strict + ordinary saga failure must proceed (logged)");
}

#[tokio::test]
async fn run_bootstrap_phase_nonstrict_root_id_drift_is_fatal() {
    let repo = Arc::new(FakeTenantRepo::new());
    let expected_type = gts::GtsId::try_new(ROOT_TENANT_TYPE)
        .expect("valid root type")
        .to_uuid();
    seed_root_with_binding(
        &repo,
        Uuid::from_u128(0xDEAD),
        expected_type,
        TenantStatus::Active,
    );
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));

    let error = run_bootstrap_phase(
        Some(valid_bootstrap_cfg(false)),
        false,
        repo,
        idp.clone() as Arc<dyn IdpPluginClient>,
        stub_types_registry(),
    )
    .await
    .expect_err("root id drift must bypass bootstrap.strict=false");

    assert!(error.to_string().contains("root binding mismatch"));
    assert_eq!(idp.provision_call_count(), 0);
}

#[tokio::test]
async fn run_bootstrap_phase_nonstrict_root_type_drift_is_fatal() {
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root_with_binding(&repo, root_id(), Uuid::nil(), TenantStatus::Active);
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));

    let error = run_bootstrap_phase(
        Some(valid_bootstrap_cfg(false)),
        false,
        repo,
        idp.clone() as Arc<dyn IdpPluginClient>,
        stub_types_registry(),
    )
    .await
    .expect_err("root type drift must bypass bootstrap.strict=false");

    assert!(error.to_string().contains("root binding mismatch"));
    assert_eq!(idp.provision_call_count(), 0);
}

#[tokio::test]
async fn run_bootstrap_phase_nonstrict_suspended_root_is_fatal() {
    let repo = Arc::new(FakeTenantRepo::new());
    seed_root_at_status(&repo, TenantStatus::Suspended);
    let idp = Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok));

    let error = run_bootstrap_phase(
        Some(valid_bootstrap_cfg(false)),
        false,
        repo,
        idp.clone() as Arc<dyn IdpPluginClient>,
        stub_types_registry(),
    )
    .await
    .expect_err("invalid root lifecycle must bypass bootstrap.strict=false");

    assert!(error.to_string().contains("root binding mismatch"));
    assert_eq!(idp.provision_call_count(), 0);
}

fn seed_root_at_status(repo: &FakeTenantRepo, status: TenantStatus) {
    let tenant_type_uuid = gts::GtsId::try_new(ROOT_TENANT_TYPE)
        .expect("valid root type")
        .to_uuid();
    seed_root_with_binding(repo, root_id(), tenant_type_uuid, status);
}

fn seed_root_with_binding(
    repo: &FakeTenantRepo,
    id: Uuid,
    tenant_type_uuid: Uuid,
    status: TenantStatus,
) {
    use crate::domain::tenant::model::TenantModel;
    use time::OffsetDateTime;
    let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch");
    repo.insert_tenant_raw(TenantModel {
        id,
        parent_id: None,
        name: "platform-root".into(),
        status,
        self_managed: false,
        tenant_type_uuid,
        depth: 0,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    });
}

/// The registry bootstrap preflight reads; see
/// [`bootstrap_registry`](crate::infra::types_registry::test_helpers::bootstrap_registry).
fn stub_types_registry() -> Arc<dyn types_registry_sdk::PlatformTypesRegistryApi> {
    crate::infra::types_registry::test_helpers::bootstrap_registry()
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
    use types_registry_sdk::{
        EntityField, FieldSelection, GtsTypeId, PlatformTypesRegistryApi,
        PlatformTypesRegistryApiExt, Projection, ReadOptions,
    };

    use crate::gear::AccountManagementGear;

    use super::ROOT_TENANT_TYPE;

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

    /// AM init with only one of the two registry APIs in the hub.
    async fn init_with_only(register: impl FnOnce(&ClientHub)) -> anyhow::Error {
        let config: Arc<dyn ConfigProvider> = Arc::new(StartupConfig {
            account_management: json!({ "config": {} }),
        });
        let hub = Arc::new(ClientHub::new());
        let authz: Arc<dyn AuthZResolverApi> = Arc::new(PermitAllAuthz);
        hub.register::<dyn AuthZResolverApi>(authz);
        register(&hub);
        let gear = AccountManagementGear::default();
        let ctx = context(
            "account-management",
            config,
            hub,
            migrated_sqlite(crate::Migrator::migrations()).await,
        );
        gear.init(&ctx)
            .await
            .expect_err("AM init must fail without both registry APIs")
    }

    /// Request paths read only through the tenant API: a host that
    /// publishes the platform API alone fails AM init rather than
    /// serving requests from the platform API (P33).
    #[tokio::test]
    async fn init_fails_without_the_tenant_api_even_with_the_platform_api() {
        let err = init_with_only(|hub| {
            let platform: Arc<dyn PlatformTypesRegistryApi> =
                Arc::new(types_registry_sdk::testing_platform::MockTypesRegistry::new());
            hub.register::<dyn PlatformTypesRegistryApi>(platform);
        })
        .await;
        assert!(
            err.to_string().contains("failed to get TypesRegistryApi"),
            "got: {err:#}"
        );
    }

    /// And the platform paths need the platform API: the tenant API alone
    /// does not stand in for it.
    #[tokio::test]
    async fn init_fails_without_the_platform_api_even_with_the_tenant_api() {
        let err = init_with_only(|hub| {
            let tenant: Arc<dyn types_registry_sdk::TypesRegistryApi> =
                Arc::new(types_registry_sdk::testing_platform::MockTypesRegistry::new());
            hub.register::<dyn types_registry_sdk::TypesRegistryApi>(tenant);
        })
        .await;
        assert!(
            err.to_string()
                .contains("failed to get PlatformTypesRegistryApi"),
            "got: {err:#}"
        );
    }

    #[tokio::test]
    async fn full_init_registers_and_validates_root_without_a_yaml_seed() {
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

        // Initialize the actual Types Registry gear on its own database with no
        // configured entities: it seeds the linked inventory and publishes the
        // platform API.
        let types_registry_gear = types_registry::gear::TypesRegistryGear::default();
        let types_registry_ctx = context(
            "types-registry",
            Arc::clone(&config),
            Arc::clone(&hub),
            migrated_sqlite(toolkit::contracts::DatabaseCapability::migrations(
                &types_registry_gear,
            ))
            .await,
        );
        types_registry_gear
            .init(&types_registry_ctx)
            .await
            .expect("Types Registry init");

        let registry = hub
            .get::<dyn PlatformTypesRegistryApi>()
            .expect("Types Registry platform API");
        let platform = PlatformSecurityContext::outbound_marker();
        let root_type = GtsTypeId::new(ROOT_TENANT_TYPE);
        assert!(
            registry
                .get_type_schema(&platform, &root_type, ReadOptions::default())
                .await
                .is_err(),
            "the test must not accidentally provide a YAML root seed"
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
            .expect("AM init without a YAML root seed");

        // AM init waits for the root's admission: it is readable at once,
        // with its materialized traits.
        let runtime_root = registry
            .get_type_schema(
                &platform,
                &root_type,
                ReadOptions {
                    projection: Projection::Select(FieldSelection::with(&[
                        EntityField::EffectiveTraits,
                        EntityField::ResolvedSchema,
                    ])),
                    ..ReadOptions::default()
                },
            )
            .await
            .expect("admitted root type");
        assert_eq!(runtime_root.type_id.as_ref(), ROOT_TENANT_TYPE);
        assert_eq!(
            runtime_root.effective_traits,
            Some(json!({"allowed_parent_types": [], "idp_provisioning": false}))
        );
        assert!(
            runtime_root.resolved_schema.is_some(),
            "the root resolves against the seeded envelope"
        );
    }

    /// AM's init over a real registry and a real AM database holding a
    /// platform root bound to another tenant type.
    async fn init_over_a_mismatched_root(strict: bool) -> (anyhow::Error, Arc<ClientHub>) {
        use sea_orm::ActiveValue;
        use toolkit_db::secure::secure_insert;

        use crate::infra::storage::entity::tenants;

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
                        "strict": strict
                    }
                }
            }),
        });
        let hub = Arc::new(ClientHub::new());
        let authz: Arc<dyn AuthZResolverApi> = Arc::new(PermitAllAuthz);
        hub.register::<dyn AuthZResolverApi>(authz);
        let types_registry_gear = types_registry::gear::TypesRegistryGear::default();
        types_registry_gear
            .init(&context(
                "types-registry",
                Arc::clone(&config),
                Arc::clone(&hub),
                migrated_sqlite(toolkit::contracts::DatabaseCapability::migrations(
                    &types_registry_gear,
                ))
                .await,
            ))
            .await
            .expect("Types Registry init");
        let resource_group_gear = resource_group::gear::ResourceGroup::default();
        resource_group_gear
            .init(&context(
                "resource-group",
                Arc::clone(&config),
                Arc::clone(&hub),
                migrated_sqlite(resource_group::infra::storage::migrations::Migrator::migrations())
                    .await,
            ))
            .await
            .expect("Resource Group init");

        let am_db = migrated_sqlite(crate::Migrator::migrations()).await;
        let provider = crate::infra::storage::repo_impl::AmDbProvider::new(am_db.clone());
        let now = time::OffsetDateTime::now_utc();
        secure_insert::<tenants::Entity>(
            tenants::ActiveModel {
                id: ActiveValue::Set(
                    uuid::Uuid::parse_str("00000000-df51-5b42-9538-d2b56b7ee953").expect("uuid"),
                ),
                parent_id: ActiveValue::Set(None),
                name: ActiveValue::Set("platform-root".to_owned()),
                status: ActiveValue::Set(
                    crate::domain::tenant::model::TenantStatus::Active.as_smallint(),
                ),
                self_managed: ActiveValue::Set(false),
                tenant_type_uuid: ActiveValue::Set(uuid::Uuid::nil()),
                depth: ActiveValue::Set(0),
                created_at: ActiveValue::Set(now),
                updated_at: ActiveValue::Set(now),
                deleted_at: ActiveValue::Set(None),
                retention_window_secs: ActiveValue::Set(None),
                claimed_by: ActiveValue::Set(None),
                claimed_at: ActiveValue::Set(None),
                terminal_failure_at: ActiveValue::Set(None),
            },
            &toolkit_db::secure::AccessScope::allow_all(),
            &provider.conn().expect("conn"),
        )
        .await
        .expect("seed a root bound to another tenant type");

        let error = AccountManagementGear::default()
            .init(&context(
                "account-management",
                config,
                Arc::clone(&hub),
                am_db,
            ))
            .await
            .expect_err("a root binding mismatch fails init");
        (error, hub)
    }

    #[tokio::test]
    async fn a_root_binding_mismatch_fails_before_any_registry_write() {
        for strict in [true, false] {
            let (error, hub) = init_over_a_mismatched_root(strict).await;

            assert!(
                format!("{error:#}").contains("root binding mismatch"),
                "strict={strict}: {error:#}"
            );
            let registry = hub
                .get::<dyn PlatformTypesRegistryApi>()
                .expect("Types Registry platform API");
            let read = registry
                .get_type_schema(
                    &PlatformSecurityContext::outbound_marker(),
                    &GtsTypeId::new(ROOT_TENANT_TYPE),
                    ReadOptions::default(),
                )
                .await;
            assert!(
                matches!(read, Err(CanonicalError::NotFound { .. })),
                "strict={strict}: no root schema was written, got {read:?}"
            );
        }
    }
}

/// Real Types Registry, Resource Group and Account Management gears booted
/// repeatedly over the same SQLite files: what a host restart keeps.
mod restart_tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use authz_resolver_sdk::{
        AuthZResolverApi,
        models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
    };
    use sea_orm::ActiveValue;
    use sea_orm_migration::{MigrationTrait, MigratorTrait};
    use serde_json::{Value, json};
    use tenant_resolver_sdk::TenantId;
    use tokio_util::sync::CancellationToken;
    use toolkit::contracts::{DatabaseCapability, RunnableCapability};
    use toolkit::{ClientHub, ConfigProvider, Gear, GearCtx};
    use toolkit_canonical_errors::CanonicalError;
    use toolkit_db::migration_runner::run_migrations_for_testing;
    use toolkit_db::secure::{AccessScope, secure_insert};
    use toolkit_db::{ConnectOpts, DBProvider, Db, connect_db};
    use toolkit_security::{PlatformSecurityContext, SecurityContext};
    use types_registry_sdk::{
        EntityField, FieldSelection, GtsTypeId, Origin, PlatformTypesRegistryApi,
        PlatformTypesRegistryApiExt, Projection, ReadOptions, TypeSchema,
    };
    use uuid::Uuid;

    use super::ROOT_TENANT_TYPE;
    use crate::gear::AccountManagementGear;
    use crate::infra::storage::entity::tenants;

    const ROOT_ID: &str = "00000000-df51-5b42-9538-d2b56b7ee953";

    struct Configs {
        account_management: Value,
        types_registry: Value,
    }

    impl ConfigProvider for Configs {
        fn get_gear_config(&self, gear_name: &str) -> Option<&Value> {
            match gear_name {
                "account-management" => Some(&self.account_management),
                "types-registry" => Some(&self.types_registry),
                _ => None,
            }
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

    /// One host's data root: a database file per gear.
    struct DataRoot(tempfile::TempDir);

    /// A booted host. Dropping it without [`Booted::shutdown`] leaves the
    /// registry's outbox running, so every test shuts each boot down.
    struct Booted {
        hub: Arc<ClientHub>,
        registry: Arc<toolkit::lifecycle::WithLifecycle<types_registry::gear::TypesRegistryGear>>,
        am_db: Db,
    }

    impl DataRoot {
        fn new() -> Self {
            Self(tempfile::tempdir().expect("temp dir"))
        }

        async fn database(&self, file: &str, migrations: Vec<Box<dyn MigrationTrait>>) -> Db {
            let dsn = format!("sqlite://{}?mode=rwc", self.0.path().join(file).display());
            let db = connect_db(
                &dsn,
                ConnectOpts {
                    max_conns: Some(4),
                    ..Default::default()
                },
            )
            .await
            .expect("connect SQLite file");
            run_migrations_for_testing(&db, migrations)
                .await
                .expect("apply gear migrations");
            db
        }

        /// Boot the three gears' `init()` with `am_config` and `registry_config`.
        async fn boot(&self, am_config: Value, registry_config: Value) -> anyhow::Result<Booted> {
            self.boot_into(Arc::new(ClientHub::new()), am_config, registry_config)
                .await
        }

        /// [`Self::boot`] over `hub`, which a failed boot leaves to inspect.
        async fn boot_into(
            &self,
            hub: Arc<ClientHub>,
            am_config: Value,
            registry_config: Value,
        ) -> anyhow::Result<Booted> {
            let config: Arc<dyn ConfigProvider> = Arc::new(Configs {
                account_management: json!({ "config": am_config }),
                types_registry: json!({ "config": registry_config }),
            });
            let authz: Arc<dyn AuthZResolverApi> = Arc::new(PermitAllAuthz);
            hub.register::<dyn AuthZResolverApi>(authz);

            let registry = Arc::new(types_registry::gear::TypesRegistryGear::default().into_gear());
            let registry_gear = registry.inner_arc();
            let registry_db = self
                .database("types_registry.db", registry_gear.migrations())
                .await;
            registry_gear
                .init(&Self::context("types-registry", &config, &hub, registry_db))
                .await?;

            let rg_db = self
                .database(
                    "resource_group.db",
                    resource_group::infra::storage::migrations::Migrator::migrations(),
                )
                .await;
            resource_group::gear::ResourceGroup::default()
                .init(&Self::context("resource-group", &config, &hub, rg_db))
                .await?;

            let am_db = self
                .database("account_management.db", crate::Migrator::migrations())
                .await;
            let booted = Booted {
                hub: Arc::clone(&hub),
                registry,
                am_db: am_db.clone(),
            };
            let am = AccountManagementGear::default()
                .init(&Self::context("account-management", &config, &hub, am_db))
                .await;
            match am {
                Ok(()) => Ok(booted),
                Err(error) => {
                    booted.shutdown().await;
                    Err(error)
                }
            }
        }

        fn context(
            gear_name: &'static str,
            config: &Arc<dyn ConfigProvider>,
            hub: &Arc<ClientHub>,
            db: Db,
        ) -> GearCtx {
            GearCtx::new(
                gear_name,
                Uuid::new_v4(),
                Arc::clone(config),
                Arc::clone(hub),
                CancellationToken::new(),
            )
            .with_db(DBProvider::new(db))
        }
    }

    impl Booted {
        /// Drain the registry's outbox as the runtime does on shutdown.
        async fn shutdown(self) {
            let token = CancellationToken::new();
            self.registry
                .start(token.clone())
                .await
                .expect("registry start");
            self.registry
                .stop(CancellationToken::new())
                .await
                .expect("registry stop");
        }

        async fn root(&self, type_id: &str) -> Result<TypeSchema, CanonicalError> {
            self.hub
                .get::<dyn PlatformTypesRegistryApi>()
                .expect("platform API")
                .get_type_schema(
                    &PlatformSecurityContext::outbound_marker(),
                    &GtsTypeId::new(type_id),
                    ReadOptions {
                        projection: Projection::Select(FieldSelection::with(&[
                            EntityField::Origin,
                            EntityField::EffectiveTraits,
                        ])),
                        ..ReadOptions::default()
                    },
                )
                .await
        }
    }

    /// The AM tenant-resolver plugin's instance identifier.
    fn tr_instance_id() -> types_registry_sdk::GtsInstanceId {
        tenant_resolver_sdk::TenantResolverPluginSpecV1::gts_make_instance_id(
            "cf.builtin.account_management_tenant_resolver.plugin.v1",
        )
    }

    /// The stored AM tenant-resolver plugin instance's revision and
    /// document, if any.
    async fn tr_instance(booted: &Booted) -> Option<(u64, Value)> {
        let read = booted
            .hub
            .get::<dyn PlatformTypesRegistryApi>()
            .expect("platform API")
            .get_instance(
                &PlatformSecurityContext::outbound_marker(),
                &tr_instance_id(),
                ReadOptions {
                    projection: Projection::Select(FieldSelection::with(&[
                        EntityField::Origin,
                        EntityField::Content,
                    ])),
                    ..ReadOptions::default()
                },
            )
            .await;
        match read {
            Ok(instance) => match instance.origin {
                Some(Origin::Managed {
                    resource_version, ..
                }) => Some((
                    resource_version,
                    instance.content.expect("selected content"),
                )),
                other => panic!("a managed origin, got {other:?}"),
            },
            Err(CanonicalError::NotFound { .. }) => None,
            Err(error) => panic!("instance read failed: {error:?}"),
        }
    }

    /// The stored AM tenant-resolver plugin instance's revision, if any.
    async fn tr_instance_revision(booted: &Booted) -> Option<u64> {
        tr_instance(booted).await.map(|(revision, _)| revision)
    }

    fn resource_version(schema: &TypeSchema) -> u64 {
        match schema.origin {
            Some(Origin::Managed {
                resource_version, ..
            }) => resource_version,
            ref other => panic!("a managed origin, got {other:?}"),
        }
    }

    fn am_config(root_type: &str, idp_provisioning: bool, tr_plugin_enabled: bool) -> Value {
        json!({
            "root_tenant_type": { "gts_id": root_type, "idp_provisioning": idp_provisioning },
            "tr_plugin": { "enabled": tr_plugin_enabled, "priority": 0 },
        })
    }

    /// AM with its tenant-resolver plugin enabled at `priority`.
    fn tr_plugin_config(priority: i16) -> Value {
        json!({
            "root_tenant_type": { "gts_id": ROOT_TENANT_TYPE, "idp_provisioning": false },
            "tr_plugin": { "enabled": true, "priority": priority },
        })
    }

    #[tokio::test]
    async fn an_equal_restart_keeps_the_root_revision_and_an_allowed_change_converges() {
        let data = DataRoot::new();

        let first = data
            .boot(am_config(ROOT_TENANT_TYPE, false, false), json!({}))
            .await
            .expect("first boot");
        let admitted = resource_version(&first.root(ROOT_TENANT_TYPE).await.expect("root"));
        first.shutdown().await;

        let equal = data
            .boot(am_config(ROOT_TENANT_TYPE, false, false), json!({}))
            .await
            .expect("equal restart");
        let root = equal.root(ROOT_TENANT_TYPE).await.expect("root");
        assert_eq!(
            resource_version(&root),
            admitted,
            "an equal restart writes nothing"
        );
        equal.shutdown().await;

        let changed = data
            .boot(am_config(ROOT_TENANT_TYPE, true, false), json!({}))
            .await
            .expect("idp_provisioning may change");
        let root = changed.root(ROOT_TENANT_TYPE).await.expect("root");
        assert!(
            resource_version(&root) > admitted,
            "the change is a new revision"
        );
        assert_eq!(
            root.effective_traits,
            Some(json!({"allowed_parent_types": [], "idp_provisioning": true}))
        );
        changed.shutdown().await;
    }

    #[tokio::test]
    async fn a_custom_root_vendor_needs_its_region_opened() {
        let custom = "gts.cf.core.am.tenant_type.v1~acme.core.am.platform.v1~";

        let closed = DataRoot::new();
        let error = closed
            .boot(am_config(custom, false, false), json!({}))
            .await
            .err()
            .expect("the default policy admits only vendor cf");
        assert!(format!("{error:#}").contains(custom), "{error:#}");

        let open = DataRoot::new();
        let booted = open
            .boot(
                am_config(custom, false, false),
                json!({
                    "registration_policy": {
                        "gts.cf.core.am.tenant_type.v1~acme.*": { "allowed_vendors": ["acme"] }
                    }
                }),
            )
            .await
            .expect("an explicitly opened region admits the custom root");
        booted.root(custom).await.expect("admitted custom root");
        booted.shutdown().await;
    }

    /// The tenant-resolver core over `booted`'s hub, reading a tenant.
    async fn resolve_root(booted: &Booted) -> Result<tenant_resolver_sdk::TenantInfo, String> {
        let service = tenant_resolver::domain::Service::new(
            Arc::clone(&booted.hub),
            "constructorfabric".to_owned(),
        );
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::from_u128(0xCAFE))
            .subject_tenant_id(Uuid::parse_str(ROOT_ID).expect("uuid"))
            .build()
            .expect("ctx");
        service
            .get_tenant(&ctx, TenantId(Uuid::parse_str(ROOT_ID).expect("uuid")))
            .await
            .map_err(|error| format!("{error:?}"))
    }

    async fn seed_root(booted: &Booted) {
        let now = time::OffsetDateTime::now_utc();
        let provider = crate::infra::storage::repo_impl::AmDbProvider::new(booted.am_db.clone());
        secure_insert::<tenants::Entity>(
            tenants::ActiveModel {
                id: ActiveValue::Set(Uuid::parse_str(ROOT_ID).expect("uuid")),
                parent_id: ActiveValue::Set(None),
                name: ActiveValue::Set("platform-root".to_owned()),
                status: ActiveValue::Set(
                    crate::domain::tenant::model::TenantStatus::Active.as_smallint(),
                ),
                self_managed: ActiveValue::Set(false),
                tenant_type_uuid: ActiveValue::Set(
                    gts::GtsId::try_new(ROOT_TENANT_TYPE)
                        .expect("valid id")
                        .to_uuid(),
                ),
                depth: ActiveValue::Set(0),
                created_at: ActiveValue::Set(now),
                updated_at: ActiveValue::Set(now),
                deleted_at: ActiveValue::Set(None),
                retention_window_secs: ActiveValue::Set(None),
                claimed_by: ActiveValue::Set(None),
                claimed_at: ActiveValue::Set(None),
                terminal_failure_at: ActiveValue::Set(None),
            },
            &AccessScope::allow_all(),
            &provider.conn().expect("conn"),
        )
        .await
        .expect("seed the platform root");
    }

    #[tokio::test]
    async fn the_am_tenant_resolver_plugin_serves_when_enabled_and_is_skipped_when_disabled() {
        let data = DataRoot::new();

        let enabled = data
            .boot(am_config(ROOT_TENANT_TYPE, false, true), json!({}))
            .await
            .expect("enabled boot");
        seed_root(&enabled).await;
        let info = resolve_root(&enabled)
            .await
            .expect("AM serves the tenant query");
        assert_eq!(info.tenant_type.as_deref(), Some(ROOT_TENANT_TYPE));
        let published = tr_instance_revision(&enabled)
            .await
            .expect("the enabled plugin published its instance");
        enabled.shutdown().await;

        // The instance published by the first boot is still stored; the
        // disabled plugin registers no backend, so nothing selects it.
        let disabled = data
            .boot(am_config(ROOT_TENANT_TYPE, false, false), json!({}))
            .await
            .expect("disabled boot");
        let error = resolve_root(&disabled)
            .await
            .expect_err("no running plugin");
        assert!(error.contains("PluginNotFound"), "{error}");
        assert_eq!(
            tr_instance_revision(&disabled).await,
            Some(published),
            "disabling neither deletes nor rewrites the stored instance"
        );
        disabled.shutdown().await;

        let again = data
            .boot(am_config(ROOT_TENANT_TYPE, false, true), json!({}))
            .await
            .expect("re-enabled boot");
        let info = resolve_root(&again)
            .await
            .expect("re-enabled AM serves again");
        assert_eq!(info.id.0, Uuid::parse_str(ROOT_ID).expect("uuid"));
        assert_eq!(
            tr_instance_revision(&again).await,
            Some(published),
            "re-enabling with the same instance writes nothing"
        );
        again.shutdown().await;
    }

    /// A restart under a changed plugin spec revises the stored instance:
    /// the revision advances and the stored document carries the new
    /// priority, where the legacy registration refused a different spec.
    #[tokio::test]
    async fn a_restart_with_a_changed_priority_revises_the_instance() {
        let data = DataRoot::new();

        let first = data
            .boot(tr_plugin_config(0), json!({}))
            .await
            .expect("first boot");
        let (published, content) = tr_instance(&first).await.expect("published");
        assert_eq!(content["priority"], json!(0));
        first.shutdown().await;

        let changed = data
            .boot(tr_plugin_config(5), json!({}))
            .await
            .expect("a changed priority is a revision");
        let (revised, content) = tr_instance(&changed).await.expect("stored");
        assert!(revised > published, "{revised} after {published}");
        assert_eq!(content["priority"], json!(5));
        changed.shutdown().await;
    }

    /// A refused publication fails AM's `init` naming the instance, and no
    /// tenant-resolver plugin backend is bound for it.
    #[tokio::test]
    async fn a_refused_publication_fails_boot_and_binds_no_backend() {
        let data = DataRoot::new();
        let first = data
            .boot(tr_plugin_config(0), json!({}))
            .await
            .expect("first boot");
        let (revision, _) = tr_instance(&first).await.expect("published");
        delete_tr_instance(&first, revision).await;
        first.shutdown().await;

        // A revision of a deleted entity is refused (`EntityDeleted`).
        let hub = Arc::new(ClientHub::new());
        let error = data
            .boot_into(Arc::clone(&hub), tr_plugin_config(5), json!({}))
            .await
            .err()
            .expect("a refused publication fails init");
        let message = format!("{error:#}");
        assert!(message.contains(tr_instance_id().as_ref()), "{message}");
        assert!(message.contains("entity_deleted"), "{message}");
        assert!(
            hub.try_get_scoped::<dyn tenant_resolver_sdk::TenantResolverPluginClient>(
                &toolkit::client_hub::ClientScope::gts_id(tr_instance_id().as_ref()),
            )
            .is_none(),
            "no backend is bound for a refused instance"
        );
    }

    /// Deletes the stored AM tenant-resolver plugin instance at `revision`
    /// and waits for the deletion to complete.
    async fn delete_tr_instance(booted: &Booted, revision: u64) {
        let registry = booted
            .hub
            .get::<dyn PlatformTypesRegistryApi>()
            .expect("platform API");
        let ctx = PlatformSecurityContext::outbound_marker();
        let accepted = registry
            .delete_entities(
                &ctx,
                types_registry_sdk::IdempotencyKey::new("delete-am-tr-instance").expect("key"),
                types_registry_sdk::DeleteEntitiesRequest {
                    items: vec![types_registry_sdk::DeleteItem {
                        key: types_registry_sdk::EntityKey::from(
                            types_registry_sdk::GtsId::try_new(tr_instance_id().as_ref())
                                .expect("valid id"),
                        ),
                        expected_resource_version: revision,
                    }],
                    dry_run: false,
                    publisher: types_registry_sdk::PublisherContext {
                        name: "test".to_owned(),
                        version: "0.0.0".parse().expect("version"),
                    },
                },
            )
            .await
            .expect("deletion accepted");
        for _ in 0..200 {
            let operation = registry
                .get_operation(&ctx, accepted.operation_id)
                .await
                .expect("operation");
            if let types_registry_sdk::Operation::Deletion(done) = &operation
                && done.status == types_registry_sdk::OperationStatus::Completed
            {
                assert!(
                    matches!(
                        done.items[0].outcome,
                        types_registry_sdk::DeletionOutcome::Succeeded { .. }
                    ),
                    "{:?}",
                    done.items[0].outcome
                );
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("the deletion did not complete");
    }
}
