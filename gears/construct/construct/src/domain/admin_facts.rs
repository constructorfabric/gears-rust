//! Administrator Fact Management: a tenant administrator reads all facts of
//! a subject of the tenant, and adds, edits or deletes one directly. An add
//! or an edit goes through the one-step plan path with the administrator as
//! origin; a delete works like the subject's delete. An edit or a delete of
//! a fact under an open review request resolves the request as the
//! administrator.

use std::collections::BTreeSet;
use std::future::ready;
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::AccessRequest;
use serde_json::Value;
use tokio::time::Instant;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use super::error::DomainError;
use super::one_step::{Gone, Held, NewFact, NotStored, OneStepDone, OneStepPath};
use super::pagination::page;
use super::plan::{NodeKey, Origin};
use super::planner::manage::SUBJECT_FACTS_RESOURCE;
use super::profile::Category;
use super::profile_writes::{ProfileWrites, WriteError};
use super::review_queue::{
    Resolution, Resolver, ReviewConflict, ReviewOutcome, ReviewQueueService, ReviewRequest,
    ReviewStatus, Wrote, reaches_tenant,
};
use super::subject_control::{FactDelete, SUBJECT_FACTS_LISTING, SubjectFactsPage, with_requests};
use super::subject_settings::SubjectKey;

pub(crate) mod actions {
    /// A tenant administrator reads, adds, edits and deletes the facts of
    /// the tenant's subjects.
    pub const ADMINISTER: &str = "administer";
}

/// What an administrator did, for the audit log. No event holds a value.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminFactsEvent {
    /// The administrator was served a page of the subject's facts.
    Read {
        tenant_id: Uuid,
        subject_id: Uuid,
        by: Uuid,
        categories: BTreeSet<Category>,
    },
    /// The administrator added a fact.
    Added {
        tenant_id: Uuid,
        subject_id: Uuid,
        by: Uuid,
        fact_id: NodeKey,
        category: Category,
    },
    /// The administrator edited a fact, which has a new id now.
    Edited {
        tenant_id: Uuid,
        subject_id: Uuid,
        by: Uuid,
        fact_id: NodeKey,
        new_fact_id: NodeKey,
        category: Category,
    },
    /// The administrator deleted a fact.
    Deleted {
        tenant_id: Uuid,
        subject_id: Uuid,
        by: Uuid,
        fact_id: NodeKey,
        category: Category,
    },
    /// The checks or Admission refused an add or an edit; nothing was
    /// stored. `fact_id` is the edited fact.
    Refused {
        tenant_id: Uuid,
        subject_id: Uuid,
        by: Uuid,
        fact_id: Option<NodeKey>,
        reason: NotStored,
    },
}

/// Where Administrator Fact Management's events go: the structured log
/// until the Audit gear ships.
pub trait AdminFactsEvents: Send + Sync {
    fn record(&self, event: AdminFactsEvent);
}

/// The collaborators of Administrator Fact Management.
#[domain_model]
pub struct AdminFactsPorts {
    pub writes: Arc<ProfileWrites>,
    /// The Review Queue, and through it the review requests.
    pub review_queue: Arc<ReviewQueueService>,
    /// The one-step plan path; none where the deployment did not opt in to
    /// storing values without sensitive-data checks.
    pub one_step: Option<Arc<OneStepPath>>,
    pub events: Arc<dyn AdminFactsEvents>,
}

/// A tenant administrator's read, add, edit and delete of a subject's
/// facts, under the permission to administer a subject's facts in the
/// caller's tenant. Personalization off and an erasure under way block an
/// add and an edit, through Admission, but not a read or a delete.
#[domain_model]
pub struct AdminFactsService {
    ports: AdminFactsPorts,
    policy_enforcer: PolicyEnforcer,
}

impl AdminFactsService {
    #[must_use]
    pub fn new(ports: AdminFactsPorts, policy_enforcer: PolicyEnforcer) -> Self {
        Self {
            ports,
            policy_enforcer,
        }
    }

    /// The page of the subject's live facts that `query` asks for, each with
    /// its latest review request. A subject of another tenant, or without a
    /// profile, has none.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] without the permission;
    /// [`DomainError::InvalidQuery`] for a filter, order or cursor the list
    /// does not take; not configured without a processing identity;
    /// unavailable or internal when a dependency fails.
    ///
    /// @cpt-dod:cpt-cf-construct-dod-admin-facts-read:p2
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %subject_id, caller = %ctx.subject_id()))]
    pub async fn read(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
        query: &ODataQuery,
    ) -> Result<SubjectFactsPage, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-admin-facts-read:p2:inst-read-authorize
        let (key, scope) = self.administrator(ctx, subject_id).await?;
        // @cpt-end:cpt-cf-construct-flow-admin-facts-read:p2:inst-read-authorize
        // @cpt-begin:cpt-cf-construct-flow-admin-facts-read:p2:inst-read-facts
        let facts = self.ports.writes.facts(key).await?;
        let Page { items, page_info } = page(facts.facts, query, &SUBJECT_FACTS_LISTING)?;
        let items = with_requests(self.ports.review_queue.requests(), &scope, key, items).await?;
        // @cpt-end:cpt-cf-construct-flow-admin-facts-read:p2:inst-read-facts
        // @cpt-begin:cpt-cf-construct-flow-admin-facts-read:p2:inst-read-event
        self.ports.events.record(AdminFactsEvent::Read {
            tenant_id: key.tenant_id,
            subject_id,
            by: ctx.subject_id(),
            categories: items.iter().map(|item| item.fact.category).collect(),
        });
        // @cpt-end:cpt-cf-construct-flow-admin-facts-read:p2:inst-read-event
        Ok(SubjectFactsPage {
            version: facts.version,
            page: Page { items, page_info },
        })
    }

    /// Add a fact holding `property` with `value` to the subject's profile,
    /// with the administrator as origin; a subject without a profile gets
    /// one.
    ///
    /// NOT cancel-safe: a drop after the writer stored the plan leaves the
    /// fact added without its event.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidValue`] for a property no person type has, or a
    /// value its schema does not take; [`DomainError::FactHeld`] when the
    /// subject holds the value, or a value of a one-valued property,
    /// already; [`DomainError::NotStored`] when the checks or Admission
    /// refuse it; not configured without the opt-in or a processing
    /// identity; unavailable when the profile kept changing, or when the
    /// writer failed and the add may have landed; otherwise as
    /// [`Self::read`].
    ///
    /// @cpt-dod:cpt-cf-construct-dod-admin-facts-change:p2
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %subject_id, caller = %ctx.subject_id()))]
    pub async fn add(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
        property: &str,
        value: &Value,
    ) -> Result<NewFact, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-admin-facts-add:p2:inst-add-authorize
        let deadline = self.ports.writes.deadline();
        let (key, _) =
            ProfileWrites::before_write(deadline, "add", self.administrator(ctx, subject_id))
                .await?;
        let one_step = self.one_step()?;
        // @cpt-end:cpt-cf-construct-flow-admin-facts-add:p2:inst-add-authorize
        // @cpt-begin:cpt-cf-construct-flow-admin-facts-add:p2:inst-add-write
        let by = ctx.subject_id();
        let done = one_step
            .add(
                &self.ports.writes,
                key,
                &Origin::Administrator { administrator: by },
                property,
                value,
                deadline,
            )
            .await
            .map_err(|err| maybe_written(err, "add"))?;
        // @cpt-end:cpt-cf-construct-flow-admin-facts-add:p2:inst-add-write
        // @cpt-begin:cpt-cf-construct-flow-admin-facts-add:p2:inst-add-return
        match done {
            OneStepDone::Stored(fact) => {
                self.ports.events.record(AdminFactsEvent::Added {
                    tenant_id: key.tenant_id,
                    subject_id,
                    by,
                    fact_id: fact.node_key.clone(),
                    category: fact.category,
                });
                Ok(fact)
            }
            OneStepDone::Skipped(Held(held)) => Err(DomainError::FactHeld(held)),
            OneStepDone::NotStored(reason) => Err(self.refused(key, by, None, reason)),
        }
        // @cpt-end:cpt-cf-construct-flow-admin-facts-add:p2:inst-add-return
    }

    /// Store `value` for `property` in place of the subject's fact
    /// `fact_id`, with the administrator as origin; the fact gets a new id.
    /// An open review request on the fact closes as corrected.
    ///
    /// NOT cancel-safe: a drop after the writer stored the plan leaves the
    /// fact edited without its event; a drop during a resolution behaves as
    /// [`ReviewQueueService::resolve`] does.
    ///
    /// # Errors
    ///
    /// [`DomainError::NoSuchFact`] when the subject has no such live fact,
    /// also when it went away meanwhile; [`DomainError::InvalidValue`] for
    /// another property than the fact's, or a value its schema does not
    /// take; [`DomainError::ReviewConflict`] while a reviewer resolves the
    /// fact's request, when an earlier resolution closed it meanwhile, or
    /// when the edit was stored but the request did not close; otherwise as
    /// [`Self::add`].
    ///
    /// @cpt-dod:cpt-cf-construct-dod-admin-facts-review:p2
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %subject_id, caller = %ctx.subject_id(), fact_id = %fact_id.0))]
    pub async fn edit(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
        fact_id: &NodeKey,
        property: &str,
        value: &Value,
    ) -> Result<NewFact, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-admin-facts-edit:p2:inst-edit-read
        let deadline = self.ports.writes.deadline();
        let (key, scope) =
            ProfileWrites::before_write(deadline, "edit", self.administrator(ctx, subject_id))
                .await?;
        let one_step = self.one_step()?;
        let fact = ProfileWrites::before_write(deadline, "edit", self.ports.writes.facts(key))
            .await?
            .facts
            .into_iter()
            .find(|fact| &fact.node_key == fact_id)
            .ok_or_else(|| no_such_fact(fact_id))?;
        if fact.property != property {
            return Err(DomainError::InvalidValue {
                field: "property",
                rule: format!("an edit sets the fact's own property, `{}`", fact.property),
            });
        }
        let by = ctx.subject_id();
        // @cpt-end:cpt-cf-construct-flow-admin-facts-edit:p2:inst-edit-read
        // @cpt-begin:cpt-cf-construct-flow-admin-facts-edit:p2:inst-edit-review
        if let Some(request) = self
            .open_request(&scope, key, fact_id, "edit", deadline)
            .await?
        {
            let settled = self
                .ports
                .review_queue
                .settle(
                    &scope,
                    request.key(),
                    Resolver::Administrator(by),
                    Resolution::Corrected {
                        property: property.to_owned(),
                        value: value.clone(),
                        reason: None,
                    },
                    deadline,
                )
                .await;
            let settled = match settled {
                Ok(settled) => settled,
                Err(DomainError::NotStored(reason)) => {
                    return Err(self.refused(key, by, Some(fact_id), reason));
                }
                Err(err) => return Err(err),
            };
            return match settled.wrote {
                // Stored, and recorded even when the request did not close.
                Wrote::Corrected(new) => {
                    let new = self.edited(key, by, fact_id, new);
                    settled.closed.map(|_| new)
                }
                Wrote::Nothing | Wrote::Removed => {
                    let closed = settled.closed?;
                    match &closed.status {
                        // This edit found the fact gone, and closed the
                        // request as deleted.
                        ReviewStatus::Closed(done)
                            if done.by == by && done.outcome == ReviewOutcome::Deleted =>
                        {
                            Err(no_such_fact(fact_id))
                        }
                        // An earlier resolution's write was in the profile,
                        // and the request closed as that one.
                        _ => Err(DomainError::ReviewConflict(ReviewConflict::Closed(
                            request.id,
                        ))),
                    }
                }
            };
        }
        // @cpt-end:cpt-cf-construct-flow-admin-facts-edit:p2:inst-edit-review
        // @cpt-begin:cpt-cf-construct-flow-admin-facts-edit:p2:inst-edit-write
        let done = one_step
            .replace(
                &self.ports.writes,
                key,
                &Origin::Administrator { administrator: by },
                "edit",
                fact_id,
                &fact.property,
                value,
                deadline,
                |_| ready(Ok(())),
            )
            .await
            .map_err(|err| maybe_written(err, "edit"))?;
        let new = match done {
            OneStepDone::Stored(new) => new,
            OneStepDone::Skipped(Gone) => return Err(no_such_fact(fact_id)),
            OneStepDone::NotStored(reason) => {
                return Err(self.refused(key, by, Some(fact_id), reason));
            }
        };
        // A flag that came in after the check above points at the fact this
        // edit replaced; it closes as corrected unless a reviewer holds it.
        // A failed close is logged: its resolution finds the fact gone.
        if let Err(err) = self
            .ports
            .review_queue
            .close_unclaimed(
                &scope,
                key,
                fact_id,
                by,
                ReviewOutcome::Corrected(new.node_key.clone()),
            )
            .await
        {
            tracing::warn!(
                error = %err,
                fact_id = %fact_id.0,
                "construct: a request flagged during an administrator's edit was not closed"
            );
        }
        Ok(self.edited(key, by, fact_id, new))
        // @cpt-end:cpt-cf-construct-flow-admin-facts-edit:p2:inst-edit-write
    }

    /// Soft-delete the subject's fact `fact_id` as the subject's delete
    /// does, with the administrator as origin. An open review request on the
    /// fact closes as deleted.
    ///
    /// NOT cancel-safe, as [`Self::edit`].
    ///
    /// # Errors
    ///
    /// [`DomainError::NoSuchFact`] when the subject has no such live fact and
    /// no open request on it; [`DomainError::ReviewConflict`] while a
    /// reviewer resolves the fact's request, when an earlier resolution
    /// closed it meanwhile, or when the delete was stored but the request did
    /// not close; unavailable when the profile kept changing, when the
    /// writer failed and the delete may have landed, or when the fact's
    /// request could not be closed, the last two of which a repeat finishes;
    /// otherwise as [`Self::read`].
    ///
    /// @cpt-dod:cpt-cf-construct-dod-admin-facts-delete:p2
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %subject_id, caller = %ctx.subject_id(), fact_id = %fact_id.0))]
    pub async fn delete(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
        fact_id: &NodeKey,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-admin-facts-delete:p2:inst-delete-authorize
        let deadline = self.ports.writes.deadline();
        let (key, scope) =
            ProfileWrites::before_write(deadline, "delete", self.administrator(ctx, subject_id))
                .await?;
        let by = ctx.subject_id();
        // @cpt-end:cpt-cf-construct-flow-admin-facts-delete:p2:inst-delete-authorize
        // @cpt-begin:cpt-cf-construct-flow-admin-facts-delete:p2:inst-delete-review
        if let Some(request) = self
            .open_request(&scope, key, fact_id, "delete", deadline)
            .await?
        {
            let settled = self
                .ports
                .review_queue
                .settle(
                    &scope,
                    request.key(),
                    Resolver::Administrator(by),
                    Resolution::Deleted { reason: None },
                    deadline,
                )
                .await?;
            return match settled.wrote {
                // Removed, and recorded even when the request did not close.
                Wrote::Removed => {
                    self.deleted(key, by, fact_id, request.category);
                    settled.closed.map(|_| ())
                }
                Wrote::Nothing | Wrote::Corrected(_) => {
                    let closed = settled.closed?;
                    match &closed.status {
                        // This delete found the fact gone already, and
                        // closed its request.
                        ReviewStatus::Closed(done)
                            if done.by == by && done.outcome == ReviewOutcome::Deleted =>
                        {
                            Ok(())
                        }
                        // An earlier resolution's write was in the profile,
                        // and the request closed as that one.
                        _ => Err(DomainError::ReviewConflict(ReviewConflict::Closed(
                            request.id,
                        ))),
                    }
                }
            };
        }
        // @cpt-end:cpt-cf-construct-flow-admin-facts-delete:p2:inst-delete-review
        // @cpt-begin:cpt-cf-construct-flow-admin-facts-delete:p2:inst-delete-write
        // A request opened after the check above, or left open by an
        // earlier delete whose close failed, closes here.
        FactDelete {
            writes: &self.ports.writes,
            requests: self.ports.review_queue.requests(),
            events: self.ports.review_queue.events(),
            scope: &scope,
            key,
            by,
            origin: Origin::Administrator { administrator: by },
            deadline,
        }
        .run(fact_id, |fact| {
            self.deleted(key, by, &fact.node_key, fact.category);
        })
        .await
        // @cpt-end:cpt-cf-construct-flow-admin-facts-delete:p2:inst-delete-write
    }

    /// The subject's key in the caller's tenant, and the platform's scope for
    /// administering the tenant's subjects' facts.
    async fn administrator(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
    ) -> Result<(SubjectKey, AccessScope), DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-admin-facts-admin-only:p2:inst-admin-permission
        let tenant_id = ctx.subject_tenant_id();
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &SUBJECT_FACTS_RESOURCE,
                actions::ADMINISTER,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, tenant_id)
                    .require_constraints(true),
            )
            .await?;
        if !reaches_tenant(&scope, tenant_id) {
            return Err(DomainError::forbidden(format!(
                "the policy scope for administering facts does not reach tenant {tenant_id}"
            )));
        }
        Ok((
            SubjectKey {
                tenant_id,
                subject_id,
            },
            scope,
        ))
        // @cpt-end:cpt-cf-construct-algo-admin-facts-admin-only:p2:inst-admin-permission
    }

    /// The one-step plan path, where the deployment opted in to it.
    fn one_step(&self) -> Result<&OneStepPath, DomainError> {
        self.ports.one_step.as_deref().ok_or_else(|| {
            DomainError::NotConfigured(
                "an administrator's add or edit needs \
                 processing.run_without_sensitive_data_checks while no sensitive-data check \
                 exists"
                    .to_owned(),
            )
        })
    }

    /// The open review request on the subject's fact, if there is one, read
    /// before `deadline`, the budget of the `what` it is read for.
    async fn open_request(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
        fact_id: &NodeKey,
        what: &str,
        deadline: Instant,
    ) -> Result<Option<ReviewRequest>, DomainError> {
        let latest =
            self.ports
                .review_queue
                .requests()
                .latest_on(scope, key, std::slice::from_ref(fact_id));
        Ok(ProfileWrites::before_write(deadline, what, latest)
            .await?
            .into_iter()
            .find(ReviewRequest::is_open))
    }

    /// Record a delete the administrator made.
    fn deleted(&self, key: SubjectKey, by: Uuid, fact_id: &NodeKey, category: Category) {
        self.ports.events.record(AdminFactsEvent::Deleted {
            tenant_id: key.tenant_id,
            subject_id: key.subject_id,
            by,
            fact_id: fact_id.clone(),
            category,
        });
    }

    /// Record a stored edit, and give the new fact back.
    fn edited(&self, key: SubjectKey, by: Uuid, fact_id: &NodeKey, new: NewFact) -> NewFact {
        self.ports.events.record(AdminFactsEvent::Edited {
            tenant_id: key.tenant_id,
            subject_id: key.subject_id,
            by,
            fact_id: fact_id.clone(),
            new_fact_id: new.node_key.clone(),
            category: new.category,
        });
        new
    }

    /// Record a refused add or edit, and the error that answers it.
    fn refused(
        &self,
        key: SubjectKey,
        by: Uuid,
        fact_id: Option<&NodeKey>,
        reason: NotStored,
    ) -> DomainError {
        self.ports.events.record(AdminFactsEvent::Refused {
            tenant_id: key.tenant_id,
            subject_id: key.subject_id,
            by,
            fact_id: fact_id.cloned(),
            reason,
        });
        DomainError::NotStored(reason)
    }
}

/// A write error as the domain error that answers it: a write that may have
/// landed is unavailable, and its repeat reads the facts first.
fn maybe_written(err: WriteError, what: &str) -> DomainError {
    match err {
        WriteError::NotWritten(err) => err,
        WriteError::MaybeWritten(err) => DomainError::Unavailable(format!(
            "the {what} may have landed; read the subject's facts before sending it again: {err}"
        )),
    }
}

fn no_such_fact(fact_id: &NodeKey) -> DomainError {
    DomainError::NoSuchFact(format!("the subject has no live fact {}", fact_id.0))
}
