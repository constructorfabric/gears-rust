//! Subject Control: the subject's own view of all its facts, one fact, the
//! delete of one fact, and the export of a subject's data for the data
//! protection officer.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::AccessRequest;
use time::OffsetDateTime;
use tokio::time::Instant;
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
use super::profile_read::{OriginKind, StoredFact, granted, settings_scope};
use super::subject_settings::{SubjectKey, SubjectSettings, SubjectSettingsReader};

pub(crate) mod actions {
    /// The subject reads its own facts.
    pub const READ: &str = "read";
    /// The subject deletes one of its own facts.
    pub const DELETE: &str = "delete";
    /// The data protection officer exports a subject's data.
    pub const EXPORT: &str = "export";
}

/// How long one delete may take, its reruns and their pauses included; well
/// below the gateway's request timeout, so the caller hears how it ended.
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

/// The name of an origin in the list's `origin` field.
#[must_use]
pub fn origin_name(origin: OriginKind) -> &'static str {
    match origin {
        OriginKind::Connector => "connector",
        OriginKind::Agent => "agent",
        OriginKind::Reviewer => "reviewer",
        OriginKind::Administrator => "administrator",
    }
}

impl Listed for StoredFact {
    type Field = SubjectFactField;

    fn key(&self, field: SubjectFactField) -> Option<Key> {
        Some(match field {
            SubjectFactField::FactId => Key::text(self.node_key.0.clone()),
            SubjectFactField::EntityKind => Key::text(self.category.name()),
            SubjectFactField::Origin => Key::text(origin_name(self.origin)),
            SubjectFactField::StoredAt => Key::time(self.stored_at),
        })
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
    pub page: Page<StoredFact>,
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

/// A subject's data as the export gives it: the settings and every live
/// fact.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct SubjectExport {
    pub tenant_id: Uuid,
    pub subject_id: Uuid,
    pub settings: SubjectSettings,
    pub facts: Vec<StoredFact>,
}

/// What Subject Control did, for the audit log. No event holds a value.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubjectControlEvent {
    /// The subject was served its own facts, a page of them or one.
    Viewed {
        tenant_id: Uuid,
        subject_id: Uuid,
        categories: BTreeSet<Category>,
    },
    /// The subject deleted one of its facts.
    Deleted {
        tenant_id: Uuid,
        subject_id: Uuid,
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
        let page = page(facts.facts, query, &SUBJECT_FACTS_LISTING)?;
        // @cpt-end:cpt-cf-construct-flow-subject-control-view:p1:inst-view-read
        // @cpt-begin:cpt-cf-construct-flow-subject-control-view:p1:inst-view-event
        self.ports.events.record(SubjectControlEvent::Viewed {
            tenant_id: key.tenant_id,
            subject_id,
            categories: categories_of(&page.items),
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
    ) -> Result<StoredFact, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-subject-control-fact:p1:inst-fact-read
        let key = self.own(ctx, subject_id, actions::READ).await?;
        let fact = self
            .facts(key)
            .await?
            .facts
            .into_iter()
            .find(|fact| &fact.node_key == fact_id)
            .ok_or_else(|| no_such_fact(fact_id))?;
        // @cpt-end:cpt-cf-construct-flow-subject-control-fact:p1:inst-fact-read
        // @cpt-begin:cpt-cf-construct-flow-subject-control-fact:p1:inst-fact-event
        self.ports.events.record(SubjectControlEvent::Viewed {
            tenant_id: key.tenant_id,
            subject_id,
            categories: BTreeSet::from([fact.category]),
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
    /// fact deleted without its delete event. The budget keeps the delete
    /// well inside the gateway's timeout, so a drop comes only from the
    /// caller going away.
    ///
    /// # Errors
    ///
    /// [`DomainError::NoSuchFact`] when the subject has no such live fact,
    /// also when a concurrent write removed it before a rerun; unavailable
    /// when the profile kept changing past the reruns or the budget;
    /// otherwise as [`Self::view`].
    ///
    /// @cpt-dod:cpt-cf-construct-dod-subject-control-delete:p1
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %subject_id, fact_id = %fact_id.0))]
    pub async fn delete(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
        fact_id: &NodeKey,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-own
        let deadline = Instant::now() + self.budget;
        let key = self.own(ctx, subject_id, actions::DELETE).await?;
        let (construct, _) = self.ports.contexts.for_tenant(key.tenant_id).await?;
        // @cpt-end:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-own
        for attempt in 0..=self.max_reruns {
            // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-read
            let current = self.ports.profiles.facts(&construct, key).await?;
            let fact = current
                .facts
                .iter()
                .find(|fact| &fact.node_key == fact_id)
                .ok_or_else(|| no_such_fact(fact_id))?;
            // @cpt-end:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-read
            // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-write
            let plan = removal(key, current.version, fact);
            let written = self.ports.writer.write(&construct, &plan).await?;
            // @cpt-end:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-write
            // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-stored
            match written {
                Written::Stored => {
                    self.ports.events.record(SubjectControlEvent::Deleted {
                        tenant_id: key.tenant_id,
                        subject_id,
                        fact_id: fact.node_key.clone(),
                        category: fact.category,
                    });
                    return Ok(());
                }
                Written::Conflict => {
                    tracing::debug!(
                        attempt,
                        version = ?current.version,
                        "construct: the profile changed under a delete; reading it again"
                    );
                    let pause = rerun_pause(attempt);
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
            "construct: a delete gave up; the profile kept changing"
        );
        Err(DomainError::Unavailable(format!(
            "the profile kept changing; fact {} was not deleted",
            fact_id.0
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
        })
    }

    /// The subject's key in the caller's tenant, when the caller is the
    /// subject and the platform allows `action` on its facts.
    async fn own(
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

    /// The subject's live facts, read as Construct.
    async fn facts(&self, key: SubjectKey) -> Result<SubjectFacts, DomainError> {
        let (construct, _) = self.ports.contexts.for_tenant(key.tenant_id).await?;
        self.ports.profiles.facts(&construct, key).await
    }
}

/// The subject's delete of `fact` as a plan: one step that removes it, with
/// the subject as origin, written against `version`. The only plan with the
/// subject as origin, and it stores nothing.
fn removal(key: SubjectKey, version: Option<u64>, fact: &StoredFact) -> Plan {
    Plan {
        tenant_id: key.tenant_id,
        subject_id: key.subject_id,
        origin: Origin::Subject {
            subject: key.subject_id,
        },
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
    let base = 20_u64.saturating_mul(1 << attempt.min(6));
    Duration::from_millis(base + rand::random_range(0..=base))
}

fn categories_of(facts: &[StoredFact]) -> BTreeSet<Category> {
    facts.iter().map(|fact| fact.category).collect()
}

fn no_such_fact(fact_id: &NodeKey) -> DomainError {
    DomainError::NoSuchFact(format!("the subject has no live fact {}", fact_id.0))
}
