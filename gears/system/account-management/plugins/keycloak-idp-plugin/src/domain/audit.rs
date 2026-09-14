//! Structured audit-event emitters for `keycloak_idp_plugin.events`.
//!
//! Until the platform audit sink lands these emit as plain `tracing::info!`
//! records on the dedicated target (DESIGN "External durable audit prerequisite" deferred wiring). The
//! per-`kind` field set follows DESIGN "Audit and Metrics Boundary" (common fields)
//! (event-kinds table).
//!
//! Each emitter is `#[inline]` and side-effect-free aside from the
//! `tracing::info!` call; the production hot-paths in [`TenantFacade`] and
//! [`UserFacade`] invoke these directly at success / failure sites.
//!
//! [`TenantFacade`]: crate::domain::tenant_facade::TenantFacade
//! [`UserFacade`]: crate::domain::user_facade::UserFacade

use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::metadata_codec::RealmBinding;

/// Resolve the common `actor_*` fields per DESIGN "Audit and Metrics Boundary".
///
/// Returns `(subject_id, subject_type_class, subject_type_raw, subject_tenant_id)`.
///
/// `subject_type_class` is the dashboard-stable bucket (closed set):
///
/// * `"am.system"` — AM-internal system actor (the `for_module_init` /
///   `for_bootstrap` shape).
/// * `"keycloak_idp.plugin"` — this plugin's own system actor, identified by its
///   stable [`KEYCLOAK_IDP_PLUGIN_ACTOR_UUID`] — NOT by its `subject_type` string.
///   The authz `subject_type` is now the canonical service-account GTS (so
///   the resolver classifies + RBAC-authorizes it), which would otherwise
///   collapse this actor into the `"rest"` bucket; keying the audit class on
///   the unforgeable UUID preserves the attribution.
/// * `"rest"` — any other non-system actor with a `subject_type` set on
///   the [`SecurityContext`].
/// * `"anonymous"` — no `subject_type` recorded on the context.
///
/// `subject_type_raw` is the verbatim string from `SecurityContext::
/// subject_type()` (or `"anonymous"` when none) so a future actor
/// (e.g. `"cf.workflow.plugin"`) is still distinguishable in the
/// operator audit stream — without that field, every new actor type
/// silently collapses into the `"rest"` bucket and gets mis-attributed
/// as REST-forwarded.
fn common_fields(actor: &SecurityContext) -> (Uuid, &'static str, String, Uuid) {
    let id = actor.subject_id();
    let raw = actor.subject_type().unwrap_or("anonymous").to_owned();
    // Attribute this plugin's own system actor by its stable, unforgeable
    // UUID rather than its `subject_type` string: the authz subject_type is
    // the canonical service-account GTS now, so a string match would drop
    // it into `"rest"`. Check the UUID first; fall back to the string buckets.
    let class: &'static str = if id == crate::domain::system_actor::KEYCLOAK_IDP_PLUGIN_ACTOR_UUID {
        "keycloak_idp.plugin"
    } else {
        match actor.subject_type() {
            Some("am.system") => "am.system",
            Some(_) => "rest",
            None => "anonymous",
        }
    };
    (id, class, raw, actor.subject_tenant_id())
}

/// Stringify a [`RealmBinding`] for the `realm_binding` field. Matches the
/// snake-case wire shape persisted on the metadata blob.
fn realm_binding_str(b: RealmBinding) -> &'static str {
    match b {
        RealmBinding::Shared => "shared",
        RealmBinding::Adopted => "adopted",
        RealmBinding::Created => "created",
    }
}

/// `kind = "tenant.bound"` — fired on every `provision_tenant` success.
/// `created_realm` is `true` iff the binding mode is [`RealmBinding::Created`]
/// (in which case the realm itself was provisioned by this run).
pub fn emit_tenant_bound(
    actor: &SecurityContext,
    tenant_id: Uuid,
    realm_name: &str,
    realm_binding: RealmBinding,
    admin_secret_ref: Option<&str>,
) {
    let (actor_subject_id, actor_subject_type, actor_subject_type_raw, actor_tenant_id) =
        common_fields(actor);
    let realm_binding_str = realm_binding_str(realm_binding);
    let created_realm = matches!(realm_binding, RealmBinding::Created);
    tracing::info!(
        target: "keycloak_idp_plugin.events",
        kind = "tenant.bound",
        %actor_subject_id,
        actor_subject_type,
        actor_subject_type_raw,
        %actor_tenant_id,
        %tenant_id,
        realm_name,
        realm_binding = realm_binding_str,
        created_realm,
        admin_secret_ref = admin_secret_ref.unwrap_or("<env-backed>"),
        "tenant bound"
    );
}

/// `kind = "tenant.unbound"` — fired on every `deprovision_tenant` success
/// (including 404/410 "already gone" branches). `already_absent = true` when
/// the deprovision short-circuited because metadata was `None` (the
/// `am.system` missing-metadata branch) or the tenant group came back
/// 404/410. `removed_realm = true` when Created-mode last-tenant teardown
/// also `DELETE`d the realm.
pub fn emit_tenant_unbound(
    actor: &SecurityContext,
    tenant_id: Uuid,
    realm_name: &str,
    realm_binding: Option<RealmBinding>,
    already_absent: bool,
    removed_realm: bool,
) {
    let (actor_subject_id, actor_subject_type, actor_subject_type_raw, actor_tenant_id) =
        common_fields(actor);
    let realm_binding_str = match realm_binding {
        Some(b) => realm_binding_str(b),
        None => "unknown",
    };
    let realm_name_str = if realm_name.is_empty() {
        "unknown"
    } else {
        realm_name
    };
    tracing::info!(
        target: "keycloak_idp_plugin.events",
        kind = "tenant.unbound",
        %actor_subject_id,
        actor_subject_type,
        actor_subject_type_raw,
        %actor_tenant_id,
        %tenant_id,
        realm_name = realm_name_str,
        realm_binding = realm_binding_str,
        already_absent,
        removed_realm,
        "tenant unbound"
    );
}

/// `kind = "admin_user.bound"` — fired when `provision_shared` writes
/// `tenant_id` + `user_type` attributes onto a Phase-0-provisioned KC
/// user (the optional `admin_user_id` metadata branch). Security-relevant:
/// the bind mutates KC-side attributes that downstream authz consumes,
/// so the operator audit log must carry this event distinct from the
/// generic `tenant.bound`.
pub fn emit_admin_user_bound(
    actor: &SecurityContext,
    tenant_id: Uuid,
    realm_name: &str,
    admin_user_id: Uuid,
) {
    let (actor_subject_id, actor_subject_type, actor_subject_type_raw, actor_tenant_id) =
        common_fields(actor);
    tracing::info!(
        target: "keycloak_idp_plugin.events",
        kind = "admin_user.bound",
        %actor_subject_id,
        actor_subject_type,
        actor_subject_type_raw,
        %actor_tenant_id,
        %tenant_id,
        realm_name,
        realm_binding = "shared",
        %admin_user_id,
        "admin user bound to tenant"
    );
}

/// `kind = "realm.created"` — emitted alongside `tenant.bound` on a
/// successful Created-mode provision (the realm itself was provisioned).
pub fn emit_realm_created(actor: &SecurityContext, tenant_id: Uuid, realm_name: &str) {
    let (actor_subject_id, actor_subject_type, actor_subject_type_raw, actor_tenant_id) =
        common_fields(actor);
    tracing::info!(
        target: "keycloak_idp_plugin.events",
        kind = "realm.created",
        %actor_subject_id,
        actor_subject_type,
        actor_subject_type_raw,
        %actor_tenant_id,
        %tenant_id,
        realm_name,
        realm_binding = "created",
        "realm created"
    );
}

/// `kind = "realm.removed"` — emitted alongside `tenant.unbound` when a
/// Created-mode last-tenant teardown also `DELETE`s the realm.
pub fn emit_realm_removed(actor: &SecurityContext, tenant_id: Uuid, realm_name: &str) {
    let (actor_subject_id, actor_subject_type, actor_subject_type_raw, actor_tenant_id) =
        common_fields(actor);
    tracing::info!(
        target: "keycloak_idp_plugin.events",
        kind = "realm.removed",
        %actor_subject_id,
        actor_subject_type,
        actor_subject_type_raw,
        %actor_tenant_id,
        %tenant_id,
        realm_name,
        realm_binding = "created",
        "realm removed"
    );
}

/// `kind = "user.provisioned"` — fired on every `provision_user` success.
pub fn emit_user_provisioned(
    actor: &SecurityContext,
    tenant_id: Uuid,
    realm_name: &str,
    realm_binding: RealmBinding,
    user_id: Uuid,
    username: &str,
) {
    let (actor_subject_id, actor_subject_type, actor_subject_type_raw, actor_tenant_id) =
        common_fields(actor);
    let realm_binding_str = realm_binding_str(realm_binding);
    tracing::info!(
        target: "keycloak_idp_plugin.events",
        kind = "user.provisioned",
        %actor_subject_id,
        actor_subject_type,
        actor_subject_type_raw,
        %actor_tenant_id,
        %tenant_id,
        realm_name,
        realm_binding = realm_binding_str,
        %user_id,
        username,
        "user provisioned"
    );
}

/// `kind = "user.deprovisioned"` — fired on every `deprovision_user`
/// success (including the 404/410 idempotency branches; `already_absent`
/// distinguishes them).
pub fn emit_user_deprovisioned(
    actor: &SecurityContext,
    tenant_id: Uuid,
    realm_name: &str,
    realm_binding: RealmBinding,
    user_id: Uuid,
    already_absent: bool,
) {
    let (actor_subject_id, actor_subject_type, actor_subject_type_raw, actor_tenant_id) =
        common_fields(actor);
    let realm_binding_str = realm_binding_str(realm_binding);
    tracing::info!(
        target: "keycloak_idp_plugin.events",
        kind = "user.deprovisioned",
        %actor_subject_id,
        actor_subject_type,
        actor_subject_type_raw,
        %actor_tenant_id,
        %tenant_id,
        realm_name,
        realm_binding = realm_binding_str,
        %user_id,
        already_absent,
        "user deprovisioned"
    );
}

/// `kind = "service_accounts.purged"` — fired during tenant
/// deprovisioning after live service-account clients owned by the tenant
/// are deleted from the shared service-account realm.
pub fn emit_service_accounts_purged(
    actor: &SecurityContext,
    tenant_id: Uuid,
    deleted_count: usize,
) {
    let (actor_subject_id, actor_subject_type, actor_subject_type_raw, actor_tenant_id) =
        common_fields(actor);
    tracing::info!(
        target: "keycloak_idp_plugin.events",
        kind = "service_accounts.purged",
        %actor_subject_id,
        actor_subject_type,
        actor_subject_type_raw,
        %actor_tenant_id,
        %tenant_id,
        deleted_count,
        "service accounts purged for tenant"
    );
}
