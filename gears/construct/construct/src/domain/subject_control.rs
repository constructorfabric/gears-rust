//! Subject Control: the subject's own view of all its facts, one fact, the
//! delete of one fact, and the export of a subject's data for the data
//! protection officer.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::AccessRequest;
use time::OffsetDateTime;
use tokio::time::Instant;
use tokio_retry::strategy::jitter;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use super::error::DomainError;
use super::pagination::{Key, Listed, Listing, page};
use super::plan::{Change, NodeKey, Origin, Plan, Step};
use super::planner::manage::SUBJECT_FACTS_RESOURCE;
use super::planner::processing::{PlanWriter, ProcessingContexts, Written};
use super::profile::Category;
use super::profile_read::{StoredFact, granted, read_failure, settings_scope};
use super::review_queue::{
    Closing, ReviewOutcome, ReviewQueueEvent, ReviewQueueEvents, ReviewRequest, ReviewRequests,
};
use super::subject_settings::{SubjectKey, SubjectSettings, SubjectSettingsReader};

pub(crate) mod actions {
    pub use super::super::review_queue::actions::FLAG;
    /// The subject reads its own facts.
    pub const READ: &str = "read";
    /// The subject deletes one of its own facts.
    pub const DELETE: &str = "delete";
    /// The data protection officer exports a subject's data.
    pub const EXPORT: &str = "export";
}

/// How long one delete may read and start writes, its reruns and their
/// pauses included; past it the delete answers unavailable. A write once
/// started runs to its end under the writer's own timeouts, since cutting it
/// off could leave the fact deleted without its event.
pub const DELETE_BUDGET: Duration = Duration::from_secs(20);

/// The fields a subject's facts list filters and orders by. Exists only to
/// derive [`SubjectFactField`].
#[domain_model]
#[derive(toolkit_odata_macros::ODataFilterable)]
pub struct SubjectFactQuery {
    /// The fact's id, the graph node key.
    #[odata(filter(kind = "String"))]
    pub fact_id: String,
    /// The fact's category: `identity`, `role`, `skill` or `preference`.
    #[odata(filter(kind = "String"))]
    pub entity_kind: String,
    /// Who the fact came from: `connector`, `agent`, `reviewer` or
    /// `administrator`.
    #[odata(filter(kind = "String"))]
    pub origin: String,
    /// When the fact was stored.
    #[odata(filter(kind = "DateTimeUtc"))]
    pub stored_at: OffsetDateTime,
}

pub use SubjectFactQueryFilterField as SubjectFactField;

/// The subject's facts list: newest first, then by fact id.
pub const SUBJECT_FACTS_LISTING: Listing<SubjectFactField> = Listing {
    default_order: &[(SubjectFactField::StoredAt, SortDir::Desc)],
    tiebreaker: SubjectFactField::FactId,
};

impl Listed for StoredFact {
    type Field = SubjectFactField;

    fn key(&self, field: SubjectFactField) -> Option<Key> {
        Some(match field {
            SubjectFactField::FactId => Key::text(self.node_key.0.clone()),
            SubjectFactField::EntityKind => Key::text(self.category.name()),
            SubjectFactField::Origin => Key::text(self.origin.name()),
            SubjectFactField::StoredAt => Key::time(self.stored_at),
        })
    }
}

/// One live fact of the subject as the subject sees it: with its latest
/// review request, open or closed, if it has one.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct SubjectFact {
    pub fact: StoredFact,
    pub review_request: Option<ReviewRequest>,
}

impl Listed for SubjectFact {
    type Field = SubjectFactField;

    fn key(&self, field: SubjectFactField) -> Option<Key> {
        self.fact.key(field)
    }
}

/// A subject's live facts, and the version of its profile they belong to;
/// none for a subject without a profile.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct SubjectFacts {
    pub version: Option<u64>,
    pub facts: Vec<StoredFact>,
}

/// One page of the subject's facts, and the version of the profile it was
/// cut from.
#[domain_model]
#[derive(Debug, Clone)]
pub struct SubjectFactsPage {
    pub version: Option<u64>,
    pub page: Page<SubjectFact>,
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

/// A subject's data as the export gives it: the settings, every live fact
/// and every review request.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct SubjectExport {
    pub tenant_id: Uuid,
    pub subject_id: Uuid,
    pub settings: SubjectSettings,
    pub facts: Vec<StoredFact>,
    pub review_requests: Vec<ReviewRequest>,
}

/// What Subject Control did, for the audit log. No event holds a value.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubjectControlEvent {
    /// The subject was served its own facts, a page of them or one.
    Viewed {
        tenant_id: Uuid,
        subject_id: Uuid,
        caller: Uuid,
        categories: BTreeSet<Category>,
    },
    /// The subject deleted one of its facts.
    Deleted {
        tenant_id: Uuid,
        subject_id: Uuid,
        caller: Uuid,
        fact_id: NodeKey,
        category: Category,
    },
    /// The data protection officer exported the subject's data.
    Exported {
        tenant_id: Uuid,
        subject_id: Uuid,
        caller: Uuid,
        categories: BTreeSet<Category>,
    },
}

/// Where Subject Control's events go: the structured log until the Audit
/// gear ships.
pub trait SubjectControlEvents: Send + Sync {
    fn record(&self, event: SubjectControlEvent);
}

/// The collaborators of Subject Control.
#[domain_model]
pub struct SubjectControlPorts {
    pub profiles: Arc<dyn SubjectProfiles>,
    pub writer: Arc<dyn PlanWriter>,
    pub contexts: Arc<dyn ProcessingContexts>,
    pub settings: Arc<dyn SubjectSettingsReader>,
    pub events: Arc<dyn SubjectControlEvents>,
    pub reviews: Arc<dyn ReviewRequests>,
    pub review_events: Arc<dyn ReviewQueueEvents>,
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

/// The subject's own view, one fact and delete, owner only, and the export
/// under its own permission. Personalization off and an erasure under way
/// block none of them.
#[domain_model]
pub struct SubjectControlService {
    ports: SubjectControlPorts,
    policy_enforcer: PolicyEnforcer,
    max_reruns: u32,
    budget: Duration,
}

impl SubjectControlService {
    #[must_use]
    pub fn new(
        ports: SubjectControlPorts,
        policy_enforcer: PolicyEnforcer,
        max_reruns: u32,
    ) -> Self {
        Self {
            ports,
            policy_enforcer,
            max_reruns,
            budget: DELETE_BUDGET,
        }
    }

    /// The same service with another budget for one delete.
    #[must_use]
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// The page of the subject's live facts that `query` asks for, with its
    /// profile's version.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the caller is not the subject or the
    /// platform refuses; [`DomainError::InvalidQuery`] for a filter, order
    /// or cursor the list does not take; not configured without a
    /// processing identity; unavailable or internal when a dependency fails.
    ///
    /// @cpt-dod:cpt-cf-construct-dod-subject-control-view:p1
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %subject_id))]
    pub async fn view(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
        query: &ODataQuery,
    ) -> Result<SubjectFactsPage, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-subject-control-view:p1:inst-view-read
        let key = self.own(ctx, subject_id, actions::READ).await?;
        let facts = self.facts(key).await?;
        let items = self.with_requests(key, facts.facts).await?;
        let page = page(items, query, &SUBJECT_FACTS_LISTING)?;
        // @cpt-end:cpt-cf-construct-flow-subject-control-view:p1:inst-view-read
        // @cpt-begin:cpt-cf-construct-flow-subject-control-view:p1:inst-view-event
        self.ports.events.record(SubjectControlEvent::Viewed {
            tenant_id: key.tenant_id,
            subject_id,
            caller: ctx.subject_id(),
            categories: page.items.iter().map(|item| item.fact.category).collect(),
        });
        // @cpt-end:cpt-cf-construct-flow-subject-control-view:p1:inst-view-event
        Ok(SubjectFactsPage {
            version: facts.version,
            page,
        })
    }

    /// One live fact of the subject.
    ///
    /// # Errors
    ///
    /// [`DomainError::NoSuchFact`] when the subject has no such live fact;
    /// otherwise as [`Self::view`].
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %subject_id))]
    pub async fn fact(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
        fact_id: &NodeKey,
    ) -> Result<SubjectFact, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-subject-control-fact:p1:inst-fact-read
        let key = self.own(ctx, subject_id, actions::READ).await?;
        let fact = self
            .facts(key)
            .await?
            .facts
            .into_iter()
            .find(|fact| &fact.node_key == fact_id)
            .ok_or_else(|| no_such_fact(fact_id))?;
        let fact = self
            .with_requests(key, vec![fact])
            .await?
            .pop()
            .ok_or_else(|| DomainError::internal("a fact was lost attaching its request"))?;
        // @cpt-end:cpt-cf-construct-flow-subject-control-fact:p1:inst-fact-read
        // @cpt-begin:cpt-cf-construct-flow-subject-control-fact:p1:inst-fact-event
        self.ports.events.record(SubjectControlEvent::Viewed {
            tenant_id: key.tenant_id,
            subject_id,
            caller: ctx.subject_id(),
            categories: BTreeSet::from([fact.fact.category]),
        });
        // @cpt-end:cpt-cf-construct-flow-subject-control-fact:p1:inst-fact-event
        Ok(fact)
    }

    /// Soft-delete one live fact of the subject: a plan with one removal,
    /// written against the profile's version, and read and written again
    /// after a short pause on a conflict, at most `max_reruns` times and
    /// within the budget.
    ///
    /// NOT cancel-safe: a drop after the writer stored the plan leaves the
    /// fact deleted without its delete event. So the budget cuts off the
    /// permission check, the context and a read, and starts no write past its
    /// end, but never cuts off a write.
    ///
    /// # Errors
    ///
    /// [`DomainError::NoSuchFact`] when the subject has no such live fact,
    /// also when a concurrent write removed it before a rerun; unavailable
    /// when the profile kept changing past the reruns or the budget ran out;
    /// otherwise as [`Self::view`].
    ///
    /// @cpt-dod:cpt-cf-construct-dod-subject-control-delete:p1
    /// @cpt-dod:cpt-cf-construct-dod-review-queue-subject-control:p1
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %subject_id, fact_id = %fact_id.0))]
    pub async fn delete(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
        fact_id: &NodeKey,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-own
        let deadline = Instant::now() + self.budget;
        // The permission check and the context write nothing, so the budget
        // may cut them off, like a read.
        let (key, construct) =
            tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), async {
                let key = self.own(ctx, subject_id, actions::DELETE).await?;
                let (construct, _) = self.ports.contexts.for_tenant(key.tenant_id).await?;
                Ok::<_, DomainError>((key, construct))
            })
            .await
            .map_err(|_| out_of_time(fact_id))??;
        // @cpt-end:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-own
        let origin = Origin::Subject {
            subject: subject_id,
        };
        let Removed::Removed(fact) = self
            .remove(&construct, key, fact_id, &origin, deadline)
            .await?
        else {
            return Err(no_such_fact(fact_id));
        };
        self.ports.events.record(SubjectControlEvent::Deleted {
            tenant_id: key.tenant_id,
            subject_id,
            caller: ctx.subject_id(),
            fact_id: fact.node_key.clone(),
            category: fact.category,
        });
        // The fact is gone whatever happens here. A request left open by a
        // failure closes as deleted when a reviewer resolves it.
        let closing = Closing {
            outcome: ReviewOutcome::Deleted,
            by: subject_id,
            reason: None,
            corrected_fact_id: None,
            at: OffsetDateTime::now_utc(),
        };
        match self
            .ports
            .reviews
            .close_open_on(key, &fact.node_key, &closing)
            .await
        {
            Ok(Some(request)) => {
                if let Some(event) = ReviewQueueEvent::resolved(&request) {
                    self.ports.review_events.record(event);
                }
            }
            Ok(None) => {}
            Err(err) => tracing::error!(
                error = %err,
                "construct: the review request of a deleted fact could not be closed"
            ),
        }
        Ok(())
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
        let (profiles, writer) = (&self.ports.profiles, &self.ports.writer);
        self.with_reruns(deadline, "delete", move || async move {
            // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-read
            // A read writes nothing, so the budget may cut it off.
            let current = tokio::time::timeout(
                deadline.saturating_duration_since(Instant::now()),
                profiles.facts(construct, key),
            )
            .await
            .map_err(|_| out_of_time(fact_id))?
            .map_err(read_failure)?;
            let Some(fact) = current
                .facts
                .into_iter()
                .find(|fact| &fact.node_key == fact_id)
            else {
                return Ok(Attempt::Done(Removed::Gone));
            };
            // @cpt-end:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-read
            // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-write
            // A write is never cut off, so none starts past the budget.
            if Instant::now() >= deadline {
                return Err(out_of_time(fact_id));
            }
            let plan = removal(key, current.version, &fact, origin.clone());
            let written = writer.write(construct, &plan).await?;
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
    ///
    /// # Errors
    ///
    /// The attempt's own errors; unavailable when the profile kept changing.
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

    /// A subject's data, for the data protection officer of the caller's
    /// tenant: the settings and every live fact. A subject of another tenant
    /// has nothing here, as one with no profile.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] or [`DomainError::OutOfScope`] when the
    /// platform refuses the export or the settings; not configured without a
    /// processing identity; unavailable or internal when a dependency fails.
    ///
    /// @cpt-dod:cpt-cf-construct-dod-subject-control-export:p1
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %subject_id, caller = %ctx.subject_id()))]
    pub async fn export(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
    ) -> Result<SubjectExport, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-subject-control-export:p1:inst-export-authorize
        let key = SubjectKey {
            tenant_id: ctx.subject_tenant_id(),
            subject_id,
        };
        let scope = self.authorize(ctx, key, actions::EXPORT).await?;
        // @cpt-end:cpt-cf-construct-flow-subject-control-export:p1:inst-export-authorize
        // @cpt-begin:cpt-cf-construct-flow-subject-control-export:p1:inst-export-read
        // A refusal of the settings stays a refusal: the export answers it
        // as one, not as a missing subject.
        let settings = self
            .ports
            .settings
            .read(ctx, &settings_scope(&scope), key)
            .await?;
        let facts = self.facts(key).await?.facts;
        let review_requests = self.ports.reviews.of_subject(key).await?;
        // @cpt-end:cpt-cf-construct-flow-subject-control-export:p1:inst-export-read
        // @cpt-begin:cpt-cf-construct-flow-subject-control-export:p1:inst-export-event
        self.ports.events.record(SubjectControlEvent::Exported {
            tenant_id: key.tenant_id,
            subject_id,
            caller: ctx.subject_id(),
            categories: categories_of(&facts),
        });
        // @cpt-end:cpt-cf-construct-flow-subject-control-export:p1:inst-export-event
        Ok(SubjectExport {
            tenant_id: key.tenant_id,
            subject_id,
            settings,
            facts,
            review_requests,
        })
    }

    /// The subject's key in the caller's tenant, when the caller is the
    /// subject and the platform allows `action` on its facts.
    pub(crate) async fn own(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
        action: &str,
    ) -> Result<SubjectKey, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-subject-control-owner-only:p1:inst-owner-compare
        if ctx.subject_id() != subject_id {
            return Err(DomainError::forbidden(format!(
                "subject {} of tenant {} may not {action} the facts of subject {subject_id}",
                ctx.subject_id(),
                ctx.subject_tenant_id()
            )));
        }
        // @cpt-end:cpt-cf-construct-algo-subject-control-owner-only:p1:inst-owner-compare
        // @cpt-begin:cpt-cf-construct-algo-subject-control-owner-only:p1:inst-owner-permission
        let key = SubjectKey {
            tenant_id: ctx.subject_tenant_id(),
            subject_id,
        };
        self.authorize(ctx, key, action).await?;
        Ok(key)
        // @cpt-end:cpt-cf-construct-algo-subject-control-owner-only:p1:inst-owner-permission
    }

    /// The platform's scope for `action` on the subject's facts, when it
    /// reaches the subject in the tenant.
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        key: SubjectKey,
        action: &str,
    ) -> Result<AccessScope, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &SUBJECT_FACTS_RESOURCE,
                action,
                Some(key.subject_id),
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, key.tenant_id),
            )
            .await?;
        if granted(&scope, key).is_none() {
            return Err(DomainError::forbidden(format!(
                "the policy scope for {action} does not reach subject {} in tenant {}",
                key.subject_id, key.tenant_id
            )));
        }
        Ok(scope)
    }

    /// The subject's live facts, read as Construct. A profile too large to
    /// read is the gear's own failure, as on the profile read, not the
    /// subject's state.
    pub(crate) async fn facts(&self, key: SubjectKey) -> Result<SubjectFacts, DomainError> {
        let (construct, _) = self.ports.contexts.for_tenant(key.tenant_id).await?;
        self.ports
            .profiles
            .facts(&construct, key)
            .await
            .map_err(read_failure)
    }

    /// `facts`, each with its latest review request.
    async fn with_requests(
        &self,
        key: SubjectKey,
        facts: Vec<StoredFact>,
    ) -> Result<Vec<SubjectFact>, DomainError> {
        // The requests come newest first, so the first one of a fact is its
        // latest.
        let mut latest: BTreeMap<NodeKey, ReviewRequest> = BTreeMap::new();
        for request in self.ports.reviews.of_subject(key).await? {
            latest.entry(request.fact_id.clone()).or_insert(request);
        }
        Ok(facts
            .into_iter()
            .map(|fact| {
                let review_request = latest.remove(&fact.node_key);
                SubjectFact {
                    fact,
                    review_request,
                }
            })
            .collect())
    }

    /// The collaborators, for the Review Queue's writes.
    pub(crate) fn ports(&self) -> &SubjectControlPorts {
        &self.ports
    }

    /// Construct's contexts, for the Review Queue's writes.
    pub(crate) fn contexts(&self) -> &Arc<dyn ProcessingContexts> {
        &self.ports.contexts
    }

    /// How long one write with its reruns may take.
    pub(crate) fn budget(&self) -> Duration {
        self.budget
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

fn categories_of(facts: &[StoredFact]) -> BTreeSet<Category> {
    facts.iter().map(|fact| fact.category).collect()
}

fn out_of_time(fact_id: &NodeKey) -> DomainError {
    DomainError::Unavailable(format!(
        "the delete ran out of time; fact {} was not deleted",
        fact_id.0
    ))
}

fn no_such_fact(fact_id: &NodeKey) -> DomainError {
    DomainError::NoSuchFact(format!("the subject has no live fact {}", fact_id.0))
}
