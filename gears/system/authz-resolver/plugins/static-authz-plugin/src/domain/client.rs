// Updated: 2026-04-07 by Constructor Tech
//! Client implementation for the static `AuthZ` resolver plugin.

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
        Ok(self.evaluate(&request))
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
            |request| async move { Ok(self.evaluate(&request)) },
        )
        .await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod client_tests;
