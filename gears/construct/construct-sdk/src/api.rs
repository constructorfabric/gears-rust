//! `ConstructClientV1` trait definition.
//!
//! All methods take a `SecurityContext` for authorization and access control.

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::models::RecordOutcome;

/// Public API trait of the Construct gear (Version 1), registered in
/// `ClientHub` by the gear:
/// ```ignore
/// let construct = hub.get::<dyn ConstructClientV1>()?;
/// ```
#[async_trait]
pub trait ConstructClientV1: Send + Sync {
    /// Hand Construct one connector record for `tenant_id`, the same operation
    /// as `POST /construct/v1/records`. The caller is the connector; the
    /// platform must authorize it for the tenant.
    ///
    /// # Errors
    ///
    /// A refused record is an invalid-argument error that names the record's
    /// type, the place in the record and the broken rule. A caller the
    /// platform does not authorize for the tenant gets a permission error.
    async fn submit_record(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        record: serde_json::Value,
    ) -> Result<RecordOutcome, CanonicalError>;
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod api_tests;
