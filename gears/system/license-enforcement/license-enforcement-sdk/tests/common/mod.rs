//! Minimal plugin and evaluator for the link-time registration tests.

use async_trait::async_trait;
use gts::GtsTypeId;
use license_enforcement_sdk::models::{Conditions, Grant};
use license_enforcement_sdk::{
    Admission, AdmissionError, AdmissionInput, BindingFilter, Evaluation, EvaluationError,
    GrantAdmissionPluginV1, GrantEvaluatorV1, GrantId, Intakes, PluginFault, PluginInfo,
    PluginMaterial, StoredEvidence, Verification, VerifyOutcome,
};
use license_resolver_sdk::LicenseCheckRequest;
use time::OffsetDateTime;
use toolkit_canonical_errors::FieldViolation;

pub struct StubPlugin(pub &'static str);

#[async_trait]
impl GrantAdmissionPluginV1 for StubPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo::new(self.0, false, None)
    }

    fn intakes(&self) -> Intakes {
        Intakes::new(true, false)
    }

    async fn admit(
        &self,
        _input: AdmissionInput,
        _instant: OffsetDateTime,
        _material: &dyn PluginMaterial,
    ) -> Result<Admission, AdmissionError> {
        Ok(Admission::default())
    }

    async fn verify(
        &self,
        _root_id: GrantId,
        _evidence: &StoredEvidence,
        _instant: OffsetDateTime,
        _material: &dyn PluginMaterial,
    ) -> Result<Verification, PluginFault> {
        Ok(Verification::new(VerifyOutcome::Verified(None)))
    }

    async fn health(&self) -> Result<(), PluginFault> {
        Ok(())
    }
}

pub struct StubEvaluator;

impl GrantEvaluatorV1 for StubEvaluator {
    fn named_types(&self) -> Vec<GtsTypeId> {
        Vec::new()
    }

    fn candidate_filter(&self, _request: &LicenseCheckRequest) -> Vec<BindingFilter> {
        Vec::new()
    }

    fn evaluate(
        &self,
        _request: &LicenseCheckRequest,
        _candidate: &Grant,
        _root: &Grant,
        _instant: OffsetDateTime,
    ) -> Evaluation {
        Evaluation::Pass
    }

    fn validate(&self, _rules: &Conditions, _schema: &serde_json::Value) -> Vec<FieldViolation> {
        Vec::new()
    }

    fn matches(
        &self,
        _rules: &Conditions,
        _object: &serde_json::Value,
    ) -> Result<bool, EvaluationError> {
        Ok(true)
    }
}
