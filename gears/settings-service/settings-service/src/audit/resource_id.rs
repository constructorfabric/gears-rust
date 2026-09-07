// Created: 2026-08-13 by Constructor Tech
//! The canonical audit resource identifier.
//!
//! Every audit record this service writes carries a `resource` field built
//! here, so per-`(setting, scope)` history is a plain exact-match query against
//! the gear's own `audit_records` table (R1) and, later, the platform Audit
//! Subsystem the outbox forwards to (R2).
//!
//! DESIGN.md §4.2 requires the **same formatter** on both sides — the audit
//! write and the history read — because the format is a single point of truth.
//! Two spellings of one id would silently split a setting's history in half,
//! and the half that went missing would be the half nobody was looking at.

use uuid::Uuid;

use settings_service_sdk::SettingKey;

/// Prefix marking an audit resource owned by this service.
// @cpt-dod:cpt-cf-settings-service-dod-audit-store-resource-id:p1
const PREFIX: &str = "cf.settings:";

/// Separator between the setting key and its scope.
const SCOPE_SEPARATOR: char = '@';

/// The tenant an audit record is written against.
///
/// Always a real tenant id, never a sentinel: platform scope is the **root
/// tenant's** id like any other (DESIGN.md §4.1, §4.7). Keyed by the flat tenant
/// UUID, never a tenant path — a path is derived state, resolved by the Tenant
/// Resolver and never stored, so a path-based id would break every historical
/// record on any reparent or rename, while the immutable UUID stays valid for
/// the life of the trail.
pub type AuditTenant = Uuid;

/// Format the canonical audit resource id for a setting at a scope.
///
/// `cf.settings:{key}@{tenant_id}` for every scope; the root tenant's id is
/// platform scope.
///
/// A `(setting, scope)` tuple maps to exactly one id, so history is a single
/// exact-match query — no prefix or wildcard search.
#[must_use]
pub fn format(key: &SettingKey, tenant: AuditTenant) -> String {
    format_raw(key.as_str(), tenant)
}

/// The same formatter over an already-rendered identifier.
///
/// A category is audited under its own key rather than a setting key, and both
/// must produce ids through this one function — DESIGN.md §4.2 requires the
/// audit write and the history read to share a single formatter, and a second
/// spelling would split a resource's history in half.
#[must_use]
pub fn format_raw(id: &str, tenant: AuditTenant) -> String {
    // @cpt-begin:cpt-cf-settings-service-algo-audit-store-resource-id:p1:inst-as-rid-1
    // @cpt-begin:cpt-cf-settings-service-algo-audit-store-resource-id:p1:inst-as-rid-2
    // @cpt-begin:cpt-cf-settings-service-algo-audit-store-resource-id:p1:inst-as-rid-3
    // The key verbatim — immutable for the life of the declaration, so the
    // history stays continuous through every metadata edit — and the flat
    // tenant UUID, never a path a re-parent would invalidate. One pair, one
    // string: per-scope history is an exact match, never a prefix search.
    let mut out = String::with_capacity(PREFIX.len() + id.len() + 40);
    out.push_str(PREFIX);
    out.push_str(id);
    out.push(SCOPE_SEPARATOR);
    out.push_str(&tenant.to_string());
    out
    // @cpt-end:cpt-cf-settings-service-algo-audit-store-resource-id:p1:inst-as-rid-3
    // @cpt-end:cpt-cf-settings-service-algo-audit-store-resource-id:p1:inst-as-rid-2
    // @cpt-end:cpt-cf-settings-service-algo-audit-store-resource-id:p1:inst-as-rid-1
}

#[cfg(test)]
#[path = "resource_id_tests.rs"]
mod resource_id_tests;
