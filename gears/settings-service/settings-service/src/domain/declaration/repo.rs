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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// Surrogate identity, stable across a re-key.
    pub id: Uuid,

    /// The full setting key.
    pub key: String,

    /// The setting's own name slug, unique within its category.
    pub leaf_slug: String,

    /// GTS id of the value type. The key's left half, carried separately so a
    /// caller need not split the key to learn it.
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
}

/// Read operations on declarations.
#[async_trait]
pub trait DeclarationRepository: Send + Sync {
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
