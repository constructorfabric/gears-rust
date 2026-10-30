//! The writes Subject Control and the Review Queue make to a subject's
//! profile outside processing: each against the profile's version, read and
//! written again after a short pause on a conflict, at most as often as
//! processing reruns a plan and within a budget.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::{Instant, timeout, timeout_at};
use tokio_retry::strategy::jitter;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::error::DomainError;
use super::plan::{Change, NodeKey, Origin, Plan, Step};
use super::planner::processing::{PlanWriter, ProcessingContexts, Written};
use super::profile_read::{StoredFact, read_failure};
use super::subject_settings::SubjectKey;

/// How long one write may take, its reruns, their pauses and the reads and
/// checks before each write included; well below the gateway's request
/// timeout, so the caller hears how it ended. A write that started is not
/// cut by the budget, only by [`WRITER_LIMIT`], so the budget never leaves
/// one half known.
pub const WRITE_BUDGET: Duration = Duration::from_secs(20);

/// The longest one call of the writer is awaited: past it, the write may
/// have landed. The writer bounds each of its own calls, and this bounds
/// their sum.
pub const WRITER_LIMIT: Duration = Duration::from_secs(90);

/// How long a read after a failed write may take, to see whether the write
/// landed.
const LANDED_CHECK: Duration = Duration::from_secs(5);

/// A subject's live facts, and the version of its profile they belong to;
/// none for a subject without a profile.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct SubjectFacts {
    pub version: Option<u64>,
    pub facts: Vec<StoredFact>,
}

/// Reads a subject's live facts with its profile's version, as Construct.
#[async_trait]
pub trait SubjectProfiles: Send + Sync {
    /// # Errors
    ///
    /// The errors of the profile store, and an internal error for a stored
    /// fact that cannot be read: the subject must see, export and delete
    /// every fact, so none is left out.
    async fn facts(
        &self,
        ctx: &SecurityContext,
        key: SubjectKey,
    ) -> Result<SubjectFacts, DomainError>;
}

/// What one write against the profile's version did: done with a result, or
/// the profile changed under it.
#[domain_model]
pub(crate) enum Attempt<T> {
    Done(T),
    Conflict,
}

/// What a removal of one fact did.
#[domain_model]
pub(crate) enum Removed {
    Removed(StoredFact),
    /// No live fact had the id, at the first read or after a conflict.
    Gone,
}

/// Why a write with reruns failed, and whether anything may have been
/// stored: the writer can fail after its write landed.
#[domain_model]
#[derive(Debug)]
pub(crate) enum WriteError {
    /// Nothing was written.
    NotWritten(DomainError),
    /// The writer failed; the write may have landed.
    MaybeWritten(DomainError),
}

impl From<DomainError> for WriteError {
    fn from(err: DomainError) -> Self {
        Self::NotWritten(err)
    }
}

/// A subject's profile read and written as Construct, with the rerun limit
/// and the budget of one write.
#[domain_model]
pub struct ProfileWrites {
    profiles: Arc<dyn SubjectProfiles>,
    writer: Arc<dyn PlanWriter>,
    contexts: Arc<dyn ProcessingContexts>,
    max_reruns: u32,
    budget: Duration,
}

impl ProfileWrites {
    #[must_use]
    pub fn new(
        profiles: Arc<dyn SubjectProfiles>,
        writer: Arc<dyn PlanWriter>,
        contexts: Arc<dyn ProcessingContexts>,
        max_reruns: u32,
    ) -> Self {
        Self {
            profiles,
            writer,
            contexts,
            max_reruns,
            budget: WRITE_BUDGET,
        }
    }

    /// The same writes with another budget for one write.
    #[must_use]
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// When a write that starts now must have ended.
    pub(crate) fn deadline(&self) -> Instant {
        Instant::now() + self.budget
    }

    /// Construct's context and scope in `tenant_id`.
    pub(crate) async fn construct(
        &self,
        tenant_id: Uuid,
    ) -> Result<(SecurityContext, AccessScope), DomainError> {
        self.contexts.for_tenant(tenant_id).await
    }

    /// The subject's live facts, read as Construct. A profile too large to
    /// read is the gear's own failure, as on the profile read, not the
    /// subject's state.
    pub(crate) async fn facts(&self, key: SubjectKey) -> Result<SubjectFacts, DomainError> {
        let (construct, _) = self.construct(key.tenant_id).await?;
        self.profiles
            .facts(&construct, key)
            .await
            .map_err(read_failure)
    }

    /// The subject's live facts, read as `construct`.
    pub(crate) async fn read(
        &self,
        construct: &SecurityContext,
        key: SubjectKey,
    ) -> Result<SubjectFacts, DomainError> {
        self.profiles
            .facts(construct, key)
            .await
            .map_err(read_failure)
    }

    /// Write `plan` as `construct`, for at most [`WRITER_LIMIT`]. When the
    /// writer fails or runs out, the subject's facts are read once more, and
    /// a write `landed` finds there counts as stored; otherwise it may have
    /// landed.
    pub(crate) async fn write(
        &self,
        construct: &SecurityContext,
        plan: &Plan,
        landed: impl Fn(&SubjectFacts) -> bool,
    ) -> Result<Written, WriteError> {
        let failed = match timeout(WRITER_LIMIT, self.writer.write(construct, plan)).await {
            Ok(Ok(written)) => return Ok(written),
            Ok(Err(err)) => err,
            Err(_) => DomainError::Unavailable(format!(
                "the profile write did not end within {WRITER_LIMIT:?}"
            )),
        };
        let key = SubjectKey {
            tenant_id: plan.tenant_id,
            subject_id: plan.subject_id,
        };
        match timeout(LANDED_CHECK, self.read(construct, key)).await {
            Ok(Ok(facts)) if landed(&facts) => {
                tracing::warn!(
                    error = %failed,
                    "construct: the writer failed, but its write is in the profile"
                );
                Ok(Written::Stored)
            }
            _ => Err(WriteError::MaybeWritten(failed)),
        }
    }

    /// `step`, unless `deadline` passes first: the reads and checks before a
    /// write. Nothing is written when it runs out, and no write starts once
    /// `deadline` passed, also when `step` ended just then.
    pub(crate) async fn before_write<T>(
        deadline: Instant,
        what: &str,
        step: impl Future<Output = Result<T, DomainError>>,
    ) -> Result<T, DomainError> {
        match timeout_at(deadline, step).await {
            Ok(done) if Instant::now() < deadline => done,
            Ok(Err(err)) => Err(err),
            _ => {
                tracing::warn!(
                    what,
                    "construct: a write ran out of its budget before it started"
                );
                Err(DomainError::Unavailable(format!(
                    "the {what} ran out of its budget; nothing was written"
                )))
            }
        }
    }

    /// Remove one live fact of the subject as `origin`: a plan with one
    /// removal, written against the profile's version, read and written
    /// again after a conflict as [`Self::with_reruns`] does.
    pub(crate) async fn remove(
        &self,
        construct: &SecurityContext,
        key: SubjectKey,
        fact_id: &NodeKey,
        origin: &Origin,
        deadline: Instant,
    ) -> Result<Removed, WriteError> {
        self.with_reruns(deadline, "delete", move || async move {
            // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-read
            let current = Self::before_write(deadline, "delete", self.read(construct, key)).await?;
            let Some(fact) = current
                .facts
                .into_iter()
                .find(|fact| &fact.node_key == fact_id)
            else {
                return Ok(Attempt::Done(Removed::Gone));
            };
            // @cpt-end:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-read
            // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-write
            let plan = removal(key, current.version, &fact, origin.clone());
            let written = self
                .write(construct, &plan, |now| {
                    !now.facts.iter().any(|fact| &fact.node_key == fact_id)
                })
                .await?;
            // @cpt-end:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-write
            Ok(match written {
                Written::Stored => Attempt::Done(Removed::Removed(fact)),
                Written::Conflict => Attempt::Conflict,
            })
        })
        .await
    }

    /// Run `attempt` until it is done: after a conflict, pause briefly and
    /// run it again, at most `max_reruns` times and while `deadline` lasts.
    /// The attempt bounds its own reads with [`Self::before_write`], and
    /// [`Self::write`] bounds the write.
    ///
    /// # Errors
    ///
    /// The attempt's own errors; not written when the profile kept changing.
    pub(crate) async fn with_reruns<T, F, Fut>(
        &self,
        deadline: Instant,
        what: &str,
        mut attempt: F,
    ) -> Result<T, WriteError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<Attempt<T>, WriteError>>,
    {
        for rerun in 0..=self.max_reruns {
            // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-stored
            match attempt().await? {
                Attempt::Done(done) => return Ok(done),
                Attempt::Conflict => {
                    tracing::debug!(
                        attempt = rerun,
                        what,
                        "construct: the profile changed under a write; reading it again"
                    );
                    if rerun == self.max_reruns {
                        break;
                    }
                    let pause = rerun_pause(rerun);
                    if Instant::now() + pause >= deadline {
                        break;
                    }
                    tokio::time::sleep(pause).await;
                }
            }
            // @cpt-end:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-stored
        }
        // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-give-up
        tracing::warn!(
            max_reruns = self.max_reruns,
            what,
            "construct: a write gave up; the profile kept changing"
        );
        Err(WriteError::NotWritten(DomainError::Unavailable(format!(
            "the profile kept changing; the {what} was not written"
        ))))
        // @cpt-end:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-give-up
    }
}

/// The delete of `fact` as a plan: one step that removes it, as `origin`,
/// written against `version`. The subject's own delete is the only plan with
/// the subject as origin, and it stores nothing.
fn removal(key: SubjectKey, version: Option<u64>, fact: &StoredFact, origin: Origin) -> Plan {
    Plan {
        tenant_id: key.tenant_id,
        subject_id: key.subject_id,
        origin,
        profile_version: version,
        steps: vec![Step {
            entity_kind: fact.category.name().to_owned(),
            change: Change::RemoveEntity {
                entity: fact.node_key.clone(),
            },
            confidence: None,
        }],
    }
}

/// The pause before rerun `attempt + 1`: growing, with jitter, so a delete
/// and processing writing the same profile drift apart.
fn rerun_pause(attempt: u32) -> Duration {
    let base = Duration::from_millis(20_u64.saturating_mul(1 << attempt.min(6)));
    base.saturating_add(jitter(base))
}
