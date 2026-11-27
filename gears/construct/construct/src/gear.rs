use std::sync::{Arc, OnceLock};

use anyhow::Context as _;
use async_trait::async_trait;
use axum::Router;
use parking_lot::Mutex;
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
use crate::api::rest::types::{
    ConcreteAdminFacts, ConcreteErasure, ConcreteIntake, ConcreteProfile, ConcreteReviewQueue,
    ConcreteSchema, ConcreteSettings, ConcreteSubjectControl,
};
use crate::config::ConstructConfig;
use crate::domain::local_client::LocalClient;
use crate::domain::planner::manage::ManageFacts;
use crate::domain::record_intake::{IntakePorts, RecordIntakeService};
use crate::domain::schema_service::{SchemaService, StoredSchemas};
use crate::domain::subject_settings::SubjectSettingsService;
use crate::infra::connector_switch::ConfigConnectorSwitch;
use crate::infra::deletion_job::{ClusterLeadership, DeletionJob, Leadership};
use crate::infra::pipeline::{self, Worker};
use crate::infra::processing_metrics::WorkerLoad;
use crate::infra::record_types::RegistryRecordTypes;
use crate::infra::storage::deletion_repo::DeletionTables;
use crate::infra::storage::record_ids_repo::SeaOrmRecordIdRepository;
use crate::infra::storage::review_requests_repo::ReviewRequestStore;
use crate::infra::storage::subject_schema_repo::SeaOrmSubjectSchemaRepository;
use crate::infra::storage::subject_settings_repo::SeaOrmSubjectSettingsRepository;
use crate::infra::tenant_defaults::SettingsServiceDefaults;

/// The Construct gear: record intake for connectors, the processing of
/// received records into a subject's profile, manage-facts, and the profile
/// read, with tenant-scoped storage and an in-process client.
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
    schema: OnceLock<Arc<ConcreteSchema>>,
    profile: OnceLock<Arc<ConcreteProfile>>,
    subject_control: OnceLock<Arc<ConcreteSubjectControl>>,
    review_queue: OnceLock<Arc<ConcreteReviewQueue>>,
    admin_facts: OnceLock<Arc<ConcreteAdminFacts>>,
    manage: OnceLock<Arc<ManageFacts>>,
    erasure: OnceLock<Arc<ConcreteErasure>>,
    /// The processing worker, until `start` takes it.
    worker: Mutex<Option<Worker>>,
    running: Mutex<Option<Running>>,
    /// The deletion job, until `start` takes it.
    deletion: Mutex<Option<DeletionTasks>>,
    /// The started deletion job.
    deleting: Mutex<Option<Deleting>>,
}

/// What `start` runs for Deletion.
struct DeletionTasks {
    job: Arc<DeletionJob>,
    leadership: Arc<dyn Leadership>,
}

/// The started deletion job, and its stop token.
struct Deleting {
    token: CancellationToken,
    handles: Vec<(&'static str, JoinHandle<()>)>,
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
            schema: OnceLock::new(),
            profile: OnceLock::new(),
            subject_control: OnceLock::new(),
            review_queue: OnceLock::new(),
            admin_facts: OnceLock::new(),
            manage: OnceLock::new(),
            erasure: OnceLock::new(),
            worker: Mutex::new(None),
            running: Mutex::new(None),
            deletion: Mutex::new(None),
            deleting: Mutex::new(None),
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
        let schema_repo = Arc::new(SeaOrmSubjectSchemaRepository::new());
        let pipeline = pipeline::build(
            &cfg,
            &hub,
            graph,
            Arc::clone(&settings) as _,
            Arc::new(ReviewRequestStore::new(Arc::clone(&db))),
            Arc::new(DeletionTables::new(Arc::clone(&db))),
            policy_enforcer.clone(),
            Arc::new(StoredSchemas::new(
                Arc::clone(&db),
                Arc::clone(&schema_repo),
            )),
        )
        .context("record processing")?;
        let schema = Arc::new(SchemaService::new(
            Arc::clone(&db),
            schema_repo,
            Arc::clone(&pipeline.schema_effects),
            Arc::new(crate::infra::schema_events::LogSchemaEvents),
            policy_enforcer.clone(),
        ));
        self.schema
            .set(schema)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        // Nothing below can fail but a second init.
        self.settings
            .set(Arc::clone(&settings))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        // @cpt-end:cpt-cf-construct-algo-subject-settings-start:p1:inst-settings-start
        let profile = pipeline.profile;
        self.profile
            .set(Arc::clone(&profile))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        self.subject_control
            .set(pipeline.subject_control)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        self.review_queue
            .set(pipeline.review_queue)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        self.admin_facts
            .set(pipeline.admin_facts)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        self.manage
            .set(pipeline.manage)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        self.erasure
            .set(pipeline.erasure)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        *self.worker.lock() = pipeline.worker;
        // @cpt-begin:cpt-cf-construct-algo-erasure-wire:p1:inst-wire-tasks
        *self.deletion.lock() = Some(DeletionTasks {
            job: pipeline.deletion_job,
            leadership: Arc::new(ClusterLeadership::new(Arc::clone(&hub))),
        });
        // @cpt-end:cpt-cf-construct-algo-erasure-wire:p1:inst-wire-tasks
        let ports = IntakePorts {
            types,
            switch: Arc::new(ConfigConnectorSwitch::new(
                cfg.config().connectors_off.clone(),
            )),
            hand_off: pipeline.hand_off,
            retention: pipeline.retention,
            generations: pipeline.generations,
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
        self.routes(router, openapi)
    }
}

impl ConstructGear {
    /// Every REST route of the gear, on `router`, with its schemas in
    /// `openapi`. A host running other gears registers theirs in the same
    /// registry, which refuses two different schemas under one name.
    fn routes(&self, router: Router, openapi: &dyn OpenApiRegistry) -> anyhow::Result<Router> {
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

        let subject_control = self
            .subject_control
            .get()
            .map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("Subject control not initialized"))?;

        let review_queue = self
            .review_queue
            .get()
            .map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("Review queue not initialized"))?;

        let admin_facts = self
            .admin_facts
            .get()
            .map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("Administrator fact management not initialized"))?;
        let manage = self
            .manage
            .get()
            .map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("Manage-facts not initialized"))?;

        let router = routes::register_routes(router, openapi, intake, settings, profile);
        let schema = self
            .schema
            .get()
            .map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("Profile schema not initialized"))?;
        let router = routes::register_schema_routes(router, openapi, schema);
        let router = routes::register_subject_control_routes(router, openapi, subject_control);
        let router = routes::register_review_queue_routes(router, openapi, review_queue);
        let erasure = self
            .erasure
            .get()
            .map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("Erasure not initialized"))?;

        let router = routes::register_admin_facts_routes(router, openapi, admin_facts);
        let router = routes::register_manage_route(router, openapi, manage);
        let router = routes::register_erasure_routes(router, openapi, erasure);
        info!("Construct gear: REST routes registered");
        Ok(router)
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-rest
    }
}

#[async_trait]
impl RunnableCapability for ConstructGear {
    /// Start the leader's deletion job, and the
    /// processing of received records when it is configured.
    async fn start(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        // @cpt-begin:cpt-cf-construct-algo-erasure-wire:p1:inst-wire-start
        let tasks = self.deletion.lock().take();
        if let Some(DeletionTasks { job, leadership }) = tasks {
            let token = cancel.child_token();
            let handles = vec![(
                "deletion job",
                tokio::spawn({
                    let token = token.clone();
                    async move { leadership.lead(job, token).await }
                }),
            )];
            *self.deleting.lock() = Some(Deleting { token, handles });
        }
        // @cpt-end:cpt-cf-construct-algo-erasure-wire:p1:inst-wire-start
        // @cpt-begin:cpt-cf-construct-algo-profile-reader-wire:p1:inst-wire-start
        let worker = self.worker.lock().take();
        let Some(worker) = worker else {
            return Ok(());
        };
        let token = cancel.child_token();
        let load = worker.load();
        let handle = tokio::spawn(worker.run(token.clone()));
        *self.running.lock() = Some(Running {
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
        // The deletion job stops where it is, an erasure in flight with its
        // mark set; the next run goes on from what is left.
        let deleting = self.deleting.lock().take();
        let deletion_failed = match deleting {
            Some(deleting) => stop_deletion(deleting, &deadline).await,
            None => None,
        };
        let running = self.running.lock().take();
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
        deletion_failed.map_or(Ok(()), Err)
    }
}

/// Cancel the deletion job, wait for it until `deadline`, then abort it and
/// wait [`ABORT_GRACE`] more. Each task that panicked or did not end is
/// logged; the first is returned for the host.
async fn stop_deletion(
    Deleting { token, handles }: Deleting,
    deadline: &CancellationToken,
) -> Option<anyhow::Error> {
    token.cancel();
    let mut failed = None;
    for (name, mut handle) in handles {
        let result = tokio::select! {
            biased;
            result = &mut handle => result,
            () = deadline.cancelled() => {
                if let Some(result) = abort(name, &mut handle).await {
                    result
                } else {
                    failed.get_or_insert_with(|| {
                        anyhow::anyhow!("construct {name} did not end after it was aborted")
                    });
                    continue;
                }
            }
        };
        if let Err(err) = result
            && !err.is_cancelled()
        {
            let err = task_failed(name, err);
            failed.get_or_insert(err);
        }
    }
    failed
}

/// Abort a deletion task that did not stop by the deadline and wait
/// [`ABORT_GRACE`] for it to end; none when it did not. Both are logged.
async fn abort(
    name: &str,
    handle: &mut JoinHandle<()>,
) -> Option<Result<(), tokio::task::JoinError>> {
    warn!(
        task = name,
        "construct: a deletion task did not stop by the deadline and is aborted"
    );
    handle.abort();
    let ended = tokio::time::timeout(ABORT_GRACE, handle).await.ok();
    if ended.is_none() {
        warn!(
            task = name,
            "construct: a deletion task did not end after it was aborted"
        );
    }
    ended
}

/// Log a deletion task that ended with an error, and the error for the
/// host. Logged here too: a processing stop that fails returns its own
/// error, and this one would not reach the host.
fn task_failed(name: &str, err: tokio::task::JoinError) -> anyhow::Error {
    tracing::error!(task = name, error = %err, "construct: a deletion task ended with an error");
    anyhow::Error::new(err).context(format!("construct {name} ended with an error"))
}

/// How long `stop` waits for an aborted worker to end.
const ABORT_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

#[cfg(test)]
#[path = "gear_test.rs"]
mod gear_test;
