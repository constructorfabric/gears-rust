// Created: 2026-09-07 by Constructor Tech
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

    /// Insert or replace the pair's row, stamping `updated_at`.
    ///
    /// # Errors
    /// [`DomainError`] when the write fails.
    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        draft: RestrictionDraft,
    ) -> Result<Restriction, DomainError>;

    /// Delete the pair's row, reporting whether one existed.
    ///
    /// # Errors
    /// [`DomainError`] when the delete fails.
    async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<bool, DomainError>;
}
