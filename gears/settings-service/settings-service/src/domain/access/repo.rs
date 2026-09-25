// Created: 2026-09-07 by Virtuozzo International GmbH
//! Persistence port over `tenant_permissions`.

use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::{Restriction, RestrictionDraft};
use crate::domain::error::DomainError;

/// Persistence operations on restriction rows.
#[async_trait]
pub trait AccessRepository: Send + Sync {
    /// The rows of one declaration whose tenant is in `tenant_ids` — the chain
    /// query, one exact-match set lookup.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn find_in_tenants<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_ids: &[Uuid],
    ) -> Result<Vec<Restriction>, DomainError>;

    /// The rows of several declarations whose tenant is in `tenant_ids`, for a
    /// page resolved against one chain.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn find_for_declarations<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_ids: &[Uuid],
        tenant_ids: &[Uuid],
    ) -> Result<Vec<Restriction>, DomainError>;

    /// The row for exactly one pair.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn find_one<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<Option<Restriction>, DomainError>;

    /// Insert or replace the pair's row, stamping `updated_at`, at the state
    /// the caller compared its tag against.
    ///
    /// `expected` is the stored row's `updated_at`, or `None` when the caller
    /// saw no row. With a version the row at that version is replaced; without
    /// one a row is inserted, and the unique index on the pair decides whether
    /// "no row" still holds.
    ///
    /// # Errors
    /// [`DomainError::PreconditionFailed`] when no row is at the version, or
    /// a row appeared where the caller saw none; [`DomainError`] when the
    /// write fails.
    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        draft: RestrictionDraft,
        expected: Option<time::OffsetDateTime>,
    ) -> Result<Restriction, DomainError>;

    /// Delete the pair's row at the version the caller compared its tag
    /// against.
    ///
    /// # Errors
    /// [`DomainError::PreconditionFailed`] when no row is at that version any
    /// more; [`DomainError`] when the delete fails.
    async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_id: Uuid,
        expected: time::OffsetDateTime,
    ) -> Result<(), DomainError>;
}
