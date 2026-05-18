//! `TenantService` — the central domain orchestrator for the four
//! in-scope tenant-hierarchy-management flows: create-child, read-tenant,
//! list-children, and update-tenant-mutable.
//!
//! The service depends only on the domain-level [`TenantRepo`] and
//! the SDK-level
//! [`account_management_sdk::IdpPluginClient`] traits. All
//! tests in this file use pure in-memory fakes — no DB, no network,
//! no filesystem.

use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use modkit_macros::domain_model;
use modkit_security::{AccessScope, SecurityContext, pep_properties};
use parking_lot::Mutex as PlMutex;
use time::OffsetDateTime;
use tracing::warn;
use uuid::Uuid;

/// AM's authoritative PEP vocabulary (DESIGN §4.2 — Authorization Model).
///
/// Resource type and supported PEP properties are pinned here so the
/// enforcer call sites cannot drift from the contract. Action names
/// match DESIGN §4.2 line 1363; renaming them is a contract change.
pub(crate) mod pep {
    use super::{ResourceType, pep_properties};

    /// `Tenant` resource — `gts.cf.core.am.tenant.v1~`.
    ///
    /// Per DESIGN §4.2 line 1356 the supported PEP properties are
    /// `OWNER_TENANT_ID` and `RESOURCE_ID`. The values supplied by the
    /// AM PEP gate depend on the action:
    ///
    /// * `create`, `list_children` — `OWNER_TENANT_ID = parent_id`
    ///   (the tenant under which the action runs). The action lives
    ///   *in* the parent's scope, and barriers naturally clamp the
    ///   PDP's `parent_id ∈ subtree(Y_caller)` query against
    ///   `tenant_closure.barrier`.
    /// * `read`, `update`, `delete` — `OWNER_TENANT_ID = tenant.id`
    ///   (the row's own id). A tenant row IS its own scope: the
    ///   self-row `(id, id, barrier=0)` makes `id ∈ subtree(id)`
    ///   trivially true, while the strict-ancestor row `(M, T)`
    ///   carries `barrier=1` whenever `T` (or any tenant on the
    ///   `M → T` path excluding `M`) is `self_managed`. Sending
    ///   `parent_id` here would let `M`'s admin read a self-managed
    ///   child `T` because `(M, M, barrier=0)` is always present —
    ///   the barrier bit lives on `(M, T)`, so the PDP must query
    ///   the descendant's own id to consult it.
    ///
    /// `RESOURCE_ID` is conveyed via the standard `resource_id`
    /// argument (the tenant id itself for `read` / `update` / `delete`,
    /// absent for `create` / `list_children` which have no single
    /// target tenant).
    pub const TENANT: ResourceType = ResourceType {
        name: "gts.cf.core.am.tenant.v1~",
        supported_properties: &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    };

    /// Action vocabulary mirroring DESIGN §4.2 line 1363.
    pub mod actions {
        pub const CREATE: &str = "create";
        pub const READ: &str = "read";
        pub const UPDATE: &str = "update";
        pub const DELETE: &str = "delete";
        pub const LIST_CHILDREN: &str = "list_children";
    }
}

use account_management_sdk::{
    CreateTenantRequest, IdpDeprovisionFailure, IdpDeprovisionTenantRequest, IdpPluginClient,
    IdpProvisionFailure, IdpProvisionTenantRequest, IdpTenantContext, Tenant, UpdateTenantRequest,
};
use modkit_odata::{ODataQuery, Page};
use serde_json::Value;
use tenant_resolver_sdk::TenantId;
use types_registry_sdk::{TypesRegistryClient, TypesRegistryError};

use crate::config::AccountManagementConfig;
use crate::domain::error::DomainError;
use crate::domain::idp::ProvisionFailureExt;
use crate::domain::metrics::{
    AM_DEPENDENCY_HEALTH, AM_HIERARCHY_DEPTH_EXCEEDANCE, AM_HIERARCHY_INTEGRITY_REPAIRED,
    AM_HIERARCHY_INTEGRITY_VIOLATIONS, AM_TENANT_RETENTION, MetricKind, emit_gauge_value,
    emit_metric,
};
use crate::domain::tenant::closure::build_activation_rows;
use crate::domain::tenant::context::TenantContext;
use crate::domain::tenant::hooks::TenantHardDeleteHook;
use crate::domain::tenant::integrity::{IntegrityCategory, IntegrityReport, Violation};
use crate::domain::tenant::model::{ChildCountFilter, NewTenant, TenantModel, TenantStatus};
use crate::domain::tenant::repo::TenantRepo;
use crate::domain::tenant::resource_checker::ResourceOwnershipChecker;
use crate::domain::tenant_type::checker::TenantTypeChecker;

/// Upper bound on the byte size of the opaque `IdP`-metadata blob AM
/// will persist into `tenant_idp_metadata` and replay on every
/// subsequent `IdpPluginClient` call. The cap protects against:
///
/// * A buggy / hostile plugin returning a multi-MB
///   `IdpProvisionResult::metadata` blob (which would then be reshipped
///   on every `provision_user` / `deprovision_user` / `list_users` /
///   `deprovision_tenant` call for the tenant — the cost amplifies
///   per user-op, not per provisioning call).
/// * A caller-supplied `provisioning_metadata` / `root_tenant_metadata`
///   blob exceeding the AM-side serialization budget on the
///   activation `SERIALIZABLE` TX.
///
/// 64 KiB is generous for realistic plugin state (Keycloak realm
/// name + vendor org id + a few token-bearing fields fit comfortably
/// in `~1 KiB`) and matches the order-of-magnitude conservatism of
/// `IdpUserPagination::MAX_CURSOR_LEN` (4 KiB) and
/// `child_tenant_name` (255 chars at most). Above this cap the
/// service rejects with `DomainError::Validation` before the `IdP`
/// round-trip / DB write happens. The doc string on
/// `tenant_idp_metadata` (entity + `docs/migration.sql` + SDK
/// `IdpProvisionResult::metadata`) references this constant as the
/// load-bearing cap.
pub const MAX_IDP_METADATA_BYTES: usize = 64 * 1024;

/// Reject an opaque IdP-metadata blob whose serialised JSON
/// representation exceeds [`MAX_IDP_METADATA_BYTES`]. `None`
/// short-circuits to `Ok(())` — the documented "plugin owns no
/// per-tenant state" / "caller submitted no provisioning hint" path
/// has nothing to measure. Used by the create-child saga (both on
/// the caller-supplied input and on the plugin-returned blob) and by
/// the platform-bootstrap saga.
///
/// `pub(crate)` so the bootstrap module shares the same cap without
/// duplicating the const + serde-roundtrip logic.
pub(crate) fn check_idp_metadata_size(
    label: &'static str,
    value: Option<&Value>,
) -> Result<(), DomainError> {
    let Some(v) = value else {
        return Ok(());
    };
    let bytes = serde_json::to_vec(v).map_err(|err| DomainError::Internal {
        diagnostic: format!("{label}: idp metadata serialization failed: {err}"),
        cause: None,
    })?;
    if bytes.len() > MAX_IDP_METADATA_BYTES {
        return Err(DomainError::Validation {
            detail: format!(
                "{label}: idp metadata exceeds the {MAX_IDP_METADATA_BYTES}-byte AM boundary cap (got {} bytes)",
                bytes.len()
            ),
        });
    }
    Ok(())
}

/// Central AM domain service for tenant-hierarchy CRUD.
#[domain_model]
pub struct TenantService<R: TenantRepo> {
    repo: Arc<R>,
    idp: Arc<dyn IdpPluginClient>,
    cfg: AccountManagementConfig,
    /// Cascade hooks registered by sibling AM features (user-groups,
    /// tenant-metadata). Invoked in registration order at the start of
    /// the hard-delete pipeline — before the `IdP` call and the DB
    /// teardown.
    hooks: Arc<PlMutex<Vec<TenantHardDeleteHook>>>,
    /// Resource-ownership probe; owns the `tenant_has_resources` reject
    /// path inside `delete_tenant`.
    resource_checker: Arc<dyn ResourceOwnershipChecker>,
    /// Tenant-type compatibility barrier (FEATURE 2.3
    /// `tenant-type-enforcement`). Invoked at saga step 3
    /// (`inst-algo-saga-type-check`) of `create_tenant` between the
    /// parent-read and the `provisioning` row insert; rejects
    /// incompatible parent / child type pairings before any tenants /
    /// closure rows are written.
    tenant_type_checker: Arc<dyn TenantTypeChecker + Send + Sync>,
    /// Optional GTS Types Registry client used to resolve a tenant's
    /// `tenant_type_uuid` back to its chained-id string before the
    /// service hands a [`Tenant`] to public callers. `None` in
    /// dev / test deployments without a registry plugin (in which case
    /// `Tenant.tenant_type` is left as `None`). Production reads
    /// rely on the registry client's own caching — every CRUD return
    /// fans out one `get_type_schema_by_uuid` call (or one batched
    /// `get_type_schemas_by_uuid` for `list_children`); the cache
    /// keeps real RTT off the hot path.
    types_registry: Option<Arc<dyn TypesRegistryClient>>,
    /// PEP boundary (DESIGN §4.2). Every public CRUD method calls
    /// [`Self::authorize`] before any structural precondition or
    /// repo read. Cross-tenant authorization (subtree clamp,
    /// platform-admin override) is owned by the PDP behind this
    /// enforcer — AM does not duplicate the check at the service
    /// layer. The Tenant Resolver Plugin (separate PR in this
    /// stack) feeds the PDP the tenant hierarchy via the standard
    /// `in_tenant_subtree` constraint.
    enforcer: PolicyEnforcer,
}

pub(super) mod reaper;
pub(super) mod retention;

impl<R: TenantRepo> TenantService<R> {
    /// Construct a fully-wired service. Production wiring lives in the
    /// AM module entry-point ([`crate::module::AccountManagementModule`]):
    /// `types-registry` and `resource-group` are declared as hard `deps`,
    /// so the entry-point hard-resolves `TypesRegistryClient` (passes
    /// [`crate::infra::types_registry::GtsTenantTypeChecker`]) and
    /// `ResourceGroupClient` (passes
    /// [`crate::infra::rg::RgResourceOwnershipChecker`]); a missing
    /// client fails `init`. Tests pass explicit checkers (typically the
    /// inert ones or test fakes) directly.
    #[must_use]
    pub fn new(
        repo: Arc<R>,
        idp: Arc<dyn IdpPluginClient>,
        resource_checker: Arc<dyn ResourceOwnershipChecker>,
        tenant_type_checker: Arc<dyn TenantTypeChecker + Send + Sync>,
        enforcer: PolicyEnforcer,
        cfg: AccountManagementConfig,
    ) -> Self {
        Self {
            repo,
            idp,
            cfg,
            hooks: Arc::new(PlMutex::new(Vec::new())),
            resource_checker,
            tenant_type_checker,
            types_registry: None,
            enforcer,
        }
    }

    /// Wire a Types Registry client used to resolve `tenant_type_uuid`
    /// back to its chained GTS id when lowering [`TenantModel`] to
    /// [`Tenant`] on public CRUD return values. Without this the
    /// service still works — `Tenant.tenant_type` is just left as
    /// `None`. Production wiring (`module.rs`) calls this from
    /// `init` after the registry client resolves from `ClientHub`;
    /// tests (which usually pin an inert tenant-type checker) leave
    /// it unset.
    #[must_use]
    pub fn with_types_registry(mut self, registry: Arc<dyn TypesRegistryClient>) -> Self {
        self.types_registry = Some(registry);
        self
    }

    /// Assemble a [`TenantContext`] for a tenant the reaper / retention
    /// pipeline is about to feed into
    /// [`account_management_sdk::IdpPluginClient::deprovision_tenant`].
    /// Fetches the `tenants` row, resolves `tenant_type_uuid` to the
    /// chained [`gts::GtsSchemaId`] via the configured registry, and
    /// loads the opaque plugin-private metadata from
    /// `tenant_idp_metadata`. A registry blip surfaces as
    /// [`DomainError::service_unavailable`] (uniform with the user-ops
    /// path) so the caller knows to defer the row to the next tick
    /// instead of calling the plugin with a stale or invented type.
    ///
    /// Both retention (`hard_delete_batch`) and the provisioning
    /// reaper consume this helper. A row that has been removed
    /// underneath them returns [`DomainError::NotFound`].
    pub(crate) async fn load_tenant_context(
        &self,
        tenant_id: Uuid,
    ) -> Result<TenantContext, DomainError> {
        let system_scope = AccessScope::allow_all();
        let tenant = self
            .repo
            .find_by_id(&system_scope, tenant_id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                detail: format!("tenant {tenant_id} not found"),
                resource: tenant_id.to_string(),
            })?;
        let registry = self.types_registry.as_ref().ok_or_else(|| {
            DomainError::service_unavailable(format!(
                "tenant_type resolution requires a types-registry client \
                 (tenant {tenant_id} cannot be fed to IdpPluginClient::deprovision_tenant \
                 without one)"
            ))
        })?;
        let tenant_type = match registry
            .get_type_schema_by_uuid(tenant.tenant_type_uuid)
            .await
        {
            Ok(schema) => schema.type_id,
            // Catalog drift: the tenant row references a
            // `tenant_type_uuid` that the registry no longer
            // resolves. SDK contract on `IdpTenantContext::tenant_type`
            // is that the value is the *resolved* chained
            // `GtsSchemaId` ("AM treats failures of the underlying
            // Types Registry reverse-resolve as service-level errors
            // rather than leaking an `Option` into the plugin"), so
            // a synthesised placeholder would violate the contract
            // — a plugin that routes teardown by `tenant_type` could
            // either no-op (leaving IdP state behind) or target the
            // wrong vendor backend. Surface `ServiceUnavailable` so
            // the calling pipeline (`reap_stuck_provisioning` /
            // `hard_delete_batch`) routes the row through its
            // existing `context_load_failed` Defer arm. Drift is
            // operationally observable via the
            // `am.tenant.retention{outcome="context_load_failed"}`
            // counter plus the dedicated `am.tenant.service` warn
            // event below; recovery is a registry restore or a
            // backfill of the missing schema, not a silent fake.
            Err(TypesRegistryError::GtsTypeSchemaNotFound(_)) => {
                tracing::warn!(
                    target: "am.tenant.service",
                    tenant_id = %tenant.id,
                    tenant_type_uuid = %tenant.tenant_type_uuid,
                    "tenant_type uuid not registered (catalog drift); deferring row \
                     to the next cleanup tick -- operator must restore or backfill the \
                     missing type schema before the IdP plugin can be called"
                );
                return Err(DomainError::service_unavailable(format!(
                    "tenant_type uuid {} is not registered in the Types Registry \
                     (catalog drift); deprovision_tenant cannot be called without a \
                     resolved tenant_type",
                    tenant.tenant_type_uuid
                )));
            }
            Err(err) => {
                return Err(DomainError::service_unavailable(format!(
                    "tenant_type resolution failed for tenant {tenant_id}: {err}"
                )));
            }
        };
        let metadata = self
            .repo
            .find_idp_metadata(&system_scope, tenant_id)
            .await?;
        Ok(TenantContext::new(
            tenant.id,
            tenant.name,
            tenant_type,
            metadata,
        ))
    }

    /// Resolve a tenant-type UUID into its chained GTS string via the
    /// configured registry client. Returns `None` when no registry is
    /// wired or when the lookup fails — the public `tenant_type` field
    /// on [`Tenant`] is `Option<String>` precisely so a registry
    /// blip doesn't fail an otherwise-fine read.
    async fn resolve_tenant_type(&self, type_uuid: Uuid) -> Option<String> {
        let registry = self.types_registry.as_ref()?;
        match registry.get_type_schema_by_uuid(type_uuid).await {
            Ok(schema) => Some(schema.type_id.as_ref().to_owned()),
            Err(err) => {
                tracing::warn!(
                    target: "am.tenant.service",
                    tenant_type_uuid = %type_uuid,
                    error = %err,
                    "tenant_type uuid -> chained-id resolution failed; returning None"
                );
                None
            }
        }
    }

    /// Lower an internal [`TenantModel`] to the public [`Tenant`]
    /// shape (consumed by REST consumers / sibling SDK callers via
    /// `tenant-resolver-sdk`).
    ///
    /// **Invariant:** the caller must filter `Provisioning` rows
    /// before invoking this helper — every public CRUD method either
    /// short-circuits with `NotFound` for SDK-invisible rows
    /// (`get_tenant`, `update_tenant`, `delete_tenant`) or filters
    /// them out (`list_children`). The status conversion uses
    /// `TryFrom<TenantStatus>` so a bypass surfaces as
    /// [`DomainError::Internal`] (HTTP 500) instead of a process
    /// panic.
    async fn lower_to_tenant(&self, model: TenantModel) -> Result<Tenant, DomainError> {
        let tenant_type = self.resolve_tenant_type(model.tenant_type_uuid).await;
        let status =
            account_management_sdk::TenantStatus::try_from(model.status).map_err(|_| {
                DomainError::Internal {
                    diagnostic: format!(
                        "tenant {} reached lower_to_tenant with status=Provisioning; \
                     upstream is_sdk_visible filter was bypassed",
                        model.id
                    ),
                    cause: None,
                }
            })?;
        Ok(Tenant {
            id: TenantId(model.id),
            name: model.name,
            status,
            tenant_type,
            parent_id: model.parent_id.map(TenantId),
            self_managed: model.self_managed,
            depth: model.depth,
            created_at: model.created_at,
            updated_at: model.updated_at,
            deleted_at: model.deleted_at,
        })
    }

    /// Batched lowering for `list_children`. Issues one
    /// `get_type_schemas_by_uuid` round-trip for the page so latency
    /// scales with number of pages, not number of rows.
    async fn lower_to_tenant_page(
        &self,
        page: Page<TenantModel>,
    ) -> Result<Page<Tenant>, DomainError> {
        let Page { items, page_info } = page;

        // Fan out one batch lookup for distinct uuids in the page; we
        // tolerate per-uuid registry failures by leaving `tenant_type`
        // as `None` for the affected row (same policy as the
        // single-row helper).
        let mut type_strings: std::collections::HashMap<Uuid, String> =
            std::collections::HashMap::new();
        if let Some(registry) = self.types_registry.as_ref() {
            let mut distinct: Vec<Uuid> = items.iter().map(|m| m.tenant_type_uuid).collect();
            distinct.sort_unstable();
            distinct.dedup();
            if !distinct.is_empty() {
                let resolved = registry.get_type_schemas_by_uuid(distinct).await;
                for (uuid, res) in resolved {
                    if let Ok(schema) = res {
                        type_strings.insert(uuid, schema.type_id.as_ref().to_owned());
                    }
                }
            }
        }

        let mut mapped: Vec<Tenant> = Vec::with_capacity(items.len());
        for m in items {
            let status =
                account_management_sdk::TenantStatus::try_from(m.status).map_err(|_| {
                    DomainError::Internal {
                        diagnostic: format!(
                            "tenant {} reached lower_to_tenant_page with status=Provisioning; \
                             upstream is_sdk_visible retain was bypassed",
                            m.id
                        ),
                        cause: None,
                    }
                })?;
            mapped.push(Tenant {
                id: TenantId(m.id),
                name: m.name,
                status,
                tenant_type: type_strings.get(&m.tenant_type_uuid).cloned(),
                parent_id: m.parent_id.map(TenantId),
                self_managed: m.self_managed,
                depth: m.depth,
                created_at: m.created_at,
                updated_at: m.updated_at,
                deleted_at: m.deleted_at,
            });
        }

        Ok(Page::new(mapped, page_info))
    }

    /// Append a cascade hook. Hooks run in registration order.
    ///
    /// Eventual consistency: each retention tick snapshots the hook
    /// list once at the start (`service/retention.rs:hook_snapshot`)
    /// to avoid re-cloning the registration `Vec` on every row in the
    /// batch. A hook registered while a tick is in flight therefore
    /// takes effect on the **next** tick, not the current one. In
    /// practice all hooks are registered at module-init time before
    /// the first tick fires, so the window is empty in production.
    pub fn register_hard_delete_hook(&self, hook: TenantHardDeleteHook) {
        self.hooks.lock().push(hook);
    }

    /// Borrow the configured retention tick interval (used by the
    /// module `serve` lifecycle entry).
    #[must_use]
    pub fn retention_tick(&self) -> Duration {
        Duration::from_secs(self.cfg.retention.tick_secs)
    }

    /// Borrow the configured `$top` cap for `listChildren` (used by
    /// the REST handler so the operator-tunable cap is honoured at
    /// the API boundary instead of a hardcoded 200).
    #[must_use]
    pub const fn max_list_children_top(&self) -> u32 {
        self.cfg.listing.max_top
    }

    /// Borrow the configured reaper tick interval.
    #[must_use]
    pub fn reaper_tick(&self) -> Duration {
        Duration::from_secs(self.cfg.reaper.tick_secs)
    }

    /// Borrow the configured hard-delete batch size cap.
    #[must_use]
    pub fn hard_delete_batch_size(&self) -> usize {
        self.cfg.retention.hard_delete_batch_size
    }

    /// Borrow the configured provisioning-timeout threshold.
    #[must_use]
    pub fn provisioning_timeout(&self) -> Duration {
        Duration::from_secs(self.cfg.reaper.provisioning_timeout_secs)
    }

    /// Borrow the configured periodic integrity-check job
    /// configuration. Cloned (cheaply — `IntegrityCheckConfig` is a
    /// handful of `u64` / `f64` / `bool` fields) by `serve` before
    /// the loop is spawned so the loop owns its config end-to-end.
    #[must_use]
    pub fn integrity_check_config(&self) -> crate::domain::integrity_check::IntegrityCheckConfig {
        self.cfg.integrity_check.clone()
    }

    // -----------------------------------------------------------------
    // PEP gate
    // -----------------------------------------------------------------

    /// PEP gate. Calls the platform-side `PolicyEnforcer`, returns the
    /// compiled [`AccessScope`] the caller is permitted to see for
    /// `(action, resource_id)` on the `Tenant` resource type. The
    /// PDP owns the cross-tenant decision — including subtree-clamp
    /// and platform-admin override — fed by the Tenant Resolver
    /// Plugin's hierarchy projection.
    ///
    /// `OWNER_TENANT_ID` is supplied by the call site per the
    /// `pep::TENANT` contract: `parent_id` for `create` /
    /// `list_children` (the action lives in the parent's scope) and
    /// the target tenant's own `id` for `read` / `update` / `delete`
    /// (a tenant row IS its own scope; the closure's `barrier` bit
    /// sits on `(ancestor, descendant)` pairs, so the PDP must
    /// receive the row's own id to evaluate barrier-clamp on
    /// self-managed descendants).
    ///
    /// `RESOURCE_ID` is set on the [`AccessRequest`] (when
    /// `resource_id.is_some()`) **in addition** to flowing through
    /// the standard `resource_id` argument. The duplication is
    /// intentional: the standard argument lands on
    /// `EvaluationRequest::resource.id` per the `AuthZEN` spec, while
    /// the `resource_property` slot is what the PEP compiler reads
    /// to bind the `InTenantSubtree(RESOURCE_ID, …)` predicate's
    /// property value at constraint-compile time. Without the
    /// property bind, a PDP that returns `InTenantSubtree` on
    /// `RESOURCE_ID` would compile cleanly but the secure-extension
    /// would have nothing to clamp against.
    ///
    /// Errors:
    /// - PDP `Denied` → [`DomainError::CrossTenantDenied`] (HTTP 403).
    /// - PDP transport failure → [`DomainError::ServiceUnavailable`]
    ///   (HTTP 503). DESIGN §4.3 mandates fail-closed; AM does not
    ///   provide a local authorization fallback.
    /// - Constraint compile failure (unsupported predicate shape,
    ///   empty constraints with `require_constraints=true`, etc.) →
    ///   [`DomainError::CrossTenantDenied`] (HTTP 403). The
    ///   `EnforcerError::CompileFailed → CrossTenantDenied` mapping
    ///   lives in [`crate::domain::error`]; this is **fail-closed**,
    ///   not a 5xx — a misconfigured policy bundle denies access
    ///   rather than leaking it via a silent `allow_all`.
    ///
    /// `require_constraints(true)` — the AM PEP-side
    /// `InTenantSubtree` compiler is live (cyberware-rust#1813) and
    /// the `tenants` entity now declares `resource_col = "id"`, so
    /// the compiled `AccessScope` materialises a subtree-clamp JOIN
    /// against `tenant_closure` at the database. A PDP that emits
    /// `decision: true, constraints: []` against `require_constraints
    /// = true` fails loudly via the `CompileFailed → CrossTenantDenied`
    /// mapping rather than silently widening the read.
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        owner_tenant_id: Uuid,
        resource_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        // Pass both `OWNER_TENANT_ID` (the parent / acted-on tenant,
        // consumed by ownership-style policies) AND `RESOURCE_ID`
        // (the target tenant id, consumed by the `InTenantSubtree`
        // predicate on `tenants.id`). `require_constraints(true)`
        // turns missing-policy / unsupported-constraint shapes into
        // `CrossTenantDenied` (HTTP 403, fail-closed) rather than a
        // silent `allow_all`.
        let mut request = AccessRequest::new()
            .resource_property(pep_properties::OWNER_TENANT_ID, owner_tenant_id)
            .require_constraints(true);
        if let Some(rid) = resource_id {
            request = request.resource_property(pep_properties::RESOURCE_ID, rid);
        }
        let scope = self
            .enforcer
            .access_scope_with(ctx, &pep::TENANT, action, resource_id, &request)
            .await?;
        Ok(scope)
    }

    // -----------------------------------------------------------------
    // Create child tenant (three-step saga)
    // -----------------------------------------------------------------

    /// Implements FEATURE `Create Child Tenant` (flow §2) + `Create-Tenant
    /// Saga` (algo §3). Runs saga steps 1–3 inline.
    ///
    /// # Errors
    ///
    /// - [`DomainError::CrossTenantDenied`] when the PDP denies the
    ///   caller access to the parent tenant.
    /// - [`DomainError::Validation`] when the parent is missing or not
    ///   `Active` (create under a suspended / deleted / provisioning
    ///   parent is rejected).
    /// - [`DomainError::ServiceUnavailable`] when the provider reports a
    ///   clean compensable failure; the `provisioning` row is removed.
    /// - [`DomainError::UnsupportedOperation`] when the provider signals
    ///   it cannot perform the requested provisioning.
    /// - [`DomainError::Internal`] when the provider outcome is ambiguous;
    ///   the `provisioning` row is left for the reaper to compensate.
    // @cpt-begin:cpt-cf-account-management-flow-tenant-hierarchy-management-create-child-tenant:p1:inst-flow-create-service
    // @cpt-begin:cpt-cf-account-management-algo-tenant-hierarchy-management-create-tenant-saga:p1:inst-algo-saga-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-create-child-tenant-saga:p1:inst-dod-create-child-saga
    #[allow(
        clippy::cognitive_complexity,
        reason = "linear saga: authorize -> ancestor check -> validations -> insert -> IdP -> activate; \
                  splitting fragments the saga and obscures compensation branches"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "saga has six steps inline (PEP -> parent fetch -> tenant_type checker -> insert -> IdP dispatch with three compensation branches -> activate -> event); \
                  the hot variant is the linear happy path, hoisting compensation branches out splits the IdP-failure classification ladder into a method body that loses access to the saga's local state and forces re-clones"
    )]
    pub async fn create_tenant(
        &self,
        ctx: &SecurityContext,
        input: CreateTenantRequest,
    ) -> Result<Tenant, DomainError> {
        // PEP gate (DESIGN §4.2). For `Tenant.create` the resource
        // owner is the parent tenant; the child id is not yet a
        // committed resource so `resource_id` is `None`.
        let _scope = self
            .authorize(ctx, pep::actions::CREATE, input.parent_id, None)
            .await?;

        // Pure parse — derives the canonical UUIDv5 from the chained
        // GTS string. No IO. `gts::GtsID::new` enforces the chain
        // shape; `to_uuid()` is the same algorithm Types Registry
        // uses internally (`types-registry-sdk/src/models.rs:152`),
        // so the derived uuid is the lookup key the registry stores
        // under. This replaces the older `tenant_type_uuid` field on
        // `CreateTenantRequest`, removing the AM/IdP state-divergence
        // class where the two could drift. Runs ahead of any
        // registry-backed validation so a malformed `tenant_type`
        // string fails fast as `InvalidTenantType` rather than after
        // a wasted Types Registry round-trip.
        let tenant_type_uuid = gts::GtsID::new(input.tenant_type.as_ref())
            .map_err(|e| DomainError::InvalidTenantType {
                detail: format!("invalid tenant_type chain `{}`: {e}", input.tenant_type),
            })?
            .to_uuid();

        // Saga pre-step: validate parent exists + is Active.
        // allow_all: structural read per DESIGN §4.2 — the PEP has
        // already gated the operation; the parent-status check is a
        // saga precondition, not a data-disclosure read. Runs BEFORE
        // any registry-backed GTS validation so a Types Registry
        // outage cannot mask `parent tenant not found / not active`
        // as a 503 or add external latency to a request that would
        // fail locally anyway — same error-channel protection that
        // `update_tenant` already applies.
        let parent = self
            .repo
            .find_by_id(&AccessScope::allow_all(), input.parent_id)
            .await?
            .ok_or_else(|| DomainError::Validation {
                detail: format!("parent tenant {} not found", input.parent_id),
            })?;
        if !matches!(parent.status, TenantStatus::Active) {
            return Err(DomainError::Validation {
                detail: format!(
                    "parent tenant {} not active (status={:?}); child creation requires active parent",
                    parent.id, parent.status
                ),
            });
        }

        // Validate the caller-supplied tenant name through the
        // published `gts.cf.core.am.tenant.v1~` schema. Mirrors the
        // resource-group `validate_metadata_via_gts` posture: when the
        // registry has the schema the JSON-Schema bounds (`minLength`,
        // `maxLength`) gate the call; when the schema is not yet
        // registered the helper short-circuits to `Ok(())` and the DB
        // `CHECK (length(name) BETWEEN 1 AND 255)` constraint serves
        // as the last-line guard. Tests that pin a deterministic
        // rejection inject a registry that has the schema registered.
        //
        // Runs AFTER the local parent precondition so a missing or
        // inactive parent fails fast on the local read instead of
        // burning a Types Registry round-trip first.
        if let Some(registry) = self.types_registry.as_ref() {
            crate::domain::gts_validation::validate_tenant_name_via_gts(
                &input.name,
                registry.as_ref(),
            )
            .await?;
        }
        // No AM-side schema validation of `input.provisioning_metadata`:
        // the IdP plugin owns that shape end-to-end per the
        // IdP-metadata isolation contract. A misshaped payload
        // surfaces during the IdP call below and routes through
        // the standard `IdpProvisionFailure` ladder.
        //
        // Size-only cap is enforced here: the blob will be reshipped
        // on every subsequent IdP call via `TenantContext::metadata`,
        // so an unbounded payload amplifies per user-op (not per
        // provisioning call). Cap at the AM boundary before the
        // saga's first DB write so an oversize payload never
        // produces a `provisioning` row.
        check_idp_metadata_size(
            "create_tenant.provisioning_metadata",
            input.provisioning_metadata.as_ref(),
        )?;

        // Pre-saga gate — `inst-algo-saga-type-check`. Pre-write
        // tenant-type compatibility barrier (FEATURE 2.3
        // `tenant-type-enforcement`). Runs BEFORE the `provisioning`
        // row insert (saga step 1) so an incompatible parent / child
        // type pairing never produces a `tenants` row, and BEFORE the
        // depth check so type-incompatibility reports as
        // `type_not_allowed` rather than masking under a depth
        // rejection. Registry unavailability surfaces as
        // `service_unavailable` (HTTP 503) with no DB side effects.
        self.tenant_type_checker
            .check_parent_child(parent.tenant_type_uuid, tenant_type_uuid)
            .await?;

        // `checked_add` rather than `saturating_add` so an overflow
        // surfaces as a loud `Internal` rather than silently saturating
        // and tripping the threshold check below with a misleading
        // `observed_depth = u32::MAX`. `AccountManagementConfig::validate`
        // bounds `depth_threshold ≤ MAX_DEPTH_THRESHOLD` (1_000_000),
        // and the schema-level depth column is `INT4` whose values
        // never approach `u32::MAX` in practice, so this is purely
        // defensive.
        let observed_depth = parent.depth.checked_add(1).ok_or_else(|| {
            DomainError::internal(format!(
                "parent.depth + 1 overflowed u32 (parent_id={}, parent.depth={})",
                parent.id, parent.depth,
            ))
        })?;
        let threshold = self.cfg.hierarchy.depth_threshold;
        let threshold_str = threshold.to_string();

        // Per `algo-depth-threshold-evaluation` (feature-tenant-
        // hierarchy-management.md §3 lines 301-308) the contract is:
        //   IF depth ≤ threshold → proceed silently
        //   ELSE IF advisory     → emit + proceed
        //   ELSE strict          → reject with `tenant_depth_exceeded`
        // Both branches fire at `depth > threshold`. Strict-mode is
        // checked first so a strict reject pre-empts the advisory
        // emission at the same boundary.
        // @cpt-begin:cpt-cf-account-management-algo-tenant-hierarchy-management-depth-threshold-evaluation:p1:inst-algo-depth-evaluate-create
        // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-depth-threshold:p1:inst-dod-depth-threshold-create
        if observed_depth > threshold {
            if self.cfg.hierarchy.depth_strict_mode {
                emit_metric(
                    AM_HIERARCHY_DEPTH_EXCEEDANCE,
                    MetricKind::Counter,
                    &[
                        ("mode", "strict"),
                        ("outcome", "reject"),
                        ("threshold", threshold_str.as_str()),
                    ],
                );
                return Err(DomainError::TenantDepthExceeded {
                    detail: format!("child depth {observed_depth} > strict limit {threshold}"),
                });
            }

            // Advisory mode — log + metric, then proceed. The log
            // structure is fingerprinted by AC `inst-algo-depth-
            // advisory-log` (line 305).
            warn!(
                target: "am.tenant.hierarchy",
                tenant_id = %input.child_id,
                parent_id = %parent.id,
                observed_depth,
                threshold,
                "tenant hierarchy advisory depth threshold exceeded"
            );
            emit_metric(
                AM_HIERARCHY_DEPTH_EXCEEDANCE,
                MetricKind::Counter,
                &[
                    ("mode", "advisory"),
                    ("outcome", "warn"),
                    ("threshold", threshold_str.as_str()),
                ],
            );
        }
        // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-depth-threshold:p1:inst-dod-depth-threshold-create
        // @cpt-end:cpt-cf-account-management-algo-tenant-hierarchy-management-depth-threshold-evaluation:p1:inst-algo-depth-evaluate-create

        // Saga step 1 — insert `provisioning` row (no closure writes).
        let new_tenant = NewTenant {
            id: input.child_id,
            parent_id: Some(parent.id),
            name: input.name.clone(),
            self_managed: input.self_managed,
            tenant_type_uuid,
            depth: observed_depth,
        };
        let provisioning_row = self
            .repo
            .insert_provisioning(&AccessScope::allow_all(), &new_tenant)
            .await?;

        // Saga step 2 — invoke IdP provider outside any TX.
        // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-idp-tenant-provision:p1:inst-dod-idp-provision-call
        let mut req = IdpProvisionTenantRequest::new(
            provisioning_row.id,
            parent.id,
            input.name.clone(),
            input.tenant_type.clone(),
        );
        if let Some(meta) = input.provisioning_metadata.clone() {
            req = req.with_metadata(meta);
        }
        let provision_result = match self.idp.provision_tenant(&req).await {
            Ok(result) => {
                emit_metric(
                    AM_DEPENDENCY_HEALTH,
                    MetricKind::Counter,
                    &[
                        ("target", "idp"),
                        ("op", "provision_tenant"),
                        ("outcome", "success"),
                    ],
                );
                result
            }
            Err(failure) => {
                // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-idp-tenant-provisioning-failure:p1:inst-dod-idp-provision-failure-classify
                emit_metric(
                    AM_DEPENDENCY_HEALTH,
                    MetricKind::Counter,
                    &[
                        ("target", "idp"),
                        ("op", "provision_tenant"),
                        ("outcome", failure.as_metric_label()),
                    ],
                );
                match failure {
                    IdpProvisionFailure::CleanFailure { detail } => {
                        // Compensating TX — delete the provisioning row. No
                        // closure cleanup needed: nothing was ever written.
                        // Log compensation failures but always return the
                        // original IdP error so the caller sees the right
                        // variant; a stray provisioning row is handled by
                        // the reaper.
                        if let Err(e) = self
                            .repo
                            // Saga path: pass `None` so the repo
                            // fences the DELETE on `claimed_by IS
                            // NULL`. If a peer reaper claimed the
                            // row mid-IdP-call (timeout-driven
                            // takeover), the compensation MUST
                            // refuse rather than erase the
                            // reaper's in-flight work.
                            .compensate_provisioning(
                                &AccessScope::allow_all(),
                                provisioning_row.id,
                                None,
                            )
                            .await
                        {
                            warn!(
                                target: "am.tenant.saga",
                                tenant_id = %provisioning_row.id,
                                error = %e,
                                "compensate_provisioning failed after IdP CleanFailure; \
                                 provisioning row left for reaper"
                            );
                            // Mirrors the reaper-side
                            // `compensate_failed` counter so the
                            // dashboard can show saga-vs-reaper
                            // compensation ratios. Distinct `job`
                            // label so saga-side compensation
                            // health is separable from the reaper
                            // safety-net.
                            emit_metric(
                                AM_TENANT_RETENTION,
                                MetricKind::Counter,
                                &[
                                    ("job", "saga_compensation"),
                                    ("outcome", "compensate_failed"),
                                ],
                            );
                        }
                        return Err(IdpProvisionFailure::CleanFailure { detail }
                            .into_domain_error(provisioning_row.id));
                    }
                    IdpProvisionFailure::Ambiguous { detail } => {
                        // Leave the provisioning row in place for the reaper.
                        // The `From<IdpProvisionFailure> for DomainError` impl
                        // redacts the raw provider detail so vendor text
                        // never reaches the public envelope.
                        return Err(IdpProvisionFailure::Ambiguous { detail }
                            .into_domain_error(provisioning_row.id));
                    }
                    IdpProvisionFailure::UnsupportedOperation { detail } => {
                        // Treat as clean compensable — no IdP-side state exists.
                        // Same compensation-failure policy as CleanFailure above.
                        if let Err(e) = self
                            .repo
                            // Saga path: pass `None` so the repo
                            // fences the DELETE on `claimed_by IS
                            // NULL`. If a peer reaper claimed the
                            // row mid-IdP-call (timeout-driven
                            // takeover), the compensation MUST
                            // refuse rather than erase the
                            // reaper's in-flight work.
                            .compensate_provisioning(
                                &AccessScope::allow_all(),
                                provisioning_row.id,
                                None,
                            )
                            .await
                        {
                            warn!(
                                target: "am.tenant.saga",
                                tenant_id = %provisioning_row.id,
                                error = %e,
                                "compensate_provisioning failed after IdP UnsupportedOperation; \
                                 provisioning row left for reaper"
                            );
                            emit_metric(
                                AM_TENANT_RETENTION,
                                MetricKind::Counter,
                                &[
                                    ("job", "saga_compensation"),
                                    ("outcome", "compensate_failed"),
                                ],
                            );
                        }
                        return Err(IdpProvisionFailure::UnsupportedOperation { detail }
                            .into_domain_error(provisioning_row.id));
                    }
                    other => {
                        // SDK is `#[non_exhaustive]`; future variants
                        // arrive without an AM-side recompile. We do
                        // not know whether the new variant proves
                        // "no IdP-side state retained" (compensable)
                        // or leaves residue (ambiguous), so we take
                        // the conservative path: leave the
                        // provisioning row for the reaper to
                        // compensate, and surface as the From-impl
                        // wildcard chooses (Internal). Logged loudly
                        // so the missing arm surfaces in operator
                        // logs the moment the new variant ships.
                        tracing::error!(
                            target: "am.tenant.saga",
                            tenant_id = %provisioning_row.id,
                            variant = other.as_metric_label(),
                            "unknown IdpProvisionFailure variant; treating as Ambiguous (provisioning row left for reaper)"
                        );
                        return Err(other.into_domain_error(provisioning_row.id));
                    }
                }
                // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-idp-tenant-provisioning-failure:p1:inst-dod-idp-provision-failure-classify
            }
        };
        // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-idp-tenant-provision:p1:inst-dod-idp-provision-call

        // Saga step 3 — finalize: load the ancestor chain, build closure
        // rows, and flip the tenant to Active in one TX. The caller
        // already holds the parent row in scope, so feed `parent.id`
        // directly to skip the redundant child-row fetch the previous
        // child-keyed shape required.
        //
        // Best-effort IdP+row compensation on step-3 failure: once
        // `provision_tenant` succeeded the IdP holds vendor-side
        // state. If the closure load or `activate_tenant` step then
        // fails, we run a best-effort `deprovision_tenant` + row
        // compensation here so the failure does not leave orphaned
        // IdP state until the next reaper tick. Failures of the
        // compensation itself are logged and the original error is
        // propagated unchanged — the reaper still owns the
        // last-resort cleanup if any compensation step fails.
        // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-idp-tenant-provision:p1:inst-dod-idp-provision-metadata
        let idp_metadata = provision_result.metadata;
        // Size cap on the plugin-returned blob: same
        // `MAX_IDP_METADATA_BYTES` budget as the caller-supplied
        // input above. Reject + run best-effort `deprovision_tenant`
        // compensation so an oversize plugin blob does not orphan
        // vendor-side state OR get persisted into
        // `tenant_idp_metadata` past the boundary cap.
        if let Err(size_err) =
            check_idp_metadata_size("create_tenant.idp_returned_metadata", idp_metadata.as_ref())
        {
            tracing::warn!(
                target: "am.tenant.saga",
                tenant_id = %provisioning_row.id,
                error = %size_err,
                "plugin returned an oversize idp metadata blob; running best-effort \
                 IdP compensation so the vendor-side tenant does not orphan"
            );
            self.compensate_failed_activation(
                &parent,
                provisioning_row.id,
                input.tenant_type.clone(),
                input.name.clone(),
                idp_metadata.as_ref(),
            )
            .await;
            return Err(size_err);
        }
        // Persist the plugin-private metadata BEFORE
        // `finalize_provisioning` opens its SERIALIZABLE TX. If the
        // activation TX then fails AND the best-effort
        // `compensate_failed_activation` cannot confirm vendor-side
        // teardown (Retryable / Terminal / UnsupportedOperation
        // under `idp.required=true`), the row is left for the reaper
        // — and the reaper rebuilds `IdpDeprovisionTenantRequest`
        // exclusively from `tenant_idp_metadata`. Without this
        // up-front upsert, the only copy of the blob would die in
        // this stack frame, the reaper would forward an empty
        // `TenantContext::metadata`, and any plugin that keys
        // vendor-side state off the blob would silently leak the
        // tenant. On a successful activation the in-TX write inside
        // `activate_tenant` overwrites this row with the same value
        // atomically with the status flip (idempotent).
        //
        // # Upsert failure → `?` would orphan IdP state
        //
        // A naked `?` here would bypass `compensate_failed_activation`
        // entirely on a transient DB blip (pool exhaustion,
        // serialization conflict, fatal-but-retryable disconnect), so
        // `provision_tenant` would leave vendor-side state with no
        // local record. Route the upsert failure through the same
        // compensation rung the activation-failure branch uses below
        // — the saga MUST attempt IdP teardown for every post-
        // provision failure, not only the ones that surface from
        // `finalize_provisioning`.
        if let Err(upsert_err) = self
            .repo
            .upsert_idp_metadata(
                &AccessScope::allow_all(),
                provisioning_row.id,
                idp_metadata.as_ref(),
            )
            .await
        {
            tracing::warn!(
                target: "am.tenant.saga",
                tenant_id = %provisioning_row.id,
                error = %upsert_err,
                "saga step-3 pre-activation metadata upsert failed; running best-effort \
                 IdP compensation so the vendor-side tenant does not orphan"
            );
            self.compensate_failed_activation(
                &parent,
                provisioning_row.id,
                input.tenant_type.clone(),
                input.name.clone(),
                idp_metadata.as_ref(),
            )
            .await;
            return Err(upsert_err);
        }
        let activated = match self
            .finalize_provisioning(
                &parent,
                provisioning_row.id,
                input.self_managed,
                idp_metadata.as_ref(),
            )
            .await
        {
            Ok(row) => row,
            Err(err) => {
                // Pass the same `idp_metadata` blob through to the
                // compensation path so `deprovision_tenant` sees the
                // plugin's own per-tenant state when tearing down.
                self.compensate_failed_activation(
                    &parent,
                    provisioning_row.id,
                    input.tenant_type.clone(),
                    input.name.clone(),
                    idp_metadata.as_ref(),
                )
                .await;
                return Err(err);
            }
        };
        // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-idp-tenant-provision:p1:inst-dod-idp-provision-metadata

        // TODO(events): emit AM event when platform event-bus lands.
        // Placeholder log marks the emission point so the future
        // event-bus wiring can replace this site without losing the
        // payload shape.
        tracing::info!(
            target: "am.events",
            kind = "tenantStateChanged",
            actor = "tenant_scoped",
            subject_id = %ctx.subject_id(),
            subject_tenant_id = %ctx.subject_tenant_id(),
            tenant_id = %activated.id,
            event = "created",
            parent_id = ?activated.parent_id,
            tenant_type = %input.tenant_type,
            self_managed = input.self_managed,
            depth = activated.depth,
            "am tenant state changed"
        );

        // Saga step 3 returns an `Active` row, so lowering through the
        // `is_sdk_visible` invariant is safe. We forward the
        // caller-supplied `tenant_type` string verbatim — saga step 3
        // already proved the registry chain is valid (and resolved its
        // schema) so we don't need a registry RTT here.
        let mut info = self.lower_to_tenant(activated).await?;
        if info.tenant_type.is_none() {
            // `Tenant.tenant_type` is `Option<String>` per the
            // tenant-resolver-sdk public shape; lower the typed
            // `GtsSchemaId` back to its wire form.
            info.tenant_type = Some(input.tenant_type.into_string());
        }
        Ok(info)
    }
    // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-create-child-tenant-saga:p1:inst-dod-create-child-saga
    // @cpt-end:cpt-cf-account-management-algo-tenant-hierarchy-management-create-tenant-saga:p1:inst-algo-saga-service
    // @cpt-end:cpt-cf-account-management-flow-tenant-hierarchy-management-create-child-tenant:p1:inst-flow-create-service

    /// Saga step 3 inner pipeline: load ancestor chain → build
    /// closure rows → flip the tenant to `Active`. Extracted so the
    /// caller (`create_tenant`) can wrap it in a single error-handling
    /// `match` and drive [`Self::compensate_failed_activation`] on
    /// any failure.
    async fn finalize_provisioning(
        &self,
        parent: &TenantModel,
        provisioning_id: Uuid,
        self_managed: bool,
        idp_metadata: Option<&Value>,
    ) -> Result<TenantModel, DomainError> {
        // No AM-side schema validation of the IdP-returned blob —
        // the plugin owns the shape end-to-end per the IdP-metadata
        // isolation contract. Whatever the plugin produced is
        // persisted verbatim in `tenant_idp_metadata` and replayed
        // through `TenantContext::metadata` on every subsequent
        // IdP call for this tenant.
        let ancestors = self
            .repo
            .load_ancestor_chain_through_parent(&AccessScope::allow_all(), parent.id)
            .await?;
        let closure_rows = build_activation_rows(
            provisioning_id,
            TenantStatus::Active,
            self_managed,
            ancestors.as_slice(),
        );
        self.repo
            .activate_tenant(
                &AccessScope::allow_all(),
                provisioning_id,
                &closure_rows,
                idp_metadata,
            )
            .await
    }

    /// Best-effort cleanup after `finalize_provisioning` fails post-
    /// `idp.provision_tenant` success. Runs:
    ///   1. `idp.deprovision_tenant` to release vendor-side state
    ///      (skipped on `Retryable` / `Terminal` outcomes — those
    ///      need vendor-side retry, not a row delete);
    ///   2. `compensate_provisioning(claimed_by = None)` to remove
    ///      the local `Provisioning` row, fenced on no peer reaper
    ///      having claimed it (fence rejection means the reaper
    ///      already owns the row and will compensate via its own
    ///      pipeline).
    ///
    /// Every failure here is logged at `am.tenant.saga` and
    /// suppressed — the caller propagates the **original** error
    /// from saga step 3. The reaper remains the last-resort cleanup.
    #[allow(
        clippy::cognitive_complexity,
        reason = "best-effort multi-step compensation: each branch logs a distinct outcome and degrades silently to the reaper; collapsing the IdP/row legs hides which step ran"
    )]
    async fn compensate_failed_activation(
        &self,
        parent: &TenantModel,
        provisioning_id: Uuid,
        tenant_type: gts::GtsSchemaId,
        tenant_name: String,
        idp_metadata: Option<&Value>,
    ) {
        let _ = parent; // reserved for future caller-context logging
        // Step 1 — vendor-side cleanup. We only run the row delete
        // when the IdP confirms there is nothing left to orphan
        // (`Ok` / `NotFound` / `UnsupportedOperation` under
        // `idp.required=false`). Retryable / Terminal failures —
        // and `UnsupportedOperation` from a real plugin under
        // `idp.required=true` — require vendor-side resolution;
        // leave the row for the reaper, which already classifies
        // these outcomes correctly.
        //
        // Build the AM-internal `TenantContext` from the saga's
        // in-scope facts: the in-flight tenant id + name + chained
        // type the caller submitted (saga step 2 already validated
        // the chain shape via `tenant_type_uuid` derivation) plus
        // whatever the plugin returned from `provision_tenant`.
        // Convert to the SDK envelope `IdpTenantContext` at the
        // `IdpPluginClient::deprovision_tenant` boundary so the
        // public plugin contract sees only the SDK type — the
        // internal `TenantContext` is kept distinct so future
        // AM-internal additions do not leak through the SPI.
        let tenant_context = TenantContext::new(
            provisioning_id,
            tenant_name,
            tenant_type,
            idp_metadata.cloned(),
        );
        let idp_clean = match self
            .idp
            .deprovision_tenant(&IdpDeprovisionTenantRequest::new(IdpTenantContext::from(
                &tenant_context,
            )))
            .await
        {
            Ok(()) | Err(IdpDeprovisionFailure::NotFound { .. }) => true,
            Err(IdpDeprovisionFailure::UnsupportedOperation { .. }) => {
                // Symmetric with the retention pipeline (see
                // `process_single_hard_delete`'s `UnsupportedOperation`
                // arm) and the reaper (`classify_deprovision`):
                // `UnsupportedOperation` is only safe to treat as
                // "no IdP-side state retained" when the deployment
                // explicitly opted out of an IdP via
                // `cfg.idp.required = false` (the wired-in
                // `NoopIdpProvider` path). A real plugin returning
                // this variant under `idp.required = true` signals
                // that vendor-side state may exist but the plugin
                // can't deprovision it — hard-deleting the AM row
                // would orphan that state with no local repair
                // handle. Defer to the reaper instead.
                if self.cfg.idp.required {
                    warn!(
                        target: "am.tenant.saga",
                        tenant_id = %provisioning_id,
                        outcome = "unsupported_required",
                        "saga step-3 compensation: IdP plugin returned UnsupportedOperation \
                         but idp.required=true; refusing to orphan vendor-side state, \
                         leaving provisioning row for the reaper"
                    );
                    emit_metric(
                        AM_TENANT_RETENTION,
                        MetricKind::Counter,
                        &[
                            ("job", "saga_compensation"),
                            ("outcome", "unsupported_required"),
                        ],
                    );
                    false
                } else {
                    true
                }
            }
            Err(failure) => {
                let label = failure.as_metric_label();
                warn!(
                    target: "am.tenant.saga",
                    tenant_id = %provisioning_id,
                    outcome = label,
                    "saga step-3 compensation: IdP deprovision did not confirm cleanup; \
                     leaving provisioning row for the reaper"
                );
                // Saga-side step-3 cleanup that did not confirm
                // vendor-side teardown. Distinct `outcome` label so
                // operators can tell this apart from a storage
                // fault on the row delete below; both are still
                // "saga failed → reaper covers", but the remediation
                // is different (IdP plugin health vs DB health).
                emit_metric(
                    AM_TENANT_RETENTION,
                    MetricKind::Counter,
                    &[("job", "saga_compensation"), ("outcome", "idp_unconfirmed")],
                );
                false
            }
        };
        if !idp_clean {
            return;
        }

        // Step 2 — local row delete, fenced on no peer reaper claim
        // and no terminal stamp. A `Conflict` here is the documented
        // "reaper got there first" path — log and let the reaper's
        // own `compensate_provisioning_row` finish the cleanup.
        if let Err(err) = self
            .repo
            .compensate_provisioning(&AccessScope::allow_all(), provisioning_id, None)
            .await
        {
            warn!(
                target: "am.tenant.saga",
                tenant_id = %provisioning_id,
                error = %err,
                "saga step-3 compensation: row delete failed (peer reaper may have \
                 claimed the row mid-activation); leaving for the reaper"
            );
            emit_metric(
                AM_TENANT_RETENTION,
                MetricKind::Counter,
                &[
                    ("job", "saga_compensation"),
                    ("outcome", "compensate_failed"),
                ],
            );
        }
    }

    // -----------------------------------------------------------------
    // Read tenant details
    // -----------------------------------------------------------------

    /// Implements FEATURE `Read Tenant Details`. Returns `NotFound` when
    /// the row is absent OR the row is SDK-invisible (`Provisioning`).
    ///
    /// # Errors
    ///
    /// - [`DomainError::CrossTenantDenied`] when the PDP denies the
    ///   caller access to the target tenant.
    /// - [`DomainError::NotFound`] when the tenant does not exist or is in
    ///   the internal `Provisioning` state.
    // @cpt-begin:cpt-cf-account-management-flow-tenant-hierarchy-management-read-tenant:p1:inst-flow-read-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-tenant-read-scope:p1:inst-dod-read-scope-service
    pub async fn get_tenant(&self, ctx: &SecurityContext, id: Uuid) -> Result<Tenant, DomainError> {
        // PEP gate (DESIGN §4.2) + DB-level subtree clamp (cyberware-rust#1813).
        // The `tenants` entity now declares `resource_col = "id"`, so
        // the compiled `AccessScope` materialises a subtree-clamp
        // JOIN against `tenant_closure` at the database — a caller
        // whose PDP-narrowed scope clamps to a subtree NOT containing
        // `id` collapses to `NotFound` even though `authorize`
        // returned a permit. Defence-in-depth: the gate authorizes
        // the operation, the SQL JOIN clamps the row.
        let scope = self
            .authorize(ctx, pep::actions::READ, id, Some(id))
            .await?;
        let tenant =
            self.repo
                .find_by_id(&scope, id)
                .await?
                .ok_or_else(|| DomainError::NotFound {
                    detail: format!("tenant {id} not found"),
                    resource: id.to_string(),
                })?;
        // `Provisioning` is AM-internal and has no SDK representation.
        // `Deleted` rows are public tombstones (the `Tenant` projection
        // carries `deleted_at`), so they are returned as-is —
        // matching the SDK contract.
        if matches!(tenant.status, TenantStatus::Provisioning) {
            return Err(DomainError::NotFound {
                detail: format!("tenant {id} not found"),
                resource: id.to_string(),
            });
        }
        self.lower_to_tenant(tenant).await
    }
    // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-tenant-read-scope:p1:inst-dod-read-scope-service
    // @cpt-end:cpt-cf-account-management-flow-tenant-hierarchy-management-read-tenant:p1:inst-flow-read-service

    // -----------------------------------------------------------------
    // List children (paginated, status-filterable)
    // -----------------------------------------------------------------

    /// Implements FEATURE `List Children (Paginated, Status-Filterable)`.
    /// The parent itself must exist + be SDK-visible, otherwise the
    /// whole call is `NotFound`.
    ///
    /// # Errors
    ///
    /// - [`DomainError::CrossTenantDenied`] when the PDP denies the
    ///   caller access to the parent tenant.
    /// - [`DomainError::NotFound`] when the parent does not exist or is
    ///   SDK-invisible (`Provisioning`). Repository-level errors are
    ///   propagated unchanged.
    // @cpt-begin:cpt-cf-account-management-flow-tenant-hierarchy-management-list-children:p1:inst-flow-listch-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-children-query-paginated:p1:inst-dod-children-query-service
    pub async fn list_children(
        &self,
        ctx: &SecurityContext,
        parent_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<Tenant>, DomainError> {
        // PEP gate (DESIGN §4.2) + DB-level subtree clamp
        // (cyberware-rust#1813). Authorization for the parent tenant
        // runs through `authorize`; the compiled scope is forwarded
        // verbatim into the parent existence check AND the children
        // listing so a caller whose subtree does not contain
        // `parent_id` collapses to `NotFound`, and a caller whose
        // subtree does contain `parent_id` but excludes some
        // self-managed children sees the listing clamped to their
        // own subtree at the database. `resource_id` is `None` for
        // listings because the call returns a collection.
        let scope = self
            .authorize(ctx, pep::actions::LIST_CHILDREN, parent_id, None)
            .await?;
        let parent = self
            .repo
            .find_by_id(&scope, parent_id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                detail: format!("tenant {parent_id} not found"),
                resource: parent_id.to_string(),
            })?;
        if !parent.status.is_sdk_visible() {
            return Err(DomainError::NotFound {
                detail: format!("tenant {parent_id} not found"),
                resource: parent_id.to_string(),
            });
        }
        let mut page = self.repo.list_children(&scope, parent_id, query).await?;
        // Defense-in-depth: drop any `Provisioning` row that slips
        // through the repo filter before we lower the page to the
        // public shape (where `Provisioning` is unrepresentable).
        page.items.retain(|r| r.status.is_sdk_visible());
        self.lower_to_tenant_page(page).await
    }
    // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-children-query-paginated:p1:inst-dod-children-query-service
    // @cpt-end:cpt-cf-account-management-flow-tenant-hierarchy-management-list-children:p1:inst-flow-listch-service

    // -----------------------------------------------------------------
    // Update tenant (mutable-fields-only)
    // -----------------------------------------------------------------

    /// Implements FEATURE `Update Tenant Mutable Fields`.
    ///
    /// PATCH carries only the `name` field. Lifecycle transitions
    /// (`Active` ↔ `Suspended`, soft-delete) go through dedicated
    /// methods ([`Self::suspend_tenant`], [`Self::unsuspend_tenant`],
    /// [`Self::delete_tenant`]) so each transition stays idempotent on
    /// its own surface.
    ///
    /// # Errors
    ///
    /// - [`DomainError::CrossTenantDenied`] when the PDP denies the
    ///   caller access to the target tenant.
    /// - [`DomainError::Validation`] when the patch is empty or the new
    ///   name fails GTS validation.
    /// - [`DomainError::Conflict`] when the target tenant is in
    ///   `Deleted` status (read-only during retention).
    /// - [`DomainError::NotFound`] when the target tenant does not exist or
    ///   is `Provisioning`.
    // @cpt-begin:cpt-cf-account-management-flow-tenant-hierarchy-management-update-tenant:p1:inst-flow-update-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-update-mutable-only:p1:inst-dod-update-mutable-service
    pub async fn update_tenant(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        patch: UpdateTenantRequest,
    ) -> Result<Tenant, DomainError> {
        if patch.is_empty() {
            return Err(DomainError::Validation {
                detail: "update patch is empty; at least one field required".into(),
            });
        }
        // PEP gate (DESIGN §4.2) + DB-level subtree clamp
        // (cyberware-rust#1813). The compiled scope flows into the
        // pre-update load AND the mutating write below; the
        // secure-extension materialises a `tenants.id IN subtree(...)`
        // JOIN against `tenant_closure` so a caller scoped to a
        // subtree that does NOT contain `id` collapses to `NotFound`
        // on the load (and the write fences inside the SERIALIZABLE
        // retry catch the same clamp on the UPDATE).
        let scope = self
            .authorize(ctx, pep::actions::UPDATE, id, Some(id))
            .await?;
        // Load the current row BEFORE any GTS round-trip so an
        // idempotent same-name PATCH (or a PATCH against a missing
        // tenant) reaches the no-op / `NotFound` path without paying a
        // registry call. A naive ordering that hit GTS first would
        // turn every registry blip into a `503` for PATCH requests
        // that would otherwise be 200 no-ops or 404s.
        let current =
            self.repo
                .find_by_id(&scope, id)
                .await?
                .ok_or_else(|| DomainError::NotFound {
                    detail: format!("tenant {id} not found"),
                    resource: id.to_string(),
                })?;
        // `Provisioning` is AM-internal — surface as `NotFound` so the
        // boundary never leaks the internal status. `Deleted` rows are
        // SDK-visible (tombstone), but read-only — they fall through
        // to the repo guard which returns `Conflict` (defence-in-depth
        // against a future caller that mints a patch through an
        // internal seam).
        if matches!(current.status, TenantStatus::Provisioning) {
            return Err(DomainError::NotFound {
                detail: format!("tenant {id} not found"),
                resource: id.to_string(),
            });
        }
        // Schema validation runs AFTER `authorize` AND `find_by_id`
        // so a registry outage / malformed-schema response surfaces
        // only to already-authorized callers acting on a real row —
        // an out-of-scope caller still gets `403 CrossTenantDenied`,
        // a missing-row caller still gets `404 NotFound`, instead of
        // `503` leaking registry health through the error channel.
        // Mirrors the ordering in `create_tenant`. Skipped when the
        // patched name is identical to the current name (idempotent
        // PATCH — no shape change, no need to validate).
        if let Some(ref new_name) = patch.name
            && new_name != &current.name
            && let Some(registry) = self.types_registry.as_ref()
        {
            crate::domain::gts_validation::validate_tenant_name_via_gts(
                new_name,
                registry.as_ref(),
            )
            .await?;
        }
        let updated = self.repo.update_tenant_mutable(&scope, id, &patch).await?;

        // Suppress the lifecycle event log on an idempotent no-op
        // (option A — true HTTP PATCH idempotency). The repo skips the
        // DB write when `name` is unchanged, so `updated.updated_at`
        // stays equal to `current.updated_at`.
        let was_no_op = updated.updated_at == current.updated_at;
        if !was_no_op {
            // TODO(events): emit AM event when platform event-bus lands.
            tracing::info!(
                target: "am.events",
                kind = "tenantStateChanged",
                actor = "tenant_scoped",
                subject_id = %ctx.subject_id(),
                subject_tenant_id = %ctx.subject_tenant_id(),
                tenant_id = %updated.id,
                event = "updated",
                name_from = %current.name,
                name_to = ?patch.name.as_deref(),
                "am tenant state changed"
            );
        }

        self.lower_to_tenant(updated).await
    }
    // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-update-mutable-only:p1:inst-dod-update-mutable-service
    // @cpt-end:cpt-cf-account-management-flow-tenant-hierarchy-management-update-tenant:p1:inst-flow-update-service

    /// Transition `id` from `Active` to `Suspended`. Idempotent on
    /// already-suspended rows.
    ///
    /// # Errors
    ///
    /// - [`DomainError::CrossTenantDenied`] when the PDP denies the
    ///   caller access.
    /// - [`DomainError::NotFound`] when the tenant does not exist or is
    ///   `Provisioning`.
    /// - [`DomainError::Conflict`] when the tenant is in `Deleted`
    ///   status (terminal during retention).
    // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-status-change-non-cascading:p1:inst-dod-status-change-service-suspend
    pub async fn suspend_tenant(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Tenant, DomainError> {
        self.set_status_internal(ctx, id, TenantStatus::Suspended, "suspended")
            .await
    }
    // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-status-change-non-cascading:p1:inst-dod-status-change-service-suspend

    /// Transition `id` from `Suspended` back to `Active`. Idempotent
    /// on already-active rows.
    ///
    /// # Errors
    ///
    /// See [`Self::suspend_tenant`].
    // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-status-change-non-cascading:p1:inst-dod-status-change-service-unsuspend
    pub async fn unsuspend_tenant(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Tenant, DomainError> {
        self.set_status_internal(ctx, id, TenantStatus::Active, "unsuspended")
            .await
    }
    // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-status-change-non-cascading:p1:inst-dod-status-change-service-unsuspend

    /// Shared body for [`Self::suspend_tenant`] /
    /// [`Self::unsuspend_tenant`]: PEP gate, `Provisioning`/`Deleted`
    /// rejection, dispatch to `TenantRepo::set_status`, audit log on
    /// non-no-op transitions.
    async fn set_status_internal(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        target: TenantStatus,
        event_label: &'static str,
    ) -> Result<Tenant, DomainError> {
        let scope = self
            .authorize(ctx, pep::actions::UPDATE, id, Some(id))
            .await?;
        let current =
            self.repo
                .find_by_id(&scope, id)
                .await?
                .ok_or_else(|| DomainError::NotFound {
                    detail: format!("tenant {id} not found"),
                    resource: id.to_string(),
                })?;
        // `Provisioning` is AM-internal — surface as `NotFound`.
        // `Deleted` is terminal during retention — surface as
        // `Conflict` so the caller can distinguish "tenant gone" from
        // "tenant frozen". The repo layer enforces both again under
        // SERIALIZABLE retry; the eager checks here keep error
        // messages crisp and avoid a TX round-trip on the obvious
        // rejections.
        if matches!(current.status, TenantStatus::Provisioning) {
            return Err(DomainError::NotFound {
                detail: format!("tenant {id} not found"),
                resource: id.to_string(),
            });
        }
        if matches!(current.status, TenantStatus::Deleted) {
            return Err(DomainError::Conflict {
                detail: format!("tenant {id} is deleted; status is terminal during retention"),
            });
        }
        let now = OffsetDateTime::now_utc();
        let updated = self.repo.set_status(&scope, id, target, now).await?;
        let was_no_op = updated.updated_at == current.updated_at;
        if !was_no_op {
            // TODO(events): emit AM event when platform event-bus lands.
            tracing::info!(
                target: "am.events",
                kind = "tenantStateChanged",
                actor = "tenant_scoped",
                subject_id = %ctx.subject_id(),
                subject_tenant_id = %ctx.subject_tenant_id(),
                tenant_id = %updated.id,
                event = event_label,
                status_from = current.status.as_str(),
                status_to = target.as_str(),
                "am tenant state changed"
            );
        }
        self.lower_to_tenant(updated).await
    }

    // -----------------------------------------------------------------
    // Phase 3 — soft delete + hard-delete batch + reaper + integrity
    // -----------------------------------------------------------------

    /// Implements FEATURE `Soft-Delete Tenant`.
    ///
    /// Idempotent: calling on a tenant that is already in `Deleted`
    /// status short-circuits to the existing tombstone (no preflight,
    /// no RG probe, no DB write — the retention timer is preserved).
    ///
    /// # Errors
    ///
    /// - [`DomainError::CrossTenantDenied`] when the PDP denies the
    ///   caller access to the target tenant.
    /// - [`DomainError::RootTenantCannotDelete`] when `tenant_id` is the root tenant.
    /// - [`DomainError::NotFound`] when the tenant does not exist or is `Provisioning`.
    /// - [`DomainError::TenantHasChildren`] when any child tenant still exists.
    /// - [`DomainError::TenantHasResources`] when the RG ownership probe finds any rows.
    // @cpt-begin:cpt-cf-account-management-flow-tenant-hierarchy-management-soft-delete-tenant:p1:inst-flow-sdel-service
    // @cpt-begin:cpt-cf-account-management-algo-tenant-hierarchy-management-soft-delete-preconditions:p1:inst-algo-sdelpc-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-soft-delete-preconditions:p1:inst-dod-soft-delete-preconditions
    // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-data-lifecycle:p1:inst-dod-data-lifecycle-soft-delete
    pub async fn delete_tenant(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<Tenant, DomainError> {
        // PEP gate (DESIGN §4.2) + DB-level subtree clamp
        // (cyberware-rust#1813). The compiled scope flows into the
        // disclosure read AND the mutating `schedule_deletion` write
        // so a caller scoped to a subtree that does NOT contain
        // `tenant_id` collapses to `NotFound` at the database.
        // Structural precondition checks (`count_children`,
        // `count_ownership_links`) stay at `allow_all` — those are
        // saga-internal guard counts asserting "this row has no
        // descendants / no resources", not data disclosure of the
        // tenant being acted on.
        let scope = self
            .authorize(ctx, pep::actions::DELETE, tenant_id, Some(tenant_id))
            .await?;
        let tenant = self
            .repo
            .find_by_id(&scope, tenant_id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                detail: format!("tenant {tenant_id} not found"),
                resource: tenant_id.to_string(),
            })?;
        if tenant.parent_id.is_none() {
            return Err(DomainError::RootTenantCannotDelete);
        }
        // `Provisioning` rows are AM-internal — they have no public
        // representation and are reaped through the provisioning
        // saga / reaper, not soft-delete. Surface as `NotFound` so the
        // SDK boundary never observes the internal status.
        if matches!(tenant.status, TenantStatus::Provisioning) {
            return Err(DomainError::NotFound {
                detail: format!("tenant {tenant_id} not found"),
                resource: tenant_id.to_string(),
            });
        }
        // Idempotent short-circuit: already in `Deleted`, the row is
        // a tombstone. Return it without re-running RG probes or
        // re-stamping `deleted_at` (which would push back the
        // retention deadline and let a malicious caller indefinitely
        // delay the hard-delete sweep). The authoritative idempotency
        // boundary lives inside `TenantRepo::schedule_deletion`'s
        // SERIALIZABLE TX (it also returns the existing tombstone on
        // re-entry); this service-level short-circuit is a pure
        // optimisation to skip the RG ownership probe on the
        // un-contended retry path. Audit log emission is intentionally
        // skipped here — idempotent retries are silent, mirroring the
        // no-op suppression on `update_tenant` / `set_status`. If a
        // future audit policy requires a "retry observed" signal,
        // wire it through the `am.events` channel with a distinct
        // event label (e.g. `event = "soft_delete_retry_noop"`) so
        // downstream filters can distinguish first-write events from
        // idempotent retries.
        if matches!(tenant.status, TenantStatus::Deleted) {
            return self.lower_to_tenant(tenant).await;
        }
        // 1. Child-rejection guard. `include_deleted = false` excludes
        // ONLY rows in `Deleted` status; `Provisioning`, `Active` and
        // `Suspended` children all count and block the soft-delete with
        // `TenantHasChildren`. This is intentional:
        // - `Provisioning` children are mid-saga and may still settle
        //   into `Active`; the parent's deletion must wait for them.
        // - `Deleted` children are already in the retention pipeline
        //   and the leaf-first ordering of the hard-delete batch
        //   guarantees they get reaped before their parent's row
        //   teardown runs. Counting them here would deadlock: parent
        //   never goes to `Deleted`, so children never get reaped.
        let child_count = self
            .repo
            .count_children(
                &AccessScope::allow_all(),
                tenant_id,
                ChildCountFilter::NonDeleted,
            )
            .await?;
        if child_count > 0 {
            return Err(DomainError::TenantHasChildren);
        }
        // 2. Resource-ownership rejection. Caller's `ctx` is propagated
        // so RG-side AuthZ + SecureORM resolve the parent's
        // `AccessScope`; the checker narrows further to `tenant_id` via
        // an OData filter so the answer reflects this child specifically
        // rather than the whole reachable subtree.
        let rg_links = self
            .resource_checker
            .count_ownership_links(ctx, tenant_id)
            .await?;
        if rg_links > 0 {
            return Err(DomainError::TenantHasResources);
        }
        // 3. Flip row + retention columns in one TX.
        let now = OffsetDateTime::now_utc();
        let retention: Option<Duration> = if self.cfg.retention.default_window_secs == 0 {
            None
        } else {
            Some(Duration::from_secs(self.cfg.retention.default_window_secs))
        };
        let updated = self
            .repo
            .schedule_deletion(&scope, tenant_id, now, retention)
            .await?;
        // TODO(events): emit AM event when platform event-bus lands.
        tracing::info!(
            target: "am.events",
            kind = "tenantStateChanged",
            actor = "tenant_scoped",
            subject_id = %ctx.subject_id(),
            subject_tenant_id = %ctx.subject_tenant_id(),
            tenant_id = %tenant_id,
            event = "soft_delete_requested",
            retention_secs = self.cfg.retention.default_window_secs,
            "am tenant state changed"
        );
        self.lower_to_tenant(updated).await
    }
    // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-data-lifecycle:p1:inst-dod-data-lifecycle-soft-delete
    // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-soft-delete-preconditions:p1:inst-dod-soft-delete-preconditions
    // @cpt-end:cpt-cf-account-management-algo-tenant-hierarchy-management-soft-delete-preconditions:p1:inst-algo-sdelpc-service
    // @cpt-end:cpt-cf-account-management-flow-tenant-hierarchy-management-soft-delete-tenant:p1:inst-flow-sdel-service

    /// Repair derivable hierarchy-integrity violations and emit
    /// per-category telemetry.
    ///
    /// Forwards to
    /// [`crate::domain::tenant::repo::TenantRepo::repair_derivable_closure_violations`]
    /// with `AccessScope::allow_all()` (closure rows are
    /// `no_tenant/no_resource`, see the `check_hierarchy_integrity`
    /// rationale a few methods below — same gate). Emits one
    /// [`crate::domain::metrics::AM_HIERARCHY_INTEGRITY_REPAIRED`]
    /// gauge per category in fixed order with `bucket = repaired |
    /// deferred` so dashboards always see a stable shape.
    /// `warn`-logs the deferred bucket if any non-derivable
    /// category carries a non-zero count — those are the
    /// operator-triage signals.
    ///
    /// # Errors
    ///
    /// * [`DomainError::FeatureDisabled`] — the staged-rollout
    ///   master switch `integrity_check.repair.enabled` is `false`
    ///   (default).
    /// * [`DomainError::IntegrityCheckInProgress`] — a concurrent
    ///   check or repair holds the single-flight gate.
    /// * Any other [`DomainError`] produced by the repository.
    ///
    /// # Visibility
    ///
    /// Crate-private (`pub(crate)`) on purpose: the only production
    /// caller is the periodic-job [`IntegrityChecker`] impl in
    /// `crate::domain::integrity_check::service`, which runs under
    /// the in-process scheduler with no caller `SecurityContext` and
    /// hardcodes [`AccessScope::allow_all`]. Exposing this method as
    /// `pub` would let the first admin REST handler or sibling
    /// module that reuses it bypass authorization by construction.
    /// REST exposure lands together with the `InTenantSubtree`
    /// predicate (cyberware-rust#1813) and will go through a
    /// privileged-context wrapper at that point.
    pub(crate) async fn repair_hierarchy_integrity(
        &self,
    ) -> Result<crate::domain::tenant::integrity::RepairReport, DomainError> {
        // `integrity_check.repair.enabled` is the staged-rollout
        // master switch: while it is `false` (default), the repair
        // path MUST NOT mutate `tenant_closure`, even from on-demand
        // admin entry points. The periodic loop already gates on
        // this flag (`auto_after_check && repair.enabled`); the same
        // gate has to live here so the SDK / admin REST surface
        // honours the rollout switch instead of bypassing it.
        if !self.cfg.integrity_check.repair.enabled {
            return Err(DomainError::FeatureDisabled {
                detail: "integrity_check.repair is disabled by configuration".to_owned(),
            });
        }

        let report = self
            .repo
            .repair_derivable_closure_violations(&AccessScope::allow_all())
            .await?;

        // Iterate `IntegrityCategory::all()` rather than the report
        // maps so a category that was non-zero on a previous tick
        // and absent on this one still emits a fresh zero. Mirrors
        // `check_hierarchy_integrity`'s emission shape so the
        // dashboard sees a stable per-category gauge across all
        // ticks even if a future refactor produces sparse maps.
        let repaired_lookup: std::collections::HashMap<IntegrityCategory, usize> =
            report.repaired_per_category.iter().copied().collect();
        let deferred_lookup: std::collections::HashMap<IntegrityCategory, usize> =
            report.deferred_per_category.iter().copied().collect();
        for cat in IntegrityCategory::all() {
            if cat.is_derivable() {
                let count = repaired_lookup.get(&cat).copied().unwrap_or(0);
                emit_gauge_value(
                    AM_HIERARCHY_INTEGRITY_REPAIRED,
                    i64::try_from(count).unwrap_or(i64::MAX),
                    &[("category", cat.as_str()), ("bucket", "repaired")],
                );
            } else {
                let count = deferred_lookup.get(&cat).copied().unwrap_or(0);
                emit_gauge_value(
                    AM_HIERARCHY_INTEGRITY_REPAIRED,
                    i64::try_from(count).unwrap_or(i64::MAX),
                    &[("category", cat.as_str()), ("bucket", "deferred")],
                );
            }
        }

        if report.total_deferred() > 0 {
            warn!(
                target: "am.integrity",
                deferred_total = report.total_deferred(),
                repaired_total = report.total_repaired(),
                "hierarchy integrity repair deferred non-derivable violations to operator triage"
            );
        }

        Ok(report)
    }

    /// Hierarchy-integrity check. Drives the Rust-side classifier
    /// pipeline through `TenantRepo::run_integrity_check`, buckets
    /// the flat violation pairs into the fixed-category report shape,
    /// and emits one `AM_HIERARCHY_INTEGRITY_VIOLATIONS` gauge sample
    /// per category (including zero-valued ones, so the dashboard can
    /// distinguish "no violations" from "checker never ran").
    ///
    /// The service forwards `AccessScope::allow_all()` to the repo
    /// because `tenants` and `tenant_closure` are declared
    /// `no_tenant/no_resource/no_owner/no_type`; per-caller subtree
    /// clamping lands with the `InTenantSubtree` predicate
    /// (cyberware-rust#1813).
    ///
    /// # Errors
    ///
    /// Propagates any [`DomainError`] produced by the repository:
    /// notably [`DomainError::IntegrityCheckInProgress`] when another worker
    /// holds the single-flight gate.
    ///
    /// # Visibility
    ///
    /// Crate-private (`pub(crate)`) for the same reason as
    /// [`Self::repair_hierarchy_integrity`]: the only production
    /// caller is the periodic-job [`IntegrityChecker`] impl, which
    /// runs under the in-process scheduler with no
    /// `SecurityContext` and hardcodes [`AccessScope::allow_all`].
    /// REST exposure ships together with the `InTenantSubtree`
    /// predicate (cyberware-rust#1813).
    // @cpt-begin:cpt-cf-account-management-algo-tenant-hierarchy-management-hierarchy-integrity-check:p2:inst-algo-integ-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-integrity-diagnostics:p2:inst-dod-integrity-diagnostics-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-data-remediation:p2:inst-dod-data-remediation-integrity
    pub(crate) async fn check_hierarchy_integrity(&self) -> Result<IntegrityReport, DomainError> {
        let pairs = self
            .repo
            .run_integrity_check(&AccessScope::allow_all())
            .await?;

        let mut bucketed: std::collections::HashMap<IntegrityCategory, Vec<Violation>> =
            std::collections::HashMap::new();
        for (cat, viol) in pairs {
            bucketed.entry(cat).or_default().push(viol);
        }

        let violations_by_category: Vec<(IntegrityCategory, Vec<Violation>)> =
            IntegrityCategory::all()
                .iter()
                .map(|cat| (*cat, bucketed.remove(cat).unwrap_or_default()))
                .collect();

        for (cat, viols) in &violations_by_category {
            let count = viols.len();
            emit_gauge_value(
                AM_HIERARCHY_INTEGRITY_VIOLATIONS,
                i64::try_from(count).unwrap_or(i64::MAX),
                &[("category", cat.as_str())],
            );
            if count > 0 {
                warn!(
                    target: "am.integrity",
                    category = cat.as_str(),
                    count,
                    "hierarchy integrity violations detected"
                );
            }
        }

        Ok(IntegrityReport {
            violations_by_category,
        })
    }
    // @cpt-end:cpt-cf-account-management-algo-tenant-hierarchy-management-hierarchy-integrity-check:p2:inst-algo-integ-service
    // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-integrity-diagnostics:p2:inst-dod-integrity-diagnostics-service
    // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-data-remediation:p2:inst-dod-data-remediation-integrity
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
