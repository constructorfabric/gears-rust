//! Readiness health check. `api-gateway` aggregates it into `/readyz` and
//! `/health`, so a failed bootstrap names its dependency to operators.
//!
//! Once bootstrap is ready, the check also polls the cluster SDK's readiness
//! contributor. That contributor re-validates the `quota-enforcement` profile
//! requirements when the resolve deferred them (a cold start against a remote
//! cluster) and reports a process with no cluster client wired at all, so a
//! build or configuration mistake cannot hide behind a lazy binding.

use std::sync::Arc;

use async_trait::async_trait;
use toolkit::{Healthcheck, HealthcheckResult, HealthcheckStatus};

use crate::domain::{Readiness, ReadinessState};

/// Check name shown in `/health`.
pub const CHECK_NAME: &str = "quota-enforcement-bootstrap";

const LOG_TARGET: &str = "qe.health";

/// Reports the bootstrap state, then the cluster requirements verdict.
pub struct ReadinessCheck {
    readiness: Arc<Readiness>,
    cluster: Option<Arc<dyn Healthcheck>>,
}

impl ReadinessCheck {
    /// Wrap the shared readiness cell and, when present, the cluster SDK's
    /// readiness contributor.
    #[must_use]
    pub fn new(readiness: Arc<Readiness>, cluster: Option<Arc<dyn Healthcheck>>) -> Self {
        Self { readiness, cluster }
    }

    async fn cluster_verdict(&self) -> HealthcheckResult {
        let Some(cluster) = &self.cluster else {
            return HealthcheckResult::healthy();
        };
        let verdict = cluster.check().await;
        if verdict.status != HealthcheckStatus::Healthy {
            // The detail stays in the log: `/health` is unauthenticated.
            tracing::warn!(
                target: LOG_TARGET,
                status = ?verdict.status,
                code = verdict.code.as_deref().unwrap_or_default(),
                detail = verdict.message.as_deref().unwrap_or_default(),
                "cluster requirements not met"
            );
        }
        match verdict.status {
            HealthcheckStatus::Healthy => HealthcheckResult::healthy(),
            HealthcheckStatus::Degraded => {
                HealthcheckResult::degraded("cluster degraded").with_code("qe_cluster_degraded")
            }
            HealthcheckStatus::Unhealthy => HealthcheckResult::unhealthy("cluster unavailable")
                .with_code("qe_cluster_unavailable"),
        }
    }
}

#[async_trait]
impl Healthcheck for ReadinessCheck {
    fn name(&self) -> &'static str {
        CHECK_NAME
    }

    // @cpt-flow:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1
    async fn check(&self) -> HealthcheckResult {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-probe-abort
        match self.readiness.snapshot() {
            ReadinessState::Ready => self.cluster_verdict().await,
            ReadinessState::Starting => HealthcheckResult::unhealthy("bootstrap in progress")
                .with_code("qe_bootstrap_pending"),
            // The reason stays in the bootstrap log: `/health` is unauthenticated.
            ReadinessState::Failed { dependency, .. } => {
                HealthcheckResult::unhealthy(format!("{dependency} unavailable"))
                    .with_code(format!("qe_{}_unavailable", dependency.as_label()))
            }
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-probe-abort
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "healthcheck_tests.rs"]
mod healthcheck_tests;
