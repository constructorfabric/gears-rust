//! The profile read for applications and agents: a subject's facts by
//! category, for a caller the platform lets act for the subject.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::models::TenantMode;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use authz_resolver_sdk::{EnforcerError, PolicyEnforcer};
use futures::future::join_all;
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, ScopeConstraint, SecurityContext, pep_properties};
use uuid::Uuid;

use super::error::DomainError;
use super::plan::NodeKey;
use super::planner::processing::ProcessingContexts;
use super::profile::Category;
use super::subject_settings::{SubjectKey, SubjectSettingsReader};

/// The policy property that names a profile category.
pub const CATEGORY_PROPERTY: &str = "category";

/// Authorization resource type for a subject's profile, scoped by tenant, by
/// subject and by category.
pub(crate) const PROFILE_RESOURCE: ResourceType = ResourceType::from_static(
    "construct.profile",
    &[
        pep_properties::OWNER_TENANT_ID,
        pep_properties::RESOURCE_ID,
        CATEGORY_PROPERTY,
    ],
);

pub(crate) mod actions {
    pub const READ: &str = "read";
}

/// Who a stored fact came from.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginKind {
    Connector,
    Agent,
    Reviewer,
    Administrator,
}

impl OriginKind {
    /// The name of the kind on the wire.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Connector => "connector",
            Self::Agent => "agent",
            Self::Reviewer => "reviewer",
            Self::Administrator => "administrator",
        }
    }
}

/// One stored fact as a reader sees it: its key, category, property and value,
/// who it came from, when it was stored, and when its source asserted it.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct StoredFact {
    pub node_key: NodeKey,
    pub category: Category,
    pub property: String,
    pub value: Value,
    pub origin: OriginKind,
    pub stored_at: OffsetDateTime,
    pub produced_at: Option<OffsetDateTime>,
}

/// The facts of one category.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct CategoryFacts {
    pub category: Category,
    pub facts: Vec<StoredFact>,
}

/// A subject's profile as an application reads it: the categories with facts
/// the caller may read. A category it may not read, or without facts, is not
/// listed, so the two look the same.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileView {
    pub subject_id: Uuid,
    pub categories: Vec<CategoryFacts>,
}

/// Reads a subject's stored facts, with their origin and times.
#[async_trait]
pub trait StoredProfiles: Send + Sync {
    /// # Errors
    ///
    /// When the profile cannot be read.
    async fn stored(
        &self,
        ctx: &SecurityContext,
        key: SubjectKey,
    ) -> Result<Vec<StoredFact>, DomainError>;
}

/// A content-free event for one profile read: who read which subject's
/// categories, in which tenant.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileReadEvent {
    pub tenant_id: Uuid,
    pub subject_id: Uuid,
    pub caller: Uuid,
    pub categories: Vec<Category>,
}

/// Where profile read events go. The Audit gear takes them once it ships.
pub trait ProfileEvents: Send + Sync {
    fn read(&self, event: ProfileReadEvent);
}

/// How the readable categories are asked of the platform.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CategoryCheck {
    /// One policy call, whose scope names the readable categories. A scope
    /// that names none lets every category be read.
    #[default]
    OneCall,
    /// One policy call per category, for a policy plugin that cannot answer
    /// with a category constraint.
    PerCategory,
}

/// The collaborators of the profile read.
#[domain_model]
pub struct ProfileReadPorts {
    pub settings: Arc<dyn SubjectSettingsReader>,
    pub profiles: Arc<dyn StoredProfiles>,
    pub contexts: Arc<dyn ProcessingContexts>,
    pub events: Arc<dyn ProfileEvents>,
}

/// @cpt-dod:cpt-cf-construct-dod-profile-reader-read:p1
#[domain_model]
pub struct ProfileReadService {
    ports: ProfileReadPorts,
    policy_enforcer: PolicyEnforcer,
    check: CategoryCheck,
}

/// The request for the profile of `subject_id` in `tenant_id`, with the
/// category when one is asked about.
fn profile_request(tenant_id: Uuid, category: Option<Category>) -> AccessRequest {
    let request = AccessRequest::new()
        .context_tenant_id(tenant_id)
        .tenant_mode(TenantMode::RootOnly)
        .resource_property(pep_properties::OWNER_TENANT_ID, tenant_id);
    match category {
        Some(category) => request.resource_property(CATEGORY_PROPERTY, category.name()),
        None => request,
    }
}

/// What one constraint of a policy scope grants on the profile of `subject_id`
/// in `tenant_id`: nothing when it does not reach them, otherwise the
/// categories it names, or every category when it names none. A filter that
/// cannot be decided in memory (a group or tenant-subtree filter) grants
/// nothing, and so does a filter on a property the profile has not.
fn granted_by(
    constraint: &ScopeConstraint,
    tenant_id: Uuid,
    subject_id: Uuid,
) -> Option<HashSet<Category>> {
    let mut named: Option<HashSet<Category>> = None;
    for filter in constraint.filters() {
        if !filter.is_representable_in_memory() {
            return None;
        }
        let values = filter.values();
        let holds = |id: Uuid| values.iter().any(|value| value.as_uuid() == Some(id));
        match filter.property() {
            pep_properties::OWNER_TENANT_ID if !holds(tenant_id) => return None,
            pep_properties::RESOURCE_ID if !holds(subject_id) => return None,
            pep_properties::OWNER_TENANT_ID | pep_properties::RESOURCE_ID => {}
            CATEGORY_PROPERTY => {
                let these: HashSet<Category> = Category::ALL
                    .into_iter()
                    .filter(|category| {
                        values
                            .iter()
                            .any(|value| value.to_string() == category.name())
                    })
                    .collect();
                named = Some(match named {
                    Some(before) => before.intersection(&these).copied().collect(),
                    None => these,
                });
            }
            _ => return None,
        }
    }
    Some(named.unwrap_or_else(|| Category::ALL.into_iter().collect()))
}

/// What a policy scope grants on the profile of `subject_id` in `tenant_id`:
/// nothing when no constraint reaches them, otherwise the categories of the
/// constraints that do. Each constraint is one path to the profile, so the
/// tenant, the subject and the categories must all come from the same one.
fn granted(scope: &AccessScope, tenant_id: Uuid, subject_id: Uuid) -> Option<HashSet<Category>> {
    if scope.is_unconstrained() {
        return Some(Category::ALL.into_iter().collect());
    }
    scope
        .constraints()
        .iter()
        .filter_map(|constraint| granted_by(constraint, tenant_id, subject_id))
        .reduce(|mut all, these| {
            all.extend(these);
            all
        })
}

/// A refused or uncompilable policy answer as a subject that is not visible;
/// any other failure as the gear's own error.
fn not_visible(err: EnforcerError) -> DomainError {
    match err {
        EnforcerError::Denied { .. } | EnforcerError::CompileFailed(_) => {
            DomainError::NotVisible("the platform refuses the read".to_owned())
        }
        other @ EnforcerError::EvaluationFailed(_) => other.into(),
    }
}

impl ProfileReadService {
    #[must_use]
    pub fn new(
        ports: ProfileReadPorts,
        policy_enforcer: PolicyEnforcer,
        check: CategoryCheck,
    ) -> Self {
        Self {
            ports,
            policy_enforcer,
            check,
        }
    }

    /// The profile of `subject_id`, for a caller the platform lets act for
    /// the subject. The tenant is the one named, or the caller's own.
    ///
    /// # Errors
    ///
    /// [`DomainError::NotVisible`] when the caller may not act for the subject
    /// in the tenant, which both transports answer like a subject that does
    /// not exist. Unavailable or internal when a dependency fails.
    #[tracing::instrument(skip_all, fields(%subject_id, caller = %ctx.subject_id()))]
    pub async fn read(
        &self,
        ctx: &SecurityContext,
        tenant: Option<Uuid>,
        subject_id: Uuid,
    ) -> Result<ProfileView, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-authorize
        let tenant_id = tenant.unwrap_or_else(|| ctx.subject_tenant_id());
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &PROFILE_RESOURCE,
                actions::READ,
                Some(subject_id),
                &profile_request(tenant_id, None),
            )
            .await
            .map_err(not_visible)?;
        let Some(granted) = granted(&scope, tenant_id, subject_id) else {
            return Err(DomainError::NotVisible(
                "the policy scope does not reach the subject".to_owned(),
            ));
        };
        // @cpt-end:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-authorize

        // @cpt-begin:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-categories
        let readable = match self.check {
            CategoryCheck::OneCall => granted,
            CategoryCheck::PerCategory => {
                self.readable_one_by_one(ctx, tenant_id, subject_id).await?
            }
        };
        // @cpt-end:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-categories

        let categories = self
            .facts(ctx, &scope, tenant_id, subject_id, &readable)
            .await?;

        // @cpt-begin:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-event
        self.ports.events.read(ProfileReadEvent {
            tenant_id,
            subject_id,
            caller: ctx.subject_id(),
            categories: categories.iter().map(|group| group.category).collect(),
        });
        Ok(ProfileView {
            subject_id,
            categories,
        })
        // @cpt-end:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-event
    }

    /// The categories one policy call each allows. The calls run at the same
    /// time; a failure other than a refusal fails the read rather than
    /// hiding the category.
    async fn readable_one_by_one(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        subject_id: Uuid,
    ) -> Result<HashSet<Category>, DomainError> {
        let decisions = join_all(Category::ALL.into_iter().map(|category| async move {
            let request = profile_request(tenant_id, Some(category));
            let decided = self
                .policy_enforcer
                .access_scope_with(
                    ctx,
                    &PROFILE_RESOURCE,
                    actions::READ,
                    Some(subject_id),
                    &request,
                )
                .await;
            (category, decided)
        }))
        .await;
        let mut readable = HashSet::new();
        for (category, decided) in decisions {
            match decided {
                Ok(scope) => {
                    if granted(&scope, tenant_id, subject_id)
                        .is_some_and(|categories| categories.contains(&category))
                    {
                        readable.insert(category);
                    }
                }
                Err(EnforcerError::Denied { .. }) => {}
                Err(err) => return Err(err.into()),
            }
        }
        Ok(readable)
    }

    /// The readable facts by category, or none while personalization is off
    /// or an erasure is under way.
    async fn facts(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        tenant_id: Uuid,
        subject_id: Uuid,
        readable: &HashSet<Category>,
    ) -> Result<Vec<CategoryFacts>, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-settings
        let settings = self
            .ports
            .settings
            .read(
                ctx,
                &scope.tenant_only(),
                SubjectKey {
                    tenant_id,
                    subject_id,
                },
            )
            .await?;
        if !settings.personalization_enabled || settings.erasure_in_progress || readable.is_empty()
        {
            return Ok(Vec::new());
        }
        // @cpt-end:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-settings

        // @cpt-begin:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-facts
        // The facts are Construct's own, so graph storage is read as
        // Construct, not as the caller.
        let (construct, _) = self.ports.contexts.for_tenant(tenant_id).await?;
        let stored = self
            .ports
            .profiles
            .stored(
                &construct,
                SubjectKey {
                    tenant_id,
                    subject_id,
                },
            )
            .await?;
        let mut groups: Vec<CategoryFacts> = Category::ALL
            .into_iter()
            .filter(|category| readable.contains(category))
            .map(|category| CategoryFacts {
                category,
                facts: Vec::new(),
            })
            .collect();
        for fact in stored {
            if let Some(group) = groups
                .iter_mut()
                .find(|group| group.category == fact.category)
            {
                group.facts.push(fact);
            }
        }
        groups.retain(|group| !group.facts.is_empty());
        Ok(groups)
        // @cpt-end:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-facts
    }
}
