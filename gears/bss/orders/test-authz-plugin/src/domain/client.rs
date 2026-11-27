//! Client implementation for the e2e policy `AuthZ` resolver plugin.

use std::num::NonZeroUsize;

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverError, AuthZResolverPluginClient, EvaluationRequest, EvaluationResponse,
    EvaluationsSemantic, Semaphore, evaluate_batch_concurrently,
};

use super::service::Service;

#[async_trait]
impl AuthZResolverPluginClient for Service {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(self.policy.evaluate(&request))
    }

    async fn evaluate_batch(
        &self,
        requests: Vec<EvaluationRequest>,
        semantic: EvaluationsSemantic,
        max_concurrency: NonZeroUsize,
        in_flight: &Semaphore,
    ) -> Result<Vec<EvaluationResponse>, AuthZResolverError> {
        // Local policy: fan out over `evaluate`, bounded and in request order,
        // so each result equals an independent single evaluation. The helper
        // holds an `in_flight` permit per item while it runs.
        evaluate_batch_concurrently(
            requests,
            semantic,
            max_concurrency,
            in_flight,
            |request| async move { Ok(self.policy.evaluate(&request)) },
        )
        .await
    }
}
