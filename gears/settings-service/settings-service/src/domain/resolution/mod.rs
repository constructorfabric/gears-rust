// Created: 2026-09-06 by Constructor Tech
//! Effective value resolution: the hot read path.
//!
//! A read dispatches on the declaration's scope class, walks the caller's own
//! ancestor chain for a cascading setting, skips overrides flagged for review,
//! and always terminates in the Schema Default. The result is computed on each
//! read and never persisted; a local cache keeps the hot path off the database.

pub mod cache;
pub mod resolver;

use async_trait::async_trait;
use serde_json::Value;
use settings_service_sdk::EffectiveSource;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;

pub use cache::EffectiveCache;
pub use resolver::ValueResolver;

/// The fixed token an administrative read shows in place of a masked value.
pub const MASK_TOKEN: &str = "********";

/// The scope class names as stored on a declaration.
pub mod scope_class {
    /// One value for the whole platform, set at the root tenant only.
    pub const GLOBAL: &str = "global";
    /// Inherited down the tenant tree, nearest override wins.
    pub const CASCADING: &str = "cascading";
    /// One value per tenant, never inherited.
    pub const LOCAL: &str = "local";
}

/// Where a read is resolved for: the platform, or one tenant.
///
/// Platform scope *is* the root tenant — its rows carry the root tenant's id —
/// so the two spellings converge once the root is known; the distinction is
/// kept only as long as the caller's wording is, for the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeTarget {
    /// `/`, the platform root.
    Platform,
    /// `/tenants/{id}`.
    Tenant(Uuid),
}

impl ScopeTarget {
    /// Parse a scope path: `/` or `/tenants/{uuid}`.
    ///
    /// # Errors
    /// [`DomainError::Validation`] on any other shape. The path is never
    /// parsed further: ancestry comes from the tenant resolver, not from the
    /// string.
    pub fn parse(scope: &str) -> Result<Self, DomainError> {
        let trimmed = scope.trim_end_matches('/');
        if trimmed.is_empty() {
            return Ok(Self::Platform);
        }
        if let Some(id) = trimmed.strip_prefix("/tenants/")
            && let Ok(uuid) = Uuid::parse_str(id)
        {
            return Ok(Self::Tenant(uuid));
        }
        Err(DomainError::Validation {
            field: "scope".to_owned(),
            code: crate::field::SCOPE_PATH,
            message: format!("`{scope}` is not `/` or `/tenants/{{id}}`"),
        })
    }

    /// The target as a tenant id, given the root.
    #[must_use]
    pub fn tenant_id(self, root: Uuid) -> Uuid {
        match self {
            Self::Platform => root,
            Self::Tenant(id) => id,
        }
    }
}

/// The scope path of a tenant id: `/` for the root, `/tenants/{id}` otherwise.
#[must_use]
pub fn scope_path(tenant: Uuid, root: Uuid) -> String {
    if tenant == root {
        "/".to_owned()
    } else {
        format!("/tenants/{tenant}")
    }
}

/// The tenant hierarchy, as the resolver needs it.
///
/// A port over the tenant resolver: ancestry is owned there and is never
/// reconstructed from stored scope values here.
#[async_trait]
pub trait TenantHierarchy: Send + Sync {
    /// The ancestor ids of `tenant`, ordered root to self and including both.
    ///
    /// Barriers are ignored: a standalone tenant still runs on the platform
    /// and still inherits the platform's defaults, so runtime resolution walks
    /// the whole chain.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] when the tenant does not exist;
    /// [`DomainError::Unavailable`] when the resolver cannot answer.
    async fn chain(&self, tenant: Uuid) -> Result<Vec<Uuid>, DomainError>;

    /// Whether `target` is `caller` itself or a descendant reachable from it
    /// without crossing a standalone barrier.
    ///
    /// # Errors
    /// As [`Self::chain`].
    async fn is_within_subtree(&self, caller: Uuid, target: Uuid) -> Result<bool, DomainError>;

    /// Whether the tenant is marked standalone, sealed from administration
    /// above it.
    ///
    /// # Errors
    /// As [`Self::chain`].
    async fn is_standalone(&self, tenant: Uuid) -> Result<bool, DomainError>;

    /// The descendants of `tenant`, excluding it, with standalone subtrees
    /// left out.
    ///
    /// # Errors
    /// As [`Self::chain`].
    async fn descendants(&self, tenant: Uuid) -> Result<Vec<Uuid>, DomainError>;

    /// The descendants of `tenant` in breadth-first order, at most `budget` of
    /// them, standalone subtrees left out; the flag says whether the budget
    /// cut the walk short.
    ///
    /// # Errors
    /// As [`Self::chain`].
    async fn descendants_bfs(
        &self,
        tenant: Uuid,
        budget: usize,
    ) -> Result<(Vec<Uuid>, bool), DomainError>;
}

/// One scope the resolver inspected, in full.
///
/// The consumer projection drops `set_by` and `last_change_at`; the
/// administrative read keeps them.
// Three independent facts about one scope; an enum would only re-encode them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailEntry {
    /// The tenant inspected.
    pub tenant_id: Uuid,
    /// Its scope path.
    pub scope: String,
    /// Whether an override row exists here.
    pub has_override: bool,
    /// Whether this scope supplied the effective value.
    pub provided_value: bool,
    /// Whether the row here is flagged for review and was skipped.
    pub needs_review: bool,
    /// Who set the row here, when one exists.
    pub set_by: Option<String>,
    /// When the row here last changed, when one exists.
    pub last_change_at: Option<OffsetDateTime>,
}

/// The requested scope's own row, as the administrative read reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnRow {
    /// Flagged for review: the resolver fell through past it.
    pub needs_review: bool,
    /// Why.
    pub needs_review_detail: Option<String>,
    /// When the row last changed — what the value state tag derives from.
    pub last_change_at: OffsetDateTime,
    /// The row version.
    pub updated_at: OffsetDateTime,
}

/// A resolved effective value with its trace — computed, never persisted.
// @cpt-dod:cpt-cf-settings-service-dod-value-resolution-shape:p1
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveValue {
    /// The setting key.
    pub key: String,
    /// The declaration's id.
    pub declaration_id: Uuid,
    /// The requested scope path.
    pub scope: String,
    /// The requested scope as a tenant id.
    pub tenant_id: Uuid,
    /// The resolved value. For a secret row this is the opaque handle token,
    /// never plaintext.
    pub value: Value,
    /// Where the value came from.
    pub source: EffectiveSource,
    /// The scope that supplied it; absent for a Schema Default.
    pub source_scope: Option<String>,
    /// The value type's resolved trait set.
    pub traits: Value,
    /// The scopes inspected, root to self — the caller's own chain only.
    pub trail: Vec<TrailEntry>,
    /// The declaration's classification.
    pub data_classification: String,
    /// The declaration's administrative domain, for the visibility rule the
    /// administrative surface applies.
    pub domain_affinity: Option<String>,
    /// Whether the value came from a secret row.
    pub secret_backed: bool,
    /// The definition arm of the recency indicator.
    pub declaration_last_change_at: OffsetDateTime,
    /// The value arm: the resolved row's own timestamp, never a maximum over
    /// sibling or descendant scopes.
    pub resolved_row_last_change_at: Option<OffsetDateTime>,
    /// The requested scope's own row, when one exists.
    pub own_row: Option<OwnRow>,
}
