//! The admission client: the surface enforcing gears link.
//!
//! Registered in `ClientHub` without scope.

use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::error::AdmissionError;
use crate::models::{AdmissionRequest, Verdict};

/// Admission client.
///
/// The caller's `SecurityContext` is the **only** source of the subject and
/// the subject's tenant. Any `Err` is a refusal: never proceed on an error.
#[async_trait]
pub trait AdmissionClientV1: Send + Sync {
    /// Admits or refuses one intended operation.
    ///
    /// # Errors
    ///
    /// An [`AdmissionError`] when the call itself cannot be served
    /// (unauthenticated context, malformed identifiers); the caller must
    /// treat it as a refusal.
    async fn admit(
        &self,
        ctx: &SecurityContext,
        request: &AdmissionRequest,
    ) -> Result<Verdict, AdmissionError>;
}
