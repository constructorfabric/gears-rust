//! In-process admission client: [`AdmissionClientV1`] over the
//! [`AdmissionService`], registered in `ClientHub` without scope.

use std::sync::Arc;

use admission_control_sdk::{AdmissionClientV1, AdmissionError, AdmissionRequest, Verdict};
use async_trait::async_trait;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

use crate::domain::service::AdmissionService;

/// The local admission client.
#[domain_model]
#[derive(Debug, Clone)]
pub struct AdmissionLocalClient {
    service: Arc<AdmissionService>,
}

impl AdmissionLocalClient {
    /// A client delegating to `service`.
    #[must_use]
    pub fn new(service: Arc<AdmissionService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl AdmissionClientV1 for AdmissionLocalClient {
    async fn admit(
        &self,
        ctx: &SecurityContext,
        request: &AdmissionRequest,
    ) -> Result<Verdict, AdmissionError> {
        self.service.admit(ctx, request).await
    }
}
