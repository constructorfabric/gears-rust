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
use toolkit_security::{AccessScope, ScopeConstraint, ScopeValue, SecurityContext, pep_properties};
use uuid::Uuid;

use super::error::DomainError;
use super::plan::NodeKey;
use super::planner::processing::ProcessingContexts;
use super::profile::Category;
use super::subject_settings::{SubjectKey, SubjectSettingsReader};

/// The policy property that names a profile category.
pub const CATEGORY_PROPERTY: &str = "category";

/// The GTS type a policy names a subject's profile by.
pub(crate) const PROFILE_TYPE: &str = toolkit_gts::gts_id!("cf.construct.subject.profile.v1~");

/// Authorization resource type for a subject's profile, scoped by tenant, by
/// subject and by category.
pub(crate) const PROFILE_RESOURCE: ResourceType = ResourceType::from_static(
    PROFILE_TYPE,
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

/// How the readable categories are asked of the platform. It has no
/// default, because the two differ in what an unconstrained answer grants.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryCheck {
    /// One policy call, whose scope names the readable categories. A scope
    /// that names none lets every category be read.
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

/// The request for the profile of the subject at `key`, with the category
/// when one is asked about.
fn profile_request(key: SubjectKey, category: Option<Category>) -> AccessRequest {
    let request = AccessRequest::new()
        .context_tenant_id(key.tenant_id)
        .tenant_mode(TenantMode::RootOnly)
        .resource_property(pep_properties::OWNER_TENANT_ID, key.tenant_id);
    match category {
        Some(category) => request.resource_property(CATEGORY_PROPERTY, category.name()),
        None => request,
    }
}

/// The categories a category filter names; a value that names no category
/// names nothing.
fn named_categories<'a>(values: impl IntoIterator<Item = &'a ScopeValue>) -> HashSet<Category> {
    values
        .into_iter()
        .filter_map(|value| match value {
            ScopeValue::String(name) => Category::from_name(name),
            _ => None,
        })
        .collect()
}

/// What one constraint of a policy scope grants on the profile at `key`:
/// nothing when it does not reach it, otherwise the categories it names, or
/// every category when it names none. A filter that cannot be decided in
/// memory (a group or tenant-subtree filter) grants nothing, and so does a
/// filter on a property the profile has not.
///
/// Reading profiles is a permission of its own, which the platform grants to
/// the applications and agents that act for the tenant's subjects: a
/// constraint that reaches the tenant lets the caller read every subject in
/// it. A policy may narrow that to named subjects with a resource id filter.
fn granted_by(constraint: &ScopeConstraint, key: SubjectKey) -> Option<HashSet<Category>> {
    let mut named: Option<HashSet<Category>> = None;
    for filter in constraint.filters() {
        if !filter.is_representable_in_memory() {
            return None;
        }
        let values = filter.values();
        let holds = |id: Uuid| values.iter().any(|value| value.as_uuid() == Some(id));
        match filter.property() {
            pep_properties::OWNER_TENANT_ID if !holds(key.tenant_id) => return None,
            pep_properties::RESOURCE_ID if !holds(key.subject_id) => return None,
            pep_properties::OWNER_TENANT_ID | pep_properties::RESOURCE_ID => {}
            CATEGORY_PROPERTY => {
                let these = named_categories(&values);
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

/// What a policy scope grants on the profile at `key`: nothing when no
/// constraint reaches it, otherwise the categories of the constraints that
/// do. Each constraint is one path to the profile, so the tenant, the subject
/// and the categories must all come from the same one.
///
/// The read asks with constraints required, so the enforcer never answers
/// with an unconstrained scope; one that does is the platform allowing
/// everything, and grants every category.
fn granted(scope: &AccessScope, key: SubjectKey) -> Option<HashSet<Category>> {
    if scope.is_unconstrained() {
        return Some(Category::ALL.into_iter().collect());
    }
    scope
        .constraints()
        .iter()
        .filter_map(|constraint| granted_by(constraint, key))
        .reduce(|mut all, these| {
            all.extend(these);
            all
        })
}

/// The scope the subject settings are read under: the profile scope's tenant
/// filters, as the settings rows are scoped by tenant only. An unconstrained
/// scope stays as it is, since `tenant_only` would turn it into deny-all.
fn settings_scope(scope: &AccessScope) -> AccessScope {
    if scope.is_unconstrained() {
        scope.clone()
    } else {
        scope.tenant_only()
    }
}

/// What a failed policy call means for the read.
enum PolicyFailure {
    /// The platform refused; the caller is not allowed. The reason is for
    /// the log only.
    Denied(String),
    /// The platform answered with a scope Construct cannot compile, a defect
    /// of the policy plugin or its config; the caller is not allowed either.
    Uncompilable(String),
    /// The policy service could not answer; the read fails.
    Failed(DomainError),
}

/// The one classification of a failed policy call, for the profile and for
/// each category alike.
fn policy_failure(err: EnforcerError) -> PolicyFailure {
    match err {
        EnforcerError::Denied {
            deny_reason: Some(reason),
        } => PolicyFailure::Denied(format!(
            "the platform refuses the read: {}{}",
            reason.error_code,
            reason
                .details
                .map(|details| format!(" ({details})"))
                .unwrap_or_default()
        )),
        EnforcerError::Denied { deny_reason: None } => {
            PolicyFailure::Denied("the platform refuses the read".to_owned())
        }
        EnforcerError::CompileFailed(cause) => PolicyFailure::Uncompilable(format!(
            "the platform's answer cannot be compiled: {cause}"
        )),
        other @ EnforcerError::EvaluationFailed(_) => PolicyFailure::Failed(other.into()),
    }
}

/// A failed policy call on the profile: a refusal as a subject that is not
/// visible, any other failure as the gear's own error.
fn not_visible(err: EnforcerError) -> DomainError {
    match policy_failure(err) {
        PolicyFailure::Denied(reason) | PolicyFailure::Uncompilable(reason) => {
            DomainError::NotVisible(reason)
        }
        PolicyFailure::Failed(err) => err,
    }
}

/// The read's own answer for a failure of the settings read or the facts
/// read. A refusal is not visible: the read has already decided that the
/// caller reaches the subject, so a scope that does not reach its settings is
/// the same answer. A profile too large to read is the gear's own limit, not
/// something the reader can change. The read answers with nothing else, so
/// both transports map one set of errors.
fn read_failure(err: DomainError) -> DomainError {
    match err {
        DomainError::OutOfScope(refused) => {
            tracing::warn!(
                caller = %refused.caller,
                caller_tenant = %refused.caller_tenant,
                tenant_id = %refused.tenant_id,
                subject_id = %refused.subject_id,
                refusal = ?refused.refusal,
                "construct profile read: the subject's settings are out of the policy scope"
            );
            DomainError::NotVisible(format!(
                "the policy scope does not reach the subject's settings ({:?})",
                refused.refusal
            ))
        }
        DomainError::Forbidden(reason) => DomainError::NotVisible(reason),
        DomainError::ProfileFull(full) => {
            tracing::warn!(
                tenant_id = %full.subject.tenant_id,
                subject_id = %full.subject.subject_id,
                facts = ?full.facts,
                limit = full.limit,
                "construct profile read: the profile is larger than one read takes"
            );
            DomainError::internal(format!(
                "the profile is larger than one read takes: {full:?}"
            ))
        }
        err @ (DomainError::Refused(_)
        | DomainError::NotVisible(_)
        | DomainError::Unavailable(_)
        | DomainError::NotConfigured(_)
        | DomainError::Internal(_)
        | DomainError::Database(_)) => err,
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

    /// The tenant a read is about: the one named, or else the caller's own.
    #[must_use]
    pub fn tenant_of(ctx: &SecurityContext, tenant: Option<Uuid>) -> Uuid {
        tenant.unwrap_or_else(|| ctx.subject_tenant_id())
    }

    /// The profile of `subject_id`, for the subject itself or a caller the
    /// platform lets act for the subject. The tenant is
    /// [`Self::tenant_of`].
    ///
    /// # Errors
    ///
    /// [`DomainError::NotVisible`] when the caller may not act for the subject
    /// in the tenant, which both transports answer like a subject that does
    /// not exist. Unavailable or internal when a dependency fails, also when
    /// no processing identity is configured. No other error.
    #[tracing::instrument(skip_all, fields(%subject_id, caller = %ctx.subject_id()))]
    pub async fn read(
        &self,
        ctx: &SecurityContext,
        tenant: Option<Uuid>,
        subject_id: Uuid,
    ) -> Result<ProfileView, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-authorize
        let key = SubjectKey {
            tenant_id: Self::tenant_of(ctx, tenant),
            subject_id,
        };
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &PROFILE_RESOURCE,
                actions::READ,
                Some(subject_id),
                &profile_request(key, None),
            )
            .await
            .map_err(not_visible)?;
        let Some(granted) = granted(&scope, key) else {
            return Err(DomainError::NotVisible(
                "the policy scope does not reach the subject".to_owned(),
            ));
        };
        // @cpt-end:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-authorize

        // @cpt-begin:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-settings
        // The facts are Construct's own, so graph storage is read as
        // Construct, not as the caller. Without an identity the read is
        // unavailable, whatever it would serve.
        let (construct, _) = self.ports.contexts.for_tenant(key.tenant_id).await?;
        let settings = self
            .ports
            .settings
            .read(ctx, &settings_scope(&scope), key)
            .await
            .map_err(read_failure)?;
        let served = settings.personalization_enabled && !settings.erasure_in_progress;
        // @cpt-end:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-settings

        // @cpt-begin:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-categories
        let readable = match (served, self.check) {
            (false, _) => HashSet::new(),
            (true, CategoryCheck::OneCall) => granted,
            (true, CategoryCheck::PerCategory) => self.readable_one_by_one(ctx, key).await?,
        };
        // @cpt-end:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-categories

        let categories = if readable.is_empty() {
            Vec::new()
        } else {
            self.facts(&construct, key, &readable).await?
        };

        // @cpt-begin:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-event
        self.ports.events.read(ProfileReadEvent {
            tenant_id: key.tenant_id,
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
    /// time. A refusal, also an answer that cannot be compiled, hides the
    /// category, as the first call hides the subject; any other failure fails
    /// the read rather than hiding the category.
    async fn readable_one_by_one(
        &self,
        ctx: &SecurityContext,
        key: SubjectKey,
    ) -> Result<HashSet<Category>, DomainError> {
        let decisions = join_all(Category::ALL.into_iter().map(|category| async move {
            let request = profile_request(key, Some(category));
            let decided = self
                .policy_enforcer
                .access_scope_with(
                    ctx,
                    &PROFILE_RESOURCE,
                    actions::READ,
                    Some(key.subject_id),
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
                    if granted(&scope, key).is_some_and(|categories| categories.contains(&category))
                    {
                        readable.insert(category);
                    }
                }
                Err(err) => match policy_failure(err) {
                    PolicyFailure::Denied(reason) => {
                        tracing::debug!(category = category.name(), %reason, "category not readable");
                    }
                    // A plugin that cannot be compiled hides the category for
                    // every reader, so it is said where an operator looks.
                    PolicyFailure::Uncompilable(reason) => {
                        tracing::warn!(category = category.name(), %reason, "category not readable");
                    }
                    PolicyFailure::Failed(err) => return Err(err),
                },
            }
        }
        Ok(readable)
    }

    /// The facts of the readable categories, read as `construct`, grouped in
    /// category order, without categories that hold none.
    async fn facts(
        &self,
        construct: &SecurityContext,
        key: SubjectKey,
        readable: &HashSet<Category>,
    ) -> Result<Vec<CategoryFacts>, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-facts
        let stored = self
            .ports
            .profiles
            .stored(construct, key)
            .await
            .map_err(read_failure)?;
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

#[cfg(test)]
mod tests {
    use toolkit_security::ScopeFilter;

    use super::*;

    fn key() -> SubjectKey {
        SubjectKey {
            tenant_id: Uuid::new_v4(),
            subject_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn an_unconstrained_scope_grants_every_category() {
        assert_eq!(
            granted(&AccessScope::allow_all(), key()),
            Some(Category::ALL.into_iter().collect())
        );
    }

    #[test]
    fn a_tenant_constraint_grants_every_subject_in_the_tenant_and_no_other_tenant() {
        let key = key();
        let tenant = AccessScope::for_tenant(key.tenant_id);

        assert_eq!(
            granted(&tenant, key),
            Some(Category::ALL.into_iter().collect())
        );
        let elsewhere = SubjectKey {
            tenant_id: Uuid::new_v4(),
            ..key
        };
        assert_eq!(granted(&tenant, elsewhere), None);
    }

    #[test]
    fn a_resource_id_filter_narrows_the_grant_to_the_subjects_it_names() {
        let key = key();
        let scope = AccessScope::single(path_to(key, &[Category::Skill]));

        assert_eq!(
            granted(&scope, key),
            Some([Category::Skill].into_iter().collect())
        );
        let other = SubjectKey {
            subject_id: Uuid::new_v4(),
            ..key
        };
        assert_eq!(granted(&scope, other), None);
    }

    /// One path to the profile at `key`, granting `categories`.
    fn path_to(key: SubjectKey, categories: &[Category]) -> ScopeConstraint {
        ScopeConstraint::new(vec![
            ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, key.tenant_id),
            ScopeFilter::eq(pep_properties::RESOURCE_ID, key.subject_id),
            ScopeFilter::r#in(
                CATEGORY_PROPERTY,
                categories
                    .iter()
                    .map(|c| ScopeValue::from(c.name()))
                    .collect(),
            ),
        ])
    }

    #[test]
    fn several_paths_to_the_profile_grant_the_union_of_their_categories() {
        let key = key();
        let scope = AccessScope::from_constraints(vec![
            path_to(key, &[Category::Skill]),
            path_to(key, &[Category::Role]),
        ]);

        assert_eq!(
            granted(&scope, key),
            Some([Category::Skill, Category::Role].into_iter().collect())
        );
    }

    #[test]
    fn the_settings_scope_keeps_an_unconstrained_scope_and_only_the_tenant_filters_of_others() {
        let key = key();

        assert!(settings_scope(&AccessScope::allow_all()).is_unconstrained());
        let narrowed = settings_scope(&AccessScope::single(path_to(key, &[Category::Skill])));
        let [constraint] = narrowed.constraints() else {
            panic!("one constraint: {narrowed:?}");
        };
        let properties: Vec<&str> = constraint
            .filters()
            .iter()
            .map(ScopeFilter::property)
            .collect();
        assert_eq!(properties, [pep_properties::OWNER_TENANT_ID]);
    }
}
