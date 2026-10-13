use std::sync::Arc;

use async_trait::async_trait;
use construct_sdk::gts::{CHAT_MESSAGE_TYPE, ENRICHMENT_FACT_CANDIDATE_TYPE};
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::agent::Planner;
use crate::domain::admission::{Admission, AdmissionOutcome};
use crate::domain::error::DomainError;
use crate::domain::plan::Plan;
use crate::domain::profile::Profile;
use crate::domain::record_intake::{
    DropEvent, DropReason, DropStage, IntakeEvents, ReceivedRecord,
};
use crate::domain::sensitive_data::{Checked, SensitiveDataChecks};

/// The record types the planner decides on. A mastery record is stored as it is, by its own story, and never reaches
/// the model.
const PLANNED_TYPES: [&str; 2] = [CHAT_MESSAGE_TYPE, ENRICHMENT_FACT_CANDIDATE_TYPE];

/// Reads a subject's profile and its version from graph storage.
#[async_trait]
pub trait ProfileSource: Send + Sync {
    /// # Errors
    ///
    /// When the profile cannot be read. The record is then dropped.
    async fn read(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        subject_id: Uuid,
    ) -> Result<Profile, DomainError>;
}

/// What writing an admitted plan gave.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    Stored,
    /// The profile changed after the plan was built on it, and nothing was stored.
    Conflict,
}

/// Stores an admitted plan in one write, expecting the profile version the plan was built on.
#[async_trait]
pub trait PlanWriter: Send + Sync {
    /// # Errors
    ///
    /// When the write cannot run. The record is then dropped.
    async fn write(&self, ctx: &SecurityContext, plan: &Plan) -> Result<Written, DomainError>;
}

/// The security context and the scope processing runs under for a tenant. Intake hands off no context: the
/// connector's token can expire before processing runs.
#[async_trait]
pub trait ProcessingContexts: Send + Sync {
    /// # Errors
    ///
    /// When no context can be built for the tenant. The record is then dropped.
    async fn for_tenant(
        &self,
        tenant_id: Uuid,
    ) -> Result<(SecurityContext, AccessScope), DomainError>;
}

/// The collaborators of processing.
#[domain_model]
pub struct ProcessingPorts {
    pub contexts: Arc<dyn ProcessingContexts>,
    pub profiles: Arc<dyn ProfileSource>,
    pub writer: Arc<dyn PlanWriter>,
    pub events: Arc<dyn IntakeEvents>,
}

/// Takes one received record through the planner, the sensitive-data checks, Admission and the writer. A write
/// conflict reruns the chain on the new profile, at most `max_reruns` times.
#[domain_model]
pub struct Processing {
    planner: Planner,
    checks: SensitiveDataChecks,
    admission: Admission,
    ports: ProcessingPorts,
    max_reruns: u32,
}

#[derive(Clone, Copy)]
enum Failure {
    NotPlanned,
    NoContext,
    ProfileRead,
    AdmissionRead,
    Write,
    RerunLimit,
}

impl Failure {
    fn stage(self) -> DropStage {
        match self {
            Self::NotPlanned | Self::NoContext | Self::ProfileRead => DropStage::Planner,
            Self::AdmissionRead => DropStage::Admission,
            Self::Write | Self::RerunLimit => DropStage::Writer,
        }
    }

    fn reason(self) -> DropReason {
        match self {
            Self::RerunLimit => DropReason::RerunLimit,
            Self::NotPlanned => DropReason::NotPlanned,
            _ => DropReason::ProcessingFailed,
        }
    }

    fn kind(self) -> &'static str {
        match self {
            Self::NotPlanned | Self::RerunLimit => self.reason().name(),
            Self::NoContext => "no_context",
            Self::ProfileRead => "profile_read_failed",
            Self::AdmissionRead => "admission_read_failed",
            Self::Write => "write_failed",
        }
    }
}

/// How one run of the chain ended without a failure of its own.
enum Run {
    /// Stored, nothing to store, or dropped by a stage that sent its own drop event.
    Done,
    Conflict,
}

impl Processing {
    #[must_use]
    pub fn new(
        planner: Planner,
        checks: SensitiveDataChecks,
        admission: Admission,
        ports: ProcessingPorts,
        max_reruns: u32,
    ) -> Self {
        Self {
            planner,
            checks,
            admission,
            ports,
            max_reruns,
        }
    }

    /// Processes `record` to its end: stored, or dropped with its drop event.
    ///
    /// @cpt-dod:cpt-cf-construct-dod-planner-processing:p1
    pub async fn process(&self, record: &ReceivedRecord) {
        if let Err(failure) = self.process_or_fail(record).await {
            tracing::warn!(
                event = "processing_dropped_record",
                tenant_id = %record.tenant_id,
                record_type = %record.envelope.type_id,
                kind = failure.kind(),
                "processing dropped a record"
            );
            self.ports.events.dropped(DropEvent::for_record(
                record,
                failure.stage(),
                failure.reason(),
            ));
        }
    }

    async fn process_or_fail(&self, record: &ReceivedRecord) -> Result<(), Failure> {
        if !PLANNED_TYPES.contains(&record.envelope.type_id.as_str()) {
            return Err(Failure::NotPlanned);
        }
        let subject_id = record.envelope.subject_id.ok_or(Failure::NotPlanned)?;
        let (ctx, scope) = self
            .ports
            .contexts
            .for_tenant(record.tenant_id)
            .await
            .map_err(|_| Failure::NoContext)?;
        for _ in 0..=self.max_reruns {
            match self.run(&ctx, &scope, record, subject_id).await? {
                Run::Done => return Ok(()),
                Run::Conflict => {}
            }
        }
        Err(Failure::RerunLimit)
    }

    async fn run(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        record: &ReceivedRecord,
        subject_id: Uuid,
    ) -> Result<Run, Failure> {
        let profile = self
            .ports
            .profiles
            .read(ctx, record.tenant_id, subject_id)
            .await
            .map_err(|_| Failure::ProfileRead)?;
        let Some(plan) = self.planner.plan(ctx, record, profile).await else {
            return Ok(Run::Done);
        };
        if plan.steps.is_empty() {
            return Ok(Run::Done);
        }
        let Checked::Verdicts(verdicts) = self.checks.run(&plan).await else {
            return Ok(Run::Done);
        };
        let admitted = match self
            .admission
            .admit(ctx, scope, plan, &verdicts)
            .await
            .map_err(|_| Failure::AdmissionRead)?
        {
            AdmissionOutcome::Admitted(admitted) => admitted.plan,
            AdmissionOutcome::Dropped(_) => return Ok(Run::Done),
        };
        if admitted.steps.is_empty() {
            return Ok(Run::Done);
        }
        match self
            .ports
            .writer
            .write(ctx, &admitted)
            .await
            .map_err(|_| Failure::Write)?
        {
            Written::Stored => Ok(Run::Done),
            Written::Conflict => Ok(Run::Conflict),
        }
    }
}
