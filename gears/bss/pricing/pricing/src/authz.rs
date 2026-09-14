//! Plan & Price Modeling authorization: PEP resource-type descriptors, action
//! names, the shared [`access_scope`] gate every ctx-bearing service path calls
//! before touching the repository, and the authz-label stub type-schemas
//! registered at gear init so RBAC role-definitions can target these labels.
//!
//! The catalog is normative in `design/05-governance.md`
//! (`cpt-cf-bss-pricing-algo-authz-catalog`); this module is its executable
//! form. Eight object-named labels, all **OUTSIDE** `gts.cf.resources.*` —
//! pricing data is commercially sensitive, so the built-in Reader / Contributor
//! / Owner roles do NOT auto-cover it and access requires explicit catalog
//! roles. Each action sits on its real object (a noun), never an authz tier.
//!
//! Two separations in the label set are load-bearing rather than tidy:
//! - `approval_policy` is its own resource, not a `config` action — segregation
//!   of duties, so a config admin cannot weaken the approval thresholds it
//!   operates under (the sibling ledger's `dual_control_policy` precedent).
//! - `customer_group` is its own resource, not part of `plan` — per-payer
//!   membership is payer-level commercial data, more sensitive than plan
//!   authoring; and `audit` is its own resource so a forensic role carries no
//!   read of live pricing and no write authority anywhere.
//!
//! The PEP advertises NO tenant-subtree capability (`PolicyEnforcer::new`), so
//! the PDP pre-expands the caller's subtree to a flat `AccessScope::In([...])`
//! that SecureORM binds to the `tenant_id` column.
//!
//! **That pre-expansion buys nothing on any mounted route, and this paragraph
//! used to imply it did.** Every handler passes `ctx.subject_tenant_id()` to the
//! repository as an explicit column predicate *in addition* to the compiled
//! scope, and the two are conjoined — so a scope naming any tenant but the
//! caller's own can only narrow the result to the empty set. A reseller
//! principal in parent tenant `P` holding a grant compiled to `In([P, C1, C2])`
//! reads `P`'s rows and gets `404` for `C1`'s, indistinguishable from absence.
//!
//! The gear is therefore **single-tenant per request by construction**, and the
//! compiled scope is defence in depth behind that predicate rather than the
//! filter that decides the answer. Making the scope actually decide reads is a
//! change to the tenant argument of every repository call on every route — a
//! decision with a blast radius, not a repair, and it is not taken here.

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

/// Authz `resource_type` label strings (the PDP-visible glob targets).
///
/// Plain `&'static str` consts so the [`resource_types`] descriptors used at
/// enforcement time and the GTS permission catalog (`crate::gts::permissions`)
/// share one source of truth; a drift test enforces that they agree.
pub mod labels {
    use toolkit_gts::gts_id;

    /// Plans, price rows and the primitives attached to a row — the authoring
    /// data plane (`write`, `publish`, `retire`, `migrate`, `read`, `preview`).
    /// Also the label the published read model and the pin frontier are read
    /// under by consumer service identities.
    pub const PLAN: &str = gts_id!("cf.bss.pricing.plan.v1~");
    /// Bundle composition and rev-share (`write`, `read`). Bundle *publish* is
    /// deliberately `plan × publish` (D-11): the composition is authored under
    /// `bundle × write` and pinned by the approval content hash at publish.
    pub const BUNDLE: &str = gts_id!("cf.bss.pricing.bundle.v1~");
    /// `PriceOverlays` of every scope (`write`, `read`). Overlay mutations are
    /// always material (D-50), so the approving role needs this `read` — the
    /// reviewability invariant (D-61).
    pub const PRICE_OVERLAY: &str = gts_id!("cf.bss.pricing.price_overlay.v1~");
    /// Customer-group taxonomy and per-payer membership (`write`, `read`). Its
    /// OWN resource: payer-level commercial data is more sensitive than plan
    /// authoring, so it is never covered by a plan grant.
    pub const CUSTOMER_GROUP: &str = gts_id!("cf.bss.pricing.customer_group.v1~");
    /// Approval decisions (`approve`, `read`). `preparer != approver` is
    /// enforced server-side even where a custom role grants both this and
    /// `plan × publish`.
    pub const APPROVAL: &str = gts_id!("cf.bss.pricing.approval.v1~");
    /// The tenant approval-threshold policy (`write`, `read`) — a SEPARATE
    /// resource from `config` by segregation of duties, and its own mutation is
    /// itself two-person (D-10).
    pub const APPROVAL_POLICY: &str = gts_id!("cf.bss.pricing.approval_policy.v1~");
    /// The tenant config plane (`write`, `read`): tax-display policy and the
    /// region / brand / partner / orgTier taxonomies (D-120). The
    /// customer-group taxonomy is deliberately NOT here.
    pub const CONFIG: &str = gts_id!("cf.bss.pricing.config.v1~");
    // There is no ninth `historical_import` label: D-330 puts historical import
    // out of scope, so S5 §3 registers no such
    // resource and no backdating grant, and `inst-rb-backdate` now says so as a
    // rule rather than being deleted. Re-introducing either needs a decision, not
    // a commit. Its two permission instances left `gts::permissions` with it, and
    // the two routes that were asking for it — the bulk import's submit, read and
    // abort — went back to the `plan` pair S5 §3's endpoint map always gave them.
    /// Audit trail read and export (`read`, `export`) — its OWN resource so an
    /// auditor role carries no read of live pricing and no write authority.
    ///
    /// **It covers the price history too.** "Finance's chronological price history is
    /// the separate `plan × read` surface" is D-12's original reading and is not true
    /// of `GET /bss-pricing/v1/history`: that route *is* the catalog audit trail, so
    /// filing it under catalog read hands "who changed what, when" to every holder of
    /// `plan × read`. `POST /bss-pricing/v1/history/export` follows it under
    /// `export` (`inst-he-export`), which is what that action was declared for:
    /// bulk extraction of a seven-year actor trail, grantable separately from
    /// reading it. The design set's own endpoint tables still carry the withdrawn
    /// reading (S12 §5, S5 §5) and are owed the correction.
    pub const AUDIT: &str = gts_id!("cf.bss.pricing.audit.v1~");

    /// Every authz label, stable order. The single canonical list driving the
    /// per-label stub type-schema registration (see
    /// [`super::authz_label_type_schemas`]) that lets RBAC role-definitions
    /// target any pricing label. MUST match the permission catalog's distinct
    /// `resource_type`s (`crate::gts::permissions`); a drift test enforces it.
    pub const ALL: &[&str] = &[
        PLAN,
        BUNDLE,
        PRICE_OVERLAY,
        CUSTOMER_GROUP,
        APPROVAL,
        APPROVAL_POLICY,
        CONFIG,
        AUDIT,
    ];
}

/// PEP action names for the catalog surfaces.
pub mod actions {
    /// Author draft state: create / update / clone a plan or price row, delete a
    /// never-published draft, run a cutover or supersession, tighten
    /// `grandfatherUntil`, schedule a window. Also the bulk plane — bulk is
    /// authoring at scale and carries no new authority. Used by `plan`,
    /// `bundle`, `price_overlay`, `customer_group`, `approval_policy` and
    /// `config`; the resource scopes what it authorizes.
    /// Write action — authoring mutations on a plan and its rows.
    ///
    /// **`inst-rb-region`'s mutation clause is untransported, and this is the
    /// third of its three halves to say so**. `05-governance.md` §5
    /// step 4 is `p1` and normative: *"mutating a price row whose pricing `region`
    /// the caller's authz scope does not grant is denied + audited"*. No mutating
    /// path in this crate compares a row's `region` against anything, and
    /// [`super::SUPPORTED_PROPERTIES`] carries two uuid-typed properties, so a
    /// region cannot be transported to the PDP at all — the same substrate gap the
    /// approval and preview planes have.
    ///
    /// **The asymmetry was the finding, not the gap.** `infra::approval`'s
    /// `RegionGrant` models the absence as a two-valued type, and [`PREVIEW`]'s doc
    /// below declares the preview half in detail — with the lesson *"a reader
    /// trusting this sentence would have taken the region question as considered
    /// and settled"*. The **mutation** half, the one governing writes and the one
    /// §5 marks `p1`, was named in no module doc, no type and no decision entry, so
    /// a reader consulting this module found the region question surveyed at length
    /// and concluded the survey was complete. It was not.
    ///
    /// What is live today: a role intended as "EU pricing author" holding
    /// `plan × write` can author, publish and price from a `us-east` row, with no
    /// refusal and — the clause's second half — no audited denial record either.
    /// The same holds for every write door: cutover, supersession, window
    /// schedule, bulk import, repricing run.
    ///
    /// Until a transport exists the gap is reported rather than only described:
    /// `api::rest::approvals::report_region_grant_transport` raises
    /// `pricing.authz.region_grant_untransported` and logs both clauses at every
    /// boot, so an operator inheriting a deployment learns it from their alerting
    /// instead of from this paragraph. Building the transport is a platform change
    /// and not a fix to this crate — it needs a PEP property a pricing region can
    /// travel in ([`SUPPORTED_PROPERTIES`] carries two uuid-typed ones), a PDP
    /// outside this repository that can constrain on it, and a comparison on every
    /// mutating path.
    pub const WRITE: &str = "write";
    /// Submit a plan (or a bundle's composition) for publish. Distinct from
    /// [`WRITE`] so an author can prepare a change without being able to put it
    /// in front of customers.
    pub const PUBLISH: &str = "publish";
    /// Retire a plan. Its own action because retirement is a publish unit with
    /// no way back (D-128): a retired plan can never publish again.
    pub const RETIRE: &str = "retire";
    /// Schedule or cancel a plan migration, and the Subscriptions execution
    /// handshake. Distinct from [`RETIRE`] so moving subscribers between plans
    /// is grantable without the authority to end a plan's life.
    pub const MIGRATE: &str = "migrate";
    /// Read action — authoring reads (including drafts), the published read
    /// model, the pin frontier, coverage / sellability, price history and the
    /// `migrated-origin` snapshot. The resource scopes what the read
    /// authorizes.
    pub const READ: &str = "read";
    /// The base-price preview grant. Distinct from [`READ`] so a caller holding
    /// it never holds an authoring read.
    ///
    /// **Called "partner-facing" until this was measured, and no partner can
    /// reach it.** `preview.rs` binds `tenant = ctx.subject_tenant_id()` and
    /// reads the frontier and the delta at *that* tenant, so a partner
    /// authenticated in tenant `P` holding a grant over vendor tenant `V` is
    /// answered `404 PRICE_ROW_ABSENT` for `V`'s plan — not `403`, and not a
    /// preview. The grant is real and the separation from [`READ`] is real; what
    /// was not real is the cross-tenant reach the name claimed. See the module
    /// doc's single-tenant paragraph for why, and for what changing it would
    /// cost.
    ///
    /// **The grant is tenant- and plan-scoped, and the region clause awaits a
    /// transport.** It is **not** *"evaluated against the grant's explicit
    /// pricing-region set"*: `05-governance.md` §5's `inst-rb-preview-scope`
    /// requires that a preview resolve only markets whose pricing `region` is a
    /// member of that set — `REGION_SCOPE_DENIED` (403) otherwise — and
    /// `api::rest::preview` takes the region from the caller's own query string,
    /// uses it only to select the market row, and never compares it against
    /// anything the gate returned.
    ///
    /// The substrate is the reason and not an excuse: [`SUPPORTED_PROPERTIES`]
    /// advertises exactly `owner_tenant_id` and `resource_id`, both uuid-typed, and
    /// a pricing region is a string on a price row's scope key — nothing transports
    /// a region grant anywhere on this platform. `REGION_SCOPE_DENIED` exists and is
    /// mapped, and its only producer is the **approval** plane's `inst-ap-scope`.
    ///
    /// What was wrong here was the asymmetry rather than the gap. The approval plane
    /// declares the same absence with unusual care — `infra::approval` models it as a
    /// two-valued `RegionGrant::{Untransported, Explicit}` so the day a transport
    /// arrives is a visible change, and names the test that must change then — while
    /// the preview plane, the surface the rule was *written for*, asserted the
    /// opposite in this catalog. A reader trusting this sentence would have taken the
    /// region question as considered and settled.
    pub const PREVIEW: &str = "preview";
    /// Approve or reject an approval record. Distinct from [`PUBLISH`] so no
    /// default role holds both, on top of the server-side
    /// `submitter != approver` rule.
    pub const APPROVE: &str = "approve";
    /// Export the audit trail. Distinct from [`READ`] so bulk extraction of a
    /// seven-year actor trail is grantable separately from reading it.
    pub const EXPORT: &str = "export";
}

/// Properties the PEP may compile from PDP constraints for catalog rows. Every
/// row is tenant-owned: `owner_tenant_id` is the tenant column the secure-ORM
/// filter binds to, `id` the row PK for single-row gates. NO subtree/group
/// property — the PDP pre-expands the subtree to a flat `In`.
///
/// # Why four of the eight labels never anchor to a resource id
///
/// `config`, `approval_policy` and `audit` are tenant singletons or a log: they have
/// no per-instance identity, so their permissions are correctly modelled as surface
/// grants and their gates pass `resource_id: None`.
///
/// **`customer_group` is not in that class, and the reason it behaves like one is
/// this list.** All four membership routes bind `{group}` as a path segment and
/// `required_group` turns it into a `ScopeValue`, so the gear treats a group as a
/// named thing with identity — yet both gates pass `None`, which means holding
/// `customer_group × write` grants write on **every** group in the tenant and no
/// role definition can be scoped to a subset. The axis physically cannot carry it:
/// both properties above are **uuid-typed** and a customer group is a taxonomy
/// *value*, a string.
///
/// So this is a modelling gap that **arms** rather than a live hole. The day a
/// partner model needs per-group delegation, the answer is not "add a constraint" —
/// it is "give groups uuids", and this paragraph is where a later reader finds that
/// out instead of discovering it against a constraint that silently compiles to
/// nothing.
pub const SUPPORTED_PROPERTIES: &[&str] =
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID];

/// PEP resource-type descriptors (one `const` per authz label).
pub mod resource_types {
    use super::{ResourceType, SUPPORTED_PROPERTIES, labels};

    /// Plans, price rows and row-attached primitives — the authoring data plane.
    pub const PLAN: ResourceType = ResourceType::from_static(labels::PLAN, SUPPORTED_PROPERTIES);
    /// Bundle composition and rev-share.
    pub const BUNDLE: ResourceType =
        ResourceType::from_static(labels::BUNDLE, SUPPORTED_PROPERTIES);
    /// `PriceOverlays` of every scope.
    pub const PRICE_OVERLAY: ResourceType =
        ResourceType::from_static(labels::PRICE_OVERLAY, SUPPORTED_PROPERTIES);
    /// Customer-group taxonomy and per-payer membership.
    pub const CUSTOMER_GROUP: ResourceType =
        ResourceType::from_static(labels::CUSTOMER_GROUP, SUPPORTED_PROPERTIES);
    /// Approval decisions.
    pub const APPROVAL: ResourceType =
        ResourceType::from_static(labels::APPROVAL, SUPPORTED_PROPERTIES);
    /// The tenant approval-threshold policy.
    pub const APPROVAL_POLICY: ResourceType =
        ResourceType::from_static(labels::APPROVAL_POLICY, SUPPORTED_PROPERTIES);
    /// The tenant config plane.
    pub const CONFIG: ResourceType =
        ResourceType::from_static(labels::CONFIG, SUPPORTED_PROPERTIES);
    /// Audit trail read and export.
    pub const AUDIT: ResourceType = ResourceType::from_static(labels::AUDIT, SUPPORTED_PROPERTIES);
}

/// One refused attempt, with every operand a denial record needs.
///
/// `inst-rb-audit` and `dod-rbac` are both `p1` and say denied attempts are
/// audit-logged. They were not: every route's 403 passed one funnel that emitted
/// nothing — no record, no metric, no log — while the fail-closed `Unavailable`
/// arm beside it logged. A `String` is why: it can carry a sentence and cannot
/// carry *who tried what against which resource*, so there was nothing to write
/// even if a writer had existed.
///
/// Carried by [`AuthzError::Denied`], so the type makes the omission impossible:
/// there is no way to deny without naming the operands.
///
/// **What is still owed.** This closes the trace, not the durable record. A row
/// in `pricing_audit_log` needs a `DbTx`, and the PEP gate runs before any
/// handler opens one — on a denied *read* there is no transaction at all. Where
/// that write belongs (a middleware over the whole layer, or a channel the
/// handler drains) is a decision, and inventing it at the end of a fix batch is
/// how the last wave introduced three defects while closing twenty-three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeniedAttempt {
    /// The principal that was refused.
    pub subject_principal_id: Uuid,
    /// The tenant the principal belongs to — not necessarily the resource's.
    pub subject_tenant_id: Uuid,
    /// The authz label the gate asked about.
    pub resource_type: String,
    /// The action asked for.
    pub action: String,
    /// The specific resource, when the gate named one.
    pub resource_id: Option<Uuid>,
    /// The resource's owning tenant, on the write paths that anchor to one.
    pub owner_tenant_id: Option<Uuid>,
    /// Why, in the words the caller may be told.
    pub reason: String,
}

/// Error from the catalog authz gate.
#[derive(Debug, thiserror::Error)]
pub enum AuthzError {
    /// The PDP explicitly denied access (or returned uncompilable constraints).
    #[error("permission denied: {}", .0.reason)]
    Denied(Box<DeniedAttempt>),
    /// The PDP was unreachable or its response could not be compiled.
    #[error("authz unavailable: {0}")]
    Unavailable(String),
}

/// The denial the PDP itself decided.
#[must_use]
pub fn pdp_denial(
    ctx: &SecurityContext,
    rt: &ResourceType,
    action: &str,
    resource_id: Option<Uuid>,
    owner_tenant_id: Option<Uuid>,
    reason: String,
) -> DeniedAttempt {
    DeniedAttempt {
        subject_principal_id: ctx.subject_id(),
        subject_tenant_id: ctx.subject_tenant_id(),
        resource_type: rt.name().to_owned(),
        action: action.to_owned(),
        resource_id,
        owner_tenant_id,
        reason,
    }
}

/// The denial this module decides itself: a write anchored to a tenant the
/// compiled scope does not contain.
///
/// Separate from [`pdp_denial`] because the PDP allowed this one — the degraded
/// flat-`In` decision does not re-validate `owner_tenant_id`, so the refusal is
/// this gate's own and a denial record must not attribute it to the PDP.
#[must_use]
pub fn cross_tenant_write_denial(
    subject_principal_id: Uuid,
    subject_tenant_id: Uuid,
    rt: &ResourceType,
    action: &str,
    resource_id: Option<Uuid>,
    target_tenant: Uuid,
) -> DeniedAttempt {
    DeniedAttempt {
        subject_principal_id,
        subject_tenant_id,
        resource_type: rt.name().to_owned(),
        action: action.to_owned(),
        resource_id,
        owner_tenant_id: Some(target_tenant),
        reason: format!(
            "subject not authorized to write resources owned by tenant {target_tenant}"
        ),
    }
}

/// Minimal, deterministic type-schema body for an authz label. Key order is
/// fixed by construction, so a re-registration is byte-identical — the registry
/// accepts identical duplicates and does not validate body richness.
fn authz_type_schema_json(gts_id: &str, title: &str) -> serde_json::Value {
    serde_json::json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": title,
        "type": "object",
    })
}

/// Stub type-schemas for every authz label ([`labels::ALL`]). The platform RBAC
/// role-definition validator resolves a rule's `target_type` through the
/// types-registry, so registering these at gear init lets a custom catalog role
/// target any pricing authz label.
#[must_use]
pub fn authz_label_type_schemas() -> Vec<serde_json::Value> {
    labels::ALL
        .iter()
        .map(|label| {
            authz_type_schema_json(
                label,
                &format!("BSS Plan & Price Modeling authz label {label}"),
            )
        })
        .collect()
}

/// The tenant a write's row is addressed to — the `OWNER_TENANT_ID` hint.
///
/// A type rather than a bare `Option<Uuid>`, because it sits beside [`ResourceRef`]
/// in [`access_scope`]'s parameter list and two `Option<Uuid>` in adjacent positions
/// are interchangeable to the compiler wherever a call passes both. Swapped, the
/// membership assertion tests a resource id against the caller's tenants and denies a
/// write it should have allowed, while the PDP authorizes against a resource that does
/// not exist — whose answer is the deployed policy's, not this gear's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnerTenant(pub Uuid);

/// The resource a request names — the `RESOURCE_ID` property.
///
/// [`OwnerTenant`]'s counterpart, for its reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceRef(pub Uuid);

/// Shared PEP gate: asks the PDP whether `(resource_type, action)` is permitted
/// for `ctx`, returning the caller's compiled [`AccessScope`]. `resource_id`
/// pins a single-row op (`None` for collections).
///
/// `owner_tenant_id` is an optional `OWNER_TENANT_ID` resource-property hint
/// describing the *resource's* owning tenant:
/// - **Reads** pass `None` — the PDP derives the scope from the subject + role,
///   never from a caller-supplied tenant; the returned scope is the SQL filter.
/// - **Writes** pass `Some(target_tenant)` — the tenant the row is written to.
///   This is NOT self-validating at the PDP: the degraded flat-`In` decision
///   does not re-check `owner_tenant_id`, so this fn asserts `target_tenant` is
///   a member of the compiled scope and denies a cross-tenant target.
///
/// **Constraints are always required, and there is no parameter for it.** An
/// unconstrained *allow* on a read is every tenant's price book, and on a write it
/// leaves the target-membership assertion below with nothing to test — so both
/// authorizing paths need the same answer and neither has a reason to differ. Held
/// as a parameter it was a `false` away from fail-open at any one of the call
/// sites, spelled `true` at every one of them, and readable at none without a
/// `/* require_constraints */` comment beside it.
///
/// # Errors
///
/// [`AuthzError::Denied`] when the PDP denies or returns uncompilable
/// constraints; [`AuthzError::Unavailable`] when the PDP is unreachable.
pub async fn access_scope(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    rt: &ResourceType,
    action: &str,
    owner_tenant_id: Option<OwnerTenant>,
    resource_id: Option<ResourceRef>,
) -> Result<AccessScope, AuthzError> {
    let owner_tenant_id = owner_tenant_id.map(|t| t.0);
    let resource_id = resource_id.map(|r| r.0);
    let mut request = AccessRequest::new().require_constraints(true);
    if let Some(tenant) = owner_tenant_id {
        request = request.resource_property(pep_properties::OWNER_TENANT_ID, tenant);
    }
    if let Some(rid) = resource_id {
        request = request.resource_property(pep_properties::RESOURCE_ID, rid);
    }
    let scope = enforcer
        .access_scope_with(ctx, rt, action, resource_id, &request)
        .await
        .map_err(|e| match e {
            authz_resolver_sdk::EnforcerError::Denied { ref deny_reason } => {
                let reason = deny_reason.as_ref().map_or_else(
                    || e.to_string(),
                    |dr| match &dr.details {
                        Some(details) => format!("{}: {details}", dr.error_code),
                        None => dr.error_code.clone(),
                    },
                );
                AuthzError::Denied(Box::new(pdp_denial(
                    ctx,
                    rt,
                    action,
                    resource_id,
                    owner_tenant_id,
                    reason,
                )))
            }
            authz_resolver_sdk::EnforcerError::CompileFailed(ref compile_err) => {
                // The compiler diagnostic names PDP predicates and properties — an
                // internal detail, not something the PDP told the caller. It goes
                // server-side only, with the same operands `authz_error_to_canonical`
                // logs for a PDP denial, so this line can be tied back to the 403 it
                // explains. The caller gets a stable machine token distinct from an
                // actual PDP denial, rather than the leaking diagnostic.
                tracing::warn!(
                    target: "pricing.authz.deny",
                    subject_principal_id = %ctx.subject_id(),
                    subject_tenant_id = %ctx.subject_tenant_id(),
                    resource_type = %rt.name(),
                    action,
                    resource_id = ?resource_id,
                    owner_tenant_id = ?owner_tenant_id,
                    error = %compile_err,
                    "authz constraint compilation failed"
                );
                AuthzError::Denied(Box::new(pdp_denial(
                    ctx,
                    rt,
                    action,
                    resource_id,
                    owner_tenant_id,
                    "constraint_compilation_failed".to_owned(),
                )))
            }
            authz_resolver_sdk::EnforcerError::EvaluationFailed(_) => {
                AuthzError::Unavailable(e.to_string())
            }
        })?;

    // Write paths anchor to a target tenant: the degraded flat-`In` PDP decision
    // does NOT re-validate `owner_tenant_id`, so assert the target is a member of
    // the compiled scope here — a target outside the caller's authorized tenants is
    // a cross-tenant write and is denied. Reads pass `owner_tenant_id = None` and
    // use the scope as the SQL filter, so this membership check is write-only, and
    // `Some(target)` is the whole of what selects it.
    if let Some(target) = owner_tenant_id
        && !scope.contains_uuid(pep_properties::OWNER_TENANT_ID, target)
    {
        return Err(AuthzError::Denied(Box::new(cross_tenant_write_denial(
            ctx.subject_id(),
            ctx.subject_tenant_id(),
            rt,
            action,
            resource_id,
            target,
        ))));
    }
    Ok(scope)
}

#[cfg(test)]
#[path = "authz_tests.rs"]
mod authz_tests;
