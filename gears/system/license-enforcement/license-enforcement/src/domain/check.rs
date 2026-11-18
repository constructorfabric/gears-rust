//! The check: the evaluation order of DESIGN §3.1.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use license_enforcement_sdk::models::{
    Binding, Grant, GrantState, TerminationReason, Validity, Window,
};
use license_enforcement_sdk::reason::{
    DIAG_GRACE, DIAG_GRANT_ID, DIAG_REASON, DIAG_TERMINATION_REASON,
};
use license_enforcement_sdk::{
    BindingFilter, DenialReason, Evaluation, EvidenceId, GrantAdmissionPluginV1, GrantEvaluatorV1,
    GrantId, MaterialError, PluginMaterial, RevocationState, RootFields, StoredEvidence,
    VerifyOutcome,
};
use license_resolver_sdk::{LicenseCheckRequest, LicenseDecision};
use time::OffsetDateTime;
use toolkit_db::DBProvider;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;

use super::error::DomainError;
use super::grants::permissions_within;
use super::repo::GrantRepo;
use super::types::{covers, filter_matches, type_key};

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
        let store = self.store;
        let evaluator = Arc::clone(&self.evaluator);
        let info = self.plugin.info();
        let (plugin_id, verifies) = (info.id, info.verifies_evidence);
        let request_owned = request.clone();
        let tx_filters = filters.clone();
        // One snapshot for every read: a `replace` that commits between two of
        // them would show a live candidate with a replaced root.
        let snapshot = self
            .db
            .transaction_with_config(self.store.snapshot_read(&self.db), move |tx| {
                Box::pin(async move {
                    let request = &request_owned;
                    read_snapshot(
                        tx,
                        store,
                        &*evaluator,
                        plugin_id,
                        verifies,
                        request,
                        &tx_filters,
                        instant,
                    )
                    .await
                })
            })
            .await?;
        let (passing, mut latest) = match snapshot {
            Snapshot::Decided(d) => return Ok(d),
            Snapshot::Passing(passing, latest) => (passing, latest),
        };

        // The roots of one `issue` share their evidence: the bytes read last
        // serve the next candidate with the same id.
        let mut read: Option<StoredEvidence> = None;
        for p in passing {
            let Some(header) = p.evidence else {
                latest.record(3, Decision::denied(DenialReason::ChainInvalid, None));
                continue;
            };
            // The bytes of one candidate at a time: the first that verifies
            // ends the check. Evidence does not change once stored.
            let evidence = if verifies {
                if read.as_ref().is_none_or(|e| e.id != header.id) {
                    let conn = self.db.conn()?;
                    read = self
                        .store
                        .evidence(&conn, &[header.id], true)
                        .await?
                        .remove(&header.id);
                }
                let Some(e) = read.as_ref() else {
                    latest.record(3, Decision::denied(DenialReason::ChainInvalid, None));
                    continue;
                };
                e
            } else {
                &header
            };
            let verification = self
                .plugin
                .verify(p.root.id, evidence, instant, &self.material)
                .await
                .map_err(|e| DomainError::DependencyUnavailable(e.to_string()))?;
            if !verification.material.is_empty() {
                self.write_material_after_verify(&verification.material)
                    .await;
            }
            match verification.outcome {
                VerifyOutcome::Verified(None) if !verifies => {
                    return Ok(Decision::granted(p.grant.id, p.grace));
                }
                // The stored fields are not trusted when the plugin verifies.
                VerifyOutcome::Verified(None) => {
                    return Err(DomainError::Internal(
                        "a plugin that verifies evidence returned no root fields".to_owned(),
                    ));
                }
                VerifyOutcome::Revoked(RevocationState::Suspended) => {
                    latest.record(3, Decision::denied(DenialReason::Suspended, None));
                }
                VerifyOutcome::Revoked(_) => {
                    latest.record(3, Decision::denied(DenialReason::Terminated, None));
                }
                VerifyOutcome::Denied(reason) => {
                    latest.record(3, Decision::denied(reason.into(), None));
                }
                VerifyOutcome::Verified(Some(fields)) => {
                    let shape = p.shape.as_ref().ok_or_else(|| {
                        DomainError::Internal(
                            "root fields from a plugin that does not verify evidence".to_owned(),
                        )
                    })?;
                    match self.against_fields(
                        request, &filters, &p.grant, &p.root, shape, &fields, instant,
                    ) {
                        Ok(fields_grace) => {
                            return Ok(Decision::granted(p.grant.id, p.grace || fields_grace));
                        }
                        Err(reason) => latest.record(3, Decision::denied(reason, None)),
                    }
                }
                // `VerifyOutcome` is non-exhaustive: an outcome this gear does not know denies.
                _ => latest.record(3, Decision::denied(DenialReason::ChainInvalid, None)),
            }
        }

        Ok(latest.0.map_or_else(
            || Decision::denied(DenialReason::NoGrantForResource, None),
            |(_, d)| d,
        ))
    }

    /// Step 3 of DESIGN §3.1 with the root fields from `verify`: `Ok(grace)`
    /// when the candidate holds against them, else the denial reason.
    #[expect(
        clippy::too_many_arguments,
        reason = "the request, the candidate, its tree and the evidence fields are all inputs of one step"
    )]
    fn against_fields(
        &self,
        request: &LicenseCheckRequest,
        filters: &[BindingFilter],
        candidate: &Grant,
        root: &Grant,
        shape: &Shape,
        fields: &RootFields,
        instant: OffsetDateTime,
    ) -> Result<bool, DenialReason> {
        // First: with a copy of a row the chain depends on which copy the
        // store read, and so would any later reason. No live row of the root:
        // the chain of live rows does not reach it.
        match shape.live_rows {
            1 => {}
            0 => return Err(DenialReason::ChainInvalid),
            _ => return Err(DenialReason::Duplicated),
        }
        let terms = &fields.terms;
        let field_window: Window = terms.window.into();
        let mut grace = false;
        // The store found the candidate through derived key columns, which the
        // signature does not cover: the request is matched again against the
        // root fields and the binding columns.
        let resource_ok = type_key(terms.resource.gts_type.as_ref())
            == type_key(request.resource.gts_type.as_ref())
            && match (&request.resource.id, &terms.resource.id) {
                (_, None) => true,
                (Some(asked), Some(id)) => asked == id,
                (None, Some(_)) => false,
            };
        let Binding::Subject { subject, .. } = &candidate.binding else {
            return Err(DenialReason::ChainInvalid);
        };
        if !resource_ok || !filters.iter().any(|f| filter_matches(f, subject)) {
            return Err(DenialReason::ChainInvalid);
        }
        if candidate.id == root.id {
            // A binding fixed by the evidence is that subject (I8); otherwise
            // the evidence permits bind.
            let bound = match &terms.fixed_subject {
                Some(f) => f.gts_type == subject.gts_type && f.id == subject.id,
                None => terms.permissions.may_bind,
            };
            if fields.holder_tenant_id != request.context.tenant_id
                || !covers(&terms.subject_type, &subject.gts_type)
                || !bound
            {
                return Err(DenialReason::ChainInvalid);
            }
            let mut as_fields = root.clone();
            as_fields.holder_tenant_id = fields.holder_tenant_id;
            as_fields.resource = terms.resource.clone().into();
            as_fields.subject_type = terms.subject_type.clone();
            as_fields.validity = Validity::Own {
                window: field_window,
            };
            as_fields.conditions.clone_from(&terms.conditions);
            as_fields.permissions = terms.permissions.into();
            match self
                .evaluator
                .evaluate(request, &as_fields, &as_fields, instant)
            {
                Evaluation::Pass => {}
                Evaluation::PassInGrace => grace = true,
                Evaluation::Deny(reason) => return Err(reason.into()),
                _ => return Err(DenialReason::ChainInvalid),
            }
        } else {
            let chain = &shape.chain;
            // The chain holds live rows only: a terminated link ends it before
            // the root.
            let rooted = chain
                .last()
                .is_some_and(|r| r.id == root.id && r.parent_grant_id.is_none())
                && fields.holder_tenant_id == root.holder_tenant_id;
            // A sub-grant has the window of its root, so its grace is the
            // grace of the root fields.
            let mut root_window = terms.window;
            crate::domain::grants::stored_window(&mut root_window);
            let subset = type_key(candidate.resource.gts_type.as_ref())
                == type_key(terms.resource.gts_type.as_ref())
                && candidate.resource.id == terms.resource.id
                && type_key(candidate.subject_type.as_ref())
                    == type_key(terms.subject_type.as_ref())
                && *candidate.validity.window() == Window::from(root_window)
                && terms
                    .conditions
                    .as_ref()
                    .is_none_or(|c| candidate.conditions.as_ref() == Some(c))
                && permissions_within(candidate.permissions, terms.permissions.into())
                && chain.iter().all(|g| g.evidence_id == root.evidence_id);
            // The binding of the sub-grant within what the evidence admits.
            let bound =
                terms.permissions.may_bind && covers(&terms.subject_type, &subject.gts_type);
            let depth = chain.len().saturating_sub(1);
            if !rooted
                || !subset
                || !bound
                || terms.fixed_subject.is_some()
                // Recipient rules are not evaluated, so no delegation carries them.
                || !crate::domain::grants::empty_rules(terms.recipient_rules.as_ref())
                || depth > usize::from(terms.permissions.max_depth)
            {
                return Err(DenialReason::ChainInvalid);
            }
            for (pair, children) in chain.windows(2).zip(&shape.live_children) {
                let (link, parent) = (&pair[0], &pair[1]);
                if !matches!(parent.binding, Binding::Delegated { grant_id, tenant_id, .. }
                        if grant_id == link.id && tenant_id == link.holder_tenant_id)
                    || *children != 1
                {
                    return Err(DenialReason::Duplicated);
                }
            }
        }
        Ok(grace)
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

/// What a check reads from the store, in one snapshot.
enum Snapshot {
    /// No candidate: the decision of the fallback.
    Decided(Decision),
    /// The candidates that pass step 2, and the latest denial of the others.
    Passing(Vec<Passing>, LatestDenial),
}

/// A candidate that passes step 2, with what step 3 reads.
struct Passing {
    grant: Grant,
    root: Grant,
    grace: bool,
    /// The evidence without its bytes; `None` when it is missing or another
    /// plugin admitted it.
    evidence: Option<StoredEvidence>,
    /// Read only for a plugin that verifies evidence: it returns root fields.
    shape: Option<Shape>,
}

/// The stored tree of a candidate, to hold it against root fields.
#[derive(Clone)]
struct Shape {
    /// The candidate, then each parent up to the root.
    chain: Vec<Grant>,
    /// The live children of each parent in `chain`, in its order.
    live_children: Vec<u64>,
    /// 1 when the root and each link of `chain` have one live row each, else
    /// the first other count. A store without its primary key can hold copies
    /// of any row (ADR-0002); a copy denies.
    live_rows: u64,
}

/// Steps 1 and 2 of DESIGN §3.1, and the rows that step 3 reads.
#[expect(
    clippy::too_many_arguments,
    reason = "the inputs of a check, moved into its snapshot transaction"
)]
async fn read_snapshot<R: GrantRepo>(
    tx: &impl DBRunner,
    store: R,
    evaluator: &dyn GrantEvaluatorV1,
    plugin_id: &str,
    verifies: bool,
    request: &LicenseCheckRequest,
    filters: &[BindingFilter],
    instant: OffsetDateTime,
) -> Result<Snapshot, DomainError> {
    let tenant = request.context.tenant_id;
    let resource_type = &request.resource.gts_type;
    let resource_id = request.resource.id.as_deref();
    let candidates = store
        .candidates(tx, tenant, resource_type, resource_id, filters, instant)
        .await?;
    if candidates.is_empty() {
        let d = fallback(tx, store, request, filters, instant).await?;
        return Ok(Snapshot::Decided(d));
    }

    let roots = store.roots(tx, &candidates).await?;
    let mut latest = LatestDenial::default();
    let mut passed = Vec::new();
    for c in candidates {
        // A broken link denies its candidate alone, as missing evidence does.
        let Some(root) = roots.get(&c.root) else {
            tracing::warn!(grant_id = %c.grant.id, root_grant_id = %c.root, "root is missing");
            latest.record(2, Decision::denied(DenialReason::ChainInvalid, None));
            continue;
        };
        let grace = match evaluator.evaluate(request, &c.grant, root, instant) {
            Evaluation::Pass => false,
            Evaluation::PassInGrace => true,
            Evaluation::Deny(reason) => {
                let termination = root.termination_reason.or(c.grant.termination_reason);
                latest.record(2, Decision::denied(reason.into(), termination));
                continue;
            }
            // `Evaluation` is non-exhaustive: an outcome this gear does not know denies.
            _ => {
                latest.record(2, Decision::denied(DenialReason::ChainInvalid, None));
                continue;
            }
        };
        passed.push((c.grant, root.clone(), grace));
    }
    // Step 3 reads for every passing candidate at once: a fixed number of
    // queries, however many candidates pass. The evidence bytes come later,
    // one candidate at a time.
    let evidence = own_evidence(tx, store, &passed, plugin_id).await?;
    let shapes = if verifies {
        shapes(tx, store, &passed, &evidence).await?
    } else {
        HashMap::new()
    };
    let passing = passed
        .into_iter()
        .map(|(grant, root, grace)| {
            let evidence = evidence.get(&root.evidence_id).cloned();
            // Rows that share an id share a shape, and the check denies them
            // as duplicated.
            let shape = shapes.get(&grant.id).cloned();
            Passing {
                grant,
                root,
                grace,
                evidence,
                shape,
            }
        })
        .collect();
    Ok(Snapshot::Passing(passing, latest))
}

/// The evidence of the roots of `passed` that the linked plugin admitted,
/// without the bytes. A missing row denies its candidates alone.
async fn own_evidence<R: GrantRepo>(
    tx: &impl DBRunner,
    store: R,
    passed: &[(Grant, Grant, bool)],
    plugin_id: &str,
) -> Result<HashMap<EvidenceId, StoredEvidence>, DomainError> {
    let ids: Vec<EvidenceId> = passed
        .iter()
        .map(|(_, root, _)| root.evidence_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut found = store.evidence(tx, &ids, false).await?;
    for id in ids.iter().filter(|id| !found.contains_key(id)) {
        tracing::warn!(evidence_id = %id, "evidence is missing");
    }
    found.retain(|id, e| {
        let own = e.plugin_id == plugin_id;
        if !own {
            tracing::warn!(
                evidence_id = %id,
                evidence_plugin = %e.plugin_id,
                "evidence was admitted by another plugin"
            );
        }
        own
    });
    Ok(found)
}

/// The stored tree of each passing candidate whose evidence the linked
/// plugin admitted, keyed by the candidate.
async fn shapes<R: GrantRepo>(
    tx: &impl DBRunner,
    store: R,
    passed: &[(Grant, Grant, bool)],
    evidence: &HashMap<EvidenceId, StoredEvidence>,
) -> Result<HashMap<GrantId, Shape>, DomainError> {
    let checked: Vec<&(Grant, Grant, bool)> = passed
        .iter()
        .filter(|(_, root, _)| evidence.contains_key(&root.evidence_id))
        .collect();
    let starts: Vec<(GrantId, GrantId)> = checked
        .iter()
        .filter(|(grant, root, _)| grant.id != root.id)
        .map(|(grant, root, _)| (grant.id, root.id))
        .collect();
    let chains = store.chains(tx, &starts).await?;
    let parents: Vec<GrantId> = chains
        .values()
        .flat_map(|chain| chain.iter().skip(1).map(|g| g.id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let children = store.live_children(tx, &parents).await?;
    let ids: Vec<GrantId> = checked
        .iter()
        .map(|(_, root, _)| root.id)
        .chain(chains.values().flatten().map(|g| g.id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let rows = store.live_rows(tx, &ids).await?;
    Ok(checked
        .into_iter()
        .map(|(grant, root, _)| {
            let chain = chains.get(&grant.id).cloned().unwrap_or_default();
            let live_children = chain
                .iter()
                .skip(1)
                .map(|p| children.get(&p.id).copied().unwrap_or(0))
                .collect();
            let live_rows = std::iter::once(root.id)
                .chain(chain.iter().map(|g| g.id))
                .map(|id| rows.get(&id).copied().unwrap_or(0))
                .find(|n| *n != 1)
                .unwrap_or(1);
            let shape = Shape {
                chain,
                live_children,
                live_rows,
            };
            (grant.id, shape)
        })
        .collect())
}

/// The decision when no candidate matches: the reason of the closest grant.
async fn fallback<R: GrantRepo>(
    tx: &impl DBRunner,
    store: R,
    request: &LicenseCheckRequest,
    filters: &[BindingFilter],
    instant: OffsetDateTime,
) -> Result<Decision, DomainError> {
    let tenant = request.context.tenant_id;
    let resource_type = &request.resource.gts_type;
    let resource_id = request.resource.id.as_deref();
    if store
        .any_live_on_resource(tx, tenant, resource_type, resource_id, instant)
        .await?
    {
        return Ok(Decision::denied(DenialReason::SubjectNotBound, None));
    }
    let last = store
        .last_ended(tx, tenant, resource_type, resource_id, filters, instant)
        .await?;
    Ok(match last {
        Some(g) if g.state == GrantState::Terminated => {
            Decision::denied(DenialReason::Terminated, g.termination_reason)
        }
        Some(g) if g.validity.window().grace_until.is_some() => {
            Decision::denied(DenialReason::GraceExpired, None)
        }
        Some(_) => Decision::denied(DenialReason::Expired, None),
        None => Decision::denied(DenialReason::NoGrantForResource, None),
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "check_tests.rs"]
mod check_tests;
