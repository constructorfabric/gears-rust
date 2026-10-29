use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use anyhow::Context as _;
use async_trait::async_trait;
use axum::Router;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::contracts::RunnableCapability;
use toolkit::{Gear, GearCtx};
use toolkit_db::DBProvider;
use toolkit_db::DbError;
use tracing::{info, warn};

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};

use construct_sdk::ConstructClientV1;
use graph_storage_sdk::GraphStorageClientV1;

use crate::api::rest::routes;
use crate::api::rest::types::{ConcreteIntake, ConcreteProfile, ConcreteSettings};
use crate::config::ConstructConfig;
use crate::domain::local_client::LocalClient;
use crate::domain::record_intake::{IntakePorts, RecordIntakeService};
use crate::domain::subject_settings::SubjectSettingsService;
use crate::infra::connector_switch::ConfigConnectorSwitch;
use crate::infra::pipeline::{self, Worker};
use crate::infra::processing_metrics::WorkerLoad;
use crate::infra::record_types::RegistryRecordTypes;
use crate::infra::storage::record_ids_repo::SeaOrmRecordIdRepository;
use crate::infra::storage::subject_settings_repo::SeaOrmSubjectSettingsRepository;
use crate::infra::tenant_defaults::SettingsServiceDefaults;

/// The Construct gear: record intake for connectors, the processing of
/// received records into a subject's profile, and the profile read, with
/// tenant-scoped storage and an in-process client.
///
/// @cpt-dod:cpt-cf-construct-dod-gear-foundation-anatomy:p1
#[toolkit::gear(
    name = "construct",
    deps = [authz_resolver, types_registry, graph_storage],
    capabilities = [rest, db, stateful]
)]
pub struct ConstructGear {
    intake: OnceLock<Arc<ConcreteIntake>>,
    settings: OnceLock<Arc<ConcreteSettings>>,
    profile: OnceLock<Arc<ConcreteProfile>>,
    /// The processing worker, until `start` takes it.
    worker: Mutex<Option<Worker>>,
    running: Mutex<Option<Running>>,
}

/// The started worker: its stop token, its task, and what it has on hand.
struct Running {
    token: CancellationToken,
    handle: JoinHandle<()>,
    load: WorkerLoad,
}

impl Default for ConstructGear {
    fn default() -> Self {
        Self {
            intake: OnceLock::new(),
            settings: OnceLock::new(),
            profile: OnceLock::new(),
            worker: Mutex::new(None),
            running: Mutex::new(None),
        }
    }
}

impl toolkit::contracts::DatabaseCapability for ConstructGear {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-migrations
        info!("Providing construct database migrations");
        crate::infra::storage::migrations::Migrator::migrations()
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-migrations
    }
}

#[async_trait]
impl Gear for ConstructGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-config
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-config-fail
        let cfg = ctx.config_or_default::<ConstructConfig>()?.validated()?;
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-config-fail
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-config

        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-db
        let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-db

        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-authz
        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverApi>()
            .context("failed to get AuthZ resolver")?;
        let policy_enforcer = PolicyEnforcer::new(authz);
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-authz

        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-service
        // The settings service and the types registry are resolved from
        // ClientHub on each read, so their start order does not matter.
        let hub = ctx.client_hub();
        // Everything that can fail runs before either service is stored, so a
        // failed start leaves the gear uninitialized and a retry can succeed.
        let types = Arc::new(
            RegistryRecordTypes::new(Arc::clone(&hub))
                .context("construct-sdk record base schema")?,
        );
        // @cpt-begin:cpt-cf-construct-algo-subject-settings-start:p1:inst-settings-start
        let defaults =
            SettingsServiceDefaults::new(Arc::clone(&hub), cfg.config().personalization_default)
                .context("personalization default setting key")?;
        let settings = Arc::new(SubjectSettingsService::new(
            Arc::clone(&db),
            Arc::new(SeaOrmSubjectSettingsRepository::new()),
            Arc::new(defaults),
            policy_enforcer.clone(),
        ));
        let graph = hub
            .get::<dyn GraphStorageClientV1>()
            .context("failed to get graph storage")?;
        let pipeline = pipeline::build(
            &cfg,
            &hub,
            graph,
            Arc::clone(&settings) as _,
            policy_enforcer.clone(),
        )
        .context("record processing")?;
        // Nothing below can fail but a second init.
        self.settings
            .set(Arc::clone(&settings))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        // @cpt-end:cpt-cf-construct-algo-subject-settings-start:p1:inst-settings-start
        let profile = pipeline.profile;
        self.profile
            .set(Arc::clone(&profile))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        *lock(&self.worker) = pipeline.worker;
        let ports = IntakePorts {
            types,
            switch: Arc::new(ConfigConnectorSwitch::new(
                cfg.config().connectors_off.clone(),
            )),
            hand_off: pipeline.hand_off,
        };
        let intake = Arc::new(RecordIntakeService::new(
            db,
            Arc::new(SeaOrmRecordIdRepository::new()),
            settings,
            ports,
            policy_enforcer,
        ));
        self.intake
            .set(Arc::clone(&intake))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-service

        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-client
        let client: Arc<dyn ConstructClientV1> = Arc::new(LocalClient::new(intake, profile));
        ctx.client_hub().register(client);
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-client

        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-return
        Ok(())
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-return
    }
}

#[async_trait]
impl toolkit::contracts::RestApiCapability for ConstructGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-rest
        let intake = self
            .intake
            .get()
            .map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?;

        let settings = self
            .settings
            .get()
            .map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("Subject settings service not initialized"))?;

        let profile = self
            .profile
            .get()
            .map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("Profile read not initialized"))?;

        let router = routes::register_routes(router, openapi, intake, settings, profile);
        info!("Construct gear: REST routes registered");
        Ok(router)
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-rest
    }
}

#[async_trait]
impl RunnableCapability for ConstructGear {
    /// Start processing received records, when processing is configured.
    async fn start(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        // @cpt-begin:cpt-cf-construct-algo-profile-reader-wire:p1:inst-wire-start
        let worker = lock(&self.worker).take();
        let Some(worker) = worker else {
            return Ok(());
        };
        let token = cancel.child_token();
        let load = worker.load();
        let handle = tokio::spawn(worker.run(token.clone()));
        *lock(&self.running) = Some(Running {
            token,
            handle,
            load,
        });
        info!("construct: record processing started");
        Ok(())
        // @cpt-end:cpt-cf-construct-algo-profile-reader-wire:p1:inst-wire-start
    }

    /// Stop processing. Records in flight are stopped; a record still queued
    /// is lost, as the DESIGN says, and its identity stays, so a resend is a
    /// repeat. How many were lost is logged.
    async fn stop(&self, deadline: CancellationToken) -> anyhow::Result<()> {
        let running = lock(&self.running).take();
        if let Some(Running {
            token,
            mut handle,
            load,
        }) = running
        {
            let (queued, in_flight) = (load.queued(), load.in_flight());
            if queued + in_flight > 0 {
                warn!(
                    queued,
                    in_flight,
                    "construct: stopping record processing; these records are lost, and a resend counts as a repeat"
                );
            }
            token.cancel();
            // The worker's own end is looked at first, so a worker that ended
            // with a panic by the deadline is reported, not aborted.
            let result = tokio::select! {
                biased;
                result = &mut handle => result,
                () = deadline.cancelled() => {
                    warn!(
                        in_flight = load.in_flight(),
                        "construct: record processing did not stop by the deadline and is aborted"
                    );
                    handle.abort();
                    // An aborted task ends at its next await. This waits for the
                    // worker to end; the record tasks it holds are aborted as it
                    // drops them.
                    let Ok(result) = tokio::time::timeout(ABORT_GRACE, &mut handle).await else {
                        // The host is told the stop was not clean; it logs it.
                        warn!("construct: record processing did not end after it was aborted");
                        return Err(anyhow::anyhow!(
                            "construct record processing did not end after it was aborted"
                        ));
                    };
                    result
                }
            };
            if let Err(err) = result
                && !err.is_cancelled()
            {
                // A worker that panicked lost the records it held; the host is
                // told the stop was not clean, and logs it.
                return Err(anyhow::Error::new(err)
                    .context("construct record processing ended with an error"));
            }
        }
        Ok(())
    }
}

/// How long `stop` waits for an aborted worker to end.
const ABORT_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

/// The guard of `mutex`, also after a panic while it was held: the worker
/// slots hold no invariant a panic can break, and dropping the worker or its
/// handle would leave records queued with nothing to process them.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "gear_test.rs"]
mod gear_test;
