//! BSS Ledger authorization: PEP resource-type descriptors, action names, the
//! shared [`access_scope`] gate every ctx-bearing service path calls before
//! touching the repository, and the authz-label stub type-schemas registered at
//! gear init so RBAC role-definitions can target the ledger's labels.
//!
//! Nine object-named labels, all OUTSIDE the `gts.cf.resources.*` family — the
//! built-in Reader / Contributor / Owner roles do NOT auto-cover them; access to
//! this finance data requires explicit billing roles:
//! - `gts.cf.bss.ledger.entry.v1~` — journal entries / balances (`post`,
//!   `reverse`, `read`).
//! - `gts.cf.bss.ledger.ledger.v1~` — the seller's ledger (`provision` — seed its
//!   chart of accounts, scales, calendar, first period; `read` — list its chart
//!   of accounts).
//! - `gts.cf.bss.ledger.fiscal_period.v1~` — a fiscal period (`close`).
//! - `gts.cf.bss.ledger.payment.v1~` — a payment (`write` — settle a receipt /
//!   allocate it to receivables; `read` — list a payment's allocations / read the
//!   payer's unallocated pool).
//! - `gts.cf.bss.ledger.credit_application.v1~` — a reusable-credit wallet
//!   operation (`write` — grant pool cash into the wallet / apply the wallet to
//!   receivables).
//! - `gts.cf.bss.ledger.dispute.v1~` — a chargeback dispute (`write` — record a
//!   dispute phase: open / win / lose).
//! - `gts.cf.bss.ledger.dual_control_policy.v1~` — the tenant dual-control
//!   threshold policy (`write` — append an effective-dated version; `read` — the
//!   effective policy).
//! - `gts.cf.bss.ledger.recognition.v1~` — ASC 606 revenue recognition (`write` —
//!   trigger a run / change a schedule; `read` — runs / schedules /
//!   disaggregation).
//! - `gts.cf.bss.ledger.reconciliation.v1~` — reconciliation & revenue assurance
//!   (`run` — trigger a check; `resolve` — resolve a close-blocking exception;
//!   `read` — the exception queue / recon runs).
//!
//! Each action sits on its real object (a noun), never an authz tier: the
//! "billing-setup" role grants `provision` + `read` on `ledger` + `close` on
//! `fiscal_period`.
//!
//! The PEP advertises NO tenant-subtree capability (`PolicyEnforcer::new`), so
//! the PDP pre-expands the caller's seller subtree to a flat
//! `AccessScope::In([...])` that SecureORM binds to the `tenant_id` column.

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

/// Authz `resource_type` label strings (the PDP-visible glob targets).
///
/// Kept as plain `&'static str` consts so both the [`resource_types`]
/// descriptors used at enforcement time and the GTS permission catalog declared
/// in the `bss-ledger` crate (`crate::gts::permissions`) share one source of
/// truth.
pub mod labels {
    /// Journal entries / balances — data plane (`post`, `reverse`, `read`).
    /// OUTSIDE `gts.cf.resources.*` so only an explicit billing role covers it.
    pub const ENTRY: &str = "gts.cf.bss.ledger.entry.v1~";
    /// The seller's ledger — `provision` seeds its chart of accounts, currency
    /// scales, fiscal calendar, and first period; `read` lists its chart of
    /// accounts. OUTSIDE `gts.cf.resources.*`.
    pub const LEDGER: &str = "gts.cf.bss.ledger.ledger.v1~";
    /// A fiscal period — `close` transitions it `OPEN`→`CLOSED`. OUTSIDE
    /// `gts.cf.resources.*`.
    pub const FISCAL_PERIOD: &str = "gts.cf.bss.ledger.fiscal_period.v1~";
    /// A payment — `write` settles a receipt / allocates it to receivables;
    /// `read` lists a payment's allocations / the payer's unallocated pool.
    /// OUTSIDE `gts.cf.resources.*`.
    pub const PAYMENT: &str = "gts.cf.bss.ledger.payment.v1~";
    /// A reusable-credit wallet operation — `write` grants the payer's
    /// unallocated pool cash into the wallet sub-grain, or applies the wallet to
    /// open receivables (architecture §5.2). OUTSIDE `gts.cf.resources.*`.
    pub const CREDIT_APPLICATION: &str = "gts.cf.bss.ledger.credit_application.v1~";
    /// A chargeback dispute — `write` records a dispute phase (open / win /
    /// lose) on a payment; `read` lists disputes / reads one by id (architecture
    /// §4.5). OUTSIDE `gts.cf.resources.*`.
    pub const DISPUTE: &str = "gts.cf.bss.ledger.dispute.v1~";
    /// The tenant dual-control threshold policy — `write` appends an
    /// effective-dated D2/A6/TTL version (DC8); `read` returns the effective
    /// policy. Its OWN resource (not a `ledger` action) so a governance-officer
    /// role grants threshold read/write independently of ledger provisioning or
    /// the `entry` data plane. OUTSIDE `gts.cf.resources.*`.
    pub const DUAL_CONTROL_POLICY: &str = "gts.cf.bss.ledger.dual_control_policy.v1~";
    /// ASC 606 revenue recognition — `write` triggers a recognition run or
    /// changes a schedule; `read` lists runs / schedules / revenue disaggregation.
    /// Its OWN resource (a job-driven revenue-accounting domain, not the `entry`
    /// data plane) so a revenue-accountant role grants recognition read/write
    /// independently of posting refunds / notes. OUTSIDE `gts.cf.resources.*`.
    pub const RECOGNITION: &str = "gts.cf.bss.ledger.recognition.v1~";
    /// Reconciliation & Revenue Assurance — `read` lists the exception queue /
    /// reconciliation runs; `run` triggers a reconciliation check. Its OWN resource
    /// (a distinct Revenue-Assurance surface, mirroring how `dispute` / `recognition`
    /// got their own) so a revenue-assurance role grants recon read/run independently
    /// of the `entry` data plane or `fiscal_period` close. OUTSIDE `gts.cf.resources.*`.
    pub const RECONCILIATION: &str = "gts.cf.bss.ledger.reconciliation.v1~";
    /// Tenant ledger config plane (VHP-1853 invoice-posting policy + VHP-1986 FX
    /// revaluation mode) — `write` appends an effective-dated version of a tenant
    /// setting; `read` the effective value. ONE shared config resource so a
    /// billing-config role grants these tenant settings together, independently of
    /// the `entry` data plane. NOTE: `dual_control_policy` is deliberately a
    /// SEPARATE resource (segregation of duties — a config admin must not be able
    /// to weaken its own approval thresholds). OUTSIDE `gts.cf.resources.*`.
    pub const LEDGER_CONFIG: &str = "gts.cf.bss.ledger.config.v1~";

    /// Every authz label, stable order. The single canonical list driving the
    /// per-label stub type-schema registration (see
    /// [`super::authz_label_type_schemas`]) that lets RBAC role-definitions
    /// target any ledger label. MUST match the permission catalog's distinct
    /// `resource_type`s (`crate::gts::permissions`); a drift test enforces it.
    pub const ALL: &[&str] = &[
        ENTRY,
        LEDGER,
        FISCAL_PERIOD,
        PAYMENT,
        CREDIT_APPLICATION,
        DISPUTE,
        DUAL_CONTROL_POLICY,
        RECOGNITION,
        RECONCILIATION,
        LEDGER_CONFIG,
    ];
}

/// PEP action names for the ledger surfaces.
pub mod actions {
    /// Post a balanced journal entry (data plane, write).
    pub const POST: &str = "post";
    /// Reverse a posted journal entry (strict line-negation) or post a
    /// `MAPPING_CORRECTION` (data plane, write). Distinct from [`POST`] so a
    /// role can grant original posting without granting reversal authority.
    pub const REVERSE: &str = "reverse";
    /// Read action — used by `entry` (balances / journal / records), `ledger`
    /// (chart of accounts), `payment` (allocations / unallocated / settlement),
    /// `dispute` (list / by-id), `dual_control_policy` (effective policy), and
    /// `recognition` (runs / schedules / disaggregation). The resource scopes what
    /// the read authorizes.
    pub const READ: &str = "read";
    /// Annotate an entry / line with a controlled non-financial note (Group 2B;
    /// the `PATCH …/annotation` surface — the typed `description` overlay).
    /// Distinct from [`POST`] / [`REVERSE`] so a role can grant annotation edits
    /// without granting the authority to post or reverse financial entries, and
    /// distinct from any authoritative workflow state (e.g. dispute, which is
    /// owned by `dispute × write`).
    pub const ANNOTATE: &str = "annotate";
    /// Read the secured audit surface for an entry / document / tamper-status
    /// (Group 2C; the `GET …/audit/*` surface). Distinct from [`READ`] so a
    /// forensic/audit role can be granted the audit-retrieval surface (incl. the
    /// cross-tenant elevation gate) without granting balance/chart reads or any
    /// write authority.
    pub const AUDIT_READ: &str = "audit_read";
    /// Erase a payer's PII map (GDPR right-to-erasure; Group 3A, the
    /// `POST …/audit/erasure` surface). DPO-scoped — distinct from every other
    /// action so a Data Protection Officer role grants erasure without granting
    /// posting, reversal, or audit-read authority.
    pub const ERASE: &str = "erase";
    /// Re-identify a (possibly erased) payer's PII reference (forensic; Group 3A,
    /// the `POST …/audit/reidentify` surface). Distinct from [`ERASE`] /
    /// [`AUDIT_READ`] so the elevated forensic re-identify can be granted on its
    /// own.
    pub const REIDENTIFY: &str = "reidentify";
    /// Provision a seller's ledger (control plane).
    pub const PROVISION: &str = "provision";
    /// Close a fiscal period (control plane).
    pub const CLOSE: &str = "close";
    /// Write action — used by `payment` (settle / allocate, decision K),
    /// `dual_control_policy` (append an effective-dated threshold version, DC8),
    /// and `recognition` (trigger a run / change a schedule). One uniform `write`
    /// verb; the resource scopes what it authorizes (so a governance-officer or
    /// revenue-accountant grant is independent of payment writes).
    pub const WRITE: &str = "write";
    /// Approve (or reject / return) a dual-control governed mutation (data plane,
    /// write). Distinct from [`POST`]/[`REVERSE`]/[`WRITE`] so a role can grant
    /// posting authority without granting approval authority — and the
    /// `preparer ≠ approver` rule is enforced server-side (VHP-1852).
    pub const APPROVE: &str = "approve";
    /// Trigger a reconciliation check (the `reconciliation` control plane, Slice 7
    /// Phase 3). Distinct from [`READ`] so a revenue-assurance role can read the
    /// exception queue / recon runs without triggering runs.
    pub const RUN: &str = "run";
    /// Resolve / acknowledge / approve a close-blocking exception (the
    /// `reconciliation` plane, Slice 7 Phase 2 — `OPEN→ACK→RESOLVED` /
    /// `APPROVED_EXCEPTION`). Distinct from [`READ`] (a dashboard viewer cannot
    /// mutate) and [`RUN`] (resolving an exception is not triggering a check).
    pub const RESOLVE: &str = "resolve";
}

/// Properties the PEP may compile from PDP constraints for ledger rows. Every
/// ledger row is tenant-owned: `owner_tenant_id` is the tenant column the
/// secure-ORM filter binds to, `id` the row PK (entry-level gates). NO
/// subtree/group property — the PDP pre-expands the subtree to a flat `In`
/// (decision A).
pub const SUPPORTED_PROPERTIES: &[&str] =
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID];

/// PEP resource-type descriptors (one `const` per ledger authz label).
pub mod resource_types {
    use super::{ResourceType, SUPPORTED_PROPERTIES, labels};

    /// Journal entries / balances — data plane (`post`, `reverse`, `read`).
    pub const ENTRY: ResourceType = ResourceType::from_static(labels::ENTRY, SUPPORTED_PROPERTIES);
    /// The seller's ledger — `provision`, `read`.
    pub const LEDGER: ResourceType =
        ResourceType::from_static(labels::LEDGER, SUPPORTED_PROPERTIES);
    /// A fiscal period — `close`.
    pub const FISCAL_PERIOD: ResourceType =
        ResourceType::from_static(labels::FISCAL_PERIOD, SUPPORTED_PROPERTIES);
    /// A payment — `write` (settle / allocate), `read` (list allocations /
    /// unallocated).
    pub const PAYMENT: ResourceType =
        ResourceType::from_static(labels::PAYMENT, SUPPORTED_PROPERTIES);
    /// A reusable-credit wallet operation — `write` (grant / apply).
    pub const CREDIT_APPLICATION: ResourceType =
        ResourceType::from_static(labels::CREDIT_APPLICATION, SUPPORTED_PROPERTIES);
    /// A chargeback dispute — `write` (record a phase), `read` (list / by-id).
    pub const DISPUTE: ResourceType =
        ResourceType::from_static(labels::DISPUTE, SUPPORTED_PROPERTIES);
    /// The tenant dual-control threshold policy — `write` (append a version),
    /// `read` (effective policy).
    pub const DUAL_CONTROL_POLICY: ResourceType =
        ResourceType::from_static(labels::DUAL_CONTROL_POLICY, SUPPORTED_PROPERTIES);
    /// ASC 606 revenue recognition — `write` (trigger run / change schedule),
    /// `read` (runs / schedules / disaggregation).
    pub const RECOGNITION: ResourceType =
        ResourceType::from_static(labels::RECOGNITION, SUPPORTED_PROPERTIES);
    /// Reconciliation & Revenue Assurance — `read` (exception queue / recon runs),
    /// `run` (trigger a reconciliation check).
    pub const RECONCILIATION: ResourceType =
        ResourceType::from_static(labels::RECONCILIATION, SUPPORTED_PROPERTIES);
    /// The tenant ledger config plane (invoice-posting policy + FX revaluation
    /// mode) — `write` (append a version of a setting), `read` (effective values).
    pub const LEDGER_CONFIG: ResourceType =
        ResourceType::from_static(labels::LEDGER_CONFIG, SUPPORTED_PROPERTIES);
}

/// Error from the ledger authz gate.
#[derive(Debug, thiserror::Error)]
pub enum AuthzError {
    /// The PDP explicitly denied access (or returned uncompilable constraints).
    #[error("permission denied: {0}")]
    Denied(String),
    /// The PDP was unreachable or its response could not be compiled.
    #[error("authz unavailable: {0}")]
    Unavailable(String),
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
/// types-registry (`get_type_schema`), so registering these at gear init lets a
/// custom billing role target any ledger authz label.
#[must_use]
pub fn authz_label_type_schemas() -> Vec<serde_json::Value> {
    labels::ALL
        .iter()
        .map(|label| authz_type_schema_json(label, &format!("BSS Ledger authz label {label}")))
        .collect()
}

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
/// `require_constraints` must be `true` on every authorizing path here — reads
/// (so the scope is a real SQL filter and an unconstrained *allow* fail-closes
/// instead of leaking every tenant) and writes (so the target-membership
/// assertion above has a constraint to test). Pass `false` only for a pure
/// allow/deny gate with no tenant anchor; no ledger path currently does.
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
    owner_tenant_id: Option<Uuid>,
    resource_id: Option<Uuid>,
    require_constraints: bool,
) -> Result<AccessScope, AuthzError> {
    let mut request = AccessRequest::new().require_constraints(require_constraints);
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
            authz_resolver_sdk::EnforcerError::Denied { .. }
            | authz_resolver_sdk::EnforcerError::CompileFailed(_) => {
                AuthzError::Denied(e.to_string())
            }
            authz_resolver_sdk::EnforcerError::EvaluationFailed(_) => {
                AuthzError::Unavailable(e.to_string())
            }
        })?;

    // Write paths anchor to a target tenant and pass `require_constraints =
    // true`: the degraded flat-`In` PDP decision does NOT re-validate
    // `owner_tenant_id`, so assert the target is a member of the compiled scope
    // here — a target outside the caller's authorized tenants is a cross-tenant
    // write and is denied. Reads pass `owner_tenant_id = None` and use the
    // scope as the SQL filter, so this membership check is write-only.
    if let Some(target) = owner_tenant_id
        && require_constraints
        && !scope.contains_uuid(pep_properties::OWNER_TENANT_ID, target)
    {
        return Err(AuthzError::Denied(format!(
            "subject not authorized to write resources owned by tenant {target}"
        )));
    }
    Ok(scope)
}

#[cfg(test)]
#[path = "authz_tests.rs"]
mod authz_tests;
