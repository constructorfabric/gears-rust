//! Grant management: `issue`, `bind`, `unbind`, reads (DESIGN §3.1, §3.3).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use authz_resolver_sdk::pep::ResourceType;
use authz_resolver_sdk::{AccessRequest, PolicyEnforcer};
use gts::GtsTypeId;
use license_enforcement_sdk::dto::{CheckExplainRequest, IssueResult};
use license_enforcement_sdk::error::{precondition, reason};
use license_enforcement_sdk::limits::{
    MAX_DELEGATION_DEPTH, MAX_GRANTS_PER_ISSUE, MAX_ID_LEN, MAX_IDEMPOTENCY_TOKEN_LEN,
    MAX_RULES_LEN, MAX_TYPE_ID_LEN,
};
use license_enforcement_sdk::models::{
    Binding, BindingCounts, Grant, GrantState, GrantTerms, Pack, SubjectRef, TerminationReason,
    Validity, WindowInput,
};
use license_enforcement_sdk::{
    AdmissionError, AdmissionInput, AdmittedRoot, CallerIdentity, EvidenceId,
    GrantAdmissionPluginV1, GrantEvaluatorV1, GrantId, Intake, MaterialWrite, NewEvidence, PackId,
    RequestedRoot,
};
use license_resolver_sdk::{LicenseCheckContext, LicenseCheckRequest};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tenant_resolver_sdk::{IsAncestorOptions, TenantId, TenantResolverClient, TenantResolverError};
use time::{Duration, OffsetDateTime};
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::FieldViolation;
use toolkit_db::{DBProvider, DbTx};
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::crypto::hash::Sha256;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use super::check::Material;
use super::error::{DomainError, Missing};
use super::repo::{Caller, GrantRepo, NewEvidenceRow};
use super::types::{Schemas, TypeResolver, conditions_schema};

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

    pub const DELEGATION: ResourceType = ResourceType::from_static(
        "license_enforcement.delegation",
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );
}

mod actions {
    pub const ISSUE: &str = "issue";
    pub const GET: &str = "get";
    pub const LIST: &str = "list";
    pub const BIND: &str = "bind";
    pub const UNBIND: &str = "unbind";
    pub const ACCEPT: &str = "accept";
    pub const REJECT: &str = "reject";
    pub const SUSPEND: &str = "suspend";
    pub const REINSTATE: &str = "reinstate";
    pub const REVOKE: &str = "revoke";
    pub const RENEW: &str = "renew";
    pub const RETURN: &str = "return";
    pub const RECALL: &str = "recall";
    pub const REPLACE: &str = "replace";
    pub const DELEGATE: &str = "delegate";
    pub const CHECK_EXPLAIN: &str = "check_explain";
    /// `get` and `list` of grants held below the caller's tenant: the policy
    /// gives it to the vendor, not to a grantor (PRD `fr-pdp-gated-management`).
    pub const READ_SUBTREE: &str = "read_subtree";
}

mod delegation;
mod lifecycle;
mod replace;

pub use lifecycle::Verb;
pub use replace::Target;

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

/// The token is required: an empty one would take the replay key of the
/// tenant and the subject for good.
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

/// How violations name the terms of a request: `grants[i]` in `issue`,
/// `successors[i].terms` in `replace`.
#[domain_model]
#[derive(Clone, Copy)]
pub(super) struct TermsField {
    list: &'static str,
    suffix: &'static str,
}

impl TermsField {
    pub(super) const ISSUE: Self = Self {
        list: "grants",
        suffix: "",
    };
    pub(super) const REPLACE: Self = Self {
        list: "successors",
        suffix: ".terms",
    };

    fn item(self, i: usize) -> String {
        format!("{}[{i}]{}", self.list, self.suffix)
    }
}

/// Violations of the size limits, checked before any other work.
fn size_violations(grants: &[GrantTerms], field: TermsField, v: &mut Vec<FieldViolation>) {
    if grants.is_empty() {
        v.push(FieldViolation::new(
            field.list,
            "no grant to issue",
            reason::REQUIRED,
        ));
    }
    too_large(field.list, grants.len(), MAX_GRANTS_PER_ISSUE, v);
    for (i, t) in grants.iter().enumerate().take(MAX_GRANTS_PER_ISSUE) {
        let f = field.item(i);
        too_large(
            format!("{f}.resource.type"),
            t.resource.gts_type.as_ref().len(),
            MAX_TYPE_ID_LEN,
            v,
        );
        too_large(
            format!("{f}.resource.id"),
            t.resource.id.as_ref().map_or(0, String::len),
            MAX_ID_LEN,
            v,
        );
        if let Some(id) = &t.resource.id {
            text_violations(&format!("{f}.resource.id"), id, v);
        }
        too_large(
            format!("{f}.subject_type"),
            t.subject_type.as_ref().len(),
            MAX_TYPE_ID_LEN,
            v,
        );
        if let Some(s) = &t.fixed_subject {
            subject_size(&format!("{f}.fixed_subject"), &s.gts_type, &s.id, v);
        }
        too_large(
            format!("{f}.permissions.max_depth"),
            usize::from(t.permissions.max_depth),
            MAX_DELEGATION_DEPTH,
            v,
        );
        for (name, rules) in [
            ("conditions", &t.conditions),
            ("recipient_rules", &t.recipient_rules),
        ] {
            if let Some(rules) = rules {
                too_large(
                    format!("{f}.{name}"),
                    rules.0.to_string().len(),
                    MAX_RULES_LEN,
                    v,
                );
            }
        }
    }
}

/// The window as the store keeps it: in UTC and to the microsecond. A time
/// outside what RFC 3339 writes stays as it is, for `window_violations`.
pub(crate) fn stored_window(w: &mut WindowInput) {
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

/// `field` is the path of the window in the request.
fn window_violations(
    field: &str,
    w: &WindowInput,
    now: OffsetDateTime,
    out: &mut Vec<FieldViolation>,
) {
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
            field,
            "need not_before < expires_at <= grace_until, each representable in UTC",
            reason::INVALID_WINDOW,
        ));
    } else if super::window::end(&license_enforcement_sdk::models::Window::from(*w)) <= now {
        out.push(FieldViolation::new(
            field,
            "the window has already ended",
            reason::INVALID_WINDOW,
        ));
    }
}

fn root_grant(root: &AdmittedRoot, pack_id: Option<PackId>, evidence_id: EvidenceId) -> Grant {
    let terms = &root.fields.terms;
    let (binding, binding_fixed) = match &terms.fixed_subject {
        Some(subject) => (Binding::subject(subject.clone().into()), true),
        None => (Binding::None, false),
    };
    let mut g = Grant::new(
        root.id,
        root.fields.holder_tenant_id,
        terms.resource.clone().into(),
        terms.subject_type.clone(),
        Validity::Own {
            window: terms.window.into(),
        },
        terms.permissions.into(),
        evidence_id,
    );
    g.binding = binding;
    g.binding_fixed = binding_fixed;
    g.conditions.clone_from(&terms.conditions);
    g.recipient_rules.clone_from(&terms.recipient_rules);
    g.pack_id = pack_id;
    if terms.permissions.requires_acceptance {
        g.state = GrantState::AwaitingAcceptance;
    }
    g
}

/// The scopes that the new rows of `issue` and `replace` are inserted under.
#[domain_model]
pub(super) struct Issued {
    grants: AccessScope,
    pack: Option<AccessScope>,
}

/// Whether the subject of a grant ended with `reason` loses the grant: the
/// termination clears the binding, and the store keeps the subject for the
/// reason a later check gives. `replace` also clears the binding of the
/// replaced grant, but moves it to the successor, so its subject keeps a grant.
pub(crate) fn subject_loses_grant(reason: TerminationReason) -> bool {
    reason == TerminationReason::Recalled
}

/// The count of its pack that a member is in. The one rule for the counts of
/// a pack: a new pack and every later write of a member count by it.
#[domain_model]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PackCount {
    /// Live, with no binding.
    Unbound,
    /// Live, bound to a subject.
    Bound,
    /// Live, bound to its sub-grant.
    Delegated,
}

impl PackCount {
    /// `None` when `member` is terminated.
    pub(crate) fn of(member: &Grant) -> Option<Self> {
        (member.state != GrantState::Terminated).then_some(match member.binding {
            Binding::Subject { .. } => Self::Bound,
            Binding::Delegated { .. } => Self::Delegated,
            _ => Self::Unbound,
        })
    }

    /// The counter of `c` that this count names.
    pub(crate) fn of_counts(self, c: &mut BindingCounts) -> &mut u64 {
        match self {
            Self::Unbound => &mut c.unbound,
            Self::Bound => &mut c.bound,
            Self::Delegated => &mut c.delegated,
        }
    }
}

fn pack_of(id: PackId, holder: Uuid, grants: &[Grant]) -> Pack {
    let mut counts: Vec<BindingCounts> = Vec::new();
    for g in grants {
        let i = counts
            .iter()
            .position(|c| c.resource == g.resource)
            .unwrap_or_else(|| {
                counts.push(BindingCounts::new(g.resource.clone()));
                counts.len() - 1
            });
        if let Some(k) = PackCount::of(g) {
            *k.of_counts(&mut counts[i]) += 1;
        }
    }
    // The order of the store: resource type, then resource id.
    counts.sort_by(|a, b| {
        (a.resource.gts_type.as_ref(), &a.resource.id)
            .cmp(&(b.resource.gts_type.as_ref(), &b.resource.id))
    });
    Pack::new(id, holder, counts, 1)
}

/// A bind or an unbind. Its serde names enter stored idempotency hashes.
#[domain_model]
#[derive(Debug, Clone, Serialize)]
pub enum BindingChange {
    #[serde(rename = "Bind")]
    Bind(SubjectRef),
    #[serde(rename = "Unbind")]
    Unbind,
}

/// `scope` allows a row of `tenant` with the id `id`. Decided in memory, for a
/// row that the store cannot filter by the tenant: a filter that only the
/// database can decide denies.
pub(super) fn admits(scope: &AccessScope, tenant: Uuid, id: Uuid) -> bool {
    admits_any(scope, tenant, Some(id))
}

/// Rules that are absent, `null`, or empty.
pub(crate) fn empty_rules(rules: Option<&license_enforcement_sdk::models::Conditions>) -> bool {
    match rules.map(|r| &r.0) {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::Object(m)) => m.is_empty(),
        Some(serde_json::Value::Array(a)) => a.is_empty(),
        Some(_) => false,
    }
}

/// I4 for permissions: `child` permits no more than `parent`, and delegates
/// less deep. Delegation, replacement, and check hold a sub-grant to it.
pub(crate) fn permissions_within(
    child: license_enforcement_sdk::models::Permissions,
    parent: license_enforcement_sdk::models::Permissions,
) -> bool {
    (!child.may_bind || parent.may_bind)
        && (child.requires_acceptance || !parent.requires_acceptance)
        && child.max_depth < parent.max_depth
}

/// A pack verb changes its live members: `scope`, the decision on the same
/// verb on grants, must allow each of them.
pub(super) fn members_admitted(scope: &AccessScope, members: &[Grant]) -> Result<(), DomainError> {
    match members
        .iter()
        .filter(|m| m.state != GrantState::Terminated)
        .find(|m| !admits(scope, m.holder_tenant_id, m.id.0))
    {
        None => Ok(()),
        Some(m) => {
            tracing::info!(grant = %m.id, "license-enforcement: the grant decision does not allow a pack member");
            Err(DomainError::PermissionDenied)
        }
    }
}

/// Whether `scope` admits every row of `tenant`; a constraint on resource ids
/// does not.
fn admits_tenant(scope: &AccessScope, tenant: Uuid) -> bool {
    admits_any(scope, tenant, None)
}

fn admits_any(scope: &AccessScope, tenant: Uuid, id: Option<Uuid>) -> bool {
    scope.is_unconstrained()
        || scope.constraints().iter().any(|c| {
            c.filters().iter().all(|f| {
                let value = match f.property() {
                    pep_properties::OWNER_TENANT_ID => tenant,
                    pep_properties::RESOURCE_ID => match id {
                        Some(id) => id,
                        None => return false,
                    },
                    _ => return false,
                };
                f.is_representable_in_memory()
                    && f.values().iter().any(|v| v.as_uuid() == Some(value))
            })
        })
}

/// A PDP request; `own_tenant` limits it to the caller's tenant.
fn request(own_tenant: bool) -> AccessRequest {
    let request = AccessRequest::new();
    if own_tenant {
        request.tenant_mode(authz_resolver_sdk::models::TenantMode::RootOnly)
    } else {
        request
    }
}

/// The idempotency key of the caller of `ctx`.
fn caller(ctx: &SecurityContext) -> Caller {
    Caller {
        tenant: ctx.subject_tenant_id(),
        subject: ctx.subject_id(),
    }
}

/// Gear and key of the advisory lock that lets one replica sweep at a time.
pub const SWEEP_LOCK: (&str, &str) = ("license-enforcement", "retention-sweep");

/// A verb before its transaction.
enum Prepared<T> {
    /// The stored response of a replay, and the scope of the PDP.
    Replayed(T, AccessScope),
    /// A first call: the scope of the PDP and the payload hash.
    Fresh { scope: AccessScope, hash: String },
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
    #[expect(
        clippy::too_many_arguments,
        reason = "the service dependencies, wired once in the gear"
    )]
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
            .idempotent_result(&conn, caller(ctx), token, OffsetDateTime::now_utc())
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

    /// The steps of a verb before its transaction: the token, the payload
    /// hash, the PDP decision on `id`, a replay, and then `violations`. A
    /// replay goes first: a window that ended since the first call must not
    /// fail its repeat (PRD `fr-idempotent-mutations`). With
    /// `own_tenant`, the scope covers the caller's tenant alone: the holder and
    /// the grantor act in their own tenant.
    #[expect(
        clippy::too_many_arguments,
        reason = "the shared preamble of every mutating verb"
    )]
    async fn prepare<T: DeserializeOwned>(
        &self,
        ctx: &SecurityContext,
        mut violations: Vec<FieldViolation>,
        op: &str,
        payload: &impl Serialize,
        token: &str,
        resource: &ResourceType,
        action: &str,
        id: Option<Uuid>,
        own_tenant: bool,
    ) -> Result<Prepared<T>, DomainError> {
        let before = violations.len();
        token_violations(token, &mut violations);
        if violations.len() > before {
            return Err(DomainError::InvalidArgument(violations));
        }
        let hash = payload_hash(op, payload)?;
        let scope = self.scope(ctx, resource, action, id, own_tenant).await?;
        match self.replayed(ctx, token, &hash).await? {
            Some(stored) => Ok(Prepared::Replayed(stored, scope)),
            None if !violations.is_empty() => Err(DomainError::InvalidArgument(violations)),
            None => Ok(Prepared::Fresh { scope, hash }),
        }
    }

    /// The PDP decisions of `issue` into `holder`, one per table it writes:
    /// `GRANT/issue` for the grants and their evidence, and with `as_pack`
    /// `PACK/issue` for the pack. `issue` and `replace` insert under them.
    async fn issue_scope(
        &self,
        ctx: &SecurityContext,
        holder: Uuid,
        as_pack: bool,
    ) -> Result<Issued, DomainError> {
        let pack = if as_pack {
            Some(self.issue_decision(ctx, &resources::PACK, holder).await?)
        } else {
            None
        };
        Ok(Issued {
            grants: self.issue_decision(ctx, &resources::GRANT, holder).await?,
            pack,
        })
    }

    async fn issue_decision(
        &self,
        ctx: &SecurityContext,
        resource: &ResourceType,
        holder: Uuid,
    ) -> Result<AccessScope, DomainError> {
        Ok(self
            .enforcer
            .access_scope_with(
                ctx,
                resource,
                actions::ISSUE,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, holder),
            )
            .await?)
    }

    /// The PDP scope of `action` on `id`; with `own_tenant`, of the caller's
    /// tenant alone.
    async fn scope(
        &self,
        ctx: &SecurityContext,
        resource: &ResourceType,
        action: &str,
        id: Option<Uuid>,
        own_tenant: bool,
    ) -> Result<AccessScope, DomainError> {
        self.scope_with(ctx, resource, action, id, request(own_tenant))
            .await
    }

    /// As [`Self::scope`] for a list: a permitted scope that matches nothing
    /// gives an empty page, not a denial.
    async fn list_scope(
        &self,
        ctx: &SecurityContext,
        resource: &ResourceType,
        action: &str,
        own_tenant: bool,
    ) -> Result<AccessScope, DomainError> {
        let request = request(own_tenant).allow_empty_scope(true);
        self.scope_with(ctx, resource, action, None, request).await
    }

    async fn scope_with(
        &self,
        ctx: &SecurityContext,
        resource: &ResourceType,
        action: &str,
        id: Option<Uuid>,
        request: AccessRequest,
    ) -> Result<AccessScope, DomainError> {
        Ok(self
            .enforcer
            .access_scope_with(ctx, resource, action, id, &request)
            .await?)
    }

    /// Run `change` in one transaction with the idempotency record of its
    /// result. `change` locks its rows and checks `expected_version`.
    async fn commit<T, F>(
        &self,
        ctx: &SecurityContext,
        token: String,
        hash: String,
        change: F,
    ) -> Result<T, DomainError>
    where
        T: Serialize + DeserializeOwned + Send + 'static,
        F: for<'a> FnOnce(
                &'a DbTx<'a>,
                R,
            )
                -> Pin<Box<dyn Future<Output = Result<T, DomainError>> + Send + 'a>>
            + Send
            + 'static,
    {
        let store = self.store;
        let caller = caller(ctx);
        let window = self.idempotency_window;
        let tx_token = token.clone();
        let tx_hash = hash.clone();
        let committed = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    let result = change(tx, store).await?;
                    let response = serde_json::to_value(&result).map_err(|e| {
                        DomainError::Internal(format!("cannot store the response: {e}"))
                    })?;
                    // The window starts at the end of the change, not before its
                    // lock waits.
                    let now = OffsetDateTime::now_utc();
                    store
                        .record_result(tx, caller, &tx_token, tx_hash, response, now + window, now)
                        .await?;
                    Ok(result)
                })
            })
            .await;
        self.or_replayed(ctx, &token, &hash, committed).await
    }

    /// `tenant` is `ancestor` or its descendant, as `tenant-resolver` shows it
    /// to `ctx`. A tenant that it does not find or show is not.
    async fn is_descendant(
        &self,
        ctx: &SecurityContext,
        ancestor: Uuid,
        tenant: Uuid,
    ) -> Result<bool, DomainError> {
        if tenant == ancestor {
            return Ok(true);
        }
        let resolver = self
            .hub
            .get::<dyn TenantResolverClient>()
            .map_err(|e| DomainError::DependencyUnavailable(format!("tenant-resolver: {e}")))?;
        match resolver
            .is_ancestor(
                ctx,
                TenantId(ancestor),
                TenantId(tenant),
                &IsAncestorOptions::default(),
            )
            .await
        {
            Ok(found) => Ok(found),
            Err(TenantResolverError::TenantNotFound { .. } | TenantResolverError::Unauthorized) => {
                Ok(false)
            }
            Err(e) => Err(DomainError::DependencyUnavailable(format!(
                "tenant-resolver: {e}"
            ))),
        }
    }

    /// The holder is the tenant of `ctx` or its descendant.
    async fn check_holder(&self, ctx: &SecurityContext, holder: Uuid) -> Result<(), DomainError> {
        if self
            .is_descendant(ctx, ctx.subject_tenant_id(), holder)
            .await?
        {
            Ok(())
        } else {
            Err(DomainError::invalid(
                "holder_tenant_id",
                "the holder is not the caller's tenant or its descendant",
                reason::HOLDER_NOT_IN_SUBTREE,
            ))
        }
    }

    /// Checks the roots of the plugin: it may return other terms than the
    /// request, so the size limits apply again.
    async fn validate_terms(
        &self,
        grants: &[GrantTerms],
        as_pack: bool,
        names: TermsField,
    ) -> Result<(), DomainError> {
        // ponytail: recipient rules need the recipient projection, which is not
        // built; a grant with rules is not admitted until it is, so its holder
        // does not get a grant that it cannot delegate.
        if grants
            .iter()
            .any(|t| !empty_rules(t.recipient_rules.as_ref()))
        {
            return Err(DomainError::NotImplemented("recipient rules"));
        }
        let mut violations = Vec::new();
        size_violations(grants, names, &mut violations);
        if as_pack
            && grants.windows(2).any(|w| {
                w[0].permissions.requires_acceptance != w[1].permissions.requires_acceptance
            })
        {
            violations.push(FieldViolation::new(
                names.list,
                "the members of a pack differ in requires_acceptance",
                reason::MIXED_ACCEPTANCE,
            ));
        }
        let mut seen = Schemas::new();
        let now = OffsetDateTime::now_utc();
        for (i, terms) in grants.iter().enumerate() {
            let field = names.item(i);
            window_violations(
                &format!("{field}.window"),
                &terms.window,
                now,
                &mut violations,
            );
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
            if let (Some(rules), Some(schema)) =
                (&terms.conditions, conditions_schema(terms, &seen))
            {
                for mut v in self.evaluator.validate(rules, &schema) {
                    v.field = format!("{field}.conditions{}", v.field);
                    violations.push(v);
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
        let mut v = Vec::new();
        token_violations(&token, &mut v);
        size_violations(&grants, TermsField::ISSUE, &mut v);
        if !v.is_empty() {
            return Err(DomainError::InvalidArgument(v));
        }
        // Before the hash: RFC 3339 does not write an offset with seconds.
        for t in &mut grants {
            stored_window(&mut t.window);
        }
        let hash = payload_hash(
            if as_pack { "issue_pack" } else { "issue_grant" },
            &(&holder, &grants),
        )?;
        let scope = self.issue_scope(ctx, holder, as_pack).await?;
        if let Some(result) = self.replayed(ctx, &token, &hash).await? {
            return self.issued_in_scope(&scope.grants, result).await;
        }
        let admitted = self
            .admit_issue(ctx, holder, grants, as_pack, &token, TermsField::ISSUE)
            .await?;
        let plugin_id = self.plugin.info().id;
        let roots_scope = scope.grants.clone();
        let result = self
            .commit(ctx, token, hash, move |tx, store| {
                Box::pin(async move {
                    store_admitted(tx, store, &scope, holder, plugin_id, admitted).await
                })
            })
            .await?;
        // A failed write answers with the result of a concurrent request with
        // the same token; that result is checked as a replay. The roots of a
        // first call were inserted in the scope and pass.
        self.issued_in_scope(&roots_scope, result).await
    }

    /// A stored `issue` result answers only when all its roots are inside
    /// `scope`, as a first call would. Only `issue` and `bind` check a replay
    /// against the scope: the replay key holds the subject, so a replay gives
    /// a subject only its own earlier answer.
    async fn issued_in_scope(
        &self,
        scope: &AccessScope,
        result: IssueResult,
    ) -> Result<IssueResult, DomainError> {
        let ids: Vec<GrantId> = result.grants.iter().map(|g| g.id).collect();
        let conn = self.db.conn()?;
        let seen = self.store.grants(&conn, scope, &ids, false).await?.len();
        if !ids.is_empty() && seen == ids.len() {
            Ok(result)
        } else {
            tracing::info!(
                issued = ids.len(),
                in_scope = seen,
                "license-enforcement: a replayed issue is outside the current scope"
            );
            Err(DomainError::PermissionDenied)
        }
    }

    /// Admit roots through the `issue` intake and check them, before the
    /// transaction that stores them.
    async fn admit_issue(
        &self,
        ctx: &SecurityContext,
        holder: Uuid,
        grants: Vec<GrantTerms>,
        as_pack: bool,
        token: &str,
        names: TermsField,
    ) -> Result<Admitted, DomainError> {
        self.check_holder(ctx, holder).await?;

        let caller = CallerIdentity {
            subject_id: ctx.subject_id(),
            subject_type: ctx.subject_type().map(str::to_owned),
            tenant_id: ctx.subject_tenant_id(),
        };
        let instant = OffsetDateTime::now_utc();
        let grants: Vec<RequestedRoot> = grants
            .into_iter()
            .map(|t| RequestedRoot::new(GrantId(Uuid::now_v7()), t))
            .collect();
        let root_ids: Vec<GrantId> = grants.iter().map(|r| r.id).collect();
        let mut admission = self
            .plugin
            .admit(
                AdmissionInput::issue(holder, grants, as_pack, caller, token),
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
        // The roots in the order of the request, found by the ids it gave.
        let mut by_id: HashMap<GrantId, AdmittedRoot> =
            admission.roots.drain(..).map(|r| (r.id, r)).collect();
        admission.roots = root_ids
            .iter()
            .map(|id| by_id.remove(id))
            .collect::<Option<Vec<_>>>()
            .filter(|_| by_id.is_empty())
            .ok_or_else(|| {
                DomainError::Internal("the plugin admitted other roots than requested".to_owned())
            })?;
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
        if admission.as_pack != as_pack {
            return Err(DomainError::Internal(
                "the plugin admitted another kind of issue than requested".to_owned(),
            ));
        }
        // What is stored is what is checked and returned: the roots of the
        // plugin.
        for r in &mut admission.roots {
            stored_window(&mut r.fields.terms.window);
        }
        let terms: Vec<GrantTerms> = admission
            .roots
            .iter()
            .map(|r| r.fields.terms.clone())
            .collect();
        self.validate_terms(&terms, as_pack, names).await?;

        let evidence_id = EvidenceId(Uuid::now_v7());
        let pack_id = admission.as_pack.then(|| PackId(Uuid::now_v7()));
        let roots: Vec<Grant> = admission
            .roots
            .iter()
            .map(|r| root_grant(r, pack_id, evidence_id))
            .collect();
        let pack = pack_id.map(|id| pack_of(id, holder, &roots));
        let result = IssueResult::new(Some(evidence_id), pack, roots);
        Ok(Admitted {
            evidence,
            result,
            material: admission.material,
        })
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
        if let BindingChange::Bind(subject) = &change {
            subject_size("subject", &subject.gts_type, &subject.id, &mut violations);
        }
        let (scope, hash) = match self
            .prepare(
                ctx,
                violations,
                op,
                &(&id, &change, &expected_version),
                &token,
                &resources::GRANT,
                action,
                Some(id.0),
                true,
            )
            .await?
        {
            // The scoped read comes before the replay answers: a grant outside
            // the current scope is not found, for a replay too.
            Prepared::Replayed(grant, scope) => {
                let conn = self.db.conn()?;
                self.store
                    .grant(&conn, &scope, id, false)
                    .await?
                    .ok_or_else(|| not_found(Missing::Grant, id.0))?;
                return Ok(grant);
            }
            Prepared::Fresh { scope, hash } => (scope, hash),
        };
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

        self.commit(ctx, token, hash, move |tx, store| {
            Box::pin(async move {
                let scope = &scope;
                let mut g = lifecycle::read_locked(
                    tx,
                    store,
                    |_| vec![id],
                    |lock| async move {
                        store
                            .grant(tx, scope, id, lock)
                            .await?
                            .ok_or_else(|| not_found(Missing::Grant, id.0))
                    },
                )
                .await?;
                if expected_version.is_some_and(|v| v != g.version) {
                    return Err(DomainError::VersionMismatch);
                }
                if g.state == GrantState::Terminated {
                    return Err(DomainError::InvalidTransition(
                        "the grant is terminated".into(),
                    ));
                }
                let root_live = store.chain(tx, g.id).await?.last().is_some_and(|r| {
                    r.parent_grant_id.is_none() && r.state != GrantState::Terminated
                });
                if !root_live {
                    return Err(DomainError::InvalidTransition(
                        "the root of the grant is terminated".into(),
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
                let now = OffsetDateTime::now_utc();
                g.binding = match (change, &g.binding) {
                    (BindingChange::Bind(subject), Binding::None) => Binding::subject(subject),
                    (BindingChange::Unbind, Binding::Subject { .. }) => Binding::None,
                    (BindingChange::Bind(_), _) => {
                        return Err(DomainError::InvalidTransition("the grant is bound".into()));
                    }
                    (BindingChange::Unbind, _) => {
                        return Err(DomainError::InvalidTransition(
                            "the grant is not bound to a subject".into(),
                        ));
                    }
                };
                g.version += 1;
                store.update_grant(tx, scope, &g, now).await?;
                Ok(g)
            })
        })
        .await
    }

    /// The row of `id`: under `get` in the caller's tenant, otherwise under
    /// `read_subtree` below it. A refusal of one action does not stop the
    /// other; `PermissionDenied` when the PDP refuses both.
    async fn read_one<T, F, Fut>(
        &self,
        ctx: &SecurityContext,
        resource: &ResourceType,
        id: Uuid,
        read: F,
    ) -> Result<Option<T>, DomainError>
    where
        F: Fn(AccessScope) -> Fut,
        Fut: Future<Output = Result<Option<T>, DomainError>>,
    {
        let mut permitted = false;
        for (action, own_tenant) in [(actions::GET, true), (actions::READ_SUBTREE, false)] {
            let scope = match self
                .scope(ctx, resource, action, Some(id), own_tenant)
                .await
            {
                Err(DomainError::PermissionDenied) => continue,
                other => other?,
            };
            permitted = true;
            if let Some(row) = read(scope).await? {
                return Ok(Some(row));
            }
        }
        if permitted {
            Ok(None)
        } else {
            Err(DomainError::PermissionDenied)
        }
    }

    /// A grant of the caller's tenant; otherwise one below it, when the PDP
    /// allows `read_subtree`.
    ///
    /// # Errors
    ///
    /// `NotFound` outside the caller's scope; `PermissionDenied` when the PDP
    /// allows neither `get` nor `read_subtree`.
    pub async fn get_grant(
        &self,
        ctx: &SecurityContext,
        id: GrantId,
    ) -> Result<Grant, DomainError> {
        let conn = self.db.conn()?;
        let conn = &conn;
        self.read_one(ctx, &resources::GRANT, id.0, |scope| async move {
            self.store.grant(conn, &scope, id, false).await
        })
        .await?
        .ok_or_else(|| not_found(Missing::Grant, id.0))
    }

    /// A pack of the caller's tenant; otherwise one below it, when the PDP
    /// allows `read_subtree`, as [`Self::get_grant`].
    ///
    /// # Errors
    ///
    /// As [`Self::get_grant`].
    pub async fn get_pack(&self, ctx: &SecurityContext, id: PackId) -> Result<Pack, DomainError> {
        let conn = self.db.conn()?;
        let conn = &conn;
        self.read_one(ctx, &resources::PACK, id.0, |scope| async move {
            self.store.pack(conn, &scope, id).await
        })
        .await?
        .ok_or_else(|| not_found(Missing::Pack, id.0))
    }

    /// The database of the service.
    pub fn db(&self) -> toolkit_db::Db {
        self.db.db()
    }

    /// Delete the trees that ended more than `retention` ago, at most
    /// `SWEEP_BATCHES` batches, and the expired idempotency records. Returns
    /// the deleted grants and records, or `None` when another replica holds the
    /// sweep lock.
    ///
    /// # Errors
    ///
    /// Store and lock faults.
    pub async fn sweep(&self, retention: Duration) -> Result<Option<(u64, u64)>, DomainError> {
        const BATCH: u64 = 500;
        const SWEEP_BATCHES: u32 = 20;
        let lock = match self.db.db().lock(SWEEP_LOCK.0, SWEEP_LOCK.1).await {
            Ok(lock) => lock,
            Err(toolkit_db::DbError::Lock(toolkit_db::DbLockError::AlreadyHeld { .. })) => {
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        };
        let now = OffsetDateTime::now_utc();
        let cutoff = now - retention;
        let mut grants = 0;
        // A failed grant batch does not stop the cleanup of idempotency
        // records; the error is returned after it.
        let mut grant_error = None;
        for _ in 0..SWEEP_BATCHES {
            let store = self.store;
            let deleted = self
                .db
                .transaction(move |tx| {
                    Box::pin(async move { store.sweep_terminated(tx, cutoff, BATCH).await })
                })
                .await;
            match deleted {
                Ok(0) => break,
                Ok(deleted) => grants += deleted,
                Err(e) => {
                    grant_error = Some(e);
                    break;
                }
            }
        }
        // Each batch is a short statement of its own, so writers wait at most
        // one batch.
        let purged = async {
            let conn = self.db.conn()?;
            let mut records = 0;
            loop {
                let deleted = self.store.purge_idempotency(&conn, now, BATCH).await?;
                records += deleted;
                if deleted < BATCH {
                    return Ok::<_, DomainError>(records);
                }
            }
        }
        .await;
        if let Err(e) = lock.release().await {
            tracing::warn!(error = %e, "license-enforcement: cannot release the sweep lock");
        }
        if let Some(e) = grant_error {
            return Err(e);
        }
        Ok(Some((grants, purged?)))
    }

    /// # Errors
    ///
    /// A malformed query, or a PDP denial.
    pub async fn list_packs(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
        include_subtree: bool,
    ) -> Result<Page<Pack>, DomainError> {
        let (action, own_tenant) = if include_subtree {
            (actions::READ_SUBTREE, false)
        } else {
            (actions::LIST, true)
        };
        let scope = self
            .list_scope(ctx, &resources::PACK, action, own_tenant)
            .await?;
        // A filter selects packs by their members: grant rows, under a decision
        // on grants over the same tenants.
        let members = match query.filter {
            Some(_) => Some(
                self.list_scope(ctx, &resources::GRANT, action, own_tenant)
                    .await?,
            ),
            None => None,
        };
        let conn = self.db.conn()?;
        self.store
            .list_packs(&conn, &scope, members.as_ref(), query)
            .await
    }

    /// The check request of a check-explain in the caller's tenant, after the
    /// PDP and the contract checks that the license-resolver gateway makes.
    ///
    /// # Errors
    ///
    /// A PDP denial, a nonconforming request, or a registry fault.
    pub async fn explain_request(
        &self,
        ctx: &SecurityContext,
        req: CheckExplainRequest,
    ) -> Result<LicenseCheckRequest, DomainError> {
        // The check reads the caller's tenant as a whole; it does not apply a
        // narrower scope, so it refuses one.
        let scope = self
            .enforcer
            .access_scope(ctx, &resources::GRANT, actions::CHECK_EXPLAIN, None)
            .await?;
        if !admits_tenant(&scope, ctx.subject_tenant_id()) {
            tracing::info!(
                tenant = %ctx.subject_tenant_id(),
                "license-enforcement: check-explain needs a scope of the whole tenant"
            );
            return Err(DomainError::PermissionDenied);
        }
        let mut violations = Vec::new();
        self.types
            .validate_check(&req.subject, &req.resource, &mut violations)
            .await?;
        if !violations.is_empty() {
            return Err(DomainError::InvalidArgument(violations));
        }
        let context = LicenseCheckContext::builder()
            .tenant_id(ctx.subject_tenant_id())
            .build()
            .map_err(|e| DomainError::Internal(format!("check context: {e}")))?;
        Ok(LicenseCheckRequest::new(req.subject, req.resource, context))
    }

    /// # Errors
    ///
    /// A malformed query, or a PDP denial. A permitted scope that matches
    /// nothing gives an empty page.
    pub async fn list_grants(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
        include_subtree: bool,
    ) -> Result<Page<Grant>, DomainError> {
        let (action, own_tenant) = if include_subtree {
            (actions::READ_SUBTREE, false)
        } else {
            (actions::LIST, true)
        };
        let scope = self
            .list_scope(ctx, &resources::GRANT, action, own_tenant)
            .await?;
        let conn = self.db.conn()?;
        self.store.list_grants(&conn, &scope, query).await
    }
}

/// Roots that the plugin admitted, ready to store.
#[domain_model]
struct Admitted {
    evidence: NewEvidence,
    result: IssueResult,
    material: Vec<MaterialWrite>,
}

/// Store what admission returned: evidence, pack, roots, plugin material.
async fn store_admitted<R: GrantRepo>(
    tx: &DbTx<'_>,
    store: R,
    issued: &Issued,
    holder: Uuid,
    plugin_id: &'static str,
    admitted: Admitted,
) -> Result<IssueResult, DomainError> {
    let Admitted {
        evidence,
        result,
        material,
    } = admitted;
    let evidence_id = result
        .evidence_id
        .ok_or_else(|| DomainError::Internal("no evidence id".to_owned()))?;
    let now = OffsetDateTime::now_utc();
    // The unique indexes of the plugin decide a reused replay key, external
    // id, or replaced evidence on insert. For `issue` the replay key is the
    // tenant, the subject and the token: a replay is a token reused after its
    // window.
    let replaces = match &evidence.replaces {
        None => None,
        Some(external_id) => Some(
            store
                .evidence_by_external_id(tx, holder, plugin_id, external_id)
                .await?
                .ok_or(DomainError::EvidenceChainBroken(
                    precondition::REPLACED_EVIDENCE_MISSING,
                ))?,
        ),
    };
    store
        .insert_evidence(
            tx,
            &issued.grants,
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
    // ponytail: supersession of the roots of the replaced evidence (DESIGN
    // §3.1) is not built; until then the gear refuses instead of keeping both
    // sets of roots live.
    if replaces.is_some() {
        return Err(DomainError::NotImplemented("evidence supersession"));
    }
    // The counts of what is inserted: `replace` moves bindings onto its
    // successors after admission.
    let mut result = result;
    if let Some(pack) = &mut result.pack {
        *pack = pack_of(pack.id, holder, &result.grants);
        let scope = issued.pack.as_ref().ok_or_else(|| {
            DomainError::Internal("a pack without the decision on packs".to_owned())
        })?;
        store.insert_pack(tx, scope, pack, now).await?;
    }
    store
        .insert_roots(tx, &issued.grants, &result.grants, now)
        .await?;
    if !store.apply_material(tx, plugin_id, &material).await? {
        return Err(DomainError::AdmissionConflict);
    }
    Ok(result)
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
