use std::sync::Arc;

use anyhow::Context as _;
use authz_resolver_sdk::AuthZResolverClient;
use modkit_db::Db;
use modkit_db::outbox::{Outbox, OutboxHandle, Partitions, WorkerTuning};
use usage_collector_sdk::UsageCollectorClientV1;

use crate::api::UsageEmitterRuntimeV1;
use crate::config::UsageEmitterConfig;
use crate::domain::factory::{SubjectChoice, UsageEmitterFactory};
use crate::infra::delivery_handler::DeliveryHandler;

/// Process-lifetime owner of the usage-emitter pipeline.
///
/// Owns the outbox worker, shared Arcs, and validated configuration. Registered in
/// `ClientHub` as `dyn UsageEmitterRuntimeV1`. Each call to
/// [`UsageEmitterRuntimeV1::factory`] hands out a module-bound
/// [`UsageEmitterFactory`] that source modules clone per call to apply scope overrides
/// and then `.authorize()` to obtain a [`crate::UsageEmitter`].
pub struct UsageEmitterRuntime {
    config: Arc<UsageEmitterConfig>,
    db: Db,
    authz: Arc<dyn AuthZResolverClient>,
    collector: Arc<dyn UsageCollectorClientV1>,
    outbox: Arc<Outbox>,
    _worker: OutboxHandle, // keeps the background task alive for process lifetime
}

impl UsageEmitterRuntime {
    /// Build a [`UsageEmitterRuntime`] and start the background outbox worker.
    ///
    /// Validates `config`, registers the `usage-records` queue, attaches
    /// `delivery_handler` for async delivery to `collector`, and wires the
    /// [`authz_resolver_sdk::pep::PolicyEnforcer`] for per-call PDP checks.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration is invalid or if the outbox worker fails to
    /// start (e.g. DB unavailable or queue registration fails).
    pub async fn build(
        config: UsageEmitterConfig,
        db: Db,
        authz: Arc<dyn AuthZResolverClient>,
        collector: Arc<dyn UsageCollectorClientV1>,
    ) -> anyhow::Result<Self> {
        config.validate()?;

        let delivery_handler = DeliveryHandler::new(Arc::clone(&collector));

        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-6a
        let processor_tuning =
            WorkerTuning::processor_default().retry_max(config.outbox_backoff_max);
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-6a

        let outbox_handle = Outbox::builder(db.clone())
            .processor_tuning(processor_tuning)
            .queue(
                &config.outbox_queue,
                Partitions::of(config.outbox_partition_count),
            )
            .leased(delivery_handler)
            .start()
            .await
            .context("usage-emitter: failed to start outbox worker")?;

        let outbox = Arc::clone(outbox_handle.outbox());

        Ok(Self {
            config: Arc::new(config),
            db,
            authz,
            collector,
            outbox,
            _worker: outbox_handle,
        })
    }
}

impl UsageEmitterRuntimeV1 for UsageEmitterRuntime {
    fn factory(&self, module_name: &str) -> UsageEmitterFactory {
        UsageEmitterFactory {
            config: Arc::clone(&self.config),
            db: self.db.clone(),
            authz: Arc::clone(&self.authz),
            collector: Arc::clone(&self.collector),
            outbox: Arc::clone(&self.outbox),
            module: module_name.to_owned(),
            tenant: None,
            // Default = "resolve from SecurityContext at authorize() time"
            // — the caller has not yet called `.with_subject(...)` or
            // `.without_subject()`. See `SubjectChoice` docs for the three-state contract.
            subject: SubjectChoice::DefaultFromCtx,
        }
    }
}
