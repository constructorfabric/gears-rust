//! Assembles what the gear runs: the processing of received records (planner,
//! checks, Admission, writer) behind a queue, and the profile read.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use graph_storage_sdk::GraphStorageClientV1;
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::ClientHub;
use toolkit_security::{AccessScope, SecurityContext};
use tracing::Instrument as _;
use uuid::Uuid;

use crate::config::{CategoryCheckConfig, ConstructConfig};
use crate::domain::admission::{Admission, AdmissionPorts};
use crate::domain::error::DomainError;
use crate::domain::model_client::ModelClient;
use crate::domain::planner::agent::{Planner, PlannerCaps};
use crate::domain::planner::catalog::PropertyCatalog;
use crate::domain::planner::processing::{Processing, ProcessingContexts, ProcessingPorts};
use crate::domain::profile_read::{CategoryCheck, ProfileReadPorts, ProfileReadService};
use crate::domain::record_intake::RecordHandOff;
use crate::domain::sensitive_data::SensitiveDataChecks;
use crate::domain::subject_settings::SubjectSettingsReader;
use crate::infra::admission_events::LogAdmissionEvents;
use crate::infra::admission_rules::ConfigAdmissionRules;
use crate::infra::intake_events::LogIntakeEvents;
use crate::infra::model::chat_completions::{ChatCompletionsModel, ChatCompletionsSettings};
use crate::infra::planner_stub::DroppingHandOff;
use crate::infra::processing_contexts::ServiceIdentity;
use crate::infra::profile_events::LogProfileEvents;
use crate::infra::record_queue::{RecordQueue, record_queue, serve};
use crate::infra::storage::graph::reader::GraphProfileSource;
use crate::infra::storage::graph::root::PayloadCounter;
use crate::infra::storage::graph::types::GraphTypes;
use crate::infra::storage::graph::writer::{GraphPlanWriter, SoftDelete, WriterPorts};

/// [`ProcessingContexts`] for a deployment without a processing identity:
/// Construct has no identity to read or write the profiles as.
struct NoIdentity;

#[async_trait]
impl ProcessingContexts for NoIdentity {
    async fn for_tenant(
        &self,
        _tenant_id: Uuid,
    ) -> Result<(SecurityContext, AccessScope), DomainError> {
        Err(DomainError::Unavailable(
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
}

impl Worker {
    /// Process queued records until `cancel` fires.
    ///
    /// NOT cancel-safe for the records in flight: when `cancel` fires, or the
    /// gear's stop deadline aborts the task, a record being processed stops
    /// where it is, with no drop event, and a record still queued is lost.
    /// Graph storage is left consistent, because a plan is stored in one
    /// write that lands whole or not at all. A stopped record's identity
    /// stays in intake, so a resend is a repeat, as the DESIGN states.
    pub async fn run(self, cancel: CancellationToken) {
        let processing = self.processing;
        serve(
            self.queue,
            move |record| {
                let processing = Arc::clone(&processing);
                let span = tracing::info_span!(
                    "construct_process_record",
                    tenant_id = %record.tenant_id,
                    record_type = %record.envelope.type_id
                );
                async move { processing.process(&record).await }.instrument(span)
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
    cfg: &ConstructConfig,
    hub: &Arc<ClientHub>,
    graph: Option<Arc<dyn GraphStorageClientV1>>,
    settings: Arc<dyn SubjectSettingsReader>,
    policy_enforcer: PolicyEnforcer,
) -> anyhow::Result<Pipeline> {
    let model: Option<Arc<dyn ModelClient>> = match &cfg.model {
        Some(model) => Some(Arc::new(ChatCompletionsModel::new(
            Arc::clone(hub),
            ChatCompletionsSettings {
                upstream_alias: model.upstream_alias.clone(),
                model: model.model.clone(),
                timeout: Duration::from_secs(model.timeout_secs.get()),
            },
        )?)),
        None => None,
    };
    assemble(cfg, model, graph, settings, policy_enforcer)
}

/// [`build`] with the model given, the seam a test uses for a scripted model.
///
/// # Errors
///
/// When the person types cannot be read, a defect in the gear.
pub fn assemble(
    cfg: &ConstructConfig,
    model: Option<Arc<dyn ModelClient>>,
    graph: Option<Arc<dyn GraphStorageClientV1>>,
    settings: Arc<dyn SubjectSettingsReader>,
    policy_enforcer: PolicyEnforcer,
) -> anyhow::Result<Pipeline> {
    let events = Arc::new(LogIntakeEvents);
    let (has_identity, has_graph, has_model) = (
        cfg.processing_identity.is_some(),
        graph.is_some(),
        model.is_some(),
    );
    let identity = cfg.processing_identity.map(ServiceIdentity::new);
    let contexts: Arc<dyn ProcessingContexts> = match identity {
        Some(identity) => Arc::new(identity),
        None => Arc::new(NoIdentity),
    };

    // @cpt-begin:cpt-cf-construct-algo-profile-reader-wire:p1:inst-wire-read
    let graph = graph.filter(|_| identity.is_some());
    let reader = graph.as_ref().map(|client| {
        Arc::new(GraphProfileSource::new(
            Arc::clone(client),
            Arc::new(PayloadCounter),
        ))
    });
    let profiles: Arc<dyn crate::domain::profile_read::StoredProfiles> = match &reader {
        Some(reader) => Arc::clone(reader) as _,
        None => Arc::new(NoProfiles),
    };
    let profile = Arc::new(ProfileReadService::new(
        ProfileReadPorts {
            settings: Arc::clone(&settings),
            profiles,
            contexts: Arc::clone(&contexts),
            events: Arc::new(LogProfileEvents),
        },
        policy_enforcer,
        match cfg.profile_categories {
            CategoryCheckConfig::OneCall => CategoryCheck::OneCall,
            CategoryCheckConfig::PerCategory => CategoryCheck::PerCategory,
        },
    ));
    // @cpt-end:cpt-cf-construct-algo-profile-reader-wire:p1:inst-wire-read

    // @cpt-begin:cpt-cf-construct-algo-profile-reader-wire:p1:inst-wire-processing
    let (Some(client), Some(reader), Some(model)) = (graph, reader, model) else {
        log_unconfigured(has_identity, has_graph, has_model);
        return Ok(Pipeline {
            hand_off: Arc::new(DroppingHandOff::new(events)),
            worker: None,
            profile,
        });
    };
    let planner = Planner::new(
        model,
        Arc::new(PropertyCatalog::load().context("the planner's property catalog")?),
        Arc::clone(&events) as _,
        PlannerCaps {
            max_rounds: cfg.processing.max_rounds,
            max_tokens: cfg.processing.max_tokens,
        },
    );
    // No real sensitive-data check exists yet; every value is allowed.
    let checks = SensitiveDataChecks::new(Vec::new(), Arc::clone(&events) as _);
    let admission = Admission::new(AdmissionPorts {
        settings,
        rules: Arc::new(ConfigAdmissionRules::new(&cfg.admission)),
        events: Arc::new(LogAdmissionEvents),
        drops: Arc::clone(&events) as _,
    });
    let writer = GraphPlanWriter::new(WriterPorts {
        client: Arc::clone(&client),
        types: Arc::new(GraphTypes::new(Arc::clone(&client))),
        version: Arc::new(PayloadCounter),
        removal: Arc::new(SoftDelete::new(client)),
    });
    let processing = Arc::new(Processing::new(
        planner,
        checks,
        admission,
        ProcessingPorts {
            contexts,
            profiles: reader,
            writer: Arc::new(writer),
            events,
        },
        cfg.processing.max_reruns,
    ));
    let (hand_off, queue) = record_queue(cfg.processing.queue_capacity);
    Ok(Pipeline {
        hand_off: Arc::new(hand_off),
        worker: Some(Worker {
            queue,
            processing,
            workers: cfg.processing.workers,
        }),
        profile,
    })
    // @cpt-end:cpt-cf-construct-algo-profile-reader-wire:p1:inst-wire-processing
}

/// Say at start-up what is not configured: an info line when processing is
/// off on purpose (neither identity nor model), a warning when it is only
/// partly configured, and a warning when the profile read is unavailable.
#[expect(
    clippy::cognitive_complexity,
    reason = "three log lines; the tracing macros count toward the complexity"
)]
fn log_unconfigured(has_identity: bool, has_graph: bool, has_model: bool) {
    if !has_identity && !has_model {
        tracing::info!(
            "construct: record processing is not configured; received records are dropped"
        );
    } else {
        tracing::warn!(
            processing_identity = has_identity,
            graph_storage = has_graph,
            model = has_model,
            "construct: record processing is only partly configured; received records are dropped"
        );
    }
    if !has_identity || !has_graph {
        tracing::warn!(
            processing_identity = has_identity,
            graph_storage = has_graph,
            "construct: the profile read answers unavailable"
        );
    }
}

/// [`crate::domain::profile_read::StoredProfiles`] for a deployment without
/// graph storage or a processing identity.
struct NoProfiles;

#[async_trait]
impl crate::domain::profile_read::StoredProfiles for NoProfiles {
    async fn stored(
        &self,
        _ctx: &SecurityContext,
        _key: crate::domain::subject_settings::SubjectKey,
    ) -> Result<Vec<crate::domain::profile_read::StoredFact>, DomainError> {
        Err(DomainError::Unavailable(
            "no profile store is configured".to_owned(),
        ))
    }
}
