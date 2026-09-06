// Created: 2026-09-06 by Constructor Tech
//! The value repository contract, as far as declaration paths need it.

use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Persistence of stored values.
///
/// Resolution and writes add their own operations in later entries; what is
/// here is what a declaration change needs.
#[async_trait]
pub trait ValueRepository: Send + Sync {
    /// Re-sync the denormalized `data_classification` of every value row of a
    /// declaration whose classification changed — in the caller's transaction,
    /// so no window exists in which the two disagree.
    async fn resync_classification<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        data_classification: &str,
    ) -> Result<u64, DomainError>;
}
