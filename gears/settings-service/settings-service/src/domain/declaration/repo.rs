// Created: 2026-08-26 by Constructor Tech
//! The declaration repository contract.
//!
//! Read-only for now: entry 2.3's read surface is the first slice, and the
//! lifecycle mutations arrive with the flows that own them. Declaring only what
//! exists keeps the trait honest about what an implementor must supply today.

use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::category::DomainVisibility;
use crate::domain::error::DomainError;

/// A declaration as the domain sees it.
///
/// Deliberately a projection, not the row. The read surface renders `key`,
/// `value_type_id` and the resolved trait set; the columns that exist only to
/// support writes or masking stay in `infra`, so a reader cannot come to depend
/// on them by accident.
// The flags are separate facts an administrator reads back one by one; a
// state enum would only re-encode them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// Surrogate identity, stable across a re-key.
    pub id: Uuid,

    /// The full setting key.
    pub key: String,

    /// The setting's own name slug, unique within its category.
    pub leaf_slug: String,

    /// GTS id of the value type the setting's values validate against — a
    /// separate fact of the declaration, not a half of the key (ADR-002).
    pub value_type_id: String,

    /// Owning category.
    pub category_id: Uuid,

    /// `global`, `cascading`, or `local`.
    pub scope_class: String,

    /// `standard` or `advanced`.
    pub mode: String,

    /// `active` or `retired`.
    pub status: String,

    /// The administrative domain, when the declaration is bound to one.
    pub domain_affinity: Option<String>,

    /// The licence feature gating the declaration, when one applies.
    ///
    /// Carried but **not yet enforced**: the gate belongs to the License
    /// Resolver, which has a design and no implementation. Surfacing the field
    /// lets a caller see what would gate it once the resolver exists.
    pub licence_feature: Option<String>,

    /// The contributing module, for module-contributed declarations.
    pub owner_module: Option<String>,

    /// Optional long-form description.
    pub description: Option<String>,

    /// The Schema Default — the floor every resolution terminates in.
    pub default_value: serde_json::Value,

    /// Whether the value type carries the `secret` trait.
    pub has_secret_trait: bool,

    /// `public`, `pii` or `secret`.
    pub data_classification: String,

    /// Whether changing the value needs a fresh re-authentication.
    pub requires_step_up: bool,

    /// Whether the effective value may be served on the anonymous surface.
    pub anonymous_exposable: bool,

    /// `admin_authored` or `module_contributed`.
    pub source: String,
}

/// A declaration about to be inserted, every column decided.
// The flags are separate facts an administrator reads back one by one; a
// state enum would only re-encode them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationDraft {
    /// The full setting key.
    pub key: String,
    /// The setting's own name slug.
    pub leaf_slug: String,
    /// The value type its values validate against.
    pub value_type_id: String,
    /// The owning category.
    pub category_id: Uuid,
    /// The Schema Default.
    pub default_value: serde_json::Value,
    /// `global`, `cascading` or `local`.
    pub scope_class: String,
    /// `standard` or `advanced`.
    pub mode: String,
    /// Whether changing the value needs a fresh re-authentication.
    pub requires_step_up: bool,
    /// Whether the effective value may be served on the anonymous surface.
    pub anonymous_exposable: bool,
    /// The administrative domain, if any.
    pub domain_affinity: Option<String>,
    /// Whether the value type carries the `secret` trait.
    pub has_secret_trait: bool,
    /// `public`, `pii` or `secret`.
    pub data_classification: String,
    /// `admin_authored` or `module_contributed`.
    pub source: String,
    /// The contributing module, for a contributed declaration.
    pub owner_module: Option<String>,
    /// The licence feature gating the setting, if any.
    pub licence_feature: Option<String>,
    /// Human-readable description.
    pub description: Option<String>,
    /// The principal or module that created the row.
    pub created_by: String,
}

/// The metadata a reconcile may change in place at the same major.
///
/// Nothing here alters a live setting's resolution: the Schema Default, the
/// value type and the scope class are absent on purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationMetadata {
    /// `standard` or `advanced`.
    pub mode: String,
    /// Human-readable description.
    pub description: Option<String>,
    /// The administrative domain, if any.
    pub domain_affinity: Option<String>,
    /// The licence feature gating the setting, if any.
    pub licence_feature: Option<String>,
    /// `public`, `pii` or `secret`.
    pub data_classification: String,
    /// Whether changing the value needs a fresh re-authentication.
    pub requires_step_up: bool,
    /// Whether the effective value may be served on the anonymous surface.
    pub anonymous_exposable: bool,
}

/// Operations on declarations.
#[async_trait]
pub trait DeclarationRepository: Send + Sync {
    /// The declaration at exactly this key, whatever its status.
    async fn find_by_key<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: &str,
    ) -> Result<Option<Declaration>, DomainError>;

    /// Every declaration whose key starts with `key_prefix` — the base and the
    /// version-stripped path followed by `.v` — whatever its status. Callers
    /// re-check the stripped path exactly, since a `LIKE` prefix is not a
    /// token boundary.
    async fn find_by_key_prefix<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key_prefix: &str,
    ) -> Result<Vec<Declaration>, DomainError>;

    /// Insert a new, active declaration.
    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        draft: DeclarationDraft,
    ) -> Result<Declaration, DomainError>;

    /// Update the metadata that may change in place, stamping `updated_at`.
    async fn update_metadata<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        metadata: DeclarationMetadata,
    ) -> Result<(), DomainError>;

    /// Move a declaration between `active` and `retired`, stamping
    /// `last_change_at` and `updated_at`.
    async fn set_status<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        status: &str,
    ) -> Result<(), DomainError>;

    /// Fetch one declaration by id, within the caller's scope and visibility.
    ///
    /// The visibility predicate is applied here rather than by the caller: a
    /// gated declaration must be indistinguishable from an absent one, and a
    /// repository that returned the row and left the filtering to a service
    /// would make that a decision each call site could forget.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        visibility: &DomainVisibility,
        id: Uuid,
    ) -> Result<Option<Declaration>, DomainError>;

    /// List declarations for the caller.
    ///
    /// # Errors
    /// [`DomainError::Validation`] when the query names an unmapped field, uses
    /// an unsupported operator, or carries an undecodable cursor.
    async fn list<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        visibility: &DomainVisibility,
        query: &ODataQuery,
    ) -> Result<Page<Declaration>, DomainError>;
}
