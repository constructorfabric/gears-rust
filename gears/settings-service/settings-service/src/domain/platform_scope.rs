// Created: 2026-09-06 by Constructor Tech
//! Where platform scope comes from.
//!
//! Platform scope is the root tenant's id — never `NULL`, never a sentinel
//! (DESIGN.md §4.1, §4.7). The id is learned from the Tenant Resolver, a
//! consumed client that this gear fetches at first use and never during its own
//! init (DESIGN.md §4.9), so no service can hold the id at construction time.
//! Services hold this port instead and ask when they need it.

use async_trait::async_trait;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Supplies the root tenant's id, which *is* platform scope.
#[async_trait]
pub trait PlatformScope: Send + Sync {
    /// The root tenant's id.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when the Tenant Resolver cannot answer; a
    /// platform-scoped mutation that cannot name its scope must not proceed.
    async fn root_tenant(&self) -> Result<Uuid, DomainError>;
}
