//! Client implementation for the rules `AuthZ` resolver plugin.

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverError, AuthZResolverPluginClient, EvaluationRequest, EvaluationResponse,
};

use super::service::Service;

#[async_trait]
impl AuthZResolverPluginClient for Service {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(Self::evaluate(self, &request))
    }
}
