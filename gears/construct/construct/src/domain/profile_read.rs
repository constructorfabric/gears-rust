//! The profile read for applications and agents: a subject's facts by
//! category, for a caller the platform lets act for the subject.

use std::collections::HashSet;
use std::sync::{Arc, Once};

use async_trait::async_trait;
use authz_resolver_sdk::models::TenantMode;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use authz_resolver_sdk::{AccessDenial, EnforcerError, PolicyEnforcer};
use futures::future::join_all;
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, ScopeConstraint, ScopeValue, SecurityContext, pep_properties};
use uuid::Uuid;

use super::deletion::{RetentionPeriods, period_start};
use super::error::DomainError;
use super::plan::NodeKey;
use super::planner::processing::ProcessingContexts;
use super::profile::{Category, Link};
use super::review_queue::FactsUnderReview;
use super::subject_reach::reach;
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

impl OriginKind {
    /// The origin's name on the wire and in a list's `origin` field.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Connector => "connector",
            Self::Agent => "agent",
            Self::Reviewer => "reviewer",
            Self::Administrator => "administrator",
        }
    }
}

/// Where a node or a link was learned: the tool and, when there is one, the message or record.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearnedFrom {
    pub tool: String,
    pub message: Option<String>,
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
    /// The node that owns this one, for a node of an entry kind.
    pub owner: Option<NodeKey>,
    /// The tools and messages the node was learned from, the newest last.
    pub learned: Vec<LearnedFrom>,
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

    /// The links between a subject's stored facts, with where each was learned. A reader that holds no links
    /// answers none.
    ///
    /// # Errors
    ///
    /// When the profile cannot be read.
    async fn links(
        &self,
        _ctx: &SecurityContext,
        _key: SubjectKey,
    ) -> Result<Vec<StoredLinkFact>, DomainError> {
        Ok(Vec::new())
    }
}

/// A link as a reader sees it: its kind, ends and fields, and where it was learned.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct StoredLinkFact {
    pub link: Link,
    pub learned: Vec<LearnedFrom>,
}

/// The node a read of one node gives: the node, the nodes it owns, the nodes one link away grouped by link kind, and
/// the tools it was learned from, with a count each.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct NodeDetails {
    pub node: StoredFact,
    pub owned: Vec<StoredFact>,
    pub neighbours: Vec<NeighbourGroup>,
    pub learned_from: Vec<ToolCount>,
}

/// The neighbours of a node by one link kind.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct NeighbourGroup {
    pub link: String,
    pub neighbours: Vec<Neighbour>,
}

/// Which way a link goes, from the node a read is about.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkDirection {
    /// The link starts at the node.
    Out,
    /// The link ends at the node.
    In,
}

impl LinkDirection {
    /// The direction's name on the wire.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Out => "out",
            Self::In => "in",
        }
    }
}

/// A node one link away, with the link's direction and fields.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Neighbour {
    pub direction: LinkDirection,
    pub fields: Value,
    pub node: StoredFact,
}

/// How many times a tool taught the profile what a node holds.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCount {
    pub tool: String,
    pub count: usize,
}

/// A content-free event for one profile read: who read which subject's
/// categories, in which tenant, and what the policy let them read.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileReadEvent {
    pub tenant_id: Uuid,
    pub subject_id: Uuid,
    pub caller: Uuid,
    /// The categories served: those granted that hold facts.
    pub categories: Vec<Category>,
    /// The categories the policy let the caller read, in category order,
    /// also those that hold no fact. Empty when the profile is not served,
    /// as they are then not asked.
    pub granted: Vec<Category>,
    /// Whether the profile was served: personalization on and no erasure
    /// under way.
    pub served: bool,
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
    pub reviews: Arc<dyn FactsUnderReview>,
    /// The tenants' retention periods: no fact stored before the tenant's
    /// period began is served, also before the retention job removes it.
    ///
    /// @cpt-dod:cpt-cf-construct-dod-retention-hide:p2
    pub retention: Arc<dyn RetentionPeriods>,
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
    // Constraints are required, so an answer without them is a refusal, never
    // a grant of every profile; `granted` relies on it.
    let request = AccessRequest::new()
        .require_constraints(true)
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
        .filter_map(|value| {
            let category = match value {
                ScopeValue::String(name) => Category::from_name(name),
                _ => None,
            };
            if category.is_none() {
                // A policy that names no category Construct knows hides it
                // from every reader; this says why.
                tracing::warn!(
                    ?value,
                    "construct: a policy names a category that is not one; it grants nothing"
                );
            }
            category
        })
        .collect()
}

/// What one constraint of a policy scope grants on the profile at `key`:
/// nothing when it does not [`reach`] it, otherwise the categories it names,
/// or every category when it names none. A filter on a property the profile
/// has not grants nothing.
///
/// Reading profiles is a permission of its own, which the platform grants to
/// the applications and agents that act for the tenant's subjects: a
/// constraint that reaches the tenant lets the caller read every subject in
/// it. A policy may narrow that to named subjects with a resource id filter.
fn granted_by(constraint: &ScopeConstraint, key: SubjectKey) -> Option<Grant> {
    let mut named: Option<HashSet<Category>> = None;
    for filter in reach(constraint, key)? {
        if filter.property() != CATEGORY_PROPERTY {
            return None;
        }
        let these = named_categories(filter.values());
        named = Some(match named {
            Some(before) => before.intersection(&these).copied().collect(),
            None => these,
        });
    }
    Some(match named {
        Some(categories) => Grant {
            categories,
            unnamed: false,
        },
        None => Grant {
            categories: Category::all().into_iter().collect(),
            unnamed: true,
        },
    })
}

/// What a policy scope grants on a profile.
#[domain_model]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Grant {
    categories: HashSet<Category>,
    /// A constraint that reaches the profile named no category, so it grants
    /// every one. Under `one_call` that is also what a plugin that cannot
    /// express categories answers.
    unnamed: bool,
}

/// What a policy scope grants on the profile at `key`: nothing when no
/// constraint reaches it, otherwise the categories of the constraints that
/// do. Each constraint is one path to the profile, so the tenant, the subject
/// and the categories must all come from the same one.
///
/// The read asks with constraints required, so the enforcer never answers
/// with an unconstrained scope. One that does grants nothing: an allow
/// without constraints is a refusal, never a grant of every profile.
pub(crate) fn granted(scope: &AccessScope, key: SubjectKey) -> Option<Grant> {
    if scope.is_unconstrained() {
        return None;
    }
    scope
        .constraints()
        .iter()
        .filter_map(|constraint| granted_by(constraint, key))
        .reduce(|mut all, these| {
            all.categories.extend(these.categories);
            all.unnamed |= these.unnamed;
            all
        })
}

/// The scope the subject settings are read under: the profile scope's tenant
/// filters, as the settings rows are scoped by tenant only. The scope is
/// never unconstrained here, since [`granted`] refuses that.
pub(crate) fn settings_scope(scope: &AccessScope) -> AccessScope {
    scope.tenant_only()
}

/// Say once per process that `one_call` met a policy answer without a
/// category constraint, which grants every category. A plugin that cannot
/// express categories answers so for every read, and needs `per_category`.
fn warn_unnamed_categories() {
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        tracing::warn!(
            "construct: profile_category_check is one_call and a policy answer named no \
             category, so it grants every category; a policy plugin that cannot express \
             categories needs per_category"
        );
    });
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
        EnforcerError::AccessDenied(AccessDenial::Pdp {
            reason: Some(reason),
        }) => PolicyFailure::Denied(format!(
            "the platform refuses the read: {}{}",
            reason.error_code,
            reason
                .details
                .map(|details| format!(" ({details})"))
                .unwrap_or_default()
        )),
        EnforcerError::AccessDenied(_) => {
            PolicyFailure::Denied("the platform refuses the read".to_owned())
        }
        EnforcerError::CompileFailed(cause) => PolicyFailure::Uncompilable(format!(
            "the platform's answer cannot be compiled: {cause}"
        )),
        other @ EnforcerError::EvaluationFailed(_) => PolicyFailure::Failed(other.into()),
        // An unknown outcome class fails the read; it is never a refusal.
        unknown => PolicyFailure::Failed(unknown.into()),
    }
}

/// A failed policy call on the profile: a refusal as a subject that is not
/// visible, any other failure as the gear's own error.
pub(crate) fn not_visible(err: EnforcerError) -> DomainError {
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
pub(crate) fn read_failure(err: DomainError) -> DomainError {
    match err {
        // Logged once, where the answer is mapped: the message carries the
        // refusal with its ids, which stay out of the answer.
        DomainError::OutOfScope(refused) => DomainError::NotVisible(format!(
            "the policy scope does not reach the subject's settings: {refused}"
        )),
        DomainError::Forbidden(reason) => DomainError::NotVisible(reason),
        DomainError::ProfileFull(full) => DomainError::internal(format!(
            "the profile is larger than one read takes: {full:?}"
        )),
        err @ (DomainError::Refused(_)
        | DomainError::NotVisible(_)
        | DomainError::Unavailable(_)
        | DomainError::WriteUnknown(_)
        | DomainError::NotConfigured(_)
        | DomainError::NoSuchFact(_)
        | DomainError::NoSuchReviewRequest(_)
        | DomainError::ReviewConflict(_)
        | DomainError::InvalidValue { .. }
        | DomainError::FactHeld(_)
        | DomainError::NotStored(_)
        | DomainError::InvalidQuery(_)
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

    /// The same read with other retention periods.
    #[cfg(test)]
    #[must_use]
    pub fn with_retention(mut self, retention: Arc<dyn RetentionPeriods>) -> Self {
        self.ports.retention = retention;
        self
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
    /// not exist. Unavailable or internal when a dependency fails. Not
    /// configured when the deployment has no processing identity. No other
    /// error.
    ///
    /// Cancel-safe: the read only reads, and its event is recorded after the
    /// last await, so a dropped read leaves nothing behind.
    #[tracing::instrument(skip_all, fields(%subject_id, caller = %ctx.subject_id()))]
    pub async fn read(
        &self,
        ctx: &SecurityContext,
        tenant: Option<Uuid>,
        subject_id: Uuid,
    ) -> Result<ProfileView, DomainError> {
        let Access {
            key,
            construct,
            construct_scope,
            readable,
            served,
        } = self.access(ctx, tenant, subject_id).await?;
        let categories = if readable.is_empty() {
            Vec::new()
        } else {
            self.facts(&construct, &construct_scope, key, &readable)
                .await?
        };

        // @cpt-begin:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-event
        self.ports.events.read(ProfileReadEvent {
            tenant_id: key.tenant_id,
            subject_id,
            caller: ctx.subject_id(),
            categories: categories.iter().map(|group| group.category).collect(),
            granted: Category::all()
                .into_iter()
                .filter(|category| readable.contains(category))
                .collect(),
            served,
        });
        Ok(ProfileView {
            subject_id,
            categories,
        })
        // @cpt-end:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-event
    }

    /// One node of the profile of `subject_id` with the nodes it owns, the nodes one link away grouped by link
    /// kind, and the tools it was learned from, in one call. Every node it gives is of a category the caller may
    /// read: a project the caller may not read is not a neighbour either.
    ///
    /// # Errors
    ///
    /// As [`Self::read`], and [`DomainError::NoSuchFact`] when the subject has no such node, the caller may not
    /// read its category, or nothing is served (personalization off, an erasure under way): the three look the
    /// same.
    ///
    /// Cancel-safe: the read only reads.
    #[tracing::instrument(skip_all, fields(%subject_id, caller = %ctx.subject_id()))]
    pub async fn read_node(
        &self,
        ctx: &SecurityContext,
        tenant: Option<Uuid>,
        subject_id: Uuid,
        node: &NodeKey,
    ) -> Result<NodeDetails, DomainError> {
        let Access {
            key,
            construct,
            construct_scope,
            readable,
            ..
        } = self.access(ctx, tenant, subject_id).await?;
        let gone = || DomainError::NoSuchFact(node.0.clone());
        if readable.is_empty() {
            return Err(gone());
        }
        let stored = self.visible(&construct, &construct_scope, key).await?;
        let links = self
            .ports
            .profiles
            .links(&construct, key)
            .await
            .map_err(read_failure)?;
        let held = stored
            .iter()
            .find(|fact| &fact.node_key == node && readable.contains(&fact.category))
            .ok_or_else(gone)?;
        let readable_fact = |key: &NodeKey| {
            stored
                .iter()
                .find(|fact| &fact.node_key == key && readable.contains(&fact.category))
        };
        let owned: Vec<StoredFact> = stored
            .iter()
            .filter(|fact| fact.owner.as_ref() == Some(node) && readable.contains(&fact.category))
            .cloned()
            .collect();
        let mut groups: Vec<NeighbourGroup> = Vec::new();
        let mut learned: Vec<&LearnedFrom> = held.learned.iter().collect();
        for stored_link in &links {
            let link = &stored_link.link;
            let (direction, other) = if &link.from == node {
                (LinkDirection::Out, &link.to)
            } else if &link.to == node {
                (LinkDirection::In, &link.from)
            } else {
                continue;
            };
            // A link to a node the caller may not read is not shown, nor counted.
            let Some(other) = readable_fact(other) else {
                continue;
            };
            learned.extend(stored_link.learned.iter());
            let neighbour = Neighbour {
                direction,
                fields: link.fields.clone(),
                node: other.clone(),
            };
            match groups.iter_mut().find(|group| group.link == link.kind) {
                Some(group) => group.neighbours.push(neighbour),
                None => groups.push(NeighbourGroup {
                    link: link.kind.clone(),
                    neighbours: vec![neighbour],
                }),
            }
        }
        groups.sort_by(|a, b| a.link.cmp(&b.link));
        let mut counts: Vec<ToolCount> = Vec::new();
        for place in learned {
            match counts.iter_mut().find(|count| count.tool == place.tool) {
                Some(count) => count.count += 1,
                None => counts.push(ToolCount {
                    tool: place.tool.clone(),
                    count: 1,
                }),
            }
        }
        counts.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tool.cmp(&b.tool)));
        let mut categories: Vec<Category> = std::iter::once(held.category)
            .chain(owned.iter().map(|fact| fact.category))
            .chain(
                groups
                    .iter()
                    .flat_map(|group| group.neighbours.iter().map(|n| n.node.category)),
            )
            .collect();
        categories.sort();
        categories.dedup();
        self.ports.events.read(ProfileReadEvent {
            tenant_id: key.tenant_id,
            subject_id,
            caller: ctx.subject_id(),
            categories,
            granted: Category::all()
                .into_iter()
                .filter(|category| readable.contains(category))
                .collect(),
            served: true,
        });
        Ok(NodeDetails {
            node: held.clone(),
            owned,
            neighbours: groups,
            learned_from: counts,
        })
    }

    /// The caller's access to the profile of `subject_id`: the policy checks, the settings, and the categories
    /// the caller may read.
    async fn access(
        &self,
        ctx: &SecurityContext,
        tenant: Option<Uuid>,
        subject_id: Uuid,
    ) -> Result<Access, DomainError> {
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
        // Construct, not as the caller. Without an identity the read is not
        // configured, whatever it would serve.
        let (construct, construct_scope) = self.ports.contexts.for_tenant(key.tenant_id).await?;
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
            (true, CategoryCheck::OneCall) => {
                if granted.unnamed {
                    warn_unnamed_categories();
                }
                granted.categories
            }
            (true, CategoryCheck::PerCategory) => self.readable_one_by_one(ctx, key).await?,
        };
        // @cpt-end:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-categories
        Ok(Access {
            key,
            construct,
            construct_scope,
            readable,
            served,
        })
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
        let decisions = join_all(Category::all().into_iter().map(|category| async move {
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
                    if granted(&scope, key)
                        .is_some_and(|grant| grant.categories.contains(&category))
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
    /// category order, without categories that hold none. A fact under an
    /// open review request is not served.
    ///
    /// @cpt-dod:cpt-cf-construct-dod-review-queue-frozen:p1
    async fn facts(
        &self,
        construct: &SecurityContext,
        construct_scope: &AccessScope,
        key: SubjectKey,
        readable: &HashSet<Category>,
    ) -> Result<Vec<CategoryFacts>, DomainError> {
        let stored = self.visible(construct, construct_scope, key).await?;
        // @cpt-begin:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-facts
        let mut groups: Vec<CategoryFacts> = Category::all()
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

    /// Every stored fact the profile serves: not one under an open review request, not one stored before the
    /// tenant's retention period began.
    async fn visible(
        &self,
        construct: &SecurityContext,
        construct_scope: &AccessScope,
        key: SubjectKey,
    ) -> Result<Vec<StoredFact>, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-facts
        let stored = self
            .ports
            .profiles
            .stored(construct, key)
            .await
            .map_err(read_failure)?;
        // @cpt-end:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-facts
        // @cpt-begin:cpt-cf-construct-algo-review-queue-hide:p1:inst-hide-filter
        let under_review = self
            .ports
            .reviews
            .under_review(construct_scope, key)
            .await?;
        let stored = stored
            .into_iter()
            .filter(|fact| !under_review.contains(&fact.node_key));
        // @cpt-end:cpt-cf-construct-algo-review-queue-hide:p1:inst-hide-filter
        // @cpt-begin:cpt-cf-construct-algo-retention-hide:p2:inst-hide-expired
        let period = self.ports.retention.period(key.tenant_id).await?;
        let kept_since = period.and_then(|period| period_start(OffsetDateTime::now_utc(), period));
        Ok(stored
            .filter(|fact| kept_since.is_none_or(|since| fact.stored_at >= since))
            .collect())
        // @cpt-end:cpt-cf-construct-algo-retention-hide:p2:inst-hide-expired
    }
}

/// What a caller may do with a subject's profile: the processing context graph storage is read with, and the
/// categories the caller may read.
struct Access {
    key: SubjectKey,
    construct: SecurityContext,
    construct_scope: AccessScope,
    readable: HashSet<Category>,
    served: bool,
}

#[cfg(test)]
mod tests {
    use toolkit_security::ScopeFilter;

    use super::*;
    use crate::domain::settings_scope::{ScopeRefusal, SettingsOutOfScope};

    fn key() -> SubjectKey {
        SubjectKey {
            tenant_id: Uuid::new_v4(),
            subject_id: Uuid::new_v4(),
        }
    }

    /// The categories `scope` grants on the profile at `key`.
    fn categories(scope: &AccessScope, key: SubjectKey) -> Option<HashSet<Category>> {
        granted(scope, key).map(|grant| grant.categories)
    }

    #[test]
    fn an_unconstrained_scope_grants_nothing() {
        assert_eq!(granted(&AccessScope::allow_all(), key()), None);
    }

    #[test]
    fn a_grant_says_when_a_constraint_named_no_category() {
        let key = key();

        let unnamed = granted(&AccessScope::for_tenant(key.tenant_id), key).unwrap();
        assert!(unnamed.unnamed);
        let named = granted(
            &AccessScope::single(path_to(key, &[Category::PROJECT])),
            key,
        )
        .unwrap();
        assert!(!named.unnamed);
        let both = granted(
            &AccessScope::from_constraints(vec![
                path_to(key, &[Category::PROJECT]),
                ScopeConstraint::new(vec![ScopeFilter::eq(
                    pep_properties::OWNER_TENANT_ID,
                    key.tenant_id,
                )]),
            ]),
            key,
        )
        .unwrap();
        assert!(both.unnamed, "one path without a category is enough");
    }

    #[test]
    fn a_constraint_with_no_category_is_no_path_to_the_profile() {
        let key = key();
        let dead = path_to(key, &[]);
        assert!(dead.matches_nothing());

        // Alone it reaches nothing: the profile is not visible, never an empty grant.
        assert_eq!(granted(&AccessScope::single(dead.clone()), key), None);
        // Beside a live path it adds nothing and takes nothing away.
        let live = path_to(key, &[Category::PROJECT]);
        assert_eq!(
            granted(
                &AccessScope::from_constraints(vec![dead, live.clone()]),
                key
            ),
            granted(&AccessScope::single(live), key)
        );
    }

    #[test]
    fn a_tenant_constraint_grants_every_subject_in_the_tenant_and_no_other_tenant() {
        let key = key();
        let tenant = AccessScope::for_tenant(key.tenant_id);

        assert_eq!(
            categories(&tenant, key),
            Some(Category::all().into_iter().collect())
        );
        let elsewhere = SubjectKey {
            tenant_id: Uuid::new_v4(),
            ..key
        };
        assert_eq!(categories(&tenant, elsewhere), None);
    }

    #[test]
    fn a_resource_id_filter_narrows_the_grant_to_the_subjects_it_names() {
        let key = key();
        let scope = AccessScope::single(path_to(key, &[Category::PROJECT]));

        assert_eq!(
            categories(&scope, key),
            Some([Category::PROJECT].into_iter().collect())
        );
        let other = SubjectKey {
            subject_id: Uuid::new_v4(),
            ..key
        };
        assert_eq!(categories(&scope, other), None);
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
    fn a_category_filter_of_unknown_names_or_other_values_grants_no_category() {
        let key = key();
        for values in [
            vec![ScopeValue::from("skills")],
            vec![ScopeValue::from(Uuid::new_v4())],
        ] {
            let scope = AccessScope::single(ScopeConstraint::new(vec![
                ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, key.tenant_id),
                ScopeFilter::r#in(CATEGORY_PROPERTY, values.clone()),
            ]));

            assert_eq!(categories(&scope, key), Some(HashSet::new()), "{values:?}");
        }
    }

    #[test]
    fn several_paths_to_the_profile_grant_the_union_of_their_categories() {
        let key = key();
        let scope = AccessScope::from_constraints(vec![
            path_to(key, &[Category::PROJECT]),
            path_to(key, &[Category::PRODUCT]),
        ]);

        assert_eq!(
            categories(&scope, key),
            Some([Category::PROJECT, Category::PRODUCT].into_iter().collect())
        );
    }

    #[test]
    fn the_settings_scope_keeps_only_the_tenant_filters() {
        let key = key();

        let narrowed = settings_scope(&AccessScope::single(path_to(key, &[Category::PROJECT])));
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

    #[test]
    fn a_settings_read_the_platform_forbids_is_not_visible() {
        assert!(matches!(
            read_failure(DomainError::forbidden("refused")),
            DomainError::NotVisible(_)
        ));
    }

    #[test]
    fn a_settings_read_out_of_the_policy_scope_is_not_visible() {
        let refused = SettingsOutOfScope {
            caller: Uuid::new_v4(),
            caller_tenant: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            subject_id: Uuid::new_v4(),
            refusal: ScopeRefusal::NotCovered,
        };

        assert!(matches!(
            read_failure(DomainError::OutOfScope(refused)),
            DomainError::NotVisible(_)
        ));
    }
}
