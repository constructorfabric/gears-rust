// Created: 2026-08-13 by Constructor Tech
//! The category repository contract.
//!
//! Declared in the domain and implemented in `infra`, so the service depends on
//! what it needs rather than on `SeaORM`. That is what lets the no-orphan guard
//! and the concurrency check be tested without a database.
//!
//! Every method is generic over [`DBRunner`], so the same code runs against a
//! plain connection and inside a transaction. That matters for the mutations:
//! a create must check uniqueness and insert in one transaction, and a delete
//! must check for referencing declarations and remove in one, or two
//! administrators racing can both pass a check that is no longer true when
//! they write.

use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use toolkit_odata::{ODataQuery, Page};

use super::CategoryKey;
use super::visibility::DomainVisibility;
use crate::domain::error::DomainError;

/// A category as the domain sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    /// Surrogate identity, stable across a rename.
    pub id: Uuid,
    /// The stable slug that becomes a setting key's category segment.
    pub key: CategoryKey,
    /// Display name, globally unique because categories are flat.
    pub name: String,
    /// Optional long-form description.
    pub description: Option<String>,
    /// Optional domain used to filter listings.
    pub domain_affinity: Option<String>,
    /// Ordering weight; lower sorts first.
    pub sort_order: i32,
    /// Optional icon reference.
    pub icon: Option<String>,
    /// The entity tag derived from the row's last write, used by `If-Match`.
    pub etag: crate::api::precondition::ETag,
}

/// What a caller may change on an existing category.
///
/// `key` is absent by construction, not by convention: a category's key is the
/// `<category>` segment of every setting declared under it, so changing it in
/// place would re-key each of them with no cascade and no tombstone
/// (DESIGN.md "Stale keys resolve as `NotFound`"). Making the field unreachable
/// from the update path is what keeps that from being one typo away.
#[derive(Debug, Clone)]
pub struct CategoryPatch {
    /// Display name.
    pub name: String,
    /// Optional long-form description.
    pub description: Option<String>,
    /// Optional domain affinity.
    pub domain_affinity: Option<String>,
    /// Ordering weight.
    pub sort_order: i32,
    /// Optional icon reference.
    pub icon: Option<String>,
}

/// What a create supplies.
///
/// Separate from [`Category`] because a caller cannot set `id` or `etag`: both
/// are the service's to assign, and accepting them would let a client claim an
/// identity or forge a precondition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryDraft {
    /// The stable slug.
    pub key: CategoryKey,
    /// Display name.
    pub name: String,
    /// Optional long-form description.
    pub description: Option<String>,
    /// Optional domain affinity.
    pub domain_affinity: Option<String>,
    /// Ordering weight.
    pub sort_order: i32,
    /// Optional icon reference.
    pub icon: Option<String>,
}

/// Persistence operations on categories.
#[async_trait]
pub trait CategoryRepository: Send + Sync {
    /// Fetch one category by id, within the caller's scope.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Category>, DomainError>;

    /// Fetch one category by its key, within the caller's scope.
    ///
    /// Used by the uniqueness check, which is why it is a lookup rather than a
    /// filtered list: `uq_category_key` is the authority, and this only tells
    /// the caller so before the database has to.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn find_by_key<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: &CategoryKey,
    ) -> Result<Option<Category>, DomainError>;

    /// Insert a new category.
    ///
    /// # Errors
    /// [`DomainError::Conflict`] when the key or name is already taken —
    /// surfaced from the unique index rather than guessed, so a race between
    /// two creates is decided by the database and not by whichever checked
    /// first.
    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        draft: CategoryDraft,
    ) -> Result<Category, DomainError>;

    /// Update an existing category, returning the refreshed row.
    ///
    /// # Errors
    /// [`DomainError::Conflict`] on a uniqueness collision;
    /// [`DomainError::NotFound`] when the id no longer exists.
    async fn update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        patch: CategoryPatch,
    ) -> Result<Category, DomainError>;

    /// Remove a category.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] when the id no longer exists.
    async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<(), DomainError>;

    /// List categories, filtered and paginated.
    ///
    /// `visibility` is applied **inside** the query alongside the caller's
    /// scope, never to the returned page: filtering afterwards yields short
    /// pages and a cursor that skips rows the caller was entitled to.
    ///
    /// Ordered by `sort_order` then `name` so the cursor is deterministic.
    ///
    /// # Errors
    /// [`DomainError::Validation`] when the query references an unmapped field
    /// or carries an undecodable cursor; [`DomainError`] when the read fails.
    async fn list<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        visibility: &DomainVisibility,
        query: &ODataQuery,
    ) -> Result<Page<Category>, DomainError>;

    /// Whether any declaration still references this category.
    ///
    /// Counts **retired** declarations too: a retired declaration keeps its
    /// `category_id`, so deleting the category out from under it would leave a
    /// dangling reference that no longer resolves — and retired declarations
    /// are exactly the ones nobody is looking at.
    ///
    /// # Errors
    /// [`DomainError`] when the check fails. A failure denies the delete: the
    /// guard exists to prevent an orphan, and an unanswerable check is not a
    /// negative answer.
    async fn count_referencing_declarations<C: DBRunner>(
        &self,
        conn: &C,
        id: Uuid,
    ) -> Result<u64, DomainError>;
}
