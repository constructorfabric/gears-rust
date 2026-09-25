// Created: 2026-09-06 by Virtuozzo International GmbH
//! Persistence port over `setting_values`.

use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::{StoredValue, ValueDraft};
use crate::domain::error::DomainError;

/// Persistence operations on stored values.
///
/// Every read here is over the **subject-less** track: rows carrying a
/// `(subject_type, subject_id)` pair belong to the subject dimension and are
/// never returned to a request that named no subject.
#[async_trait]
pub trait ValueRepository: Send + Sync {
    /// Re-sync the denormalized classification on every value row of a
    /// declaration, returning how many rows changed.
    ///
    /// # Errors
    /// [`DomainError`] when the update fails.
    async fn resync_classification<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        data_classification: &str,
    ) -> Result<u64, DomainError>;

    /// The rows of one declaration whose tenant is in `tenant_ids` — the
    /// cascading walk's one exact-match set query, never a prefix scan.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn find_in_tenants<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_ids: &[Uuid],
    ) -> Result<Vec<StoredValue>, DomainError>;

    /// The row of one declaration at exactly one tenant.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    /// Every stored row of one declaration, at any scope.
    ///
    /// What an upgrade migration carries across and what a reactivation
    /// re-validates: both have to see the rows without knowing the tenants.
    ///
    /// # Errors
    /// [`DomainError`] when the database cannot answer.
    async fn find_all<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
    ) -> Result<Vec<StoredValue>, DomainError>;

    /// Every stored row of one declaration, locked for update for the rest of
    /// the caller's transaction.
    ///
    /// What an upgrade copies to the successor: a locking read returns the
    /// latest committed rows whatever snapshot the transaction started from,
    /// and holds them against a concurrent writer until the copy commits.
    ///
    /// # Errors
    /// [`DomainError`] when the database cannot answer.
    async fn lock_all<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
    ) -> Result<Vec<StoredValue>, DomainError>;

    /// Flag one row for review with the detail that explains it, or clear the
    /// flag when `detail` is `None`.
    ///
    /// The detail travels with the flag in both directions: a flagged row says
    /// why, and clearing the flag clears the reason with it. No row at `id`
    /// within `scope` is [`DomainError::NotFound`], never silent success.
    // @cpt-dod:cpt-cf-settings-service-dod-typed-value-validation-needs-review:p1
    ///
    /// # Errors
    /// [`DomainError`] when the database cannot answer.
    async fn flag<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        detail: Option<String>,
    ) -> Result<(), DomainError>;

    async fn find_one<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<Option<StoredValue>, DomainError>;

    /// How many rows are flagged for review under declarations from `source`,
    /// across every tenant — what the needs-review gauge reports.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn count_flagged<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        source: &str,
    ) -> Result<u64, DomainError>;

    /// The rows flagged for review among `declaration_ids`, at any of
    /// `tenant_ids` — the administrative needs-review listing — in
    /// (declaration, tenant) order and at most `limit + 1` of them, so the
    /// caller can tell a full answer from one the bound cut.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn list_flagged<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_ids: &[Uuid],
        tenant_ids: &[Uuid],
        limit: usize,
    ) -> Result<Vec<StoredValue>, DomainError>;

    /// Insert a row.
    ///
    /// # Errors
    /// [`DomainError::PreconditionFailed`] when a row already exists at the
    /// scope — the caller compared the absent-state tag, and a row that
    /// appeared since is the other writer's; [`DomainError`] when the write
    /// fails.
    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        draft: ValueDraft,
    ) -> Result<StoredValue, DomainError>;

    /// Replace a row's value or secret reference, stamping `last_change_at`
    /// and `updated_at` and clearing `needs_review`: a valid re-set is what
    /// clears the flag.
    ///
    /// The write applies to the row at `expected` — the `last_change_at` the
    /// caller compared the tag against — alone.
    ///
    /// # Errors
    /// [`DomainError::PreconditionFailed`] when no row is at that version any
    /// more, moved or gone; [`DomainError`] when the write fails.
    // The row's identity, what it takes, who set it and the version it must
    // be at: a struct would only rename the same eight facts.
    #[allow(clippy::too_many_arguments)]
    async fn update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        value: Option<serde_json::Value>,
        secret_ref: Option<String>,
        set_by: &str,
        expected: time::OffsetDateTime,
    ) -> Result<StoredValue, DomainError>;

    /// Delete the row of one pair at the version the caller compared the tag
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
