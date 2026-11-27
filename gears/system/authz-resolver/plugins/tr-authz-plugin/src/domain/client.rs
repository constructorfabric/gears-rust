// Created: 2026-02-09 by Constructor Tech
// Updated: 2026-04-29 by Constructor Tech

//! Client implementation for the TR `AuthZ` resolver plugin.

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
        self.evaluate(&request).await
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
            |request| async move { self.evaluate(&request).await },
        )
        .await
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
