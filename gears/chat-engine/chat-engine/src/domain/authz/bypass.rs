//! Bypass registry — named wrappers over [`AccessScope::allow_all()`].
//!
//! `AccessScope::allow_all()` MUST NOT appear anywhere in the gear outside
//! this module. Every non-PDP database access uses one of the four wrappers
//! below, so the bypass inventory is auditable by a single grep. Each call
//! site carries a mandatory `// AUTHZ-BYPASS: <reason>` comment and a `@cpt`
//! traceability marker on the adjacent line (DESIGN §3.5.7).
//
// @cpt-cf-chat-engine-constraint-no-allow-all-outside-registry
// @cpt-cf-chat-engine-design-authz-bypass-registry

use toolkit_security::AccessScope;

/// Scope for trusted-internal pipeline writes.
///
/// Used by `finalize_assistant`, `insert_summary_message`, and
/// `insert_assistant_variant_stub` (all in `message_repo.rs`). The owner pair
/// (`owner_tenant_id`, `owner_id`) MUST be derived from the authorized parent
/// session/message read in the SAME database transaction — never from an
/// ambient or system identity.
///
/// // AUTHZ-BYPASS: pipeline write; owner inherited from parent in same txn
/// @cpt-cf-chat-engine-seq-authz-internal-write
#[inline]
#[must_use]
pub fn internal_write_scope() -> AccessScope {
    AccessScope::allow_all()
}

/// Scope for share-token (capability-URL) resolution.
///
/// Used by `find_by_share_token` on the `.public()` route only. No PDP call —
/// the high-entropy token IS the capability grant. Any miss or revoked token
/// MUST return 404 (anti-enumeration); the response projection excludes other
/// tenants' owner fields.
///
/// // AUTHZ-BYPASS: capability-URL read; token is the grant; 404 on miss/revoked
/// @cpt-cf-chat-engine-seq-authz-shared-read
#[inline]
#[must_use]
pub fn capability_read_scope() -> AccessScope {
    AccessScope::allow_all()
}

/// Scope for scheduled system / cross-tenant operations.
///
/// Used by `run_retention_cleanup_all_tenants`,
/// `list_active_sessions_for_tenant`, `list_tenants_with_active_sessions`, and
/// `find_by_session_id_unscoped`. None of these are reachable via HTTP routes.
///
/// // AUTHZ-BYPASS: system cross-tenant op; not HTTP-exposed; test-verified
/// @cpt-cf-chat-engine-design-authz-bypass-registry
#[inline]
#[must_use]
pub fn system_read_scope() -> AccessScope {
    AccessScope::allow_all()
}

/// Scope for globally non-tenant tables and legacy owner-filtered paths whose
/// row scoping is enforced by an explicit `WHERE` predicate rather than the
/// SecureORM scope.
///
/// Used categorically by `session_type_repo`, `plugin_config_repo`,
/// `stream_event_repo`, the unrestricted list/get helpers in `variant_repo`
/// and `reaction_repo`, the entity compute helpers
/// (`compute_next_variant_index`, `compute_next_part_number`), and the legacy
/// `(tenant_id, user_id)`-filtered `SessionRepo` methods superseded by their
/// `*_scoped` variants. These tables/paths are excluded from PDP scoping per
/// DESIGN §3.5.1/§3.5.3.
///
/// // AUTHZ-BYPASS: globally non-tenant table / explicit WHERE predicate; PDP scoping excluded per §3.5.1
/// @cpt-cf-chat-engine-design-authz-bypass-registry
#[inline]
#[must_use]
pub fn unrestricted_table_scope() -> AccessScope {
    AccessScope::allow_all()
}
