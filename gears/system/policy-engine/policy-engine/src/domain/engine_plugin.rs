//! Admission-control engine plugin adapter.
//!
//! [`PolicyEngineAdmissionPlugin`] implements
//! `admission_control_sdk::AdmissionEnginePluginClientV1` over the
//! [`DecisionService`], which already answers in the admission-control
//! vocabulary: the plugin only forwards.

use std::sync::Arc;

use admission_control_sdk::{
    AdmissionEnginePluginClientV1, AdmissionEnginePluginSpecV1, EngineFailure, EngineRequest,
    EngineResult,
};
use async_trait::async_trait;
use gts::GtsInstanceId;
use toolkit_gts::PluginV1;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

use super::decision::DecisionService;

/// The policy engine as an admission engine plugin.
#[domain_model]
#[derive(Debug, Clone)]
pub struct PolicyEngineAdmissionPlugin {
    service: Arc<DecisionService>,
}

impl PolicyEngineAdmissionPlugin {
    /// A plugin over `service`.
    #[must_use]
    pub const fn new(service: Arc<DecisionService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl AdmissionEnginePluginClientV1 for PolicyEngineAdmissionPlugin {
    async fn evaluate(
        &self,
        ctx: &SecurityContext,
        request: &EngineRequest,
    ) -> Result<EngineResult, EngineFailure> {
        self.service.decide(ctx, request).await
    }
}

/// The policy engine's segment of its plugin instance id; the
/// admission-control spec supplies the rest.
const INSTANCE_SEGMENT: &str = "cf.core.policy_engine.engine.v1";

/// The engine plugin registration: the well-known instance id and the
/// serialized `PluginV1<AdmissionEnginePluginSpecV1>` document.
///
/// # Errors
///
/// A `serde_json` error only if the plugin spec fails to serialize, which
/// does not happen for the fixed, data-free spec this crate builds.
pub fn registration(
    vendor: &str,
    priority: i16,
) -> serde_json::Result<(GtsInstanceId, serde_json::Value)> {
    PluginV1::<AdmissionEnginePluginSpecV1>::build_registration(INSTANCE_SEGMENT, vendor, priority)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "engine_plugin_tests.rs"]
mod engine_plugin_tests;
