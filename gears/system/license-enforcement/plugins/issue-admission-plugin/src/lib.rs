//! Admission plugin of the license-enforcement gear for the `issue` intake.
//!
//! It admits grant parameters from an authorized caller and verifies nothing:
//! the rows of the gear store are the source of truth. The evidence keeps the
//! canonical form of the request and the identity of the caller.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

use std::sync::Arc;

use async_trait::async_trait;
use license_enforcement_sdk::models::GrantTerms;
use license_enforcement_sdk::{
    Admission, AdmissionError, AdmissionInput, AdmittedRoot, GrantAdmissionPluginV1, GrantId,
    Intakes, NewEvidence, PluginFault, PluginInfo, PluginMaterial, RejectionReason, RootFields,
    StoredEvidence, Verification, VerifyOutcome,
};
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

license_enforcement_sdk::register_admission_plugin!(|| Arc::new(IssueAdmissionPlugin));

/// Id of the plugin, stored with its evidence.
pub const PLUGIN_ID: &str = "cf.builtin.issue_admission";

/// The `issue` admission plugin.
pub struct IssueAdmissionPlugin;

/// Canonical form of an `issue` request. Field order is fixed by the struct.
#[derive(Serialize)]
struct IssueEvidence<'a> {
    caller_subject_id: Uuid,
    caller_subject_type: Option<&'a str>,
    caller_tenant_id: Uuid,
    holder_tenant_id: Uuid,
    as_pack: bool,
    roots: Vec<IssuedRoot<'a>>,
}

#[derive(Serialize)]
struct IssuedRoot<'a> {
    id: GrantId,
    terms: &'a GrantTerms,
}

#[async_trait]
impl GrantAdmissionPluginV1 for IssueAdmissionPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo::new(PLUGIN_ID, false, None)
    }

    fn intakes(&self) -> Intakes {
        Intakes::new(true, false)
    }

    async fn admit(
        &self,
        input: AdmissionInput,
        _instant: OffsetDateTime,
        _material: &dyn PluginMaterial,
    ) -> Result<Admission, AdmissionError> {
        let AdmissionInput::Issue {
            holder_tenant_id,
            grants,
            as_pack,
            caller,
            idempotency_token,
        } = input
        else {
            return Err(
                RejectionReason::Malformed("only the issue intake is served".to_owned()).into(),
            );
        };

        let ids: Vec<GrantId> = grants.iter().map(|_| GrantId(Uuid::now_v7())).collect();
        let bytes = serde_json::to_vec(&IssueEvidence {
            caller_subject_id: caller.subject_id,
            caller_subject_type: caller.subject_type.as_deref(),
            caller_tenant_id: caller.tenant_id,
            holder_tenant_id,
            as_pack,
            roots: ids
                .iter()
                .zip(&grants)
                .map(|(id, terms)| IssuedRoot { id: *id, terms })
                .collect(),
        })
        .map_err(|e| PluginFault(format!("cannot serialize evidence: {e}")))?;

        let mut evidence = NewEvidence::new(bytes);
        evidence.replay_key = Some(format!("{}:{idempotency_token}", caller.tenant_id));

        let mut admission = Admission::default();
        admission.evidence = Some(evidence);
        admission.as_pack = as_pack;
        admission.roots = ids
            .into_iter()
            .zip(grants)
            .map(|(id, terms)| AdmittedRoot::new(id, RootFields::new(holder_tenant_id, terms)))
            .collect();
        Ok(admission)
    }

    async fn verify(
        &self,
        _root_id: GrantId,
        _evidence: &StoredEvidence,
        _instant: OffsetDateTime,
        _material: &dyn PluginMaterial,
    ) -> Result<Verification, PluginFault> {
        // Trusted store: the stored fields apply, and the evidence is not parsed.
        Ok(Verification::new(VerifyOutcome::Verified(None)))
    }

    async fn health(&self) -> Result<(), PluginFault> {
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "lib_tests.rs"]
mod lib_tests;
