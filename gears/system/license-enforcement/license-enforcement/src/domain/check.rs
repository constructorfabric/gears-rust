//! The check: the evaluation order of DESIGN §3.1.

use std::sync::Arc;

use async_trait::async_trait;
use license_enforcement_sdk::models::{Grant, GrantState, TerminationReason};
use license_enforcement_sdk::reason::{
    DIAG_GRACE, DIAG_GRANT_ID, DIAG_REASON, DIAG_TERMINATION_REASON,
};
use license_enforcement_sdk::{
    DenialReason, Evaluation, GrantAdmissionPluginV1, GrantEvaluatorV1, GrantId, MaterialError,
    PluginMaterial, RevocationState, StoredEvidence, VerifyOutcome,
};
use license_resolver_sdk::{LicenseCheckRequest, LicenseDecision};
use time::OffsetDateTime;
use toolkit_db::DBProvider;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;

use super::error::DomainError;
use super::repo::{Candidate, GrantRepo};

/// The outcome of a check.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub granted: bool,
    pub reason: Option<DenialReason>,
    pub termination_reason: Option<TerminationReason>,
    pub grant_id: Option<GrantId>,
    pub grace: bool,
}

impl Decision {
    fn granted(grant_id: GrantId, grace: bool) -> Self {
        Self {
            granted: true,
            reason: None,
            termination_reason: None,
            grant_id: Some(grant_id),
            grace,
        }
    }

    fn denied(reason: DenialReason, termination_reason: Option<TerminationReason>) -> Self {
        Self {
            granted: false,
            reason: Some(reason),
            termination_reason,
            grant_id: None,
            grace: false,
        }
    }

    #[must_use]
    pub fn into_license_decision(self) -> LicenseDecision {
        let mut d = LicenseDecision::new(self.granted);
        if let Some(reason) = &self.reason {
            d = d.with_diagnostic(DIAG_REASON, reason.as_wire());
        }
        if let Some(t) = self.termination_reason
            && let Ok(v) = serde_json::to_value(t)
        {
            d = d.with_diagnostic(DIAG_TERMINATION_REASON, v);
        }
        if let Some(id) = self.grant_id {
            d = d.with_diagnostic(DIAG_GRANT_ID, id.to_string());
        }
        if self.grace {
            d = d.with_diagnostic(DIAG_GRACE, true);
        }
        d
    }
}

/// The denial of the candidate that reached the latest step. The first
/// candidate wins a tie.
#[derive(Default)]
struct LatestDenial(Option<(u8, Decision)>);

impl LatestDenial {
    fn record(&mut self, step: u8, decision: Decision) {
        if self.0.as_ref().is_none_or(|(s, _)| step > *s) {
            self.0 = Some((step, decision));
        }
    }
}

/// Read access of the linked plugin to its own material.
#[domain_model]
#[derive(Clone)]
pub struct Material<R: GrantRepo> {
    db: Arc<DBProvider<DomainError>>,
    store: R,
    plugin_id: &'static str,
}

impl<R: GrantRepo> Material<R> {
    #[must_use]
    pub fn new(db: Arc<DBProvider<DomainError>>, store: R, plugin_id: &'static str) -> Self {
        Self {
            db,
            store,
            plugin_id,
        }
    }
}

#[async_trait]
impl<R: GrantRepo> PluginMaterial for Material<R> {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, MaterialError> {
        let conn = self.db.conn().map_err(|e| MaterialError(e.to_string()))?;
        self.store
            .material(&conn, self.plugin_id, key)
            .await
            .map_err(|e| MaterialError(e.to_string()))
    }
}

#[domain_model]
pub struct CheckService<R: GrantRepo> {
    db: Arc<DBProvider<DomainError>>,
    store: R,
    evaluator: Arc<dyn GrantEvaluatorV1>,
    plugin: Arc<dyn GrantAdmissionPluginV1>,
    material: Material<R>,
}

impl<R: GrantRepo> CheckService<R> {
    #[must_use]
    pub fn new(
        db: Arc<DBProvider<DomainError>>,
        store: R,
        evaluator: Arc<dyn GrantEvaluatorV1>,
        plugin: Arc<dyn GrantAdmissionPluginV1>,
    ) -> Self {
        let material = Material::new(Arc::clone(&db), store, plugin.info().id);
        Self {
            db,
            store,
            evaluator,
            plugin,
            material,
        }
    }

    /// The plugin material handle that this service writes through.
    #[must_use]
    pub fn material(&self) -> Material<R> {
        self.material.clone()
    }

    /// The decision of `request` at `instant`. A fault is an error, never a
    /// decision.
    ///
    /// # Errors
    ///
    /// Store or plugin faults.
    pub async fn check(
        &self,
        request: &LicenseCheckRequest,
        instant: OffsetDateTime,
    ) -> Result<Decision, DomainError> {
        let tenant = request.context.tenant_id;
        let resource_type = &request.resource.gts_type;
        let resource_id = request.resource.id.as_deref();
        // No grant holds a NUL, and PostgreSQL rejects it as a query parameter.
        let texts = [
            resource_type.as_ref(),
            request.subject.gts_type.as_ref(),
            resource_id.unwrap_or_default(),
            request.subject.id.as_deref().unwrap_or_default(),
        ];
        if texts.iter().any(|t| t.contains('\0')) {
            return Ok(Decision::denied(DenialReason::NoGrantForResource, None));
        }
        let filters = self.evaluator.candidate_filter(request);
        let conn = self.db.conn()?;

        let candidates = self
            .store
            .candidates(&conn, tenant, resource_type, resource_id, &filters, instant)
            .await?;
        if candidates.is_empty() {
            if self
                .store
                .any_live_on_resource(&conn, tenant, resource_type, resource_id, instant)
                .await?
            {
                return Ok(Decision::denied(DenialReason::SubjectNotBound, None));
            }
            let last = self
                .store
                .last_ended(&conn, tenant, resource_type, resource_id, &filters, instant)
                .await?;
            return Ok(match last {
                Some(g) if g.state == GrantState::Terminated => {
                    Decision::denied(DenialReason::Terminated, g.termination_reason)
                }
                Some(g) if g.validity.window().grace_until.is_some() => {
                    Decision::denied(DenialReason::GraceExpired, None)
                }
                Some(_) => Decision::denied(DenialReason::Expired, None),
                None => Decision::denied(DenialReason::NoGrantForResource, None),
            });
        }

        let roots = self.store.roots(&conn, &candidates).await?;
        let mut latest = LatestDenial::default();
        let mut passing: Vec<(&Candidate, &Grant, bool)> = Vec::new();
        for c in &candidates {
            let root = roots
                .get(&c.root)
                .ok_or_else(|| DomainError::Internal(format!("root {} is missing", c.root)))?;
            match self.evaluator.evaluate(request, &c.grant, root, instant) {
                Evaluation::Pass => passing.push((c, root, false)),
                Evaluation::PassInGrace => passing.push((c, root, true)),
                Evaluation::Deny(reason) => {
                    let termination = root.termination_reason.or(c.grant.termination_reason);
                    latest.record(2, Decision::denied(reason.into(), termination));
                }
                // `Evaluation` is non-exhaustive: an outcome this gear does not know denies.
                _ => latest.record(2, Decision::denied(DenialReason::ChainInvalid, None)),
            }
        }

        for (c, root, grace) in passing {
            let Some(evidence) = self.own_evidence(&conn, root).await? else {
                latest.record(3, Decision::denied(DenialReason::ChainInvalid, None));
                continue;
            };
            let verification = self
                .plugin
                .verify(root.id, &evidence, instant, &self.material)
                .await
                .map_err(|e| DomainError::DependencyUnavailable(e.to_string()))?;
            if !verification.material.is_empty() {
                self.write_material_after_verify(&verification.material)
                    .await;
            }
            match verification.outcome {
                VerifyOutcome::Verified(None) => return Ok(Decision::granted(c.grant.id, grace)),
                VerifyOutcome::Revoked(RevocationState::Suspended) => {
                    latest.record(3, Decision::denied(DenialReason::Suspended, None));
                }
                VerifyOutcome::Revoked(_) => {
                    latest.record(3, Decision::denied(DenialReason::Terminated, None));
                }
                VerifyOutcome::Denied(reason) => {
                    latest.record(3, Decision::denied(reason.into(), None));
                }
                // Root fields from `verify` need the chain checks of DESIGN §3.1
                // step 3, which come with the verifying plugin. Until then, fail closed.
                _ => {
                    return Err(DomainError::Internal(
                        "the linked plugin returned root fields, which this gear does not check yet"
                            .to_owned(),
                    ));
                }
            }
        }

        Ok(latest.0.map_or_else(
            || Decision::denied(DenialReason::NoGrantForResource, None),
            |(_, d)| d,
        ))
    }

    /// The evidence of `root`, or `None` when another plugin admitted it: the
    /// linked plugin cannot vouch for it. The bytes are read only for a plugin
    /// that verifies evidence.
    async fn own_evidence(
        &self,
        conn: &impl DBRunner,
        root: &Grant,
    ) -> Result<Option<StoredEvidence>, DomainError> {
        let info = self.plugin.info();
        let evidence = self
            .store
            .evidence(conn, root.evidence_id, info.verifies_evidence)
            .await?;
        if evidence.plugin_id == info.id {
            return Ok(Some(evidence));
        }
        tracing::warn!(
            root_id = %root.id,
            evidence_plugin = %evidence.plugin_id,
            "evidence was admitted by another plugin"
        );
        Ok(None)
    }

    /// A failed write after `verify` does not change the decision. The writes
    /// apply all or none.
    async fn write_material_after_verify(&self, writes: &[license_enforcement_sdk::MaterialWrite]) {
        let store = self.store;
        let plugin_id = self.material.plugin_id;
        let writes = writes.to_vec();
        let result = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    if store.apply_material(tx, plugin_id, &writes).await? {
                        Ok(())
                    } else {
                        Err(DomainError::AdmissionConflict)
                    }
                })
            })
            .await;
        match result {
            Ok(()) => {}
            Err(DomainError::AdmissionConflict) => {
                tracing::info!("plugin material write after verify dropped: expectation not met");
            }
            Err(e) => tracing::warn!(error = %e, "plugin material write after verify failed"),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "check_tests.rs"]
mod check_tests;
