//! Grant management: `issue`, `bind`, `unbind`, reads (DESIGN §3.1, §3.3).

use std::sync::Arc;

use authz_resolver_sdk::pep::ResourceType;
use authz_resolver_sdk::{AccessRequest, PolicyEnforcer};
use gts::GtsTypeId;
use license_enforcement_sdk::dto::IssueResult;
use license_enforcement_sdk::error::{precondition, reason};
use license_enforcement_sdk::limits::{
    MAX_GRANTS_PER_ISSUE, MAX_ID_LEN, MAX_IDEMPOTENCY_TOKEN_LEN, MAX_RULES_LEN, MAX_TYPE_ID_LEN,
};
use license_enforcement_sdk::models::{
    Binding, BindingCounts, Grant, GrantState, GrantTerms, Pack, SubjectRef, Validity, WindowInput,
};
use license_enforcement_sdk::{
    AdmissionError, AdmissionInput, AdmittedRoot, CallerIdentity, EvidenceId,
    GrantAdmissionPluginV1, GrantEvaluatorV1, GrantId, Intake, PackId, RejectionReason,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tenant_resolver_sdk::{IsAncestorOptions, TenantId, TenantResolverClient, TenantResolverError};
use time::{Duration, OffsetDateTime};
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::FieldViolation;
use toolkit_db::DBProvider;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::crypto::hash::Sha256;
use toolkit_security::{SecurityContext, pep_properties};
use uuid::Uuid;

use super::check::Material;
use super::error::{DomainError, Missing};
use super::repo::{GrantRepo, NewEvidenceRow};
use super::types::{Schemas, TypeResolver, conditions_schema, recipient_schema};

mod resources {
    use super::{ResourceType, pep_properties};

    pub const GRANT: ResourceType = ResourceType::from_static(
        "license_enforcement.grant",
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    pub const PACK: ResourceType = ResourceType::from_static(
        "license_enforcement.pack",
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );
}

mod actions {
    pub const ISSUE: &str = "issue";
    pub const GET: &str = "get";
    pub const LIST: &str = "list";
    pub const BIND: &str = "bind";
    pub const UNBIND: &str = "unbind";
}

/// Hex SHA-256 of the canonical JSON of an operation and its payload.
fn payload_hash(op: &str, payload: &impl Serialize) -> Result<String, DomainError> {
    let bytes = serde_json::to_vec(&(op, payload))
        .map_err(|e| DomainError::Internal(format!("cannot hash the request: {e}")))?;
    Ok(hex::encode(Sha256::digest(&bytes)))
}

fn too_large(field: impl Into<String>, len: usize, max: usize, out: &mut Vec<FieldViolation>) {
    if len > max {
        out.push(FieldViolation::new(
            field,
            format!("{len} is over the limit of {max}"),
            reason::TOO_LARGE,
        ));
    }
}

/// The token is required: an empty one would take the replay key of the
/// tenant for good.
/// A given id or token is not empty and has no NUL, which PostgreSQL does not
/// store. An absent id is not checked here.
fn text_violations(field: &str, value: &str, out: &mut Vec<FieldViolation>) {
    if value.is_empty() {
        out.push(FieldViolation::new(
            field,
            "must not be empty",
            reason::REQUIRED,
        ));
    } else if value.contains('\0') {
        out.push(FieldViolation::new(
            field,
            "must not contain NUL",
            reason::INVALID_CHARACTER,
        ));
    }
}

fn token_violations(token: &str, out: &mut Vec<FieldViolation>) {
    text_violations("idempotency_token", token, out);
    too_large(
        "idempotency_token",
        token.len(),
        MAX_IDEMPOTENCY_TOKEN_LEN,
        out,
    );
}

fn subject_size(field: &str, gts_type: &GtsTypeId, id: &str, out: &mut Vec<FieldViolation>) {
    too_large(
        format!("{field}.type"),
        gts_type.as_ref().len(),
        MAX_TYPE_ID_LEN,
        out,
    );
    too_large(format!("{field}.id"), id.len(), MAX_ID_LEN, out);
    text_violations(&format!("{field}.id"), id, out);
}

/// Violations of the size limits, checked before any other work.
fn size_violations(token: &str, grants: &[GrantTerms]) -> Result<(), DomainError> {
    let mut v = Vec::new();
    token_violations(token, &mut v);
    if grants.is_empty() {
        v.push(FieldViolation::new(
            "grants",
            "no grant to issue",
            reason::REQUIRED,
        ));
    }
    too_large("grants", grants.len(), MAX_GRANTS_PER_ISSUE, &mut v);
    for (i, t) in grants.iter().enumerate().take(MAX_GRANTS_PER_ISSUE) {
        let f = format!("grants[{i}]");
        too_large(
            format!("{f}.resource.type"),
            t.resource.gts_type.as_ref().len(),
            MAX_TYPE_ID_LEN,
            &mut v,
        );
        too_large(
            format!("{f}.resource.id"),
            t.resource.id.as_ref().map_or(0, String::len),
            MAX_ID_LEN,
            &mut v,
        );
        if let Some(id) = &t.resource.id {
            text_violations(&format!("{f}.resource.id"), id, &mut v);
        }
        too_large(
            format!("{f}.subject_type"),
            t.subject_type.as_ref().len(),
            MAX_TYPE_ID_LEN,
            &mut v,
        );
        if let Some(s) = &t.fixed_subject {
            subject_size(&format!("{f}.fixed_subject"), &s.gts_type, &s.id, &mut v);
        }
        for (name, rules) in [
            ("conditions", &t.conditions),
            ("recipient_rules", &t.recipient_rules),
        ] {
            if let Some(rules) = rules {
                too_large(
                    format!("{f}.{name}"),
                    rules.0.to_string().len(),
                    MAX_RULES_LEN,
                    &mut v,
                );
            }
        }
    }
    if v.is_empty() {
        Ok(())
    } else {
        Err(DomainError::InvalidArgument(v))
    }
}

/// The window as the store keeps it: in UTC and to the microsecond. A time
/// outside what RFC 3339 writes stays as it is, for `window_violations`.
fn stored_window(w: &mut WindowInput) {
    let stored = |t: OffsetDateTime| {
        t.checked_to_offset(time::UtcOffset::UTC)
            .filter(|u| (0..=9999).contains(&u.year()))
            .and_then(|u| {
                u.replace_nanosecond(u.nanosecond() - u.nanosecond() % 1000)
                    .ok()
            })
            .unwrap_or(t)
    };
    w.not_before = stored(w.not_before);
    w.expires_at = stored(w.expires_at);
    w.grace_until = w.grace_until.map(stored);
}

fn window_violations(
    field: &str,
    terms: &GrantTerms,
    now: OffsetDateTime,
    out: &mut Vec<FieldViolation>,
) {
    let w = &terms.window;
    let in_range = [Some(w.not_before), Some(w.expires_at), w.grace_until]
        .into_iter()
        .flatten()
        // RFC 3339 writes only years 0..=9999.
        .all(|t| {
            t.checked_to_offset(time::UtcOffset::UTC)
                .is_some_and(|u| (0..=9999).contains(&u.year()))
        });
    if !in_range || w.not_before >= w.expires_at || w.grace_until.is_some_and(|g| g < w.expires_at)
    {
        out.push(FieldViolation::new(
            format!("{field}.window"),
            "need not_before < expires_at <= grace_until, each representable in UTC",
            reason::INVALID_WINDOW,
        ));
    } else if w.grace_until.unwrap_or(w.expires_at) <= now {
        out.push(FieldViolation::new(
            format!("{field}.window"),
            "the window has already ended",
            reason::INVALID_WINDOW,
        ));
    }
}

fn root_grant(root: &AdmittedRoot, pack_id: Option<PackId>, evidence_id: EvidenceId) -> Grant {
    let terms = &root.fields.terms;
    let (binding, binding_fixed) = match &terms.fixed_subject {
        Some(subject) => (
            Binding::Subject {
                subject: subject.clone().into(),
            },
            true,
        ),
        None => (Binding::None, false),
    };
    Grant {
        id: root.id,
        holder_tenant_id: root.fields.holder_tenant_id,
        resource: terms.resource.clone().into(),
        subject_type: terms.subject_type.clone(),
        binding,
        binding_fixed,
        validity: Validity::Own {
            window: terms.window.into(),
        },
        conditions: terms.conditions.clone(),
        permissions: terms.permissions.into(),
        recipient_rules: terms.recipient_rules.clone(),
        parent_grant_id: None,
        delegation_id: None,
        pack_id,
        evidence_id,
        state: if terms.permissions.requires_acceptance {
            GrantState::AwaitingAcceptance
        } else {
            GrantState::Active
        },
        termination_reason: None,
        version: 1,
    }
}

fn pack_of(id: PackId, holder: Uuid, grants: &[Grant]) -> Pack {
    let mut counts: Vec<BindingCounts> = Vec::new();
    for g in grants {
        let i = counts
            .iter()
            .position(|c| c.resource == g.resource)
            .unwrap_or_else(|| {
                counts.push(BindingCounts {
                    resource: g.resource.clone(),
                    unbound: 0,
                    bound: 0,
                    delegated: 0,
                });
                counts.len() - 1
            });
        match g.binding {
            Binding::Subject { .. } => counts[i].bound += 1,
            Binding::Delegated { .. } => counts[i].delegated += 1,
            _ => counts[i].unbound += 1,
        }
    }
    // The order of the store: resource type, then resource id.
    counts.sort_by(|a, b| {
        (a.resource.gts_type.as_ref(), &a.resource.id)
            .cmp(&(b.resource.gts_type.as_ref(), &b.resource.id))
    });
    Pack {
        id,
        holder_tenant_id: holder,
        counts,
        version: 1,
    }
}

/// A bind or an unbind.
#[domain_model]
#[derive(Debug, Clone, Serialize)]
pub enum BindingChange {
    Bind(SubjectRef),
    Unbind,
}

#[domain_model]
pub struct GrantService<R: GrantRepo> {
    db: Arc<DBProvider<DomainError>>,
    store: R,
    enforcer: PolicyEnforcer,
    hub: Arc<ClientHub>,
    types: TypeResolver,
    evaluator: Arc<dyn GrantEvaluatorV1>,
    plugin: Arc<dyn GrantAdmissionPluginV1>,
    material: Material<R>,
    idempotency_window: Duration,
}

impl<R: GrantRepo> GrantService<R> {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        db: Arc<DBProvider<DomainError>>,
        store: R,
        enforcer: PolicyEnforcer,
        hub: Arc<ClientHub>,
        evaluator: Arc<dyn GrantEvaluatorV1>,
        plugin: Arc<dyn GrantAdmissionPluginV1>,
        material: Material<R>,
        idempotency_window: Duration,
    ) -> Self {
        Self {
            db,
            store,
            enforcer,
            hub: Arc::clone(&hub),
            types: TypeResolver::new(hub),
            evaluator,
            plugin,
            material,
            idempotency_window,
        }
    }

    #[must_use]
    pub fn plugin(&self) -> &Arc<dyn GrantAdmissionPluginV1> {
        &self.plugin
    }

    /// The stored response of a replay, `None` for a first call.
    async fn replayed<T: DeserializeOwned>(
        &self,
        ctx: &SecurityContext,
        token: &str,
        hash: &str,
    ) -> Result<Option<T>, DomainError> {
        let conn = self.db.conn()?;
        let stored = self
            .store
            .idempotent_result(
                &conn,
                ctx.subject_tenant_id(),
                token,
                OffsetDateTime::now_utc(),
            )
            .await?;
        match stored {
            None => Ok(None),
            Some((stored_hash, _)) if stored_hash != hash => Err(DomainError::IdempotencyConflict),
            Some((_, response)) => serde_json::from_value(response)
                .map(Some)
                .map_err(|e| DomainError::Internal(format!("stored response: {e}"))),
        }
    }

    /// A failed write whose token a concurrent request with the same payload
    /// committed meanwhile returns that stored response instead of the failure.
    async fn or_replayed<T: DeserializeOwned>(
        &self,
        ctx: &SecurityContext,
        token: &str,
        hash: &str,
        result: Result<T, DomainError>,
    ) -> Result<T, DomainError> {
        match result {
            Err(e) => match self.replayed(ctx, token, hash).await {
                Ok(Some(stored)) => Ok(stored),
                Err(DomainError::IdempotencyConflict) => Err(DomainError::IdempotencyConflict),
                _ => Err(e),
            },
            ok => ok,
        }
    }

    /// The holder is the tenant of `ctx` or its descendant.
    async fn check_holder(&self, ctx: &SecurityContext, holder: Uuid) -> Result<(), DomainError> {
        let own = ctx.subject_tenant_id();
        if holder == own {
            return Ok(());
        }
        let resolver = self
            .hub
            .get::<dyn TenantResolverClient>()
            .map_err(|e| DomainError::DependencyUnavailable(format!("tenant-resolver: {e}")))?;
        // A holder that the resolver does not find or does not show gets the
        // same answer as one outside the subtree.
        match resolver
            .is_ancestor(
                ctx,
                TenantId(own),
                TenantId(holder),
                &IsAncestorOptions::default(),
            )
            .await
        {
            Ok(true) => Ok(()),
            Ok(false)
            | Err(TenantResolverError::TenantNotFound { .. } | TenantResolverError::Unauthorized) => {
                Err(DomainError::invalid(
                    "holder_tenant_id",
                    "the holder is not the caller's tenant or its descendant",
                    reason::HOLDER_NOT_IN_SUBTREE,
                ))
            }
            Err(e) => Err(DomainError::DependencyUnavailable(format!(
                "tenant-resolver: {e}"
            ))),
        }
    }

    async fn validate_terms(&self, grants: &[GrantTerms]) -> Result<(), DomainError> {
        let mut violations = Vec::new();
        let mut seen = Schemas::new();
        let now = OffsetDateTime::now_utc();
        for (i, terms) in grants.iter().enumerate() {
            let field = format!("grants[{i}]");
            window_violations(&field, terms, now, &mut violations);
            self.types
                .validate_terms(&field, terms, &mut violations, &mut seen)
                .await?;
            if let Some(subject) = &terms.fixed_subject {
                self.types
                    .validate_subject_of(
                        &format!("{field}.fixed_subject.type"),
                        &terms.subject_type,
                        &subject.gts_type,
                        &mut violations,
                        &mut seen,
                    )
                    .await?;
            }
            for (name, rules, schema) in [
                (
                    "conditions",
                    &terms.conditions,
                    conditions_schema(terms, &seen),
                ),
                (
                    "recipient_rules",
                    &terms.recipient_rules,
                    Some(recipient_schema()),
                ),
            ] {
                if let (Some(rules), Some(schema)) = (rules, schema) {
                    for mut v in self.evaluator.validate(rules, &schema) {
                        v.field = format!("{field}.{name}{}", v.field);
                        violations.push(v);
                    }
                }
            }
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(DomainError::InvalidArgument(violations))
        }
    }

    /// Issue roots through the `issue` intake.
    ///
    /// # Errors
    ///
    /// See DESIGN §3.3, Error model.
    pub async fn issue(
        &self,
        ctx: &SecurityContext,
        holder: Uuid,
        mut grants: Vec<GrantTerms>,
        as_pack: bool,
        token: String,
    ) -> Result<IssueResult, DomainError> {
        if !self.plugin.intakes().issue {
            return Err(DomainError::IntakeNotServed);
        }
        size_violations(&token, &grants)?;
        // ponytail: `accept` is not built; a grant that awaits it could never be
        // accepted or removed.
        if grants.iter().any(|t| t.permissions.requires_acceptance) {
            return Err(DomainError::NotImplemented("requires_acceptance"));
        }
        // Before the hash: RFC 3339 does not write an offset with seconds.
        for t in &mut grants {
            stored_window(&mut t.window);
        }
        let hash = payload_hash(
            if as_pack { "issue_pack" } else { "issue_grant" },
            &(&holder, &grants),
        )?;
        let scope = self
            .enforcer
            .access_scope_with(
                ctx,
                &resources::GRANT,
                actions::ISSUE,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, holder),
            )
            .await?;
        if let Some(result) = self.replayed(ctx, &token, &hash).await? {
            return Ok(result);
        }
        self.check_holder(ctx, holder).await?;

        let caller = CallerIdentity {
            subject_id: ctx.subject_id(),
            subject_type: ctx.subject_type().map(str::to_owned),
            tenant_id: ctx.subject_tenant_id(),
        };
        let instant = OffsetDateTime::now_utc();
        let mut admission = self
            .plugin
            .admit(
                AdmissionInput::Issue {
                    holder_tenant_id: holder,
                    grants,
                    as_pack,
                    caller,
                    idempotency_token: token.clone(),
                },
                instant,
                &self.material,
            )
            .await
            .map_err(|e| match e {
                AdmissionError::Rejected(r) => DomainError::EvidenceRejected(r),
                other => DomainError::DependencyUnavailable(other.to_string()),
            })?;
        let evidence = admission
            .evidence
            .ok_or_else(|| DomainError::Internal("the plugin admitted no evidence".to_owned()))?;
        if admission.roots.is_empty() {
            return Err(DomainError::Internal(
                "the evidence names no root".to_owned(),
            ));
        }
        // The pack and the evidence keep one holder: the one of the request.
        if admission
            .roots
            .iter()
            .any(|r| r.fields.holder_tenant_id != holder)
        {
            return Err(DomainError::Internal(
                "the plugin admitted a root for another holder".to_owned(),
            ));
        }
        // What is stored is what is checked and returned: the roots of the
        // plugin, in the order of the request.
        for r in &mut admission.roots {
            stored_window(&mut r.fields.terms.window);
        }
        let terms: Vec<GrantTerms> = admission
            .roots
            .iter()
            .map(|r| r.fields.terms.clone())
            .collect();
        self.validate_terms(&terms).await?;
        let plugin_id = self.plugin.info().id;

        let evidence_id = EvidenceId(Uuid::now_v7());
        let pack_id = admission.as_pack.then(|| PackId(Uuid::now_v7()));
        let roots: Vec<Grant> = admission
            .roots
            .iter()
            .map(|r| root_grant(r, pack_id, evidence_id))
            .collect();
        let pack = pack_id.map(|id| pack_of(id, holder, &roots));
        let result = IssueResult {
            evidence_id: Some(evidence_id),
            pack,
            grants: roots,
        };
        let response = serde_json::to_value(&result)
            .map_err(|e| DomainError::Internal(format!("cannot store the response: {e}")))?;

        let tenant = ctx.subject_tenant_id();
        let expires_at = OffsetDateTime::now_utc() + self.idempotency_window;
        let store = self.store;
        let tx_result = result.clone();
        let material = admission.material;
        let tx_token = token.clone();
        let tx_hash = hash.clone();
        let committed = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    let now = OffsetDateTime::now_utc();
                    // For `issue` the replay key is the tenant and the token: a
                    // replay is a token reused after its window.
                    if let Some(key) = &evidence.replay_key
                        && store.replay_key_used(tx, plugin_id, key).await?
                    {
                        return Err(DomainError::invalid(
                            "idempotency_token",
                            "the token already issued grants and its idempotency window is over; use a new token",
                            reason::TOKEN_REUSED,
                        ));
                    }
                    if let Some(external_id) = &evidence.external_id
                        && store
                            .evidence_by_external_id(tx, plugin_id, external_id)
                            .await?
                            .is_some()
                    {
                        return Err(DomainError::EvidenceRejected(RejectionReason::Replay));
                    }
                    let replaces = match &evidence.replaces {
                        None => None,
                        Some(external_id) => {
                            match store
                                .evidence_by_external_id(tx, plugin_id, external_id)
                                .await?
                            {
                                None => {
                                    return Err(DomainError::EvidenceChainBroken(
                                        precondition::REPLACED_EVIDENCE_MISSING,
                                    ));
                                }
                                Some((_, true)) => {
                                    return Err(DomainError::EvidenceChainBroken(
                                        precondition::EVIDENCE_ALREADY_REPLACED,
                                    ));
                                }
                                // ponytail: supersession of the roots of the replaced
                                // evidence (DESIGN §3.1) is not built; until then the
                                // gear refuses instead of keeping both sets of roots live.
                                Some((_, false)) => {
                                    return Err(DomainError::NotImplemented(
                                        "evidence supersession",
                                    ));
                                }
                            }
                        }
                    };
                    store
                        .insert_evidence(
                            tx,
                            &scope,
                            NewEvidenceRow {
                                id: evidence_id,
                                holder,
                                plugin_id,
                                intake: Intake::Issue,
                                bytes: evidence.bytes,
                                replay_key: evidence.replay_key,
                                external_id: evidence.external_id,
                                replaces,
                            },
                            now,
                        )
                        .await?;
                    if let Some(pack) = &tx_result.pack {
                        store.insert_pack(tx, &scope, pack.id, holder, now).await?;
                    }
                    for g in &tx_result.grants {
                        store.insert_root(tx, &scope, g, now).await?;
                    }
                    if !store.apply_material(tx, plugin_id, &material).await? {
                        return Err(DomainError::AdmissionConflict);
                    }
                    store
                        .record_result(tx, tenant, &tx_token, tx_hash, response, expires_at, now)
                        .await
                })
            })
            .await;
        self.or_replayed(ctx, &token, &hash, committed.map(|()| result))
            .await
    }

    /// Bind a subject to a grant, or unbind it.
    ///
    /// # Errors
    ///
    /// See DESIGN §3.3, Error model.
    pub async fn change_binding(
        &self,
        ctx: &SecurityContext,
        id: GrantId,
        change: BindingChange,
        token: String,
        expected_version: Option<u64>,
    ) -> Result<Grant, DomainError> {
        let (op, action) = match change {
            BindingChange::Bind(_) => ("bind", actions::BIND),
            BindingChange::Unbind => ("unbind", actions::UNBIND),
        };
        let mut violations = Vec::new();
        token_violations(&token, &mut violations);
        if let BindingChange::Bind(subject) = &change {
            subject_size("subject", &subject.gts_type, &subject.id, &mut violations);
        }
        if !violations.is_empty() {
            return Err(DomainError::InvalidArgument(violations));
        }
        let hash = payload_hash(op, &(&id, &change, &expected_version))?;

        let scope = self
            .enforcer
            .access_scope(ctx, &resources::GRANT, action, Some(id.0))
            .await?;
        if let Some(grant) = self.replayed(ctx, &token, &hash).await? {
            return Ok(grant);
        }
        let conn = self.db.conn()?;
        let current = self
            .store
            .grant(&conn, &scope, id, false)
            .await?
            .ok_or_else(|| not_found(Missing::Grant, id.0))?;
        // The subject type of a grant never changes, so the prefetched one holds.
        if let BindingChange::Bind(subject) = &change {
            let mut violations = Vec::new();
            self.types
                .validate_subject_of(
                    "subject.type",
                    &current.subject_type,
                    &subject.gts_type,
                    &mut violations,
                    &mut Schemas::new(),
                )
                .await?;
            if !violations.is_empty() {
                return Err(DomainError::InvalidArgument(violations));
            }
        }

        let store = self.store;
        let tenant = ctx.subject_tenant_id();
        let expires_at = OffsetDateTime::now_utc() + self.idempotency_window;
        let tx_token = token.clone();
        let tx_hash = hash.clone();
        let committed = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    let mut g = store
                        .grant(tx, &scope, id, true)
                        .await?
                        .ok_or_else(|| not_found(Missing::Grant, id.0))?;
                    if expected_version.is_some_and(|v| v != g.version) {
                        return Err(DomainError::VersionMismatch);
                    }
                    // ponytail: a root is its own root until delegation exists;
                    // then the state of the root is read here too.
                    if g.state == GrantState::Terminated {
                        return Err(DomainError::InvalidTransition(
                            "the grant is terminated".into(),
                        ));
                    }
                    if g.binding_fixed {
                        return Err(DomainError::InvalidTransition(
                            "the binding was fixed at admission".into(),
                        ));
                    }
                    if !g.permissions.may_bind {
                        return Err(DomainError::InvalidTransition(
                            "the grant does not permit bind".into(),
                        ));
                    }
                    g.binding = match (change, &g.binding) {
                        (BindingChange::Bind(subject), Binding::None) => {
                            Binding::Subject { subject }
                        }
                        (BindingChange::Unbind, Binding::Subject { .. }) => Binding::None,
                        (BindingChange::Bind(_), _) => {
                            return Err(DomainError::InvalidTransition(
                                "the grant is bound".into(),
                            ));
                        }
                        (BindingChange::Unbind, _) => {
                            return Err(DomainError::InvalidTransition(
                                "the grant is not bound to a subject".into(),
                            ));
                        }
                    };
                    g.version += 1;
                    let now = OffsetDateTime::now_utc();
                    store.update_binding(tx, &scope, &g, now).await?;
                    let response = serde_json::to_value(&g).map_err(|e| {
                        DomainError::Internal(format!("cannot store the response: {e}"))
                    })?;
                    store
                        .record_result(tx, tenant, &tx_token, tx_hash, response, expires_at, now)
                        .await?;
                    Ok(g)
                })
            })
            .await;
        self.or_replayed(ctx, &token, &hash, committed).await
    }

    /// # Errors
    ///
    /// `NotFound` outside the caller's scope.
    pub async fn get_grant(
        &self,
        ctx: &SecurityContext,
        id: GrantId,
    ) -> Result<Grant, DomainError> {
        let scope = self
            .enforcer
            .access_scope(ctx, &resources::GRANT, actions::GET, Some(id.0))
            .await?;
        let conn = self.db.conn()?;
        self.store
            .grant(&conn, &scope, id, false)
            .await?
            .ok_or_else(|| not_found(Missing::Grant, id.0))
    }

    /// # Errors
    ///
    /// `NotFound` outside the caller's scope.
    pub async fn get_pack(&self, ctx: &SecurityContext, id: PackId) -> Result<Pack, DomainError> {
        let scope = self
            .enforcer
            .access_scope(ctx, &resources::PACK, actions::GET, Some(id.0))
            .await?;
        let conn = self.db.conn()?;
        self.store
            .pack(&conn, &scope, id)
            .await?
            .ok_or_else(|| not_found(Missing::Pack, id.0))
    }

    /// # Errors
    ///
    /// A malformed query, or a PDP denial.
    pub async fn list_grants(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<Grant>, DomainError> {
        let scope = self
            .enforcer
            .access_scope(ctx, &resources::GRANT, actions::LIST, None)
            .await?;
        let conn = self.db.conn()?;
        self.store.list_grants(&conn, &scope, query).await
    }
}

fn not_found(what: Missing, id: Uuid) -> DomainError {
    DomainError::NotFound {
        what,
        id: id.to_string(),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "grants_tests.rs"]
mod grants_tests;
