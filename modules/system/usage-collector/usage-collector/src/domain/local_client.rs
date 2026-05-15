//! Local (in-process) client for the usage-collector gateway.
//!
//! Wraps a [`Service`] and translates [`DomainError`] to the canonical
//! [`UsageCollectorError`] for consumption via `ClientHub`.

use std::sync::Arc;

use async_trait::async_trait;
use modkit_macros::domain_model;
use usage_collector_sdk::{ModuleConfig, UsageCollectorClientV1, UsageCollectorError, UsageRecord};

use super::service::Service;

/// Local client wrapping the usage-collector service.
///
/// Registered in `ClientHub` by the usage-collector module during `init()` so
/// that the embedded emitter (and any other in-process consumer) can call the
/// gateway through the public `dyn UsageCollectorClientV1` trait.
#[domain_model]
pub struct UsageCollectorLocalClient {
    svc: Arc<Service>,
}

impl UsageCollectorLocalClient {
    #[must_use]
    pub fn new(svc: Arc<Service>) -> Self {
        Self { svc }
    }
}

// @cpt-algo:cpt-cf-usage-collector-algo-sdk-and-ingest-core-gateway-ingest-handler:p1
// @cpt-dod:cpt-cf-usage-collector-dod-sdk-and-ingest-core-gateway-crate:p1
#[async_trait]
impl UsageCollectorClientV1 for UsageCollectorLocalClient {
    async fn create_usage_record(&self, record: UsageRecord) -> Result<(), UsageCollectorError> {
        self.svc
            .create_usage_record(record)
            .await
            .map_err(Into::into)
    }

    async fn get_module_config(
        &self,
        module_name: &str,
    ) -> Result<ModuleConfig, UsageCollectorError> {
        self.svc.get_module_config(module_name).map_err(Into::into)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "local_client_tests.rs"]
mod local_client_tests;
