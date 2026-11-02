//! Subject Control: the subject's own view of all its facts, one fact, the
//! delete of one fact, and the export of a subject's data for the data
//! protection officer.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::error::DomainError;
use super::pagination::{Key, Listed, Listing, page};
use super::plan::{NodeKey, Origin};
use super::profile::Category;
use super::profile_read::{StoredFact, settings_scope};
use super::profile_writes::{ProfileWrites, Removed, WriteError};
use super::review_queue::{
    ReviewOutcome, ReviewQueueEvents, ReviewRequest, ReviewRequests, close_on_change,
};
use super::subject_access::{authorize, own};
use super::subject_settings::{SubjectKey, SubjectSettings, SubjectSettingsReader};

pub(crate) mod actions {
    /// The subject reads its own facts.
    pub const READ: &str = "read";
    /// The subject deletes one of its own facts.
    pub const DELETE: &str = "delete";
    /// The data protection officer exports a subject's data.
    pub const EXPORT: &str = "export";
}

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

/// One page of the subject's facts, and the version of the profile it was
/// cut from.
#[domain_model]
#[derive(Debug, Clone)]
pub struct SubjectFactsPage {
    pub version: Option<u64>,
    pub page: Page<SubjectFact>,
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
    pub writes: Arc<ProfileWrites>,
    pub settings: Arc<dyn SubjectSettingsReader>,
    pub events: Arc<dyn SubjectControlEvents>,
    pub reviews: Arc<dyn ReviewRequests>,
    pub review_events: Arc<dyn ReviewQueueEvents>,
}

/// The subject's own view, one fact and delete, owner only, and the export
/// under its own permission. Personalization off and an erasure under way
/// block none of them.
#[domain_model]
pub struct SubjectControlService {
    ports: SubjectControlPorts,
    policy_enforcer: PolicyEnforcer,
}

impl SubjectControlService {
    #[must_use]
    pub fn new(ports: SubjectControlPorts, policy_enforcer: PolicyEnforcer) -> Self {
        Self {
            ports,
            policy_enforcer,
        }
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
        let (key, scope) = own(&self.policy_enforcer, ctx, subject_id, actions::READ).await?;
        let facts = self.ports.writes.facts(key).await?;
        let Page { items, page_info } = page(facts.facts, query, &SUBJECT_FACTS_LISTING)?;
        let items = with_requests(self.ports.reviews.as_ref(), &scope, key, items).await?;
        // @cpt-end:cpt-cf-construct-flow-subject-control-view:p1:inst-view-read
        // @cpt-begin:cpt-cf-construct-flow-subject-control-view:p1:inst-view-event
        self.ports.events.record(SubjectControlEvent::Viewed {
            tenant_id: key.tenant_id,
            subject_id,
            caller: ctx.subject_id(),
            categories: items.iter().map(|item| item.fact.category).collect(),
        });
        // @cpt-end:cpt-cf-construct-flow-subject-control-view:p1:inst-view-event
        Ok(SubjectFactsPage {
            version: facts.version,
            page: Page { items, page_info },
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
        let (key, scope) = own(&self.policy_enforcer, ctx, subject_id, actions::READ).await?;
        let fact = self
            .ports
            .writes
            .facts(key)
            .await?
            .facts
            .into_iter()
            .find(|fact| &fact.node_key == fact_id)
            .ok_or_else(|| no_such_fact(fact_id))?;
        let review_request = self
            .ports
            .reviews
            .latest_on(&scope, key, std::slice::from_ref(fact_id))
            .await?
            .pop();
        // @cpt-end:cpt-cf-construct-flow-subject-control-fact:p1:inst-fact-read
        // @cpt-begin:cpt-cf-construct-flow-subject-control-fact:p1:inst-fact-event
        self.ports.events.record(SubjectControlEvent::Viewed {
            tenant_id: key.tenant_id,
            subject_id,
            caller: ctx.subject_id(),
            categories: BTreeSet::from([fact.category]),
        });
        // @cpt-end:cpt-cf-construct-flow-subject-control-fact:p1:inst-fact-event
        Ok(SubjectFact {
            fact,
            review_request,
        })
    }

    /// Soft-delete one live fact of the subject: a plan with one removal,
    /// written against the profile's version, and read and written again
    /// after a short pause on a conflict, within the rerun limit and the
    /// budget. An open review request on the fact closes as deleted.
    ///
    /// NOT cancel-safe: a drop after the writer stored the plan leaves the
    /// fact deleted without its delete event, and its review request open
    /// until the delete is sent again. The budget bounds only the steps
    /// before a write: the permission check, the context, the reads and the
    /// pauses. So it never drops a started write; a drop comes only from the
    /// caller going away.
    ///
    /// # Errors
    ///
    /// [`DomainError::NoSuchFact`] when the subject has no such live fact,
    /// also when a concurrent write removed it before a rerun; unavailable
    /// when the profile kept changing past the reruns or the budget, when the
    /// writer failed and the fact is not seen gone after it, or when the
    /// fact's review request could not be closed, the last two of which a
    /// repeat of the delete finishes; otherwise as [`Self::view`].
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
        let deadline = self.ports.writes.deadline();
        // The permission check and the context write nothing, so the budget
        // may cut them off, like a read.
        let (key, scope, construct) = ProfileWrites::before_write(deadline, "delete", async {
            let (key, scope) = own(&self.policy_enforcer, ctx, subject_id, actions::DELETE).await?;
            let (construct, _) = self.ports.writes.construct(key.tenant_id).await?;
            Ok((key, scope, construct))
        })
        .await?;
        // @cpt-end:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-own
        let origin = Origin::Subject {
            subject: subject_id,
        };
        let removed = self
            .ports
            .writes
            .remove(&construct, key, fact_id, &origin, deadline)
            .await
            .map_err(|err| match err {
                WriteError::NotWritten(err) => err,
                // The read after the failure did not find the fact gone, so
                // its request stays open: a repeat deletes the fact or finds
                // it gone, and closes the request.
                WriteError::MaybeWritten(err) => DomainError::Unavailable(format!(
                    "the delete of fact {} may have landed; send it again: {err}",
                    fact_id.0
                )),
            })?;
        if let Removed::Removed(fact) = &removed {
            self.ports.events.record(SubjectControlEvent::Deleted {
                tenant_id: key.tenant_id,
                subject_id,
                caller: ctx.subject_id(),
                fact_id: fact.node_key.clone(),
                category: fact.category,
            });
        }
        // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-close
        // A fact already gone may still have an open request left by an
        // earlier delete whose close failed; this closes it.
        let closed = close_on_change(
            self.ports.reviews.as_ref(),
            self.ports.review_events.as_ref(),
            &self.ports.writes,
            &scope,
            key,
            fact_id,
            subject_id,
            ReviewOutcome::Deleted,
            matches!(removed, Removed::Removed(_)),
        )
        .await
        .map_err(|err| match err {
            // A store that did not answer: a repeat finds the fact gone and
            // closes the request.
            DomainError::Unavailable(_) | DomainError::Database(_) => {
                DomainError::Unavailable(format!(
                    "fact {} is deleted, but its review request was not closed; send the \
                     delete again: {err}",
                    fact_id.0
                ))
            }
            other => other,
        })?;
        match removed {
            Removed::Removed(_) => Ok(()),
            Removed::Gone if closed => Ok(()),
            Removed::Gone => Err(no_such_fact(fact_id)),
        }
        // @cpt-end:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-close
    }

    /// A subject's data, for the data protection officer of the caller's
    /// tenant: the settings, every live fact and every review request. A
    /// subject of another tenant has nothing here, as one with no profile.
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
        let scope = authorize(&self.policy_enforcer, ctx, key, actions::EXPORT).await?;
        // @cpt-end:cpt-cf-construct-flow-subject-control-export:p1:inst-export-authorize
        // @cpt-begin:cpt-cf-construct-flow-subject-control-export:p1:inst-export-read
        // A refusal of the settings stays a refusal: the export answers it
        // as one, not as a missing subject.
        let settings = self
            .ports
            .settings
            .read(ctx, &settings_scope(&scope), key)
            .await?;
        let facts = self.ports.writes.facts(key).await?.facts;
        let review_requests = self.ports.reviews.of_subject(&scope, key).await?;
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
}

/// `facts`, each with its latest review request.
pub(crate) async fn with_requests(
    reviews: &dyn ReviewRequests,
    scope: &AccessScope,
    key: SubjectKey,
    facts: Vec<StoredFact>,
) -> Result<Vec<SubjectFact>, DomainError> {
    let ids: Vec<NodeKey> = facts.iter().map(|fact| fact.node_key.clone()).collect();
    let mut latest: BTreeMap<NodeKey, ReviewRequest> = reviews
        .latest_on(scope, key, &ids)
        .await?
        .into_iter()
        .map(|request| (request.fact_id.clone(), request))
        .collect();
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

fn categories_of(facts: &[StoredFact]) -> BTreeSet<Category> {
    facts.iter().map(|fact| fact.category).collect()
}

fn no_such_fact(fact_id: &NodeKey) -> DomainError {
    DomainError::NoSuchFact(format!("the subject has no live fact {}", fact_id.0))
}
