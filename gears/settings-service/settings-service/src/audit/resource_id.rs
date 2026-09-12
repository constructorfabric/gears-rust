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

/// The scope an audit record is written against, when it has one.
///
/// `Some` for anything that happens **at a scope** — a value set, reverted,
/// removed or cloned, a tenant's access restricted, a secret resolved. The id
/// is the flat tenant UUID, never a tenant path: a path is derived state,
/// resolved by the Tenant Resolver and never stored, so a path-based id would
/// break every historical record on any reparent or rename, while the
/// immutable UUID stays valid for the life of the trail. Platform scope is the
/// root tenant's id like any other (DESIGN.md §4.1, §4.7).
///
/// `None` for what has no scope to be at. A **declaration** and a **category**
/// are platform-wide definitions, not values held somewhere: they carry no
/// `tenant_id` of their own (§4.7), and there is no tenant whose history they
/// belong to. Writing them against the root tenant would have been a loan, not
/// a fact — and an expensive one, because learning the root tenant means asking
/// the Tenant Resolver, which is a cross-gear call these paths have no reason
/// to make and, from inside their own transaction, cannot make at all.
pub type AuditTenant = Option<Uuid>;

/// Format the canonical audit resource id for a setting at a scope.
///
/// `cf.settings:{key}@{tenant_id}` when the record has a scope, and
/// `cf.settings:{key}` when it has none. The root tenant's id is platform
/// scope; the scopeless form belongs to definitions — a declaration or a
/// category — which exist once for the whole platform.
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
    //
    // A record with no scope carries the key alone. The separator is omitted
    // rather than followed by a placeholder: a sentinel would be a scope that
    // does not exist, and the whole point of the absent tenant is that there is
    // nothing there to name.
    let mut out = String::with_capacity(PREFIX.len() + id.len() + 40);
    out.push_str(PREFIX);
    out.push_str(id);
    if let Some(tenant) = tenant {
        out.push(SCOPE_SEPARATOR);
        out.push_str(&tenant.to_string());
    }
    out
    // @cpt-end:cpt-cf-settings-service-algo-audit-store-resource-id:p1:inst-as-rid-3
    // @cpt-end:cpt-cf-settings-service-algo-audit-store-resource-id:p1:inst-as-rid-2
    // @cpt-end:cpt-cf-settings-service-algo-audit-store-resource-id:p1:inst-as-rid-1
}

#[cfg(test)]
#[path = "resource_id_tests.rs"]
mod resource_id_tests;
