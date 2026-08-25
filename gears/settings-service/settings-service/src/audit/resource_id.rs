// Created: 2026-08-13 by Constructor Tech
//! The canonical audit resource identifier.
//!
//! Every audit record this service writes carries a `resource` field built
//! here, so per-`(setting, scope)` history is a plain exact-match query against
//! the platform Audit Subsystem and no local audit table is needed.
//!
//! DESIGN.md §4.2 requires the **same formatter** on both sides — the audit
//! write and the history read — because the format is a single point of truth.
//! Two spellings of one id would silently split a setting's history in half,
//! and the half that went missing would be the half nobody was looking at.

use uuid::Uuid;

use settings_service_sdk::SettingKey;

/// Prefix marking an audit resource owned by this service.
const PREFIX: &str = "cf.settings:";

/// Separator between the setting key and its scope.
const SCOPE_SEPARATOR: char = '@';

/// Scope sentinel for the platform row, where `tenant_id IS NULL`.
const PLATFORM_SCOPE: &str = "platform";

/// The scope an audit record is written against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditScope {
    /// The platform-wide row.
    Platform,
    /// A tenant's own row.
    ///
    /// Keyed by the **flat tenant UUID**, never a tenant path. A path is
    /// derived state — ancestry is resolved by the Tenant Resolver and never
    /// stored — so a path-based id would break every historical record on any
    /// reparent or rename, while the immutable UUID stays valid for the life of
    /// the trail.
    Tenant(Uuid),
}

/// Format the canonical audit resource id for a setting at a scope.
///
/// `cf.settings:{key}@{tenant_id}`, or `cf.settings:{key}@platform` for the
/// platform row.
///
/// A `(setting, scope)` tuple maps to exactly one id, so history is a single
/// exact-match query — no prefix or wildcard search.
#[must_use]
pub fn format(key: &SettingKey, scope: AuditScope) -> String {
    format_raw(key.as_str(), scope)
}

/// The same formatter over an already-rendered identifier.
///
/// A category is audited under its own key rather than a setting key, and both
/// must produce ids through this one function — DESIGN.md §4.2 requires the
/// audit write and the history read to share a single formatter, and a second
/// spelling would split a resource's history in half.
#[must_use]
pub fn format_raw(id: &str, scope: AuditScope) -> String {
    let mut out = String::with_capacity(PREFIX.len() + id.len() + 40);
    out.push_str(PREFIX);
    out.push_str(id);
    out.push(SCOPE_SEPARATOR);
    match scope {
        AuditScope::Platform => out.push_str(PLATFORM_SCOPE),
        AuditScope::Tenant(id) => out.push_str(&id.to_string()),
    }
    out
}

#[cfg(test)]
#[path = "resource_id_tests.rs"]
mod resource_id_tests;
