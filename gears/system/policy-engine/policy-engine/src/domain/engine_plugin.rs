//! Admission-control engine plugin adapter.
//!
//! [`PolicyEngineAdmissionPlugin`] implements
//! `admission_control_sdk::AdmissionEnginePluginClientV1` over the
//! [`DecisionService`]. A permit maps to `Permit`, a denial to `Deny` carrying
//! the reason code and every denying document, both with the shadow denials;
//! a failure to the matching `EngineFailure`. Failure details are fixed
//! strings, never caller-supplied values.

use std::sync::Arc;

use admission_control_sdk::{
    AdmissionEnginePluginClientV1, AdmissionEnginePluginSpecV1, EngineFailure, EngineRequest,
    EngineResult, PolicyReference,
};
use async_trait::async_trait;
use gts::GtsInstanceId;
use policy_engine_sdk::ADMISSION_ENGINE_INSTANCE_ID;
use toolkit_gts::PluginV1;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

use super::decision::{DecisionFailure, DecisionService, Verdict};
use super::eval::EvaluatedDocumentKey;

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
        match self.service.decide(ctx, request).await {
            Ok(Verdict::Permit { shadow_denials }) => Ok(EngineResult::Permit {
                shadow_denials: references(&shadow_denials),
            }),
            Ok(Verdict::Deny {
                reason_code,
                denials,
                shadow_denials,
            }) => Ok(EngineResult::Deny {
                reason_code: reason_code.to_owned(),
                denials: references(&denials),
                shadow_denials: references(&shadow_denials),
            }),
            Err(failure) => Err(map_failure(failure)),
        }
    }
}

fn references(keys: &[EvaluatedDocumentKey]) -> Vec<PolicyReference> {
    keys.iter()
        .map(|key| PolicyReference {
            bundle_id: key.bundle_id.0,
            version_id: key.version_id.0,
            document_id: key.document_id.0,
            document_name: key.document_name.clone(),
        })
        .collect()
}

fn map_failure(failure: DecisionFailure) -> EngineFailure {
    match failure {
        DecisionFailure::InvalidRequest(detail) => EngineFailure::invalid_request(detail),
        DecisionFailure::Timeout(dependency) => {
            EngineFailure::timeout(format!("{dependency} timed out"))
        }
        DecisionFailure::Unavailable(dependency) => {
            EngineFailure::unavailable(format!("{dependency} unavailable"))
        }
        DecisionFailure::Internal(detail) => EngineFailure::internal(detail),
    }
}

fn instance_segment() -> &'static str {
    ADMISSION_ENGINE_INSTANCE_ID
        .strip_prefix(<AdmissionEnginePluginSpecV1 as gts::GtsSchema>::TYPE_ID)
        .unwrap_or(ADMISSION_ENGINE_INSTANCE_ID)
}

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
    PluginV1::<AdmissionEnginePluginSpecV1>::build_registration(
        instance_segment(),
        vendor,
        priority,
    )
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "engine_plugin_tests.rs"]
mod engine_plugin_tests;
