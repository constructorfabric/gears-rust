//! Assembles what the gear runs: the processing of received records (planner,
//! checks, Admission, writer) behind a queue, and the profile read.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use futures::FutureExt as _;
use graph_storage_sdk::GraphStorageClientV1;
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::ClientHub;
use toolkit_security::{AccessScope, SecurityContext};
use tracing::Instrument as _;
use uuid::Uuid;

use crate::config::{CategoryCheckConfig, ValidConfig};
use crate::domain::admin_facts::{AdminFactsPorts, AdminFactsService};
use crate::domain::admission::{Admission, AdmissionPorts};
use crate::domain::error::DomainError;
use crate::domain::model_client::ModelClient;
use crate::domain::one_step::OneStepPath;
use crate::domain::planner::agent::{Planner, PlannerCaps};
use crate::domain::planner::catalog::PropertyCatalog;
use crate::domain::planner::processing::{
    PlanWriter, Processing, ProcessingContexts, ProcessingPorts,
};
use crate::domain::profile_read::{CategoryCheck, ProfileReadPorts, ProfileReadService};
use crate::domain::profile_writes::ProfileWrites;
use crate::domain::record_intake::{
    DropEvent, DropReason, DropStage, IntakeEvents, RecordHandOff, RecordKey,
};
use crate::domain::review_queue::{ReviewQueueService, ReviewRequests};
use crate::domain::sensitive_data::SensitiveDataChecks;
use crate::domain::subject_control::{SubjectControlPorts, SubjectControlService};
use crate::domain::subject_settings::SubjectSettingsReader;
use crate::infra::admin_facts_events::LogAdminFactsEvents;
use crate::infra::admission_events::LogAdmissionEvents;
use crate::infra::admission_rules::{ConfigAdmissionRules, warn_open_lists};
use crate::infra::intake_events::LogIntakeEvents;
use crate::infra::model::chat_completions::{ChatCompletionsModel, ChatCompletionsSettings};
use crate::infra::planner_stub::DroppingHandOff;
use crate::infra::processing_contexts::ServiceIdentity;
use crate::infra::processing_metrics::{CountedDrops, ProcessingMetrics, WorkerLoad};
use crate::infra::profile_events::LogProfileEvents;
use crate::infra::record_queue::{RecordQueue, record_queue, serve};
use crate::infra::review_queue_events::LogReviewQueueEvents;
use crate::infra::storage::graph::reader::GraphProfileSource;
use crate::infra::storage::graph::root::PayloadCounter;
use crate::infra::storage::graph::types::GraphTypes;
use crate::infra::storage::graph::writer::{GraphPlanWriter, SoftDelete, WriterPorts};
use crate::infra::subject_control_events::LogSubjectControlEvents;

/// [`ProcessingContexts`] for a deployment without a processing identity:
/// Construct has no identity to read or write the profiles as.
struct NoIdentity;

#[async_trait]
impl ProcessingContexts for NoIdentity {
    async fn for_tenant(
        &self,
        _tenant_id: Uuid,
    ) -> Result<(SecurityContext, AccessScope), DomainError> {
        Err(DomainError::NotConfigured(
            "no processing identity is configured".to_owned(),
        ))
    }
}

/// The processing worker the gear starts: the queue it drains and what it
/// runs on each record.
pub struct Worker {
    queue: RecordQueue,
    processing: Arc<Processing>,
    workers: std::num::NonZeroUsize,
    load: WorkerLoad,
    metrics: Arc<ProcessingMetrics>,
    events: Arc<dyn IntakeEvents>,
}

impl Worker {
    /// The records the worker has on hand, for the gear's stop to report.
    #[must_use]
    pub fn load(&self) -> WorkerLoad {
        self.load.clone()
    }

    /// Process queued records until `cancel` fires.
    ///
    /// NOT cancel-safe for the records in flight: when `cancel` fires, or the
    /// gear's stop deadline aborts the task, a record being processed stops
    /// where it is, with no drop event, and a record still queued is lost.
    /// Graph storage is left consistent, because a plan is stored in one
    /// write that lands whole or not at all. A stopped record's identity
    /// stays in intake, so a resend is a repeat, as the DESIGN states.
    ///
    /// A record whose processing panics is dropped with an event and an error
    /// log; the worker goes on with the next.
    pub async fn run(self, cancel: CancellationToken) {
        let (processing, load, metrics, events) =
            (self.processing, self.load, self.metrics, self.events);
        serve(
            self.queue,
            move |record| {
                let processing = Arc::clone(&processing);
                let metrics = Arc::clone(&metrics);
                let events = Arc::clone(&events);
                let in_flight = load.start();
                let span = tracing::info_span!(
                    "construct_process_record",
                    tenant_id = %record.tenant_id,
                    record_type = %record.envelope.type_id,
                    record_key = %RecordKey::of_record(&record),
                    connector = %record.connector,
                    subject_id = record.envelope.subject_id.map(tracing::field::display),
                );
                async move {
                    let started = std::time::Instant::now();
                    let processed = AssertUnwindSafe(processing.process(&record))
                        .catch_unwind()
                        .await;
                    if let Err(panic) = processed {
                        tracing::error!(
                            panic = panic_message(panic.as_ref()),
                            "construct: processing a record panicked; the record is dropped"
                        );
                        events.dropped(DropEvent::for_record(
                            &record,
                            DropStage::Worker,
                            DropReason::ProcessingFailed,
                        ));
                    }
                    metrics.processed(started.elapsed());
                    drop(in_flight);
                }
                .instrument(span)
            },
            self.workers,
            cancel,
        )
        .await;
    }
}

/// What the gear runs.
pub struct Pipeline {
    /// Where intake hands a received record.
    pub hand_off: Arc<dyn RecordHandOff>,
    /// The worker to start, none when processing is not configured.
    pub worker: Option<Worker>,
    pub profile: Arc<ProfileReadService>,
    pub subject_control: Arc<SubjectControlService>,
    pub review_queue: Arc<ReviewQueueService>,
    pub admin_facts: Arc<AdminFactsService>,
}

/// Build the pipeline from the config.
///
/// Processing runs when both a processing identity and a model are
/// configured; otherwise intake keeps dropping received records with an
/// event, as before the planner existed. The profile read needs the identity.
///
/// # Errors
///
/// When the configured model or the person types cannot be used, a defect in
/// the config or the gear.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-reader-wiring:p1
pub fn build(
    cfg: &ValidConfig,
    hub: &Arc<ClientHub>,
    graph: Arc<dyn GraphStorageClientV1>,
    settings: Arc<dyn SubjectSettingsReader>,
    reviews: Arc<dyn ReviewRequests>,
    policy_enforcer: PolicyEnforcer,
) -> anyhow::Result<Pipeline> {
    let model: Option<Arc<dyn ModelClient>> = match &cfg.config().model {
        Some(model) => Some(Arc::new(ChatCompletionsModel::new(
            Arc::clone(hub),
            ChatCompletionsSettings {
                upstream_alias: model.upstream_alias.clone(),
                model: model.model.clone(),
                timeout: model.timeout,
            },
        )?)),
        None => None,
    };
    assemble(cfg, model, graph, settings, reviews, policy_enforcer)
}

/// [`build`] with the model given, the seam a test uses for a scripted model.
///
/// # Errors
///
/// When the person types cannot be read, a defect in the gear.
pub fn assemble(
    cfg: &ValidConfig,
    model: Option<Arc<dyn ModelClient>>,
    graph: Arc<dyn GraphStorageClientV1>,
    settings: Arc<dyn SubjectSettingsReader>,
    reviews: Arc<dyn ReviewRequests>,
    policy_enforcer: PolicyEnforcer,
) -> anyhow::Result<Pipeline> {
    let configured = Configured {
        identity: Part::of(cfg.identity().as_ref()),
        model: Part::of(model.as_ref()),
        unchecked: if cfg.config().processing.run_without_sensitive_data_checks {
            Part::Present
        } else {
            Part::Missing
        },
    };
    let identity = cfg
        .identity()
        .map(|identity| ServiceIdentity::new(identity.id));
    let contexts: Arc<dyn ProcessingContexts> = match identity {
        Some(identity) => Arc::new(identity),
        None => Arc::new(NoIdentity),
    };

    // @cpt-begin:cpt-cf-construct-algo-profile-reader-wire:p1:inst-wire-read
    // Without an identity the read answers not configured at the identity, before
    // it reaches graph storage.
    let reader = Arc::new(GraphProfileSource::new(
        Arc::clone(&graph),
        Arc::new(PayloadCounter),
    ));
    let profile = Arc::new(ProfileReadService::new(
        ProfileReadPorts {
            settings: Arc::clone(&settings),
            profiles: Arc::clone(&reader) as _,
            contexts: Arc::clone(&contexts),
            events: Arc::new(LogProfileEvents),
            reviews: Arc::clone(&reviews) as _,
        },
        policy_enforcer.clone(),
        // Without an identity the read answers not configured before it
        // checks a category, so the check it would take does not matter.
        match cfg.identity().map(|identity| identity.category_check) {
            Some(CategoryCheckConfig::OneCall) => CategoryCheck::OneCall,
            Some(CategoryCheckConfig::PerCategory) | None => CategoryCheck::PerCategory,
        },
    ));
    // @cpt-end:cpt-cf-construct-algo-profile-reader-wire:p1:inst-wire-read

    // @cpt-begin:cpt-cf-construct-algo-subject-control-wire:p1:inst-wire-subject-control
    // The writer stores processing's plans and the subject's deletes, so a
    // subject deletes its facts also where no model is configured.
    let writer: Arc<dyn PlanWriter> = Arc::new(GraphPlanWriter::new(WriterPorts {
        client: Arc::clone(&graph),
        types: Arc::new(GraphTypes::new(Arc::clone(&graph))),
        version: Arc::new(PayloadCounter),
        removal: Arc::new(SoftDelete::new(graph)),
    }));
    let profile_writes = Arc::new(ProfileWrites::new(
        Arc::clone(&reader) as _,
        Arc::clone(&writer),
        Arc::clone(&contexts),
        cfg.config().processing.max_reruns,
    ));
    let subject_control = Arc::new(SubjectControlService::new(
        SubjectControlPorts {
            writes: Arc::clone(&profile_writes),
            settings: Arc::clone(&settings),
            events: Arc::new(LogSubjectControlEvents),
            reviews: Arc::clone(&reviews),
            review_events: Arc::new(LogReviewQueueEvents),
        },
        policy_enforcer.clone(),
    ));
    // @cpt-end:cpt-cf-construct-algo-subject-control-wire:p1:inst-wire-subject-control

    // @cpt-begin:cpt-cf-construct-algo-review-queue-wire:p1:inst-wire-review-queue
    // A correction stores a value, so it runs only where record processing
    // may: with an identity and the opt-in to store values unchecked. A
    // rejection and a delete need neither.
    let catalog = Arc::new(PropertyCatalog::load().context("the person types' property catalog")?);
    let stores_unchecked = identity.is_some() && configured.unchecked.present();
    if stores_unchecked {
        warn_unchecked(cfg);
    }
    let one_step = stores_unchecked.then(|| {
        let (checks, admission) =
            value_stages(cfg, Arc::clone(&settings), Arc::new(LogIntakeEvents));
        Arc::new(OneStepPath {
            checks,
            admission,
            catalog: Arc::clone(&catalog),
        })
    });
    let review_queue = Arc::new(ReviewQueueService::new(
        Arc::clone(&profile_writes),
        Arc::clone(&reviews),
        Arc::new(LogReviewQueueEvents),
        one_step.clone(),
        policy_enforcer.clone(),
    ));
    // @cpt-end:cpt-cf-construct-algo-review-queue-wire:p1:inst-wire-review-queue

    // @cpt-begin:cpt-cf-construct-algo-admin-facts-wire:p2:inst-wire-admin-facts
    // An administrator's add and edit store values through the same path as
    // a correction, under the same opt-in; a read and a delete need neither.
    let admin_facts = Arc::new(AdminFactsService::new(
        AdminFactsPorts {
            writes: profile_writes,
            review_queue: Arc::clone(&review_queue),
            one_step,
            events: Arc::new(LogAdminFactsEvents),
        },
        policy_enforcer,
    ));
    // @cpt-end:cpt-cf-construct-algo-admin-facts-wire:p2:inst-wire-admin-facts

    // @cpt-begin:cpt-cf-construct-algo-profile-reader-wire:p1:inst-wire-processing
    // No real sensitive-data check exists yet, so processing runs only where
    // the deployment opted in to storing values unchecked.
    let (true, true, Some(model)) = (identity.is_some(), configured.unchecked.present(), model)
    else {
        configured.log();
        return Ok(Pipeline {
            hand_off: Arc::new(DroppingHandOff::new(Arc::new(LogIntakeEvents))),
            worker: None,
            profile,
            subject_control,
            review_queue,
            admin_facts,
        });
    };
    let (hand_off, queue) = record_queue(cfg.config().processing.queue_capacity);
    let hand_off = Arc::new(hand_off);
    let load = WorkerLoad::new(Arc::downgrade(&hand_off));
    let metrics = Arc::new(ProcessingMetrics::new(
        &load,
        &opentelemetry::global::meter("construct"),
    ));
    let events: Arc<dyn IntakeEvents> = Arc::new(CountedDrops::new(
        Arc::new(LogIntakeEvents),
        Arc::clone(&metrics),
    ));
    let planner = Planner::new(
        model,
        catalog,
        Arc::clone(&events) as _,
        PlannerCaps {
            max_rounds: cfg.config().processing.max_rounds,
            max_tokens: cfg.config().processing.max_tokens,
        },
    );
    let (checks, admission) = value_stages(cfg, settings, Arc::clone(&events) as _);
    let processing = Arc::new(Processing::new(
        planner,
        checks,
        admission,
        ProcessingPorts {
            contexts,
            profiles: reader,
            writer,
            events: Arc::clone(&events),
            reviews: reviews as _,
        },
        cfg.config().processing.max_reruns,
    ));
    Ok(Pipeline {
        hand_off,
        worker: Some(Worker {
            queue,
            processing,
            workers: cfg.config().processing.workers,
            load,
            metrics,
            events,
        }),
        profile,
        subject_control,
        review_queue,
        admin_facts,
    })
    // @cpt-end:cpt-cf-construct-algo-profile-reader-wire:p1:inst-wire-processing
}

/// The stages every value Construct stores passes before the writer: the
/// sensitive-data checks and Admission. Record processing and a reviewer's
/// correction both take theirs from here, so a check added here guards both.
fn value_stages(
    cfg: &ValidConfig,
    settings: Arc<dyn SubjectSettingsReader>,
    drops: Arc<dyn IntakeEvents>,
) -> (SensitiveDataChecks, Admission) {
    // Every value is allowed until a real check exists; the deployment opted
    // in, and the operator is told so at start-up.
    let checks = SensitiveDataChecks::new(Vec::new(), Arc::clone(&drops));
    let admission = Admission::new(AdmissionPorts {
        settings,
        rules: Arc::new(ConfigAdmissionRules::new(&cfg.config().admission)),
        events: Arc::new(LogAdmissionEvents),
        drops,
    });
    (checks, admission)
}

/// Tell the operator at start-up that values are stored without a
/// sensitive-data check, by record processing where it runs and by
/// reviewers' corrections.
fn warn_unchecked(cfg: &ValidConfig) {
    tracing::warn!(
        "construct: no sensitive-data check exists; every value that Admission lets through is \
         stored, from records and from reviewers' corrections"
    );
    warn_open_lists(&cfg.config().admission);
}

/// The message a panic was raised with, when it is a string.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("a panic without a message")
}

/// Whether the deployment has one of the processing's prerequisites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Present,
    Missing,
}

impl Part {
    fn of<T>(value: Option<&T>) -> Self {
        if value.is_some() {
            Self::Present
        } else {
            Self::Missing
        }
    }

    fn present(self) -> bool {
        self == Self::Present
    }
}

/// Which of the processing's prerequisites the deployment has.
#[derive(Debug, Clone, Copy)]
struct Configured {
    identity: Part,
    model: Part,
    /// The opt-in to process without sensitive-data checks.
    unchecked: Part,
}

impl Configured {
    /// Say at start-up what is not configured: an info line when processing
    /// is off on purpose (neither identity nor model), a warning when it is
    /// only partly configured or lacks the opt-in to run without
    /// sensitive-data checks, and a warning when the profile read is not
    /// configured.
    #[expect(
        clippy::cognitive_complexity,
        reason = "three log lines; the tracing macros count toward the complexity"
    )]
    fn log(self) {
        let (identity, model) = (self.identity.present(), self.model.present());
        if !identity && !model {
            tracing::info!(
                "construct: record processing is not configured; received records are dropped"
            );
        } else if identity && model {
            tracing::warn!(
                "construct: record processing needs processing.run_without_sensitive_data_checks \
                 while no sensitive-data check exists; received records are dropped"
            );
        } else {
            tracing::warn!(
                processing_identity = identity,
                model = model,
                "construct: record processing is only partly configured; received records are dropped"
            );
        }
        if !identity {
            tracing::warn!(
                "construct: the profile read answers 501, not configured, until processing_identity is set"
            );
        }
    }
}
