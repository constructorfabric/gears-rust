//! The gear's readiness check: whether this instance can actually serve the
//! roles its deployment mode gives it.
//!
//! Lives in `infra/` rather than `domain/` because what it inspects is
//! infrastructure wiring - the `Storage` handles `serve()` fills in - and
//! `domain/` takes no infra dependency (`DESIGN.md` §1.3).
//!
//! It exists because the listener starts accepting traffic before that wiring
//! is done. The start phase *spawns* `serve()` and returns immediately, so
//! routes are published while `start_workers` is still resolving the cluster
//! cache and starting the outbox. Without this check `/readyz` reports `Ready`
//! throughout that window and a publish landing in it gets a `503` from a pod
//! the platform had already put into rotation.

use std::sync::Arc;

use async_trait::async_trait;
use toolkit::{Healthcheck, HealthcheckResult};

use crate::config::DeploymentMode;
use crate::infra::storage::Storage;

/// Reports `Starting` until every handle this instance's roles need is wired.
pub struct EventBrokerReadiness {
    storage: Arc<Storage>,
    mode: DeploymentMode,
}

impl EventBrokerReadiness {
    #[must_use]
    pub fn new(storage: Arc<Storage>, mode: DeploymentMode) -> Self {
        Self { storage, mode }
    }
}

#[async_trait]
impl Healthcheck for EventBrokerReadiness {
    fn name(&self) -> &'static str {
        "event-broker-readiness"
    }

    async fn check(&self) -> HealthcheckResult {
        // Each role gates only its own wiring. A dispatcher-only instance
        // installs neither and is ready as soon as its routes are up; gating
        // on both would keep it out of rotation forever.
        if self.mode.ingest_active() && !self.storage.outbox_installed() {
            return HealthcheckResult::unhealthy("ingest outbox pipeline not started")
                .with_code("starting");
        }
        if self.mode.delivery_active() && !self.storage.cache_installed() {
            return HealthcheckResult::unhealthy("cluster cache not wired").with_code("starting");
        }
        HealthcheckResult::healthy()
    }
}

#[cfg(test)]
#[path = "health_tests.rs"]
mod health_tests;
