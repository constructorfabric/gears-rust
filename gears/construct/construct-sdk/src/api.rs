//! `ConstructClientV1` trait definition.
//!
//! All methods take a `SecurityContext` for authorization and access control.

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::models::{FoundationNote, NewFoundationNote};

/// Public API trait of the Construct gear (Version 1), registered in
/// `ClientHub` by the gear:
/// ```ignore
/// let construct = hub.get::<dyn ConstructClientV1>()?;
/// ```
#[async_trait]
pub trait ConstructClientV1: Send + Sync {
    /// Create a note in the caller's tenant.
    async fn create_note(
        &self,
        ctx: &SecurityContext,
        note: NewFoundationNote,
    ) -> Result<FoundationNote, CanonicalError>;

    /// Read one note of the caller's tenant.
    async fn get_note(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<FoundationNote, CanonicalError>;
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod api_tests;
