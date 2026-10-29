//! Subject Control: the subject's own view of all its facts, one fact, the
//! delete of one fact, and the export of a subject's data for the data
//! protection officer.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::AccessRequest;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use super::error::DomainError;
use super::plan::{Change, NodeKey, Origin, Plan, Step};
use super::planner::manage::SUBJECT_FACTS_RESOURCE;
use super::planner::processing::{PlanWriter, ProcessingContexts, Written};
use super::profile::Category;
use super::profile_read::{StoredFact, granted, read_failure, settings_scope};
use super::subject_settings::{SubjectKey, SubjectSettings, SubjectSettingsReader};

pub(crate) mod actions {
    /// The subject reads its own facts.
    pub const READ: &str = "read";
    /// The subject deletes one of its own facts.
    pub const DELETE: &str = "delete";
    /// The data protection officer exports a subject's data.
    pub const EXPORT: &str = "export";
}

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
    /// The errors of the profile store.
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
    /// The subject read its own facts, all of them or one.
    Viewed {
        tenant_id: Uuid,
        subject_id: Uuid,
        categories: BTreeSet<Category>,
    },
    /// The subject deleted one of its facts.
    Deleted {
        tenant_id: Uuid,
        subject_id: Uuid,
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
        }
    }

    /// Every live fact of the subject, with its profile's version.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the caller is not the subject or the
    /// platform refuses; not configured without a processing identity;
    /// unavailable or internal when a dependency fails.
    ///
    /// @cpt-dod:cpt-cf-construct-dod-subject-control-view:p1
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %subject_id))]
    pub async fn view(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
    ) -> Result<SubjectFacts, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-subject-control-view:p1:inst-view-read
        let key = self.own(ctx, subject_id, actions::READ).await?;
        let facts = self.facts(key).await?;
        // @cpt-end:cpt-cf-construct-flow-subject-control-view:p1:inst-view-read
        // @cpt-begin:cpt-cf-construct-flow-subject-control-view:p1:inst-view-event
        self.ports.events.record(SubjectControlEvent::Viewed {
            tenant_id: key.tenant_id,
            subject_id,
            categories: categories_of(&facts.facts),
        });
        // @cpt-end:cpt-cf-construct-flow-subject-control-view:p1:inst-view-event
        Ok(facts)
    }

    /// One live fact of the subject.
    ///
    /// # Errors
    ///
    /// [`DomainError::NotVisible`] when the subject has no such live fact;
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
    /// on a conflict, at most `max_reruns` times.
    ///
    /// # Errors
    ///
    /// [`DomainError::NotVisible`] when the subject has no such live fact;
    /// unavailable when the profile kept changing past the reruns; otherwise
    /// as [`Self::view`].
    ///
    /// @cpt-dod:cpt-cf-construct-dod-subject-control-delete:p1
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %subject_id))]
    pub async fn delete(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
        fact_id: &NodeKey,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-own
        let key = self.own(ctx, subject_id, actions::DELETE).await?;
        let (construct, _) = self.ports.contexts.for_tenant(key.tenant_id).await?;
        // @cpt-end:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-own
        for _ in 0..=self.max_reruns {
            // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-read
            let current = self.ports.profiles.facts(&construct, key).await?;
            let fact = current
                .facts
                .iter()
                .find(|fact| &fact.node_key == fact_id)
                .ok_or_else(|| no_such_fact(fact_id))?;
            // @cpt-end:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-read
            // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-write
            let plan = Plan {
                tenant_id: key.tenant_id,
                subject_id,
                origin: Origin::Subject {
                    subject: subject_id,
                },
                profile_version: current.version,
                steps: vec![Step {
                    entity_kind: fact.category.name().to_owned(),
                    change: Change::RemoveEntity {
                        entity: fact.node_key.clone(),
                    },
                    confidence: None,
                }],
            };
            let written = self.ports.writer.write(&construct, &plan).await?;
            // @cpt-end:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-write
            // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-stored
            if written == Written::Stored {
                self.ports.events.record(SubjectControlEvent::Deleted {
                    tenant_id: key.tenant_id,
                    subject_id,
                    category: fact.category,
                });
                return Ok(());
            }
            // @cpt-end:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-stored
        }
        // @cpt-begin:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-give-up
        Err(DomainError::Unavailable(format!(
            "the profile kept changing; fact {} was not deleted after {} reruns",
            fact_id.0, self.max_reruns
        )))
        // @cpt-end:cpt-cf-construct-algo-subject-control-delete:p1:inst-del-give-up
    }

    /// A subject's data, for the data protection officer of the caller's
    /// tenant: the settings and every live fact. A subject of another tenant
    /// has nothing here, as one with no profile.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the platform refuses the export; not
    /// configured without a processing identity; unavailable or internal
    /// when a dependency fails.
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
        let settings = self
            .ports
            .settings
            .read(ctx, &settings_scope(&scope), key)
            .await
            .map_err(read_failure)?;
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

fn categories_of(facts: &[StoredFact]) -> BTreeSet<Category> {
    facts.iter().map(|fact| fact.category).collect()
}

fn no_such_fact(fact_id: &NodeKey) -> DomainError {
    DomainError::NotVisible(format!("the subject has no live fact {}", fact_id.0))
}
