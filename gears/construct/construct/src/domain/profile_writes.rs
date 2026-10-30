//! The writes Subject Control and the Review Queue make to a subject's
//! profile outside processing: each against the profile's version, read and
//! written again after a short pause on a conflict, at most as often as
//! processing reruns a plan and within a budget.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::{Instant, timeout_at};
use tokio_retry::strategy::jitter;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::error::DomainError;
use super::plan::{Change, NodeKey, Origin, Plan, Step};
use super::planner::processing::{PlanWriter, ProcessingContexts, Written};
use super::profile_read::{StoredFact, read_failure};
use super::subject_control::{SubjectFacts, SubjectProfiles};
use super::subject_settings::SubjectKey;

/// How long one write may read the profile and start writes, its reruns and
/// their pauses included; past it the write answers unavailable. A write once
/// started runs to its end under the writer's own timeouts, since cutting it
/// off could store the change without its event or its request's close.
pub const WRITE_BUDGET: Duration = Duration::from_secs(20);

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

    /// The subject's live facts, read as `construct`. A read writes
    /// nothing, so `deadline` cuts it off.
    pub(crate) async fn read(
        &self,
        construct: &SecurityContext,
        key: SubjectKey,
        deadline: Instant,
        what: &str,
    ) -> Result<SubjectFacts, DomainError> {
        timeout_at(deadline, self.profiles.facts(construct, key))
            .await
            .map_err(|_| out_of_budget(what))?
            .map_err(read_failure)
    }

    /// Write `plan` as `construct`. A write is never cut off, so none starts
    /// past `deadline`.
    pub(crate) async fn write(
        &self,
        construct: &SecurityContext,
        plan: &Plan,
        deadline: Instant,
        what: &str,
    ) -> Result<Written, DomainError> {
        if Instant::now() >= deadline {
            return Err(out_of_budget(what));
        }
        self.writer.write(construct, plan).await
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
    ) -> Result<Removed, DomainError> {
        self.with_reruns(deadline, "delete", move || async move {
            // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-read
            let current = self.read(construct, key, deadline, "delete").await?;
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
            let written = self.write(construct, &plan, deadline, "delete").await?;
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
    /// The attempt keeps the deadline itself, through [`Self::read`] and
    /// [`Self::write`].
    ///
    /// # Errors
    ///
    /// The attempt's own errors; unavailable when the profile kept changing
    /// or the deadline passed.
    pub(crate) async fn with_reruns<T, F, Fut>(
        &self,
        deadline: Instant,
        what: &str,
        mut attempt: F,
    ) -> Result<T, DomainError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<Attempt<T>, DomainError>>,
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
        Err(DomainError::Unavailable(format!(
            "the profile kept changing; the {what} was not written"
        )))
        // @cpt-end:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-give-up
    }
}

pub(crate) fn out_of_budget(what: &str) -> DomainError {
    tracing::warn!(what, "construct: a write ran out of its budget");
    DomainError::Unavailable(format!("the {what} ran out of its budget"))
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
