//! `UserService` -- domain orchestrator for tenant-scoped `IdP` user
//! operations.
//!
//! Composes a [`crate::domain::tenant::TenantRepo`] tenant-existence
//! guard with the resolved
//! [`account_management_sdk::IdpPluginClient`] plugin to
//! deliver the three flows defined by FEATURE
//! `idp-user-operations-contract`:
//!
//! * `create_user`  -- `POST /tenants/{tenant_id}/users` (REST drop-in)
//! * `delete_user` -- `DELETE /tenants/{tenant_id}/users/{user_id}`
//! * `list_users`      -- `GET /tenants/{tenant_id}/users`
//!
//! Every method:
//!
//! 1. Resolves `tenant_id` via `TenantRepo::find_by_id`.
//! 2. Rejects non-existent tenants with [`DomainError::NotFound`] and
//!    non-`Active` tenants with [`DomainError::Validation`] BEFORE any
//!    `IdP` call is issued, satisfying
//!    `cpt-cf-account-management-dod-idp-user-operations-contract-authenticated-tenant-scoped-invocation`.
//! 3. Builds a tenant-scope-bound contract request and forwards it to
//!    the configured [`IdpPluginClient`] per
//!    `cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation`.
//! 4. Maps the SDK [`IdpUserOperationFailure`] variants onto
//!    [`DomainError`] via the redacting boundary helper in
//!    [`crate::domain::idp`].
//!
//! `delete_user` additionally implements the
//! `cpt-cf-account-management-algo-idp-user-operations-contract-deprovision-idempotency-guard`
//! rule: `Ok(())` from the plugin is treated as idempotent success
//! regardless of whether the user was actually removed on this call
//! or was already absent — the plugin maps vendor "user does not
//! exist" responses (HTTP 404 / 410) to `Ok(())`, so the DELETE
//! endpoint stays retry-safe per
//! `cpt-cf-account-management-fr-idp-user-deprovision`.
//!
//! The service holds NO storage handles. Per
//! `cpt-cf-account-management-constraint-no-user-storage` AM persists
//! no user table, projection cache, or membership cache; every read
//! and write is a live pass-through to the `IdP`.
// @cpt-begin:cpt-cf-account-management-dod-idp-user-operations-contract-no-local-user-storage:p1:inst-dod-idp-user-operations-contract-no-local-user-storage-service

use std::sync::Arc;

use account_management_sdk::gts::{USER_GROUP_RG_TYPE_CODE, USER_RG_TYPE_CODE};
use account_management_sdk::{
    IdpDeprovisionUserRequest, IdpListUsersRequest, IdpNewUser, IdpPluginClient,
    IdpProvisionUserRequest, IdpTenantContext, IdpUpdateUserRequest, IdpUser, IdpUserFilterField,
    IdpUserOperationFailure, IdpUserPatch, ListUsersQuery,
};
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::ResourceType;
use resource_group_sdk::{ResourceGroupClient, ResourceGroupError};
use std::time::Duration;
use toolkit_macros::domain_model;
use toolkit_odata::Page;
use toolkit_odata::ast::{CompareOperator, Expr, Value};
use toolkit_odata::filter::{FilterNode, FilterOp, ODataValue};
use toolkit_odata::{CursorV1, ODataOrderBy, ODataQuery, OrderKey, SortDir};
use toolkit_security::AccessScope;
use toolkit_security::{SecurityContext, pep_properties};
use types_registry_sdk::TypesRegistryClient;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::idp::UserOperationFailureExt;
use crate::domain::metrics::{AM_DEPENDENCY_HEALTH, MetricKind, emit_metric};
use crate::domain::tenant::TenantContext;
use crate::domain::tenant::model::TenantStatus;
use crate::domain::tenant::repo::TenantRepo;

/// Upper bound on `username` length enforced at the AM boundary
/// before the `IdP` round-trip. Matches the `child_tenant_name`
/// bound declared by `m0004_create_conversion_requests` so AM-side
/// length policy stays uniform across the two identifier surfaces.
/// Counts Unicode scalars (`chars().count()`), not bytes — the GTS
/// schema's `maxLength` is also character-counted.
const MAX_USERNAME_CHARS: usize = 255;

/// Upper bound on the secondary profile fields (`email`,
/// `display_name`) enforced at the AM boundary as a cheap
/// **pre-flight cap** before the JSON-Schema validator runs.
///
/// A missing `USER_RESOURCE_TYPE` schema is NOT a fallback
/// case for this constant — that path is fail-closed in
/// [`crate::domain::gts_validation::validate_new_user_payload_via_gts`]
/// (surfaces `ServiceUnavailable` until the catalog is seeded).
/// This cap stays in place as belt-and-suspenders: it cuts
/// megabyte-scale payloads before they reach the JSON-Schema
/// validator, and keeps the AM boundary deterministic against a
/// future contract bug that lets the schema-backed validator
/// admit an oversize field by mistake.
///
/// The cap value matches `MAX_USERNAME_CHARS` so a schema
/// tightening that lowers any of the per-field `maxLength`s still
/// passes through the AM boundary unchanged (the GTS validator
/// remains authoritative).
const MAX_PROFILE_FIELD_CHARS: usize = 255;

/// PEP descriptors. The literal type-name duplicates
/// `USER_RESOURCE_TYPE` because `ResourceType.name` is `&'static str`;
/// a cross-check test pins them in sync.
pub(crate) mod pep {
    use super::{ResourceType, pep_properties};
    use account_management_sdk::USER_RESOURCE_TYPE;

    /// Resource declaration for `IdpUser`. AM persists no user table
    /// (per `cpt-cf-account-management-constraint-no-user-storage`),
    /// so the compiled `AccessScope` does NOT clamp any AM-owned
    /// table — its role here is purely the PEP-side allow/deny gate
    /// plus the `InTenantSubtree` predicate the tenant-existence
    /// guard (`resolve_active_tenant`) consults.
    ///
    /// Supported PEP properties:
    ///
    /// * `OWNER_TENANT_ID` — the tenant the `IdP` user belongs to;
    ///   ownership-style policies consume this.
    /// * `RESOURCE_ID` — set to the tenant id (the per-user
    ///   `IdpUser.id` is not policy-visible because users are
    ///   IdP-side primitives, not AM resources). Matches the
    ///   `tenants` entity's `resource_col = "id"` declaration so the
    ///   compiled subtree clamp on `tenants` resolves through this
    ///   property.
    pub const USER: ResourceType = ResourceType::from_static(
        USER_RESOURCE_TYPE,
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    /// Action vocabulary. `get_user` is a single-row `list_users`
    /// projection so it shares the `LIST` bucket.
    pub mod actions {
        pub const CREATE: &str = "create";
        pub const LIST: &str = "list";
        pub const DELETE: &str = "delete";
        pub const UPDATE: &str = "update";
    }
}

/// Central AM domain service for the `IdP` user-operations contract.
/// Pass-through service: no clock seam, no batch-size knobs — the
/// contract has no AM-side lifecycle state.
#[domain_model]
pub struct UserService {
    tenant_repo: Arc<dyn TenantRepo>,
    idp_user: Arc<dyn IdpPluginClient>,
    /// GTS schema source for the user JSON-Schema contract — keeps
    /// structural rules (length, format) authoritative in the
    /// catalogue rather than duplicated in code.
    types_registry: Arc<dyn TypesRegistryClient>,
    /// PEP gate run before any `IdP` / RG round trip.
    enforcer: PolicyEnforcer,
    /// RG client for `delete_user` membership cleanup. `None` in
    /// tests that don't exercise the cleanup path; production wiring
    /// always sets it.
    rg_client: Option<Arc<dyn ResourceGroupClient + Send + Sync>>,
    /// Per-deployment listing cap. Defaults to
    /// [`account_management_sdk::IdpUserPagination::MAX_TOP`];
    /// production wiring overrides via [`Self::with_listing_max_top`]
    /// from `cfg.listing.max_top`.
    max_listing_top: u32,
}

/// Per-call RG timeout. Matches `CASCADE_TIMEOUT` so cleanup and
/// cascade share one operator-tunable bound.
#[allow(
    clippy::duration_suboptimal_units,
    reason = "from_mins is unstable on workspace MSRV; keep from_secs"
)]
const RG_CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum user-group rows fetched per `list_groups` page during cleanup.
///
/// Matches `CASCADE_PAGE_SIZE` in `domain::user_groups::cascade`.
const RG_CLEANUP_PAGE_SIZE: u64 = 100;

/// Overall cleanup budget. Tighter than `CASCADE_BUDGET` because
/// cleanup runs on the synchronous `delete_user` request path
/// (user-visible response), not on a background retention tick.
#[allow(
    clippy::duration_suboptimal_units,
    reason = "from_mins is unstable on workspace MSRV; keep from_secs"
)]
const RG_CLEANUP_BUDGET: Duration = Duration::from_secs(60);

/// Returns `Some(uuid)` when `filter` is exactly a top-level binary
/// `IdpUserFilterField::Id eq ODataValue::Uuid(uuid)` clause —
/// the canonical "authoritative single-user existence check" shape
/// produced by [`ListUsersQuery::with_id`]. Used by `list_users` to
/// preserve the historical `user_id_filter` defensive guard against
/// plugins that silently ignore the filter and return unrelated rows.
fn extract_top_level_id_eq(filter: Option<&FilterNode<IdpUserFilterField>>) -> Option<Uuid> {
    match filter? {
        FilterNode::Binary {
            field: IdpUserFilterField::Id,
            op: FilterOp::Eq,
            value: ODataValue::Uuid(u),
        } => Some(*u),
        _ => None,
    }
}

impl UserService {
    /// Construct a fully-wired service.
    ///
    /// The optional [`ResourceGroupClient`] cleanup wiring is set via
    /// [`Self::with_rg_membership_cleanup`] after construction so
    /// existing test fixtures that don't model membership scenarios
    /// can build the service without a fake RG client.
    #[must_use]
    pub fn new(
        tenant_repo: Arc<dyn TenantRepo>,
        idp_user: Arc<dyn IdpPluginClient>,
        types_registry: Arc<dyn TypesRegistryClient>,
        enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            tenant_repo,
            idp_user,
            types_registry,
            enforcer,
            rg_client: None,
            max_listing_top: account_management_sdk::IdpUserPagination::MAX_TOP,
        }
    }

    /// Operator-tunable per-deployment listing cap. The gear
    /// bootstrap passes `cfg.listing.max_top` so the user listing
    /// surface stays uniform with the tenant / conversion / metadata
    /// listing caps.
    #[must_use]
    pub const fn with_listing_max_top(mut self, max_top: u32) -> Self {
        self.max_listing_top = max_top;
        self
    }

    /// Per-deployment `top` cap. The `list_users` REST handler clamps
    /// the caller-supplied `limit` against this ceiling so a
    /// deployment that tightened `cfg.listing.max_top` below the SDK
    /// constructor's absolute ceiling (200) sees the tighter cap take
    /// effect.
    #[must_use]
    pub const fn max_listing_top(&self) -> u32 {
        self.max_listing_top
    }

    /// PEP gate. Calls the platform-side `PolicyEnforcer`, returns
    /// the [`AccessScope`] the tenant-existence guard
    /// (`resolve_active_tenant`) forwards through `toolkit_db`'s
    /// secure builders.
    ///
    /// Mirrors `TenantService::authorize` / `MetadataService::authorize`:
    ///
    /// * `OWNER_TENANT_ID = tenant_id` — the `IdP` user's owning tenant.
    /// * `RESOURCE_ID = tenant_id` — matches `tenants.id` so the PDP-
    ///   emitted `InTenantSubtree` predicate clamps the `tenants`
    ///   read in `resolve_active_tenant` to the caller's subtree.
    /// * `require_constraints(true)` — a PDP returning `decision: true,
    ///   constraints: []` fails closed via `CompileFailed →
    ///   CrossTenantDenied` rather than silently widening the read.
    ///
    /// AM persists no user table; the PEP role here is purely the
    /// allow/deny + tenant-existence-gate subtree clamp.
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        tenant_id: Uuid,
    ) -> Result<AccessScope, DomainError> {
        // Delegates to [`crate::domain::authz::authz_scope`] for the
        // uniform PEP-gate shape. `User` keys both `OWNER_TENANT_ID`
        // and `RESOURCE_ID` on the IdP user's owning tenant.
        crate::domain::authz::authz_scope(
            &self.enforcer,
            ctx,
            &pep::USER,
            action,
            tenant_id,
            Some(tenant_id),
            |req| req,
        )
        .await
    }

    /// Wire the [`ResourceGroupClient`] used by
    /// [`Self::delete_user`] to clean up dangling user-group
    /// memberships referencing the deprovisioned user. Without this,
    /// hard-deleted AM users leave orphaned rows in RG's
    /// `resource_group_membership` table (their `resource_id`
    /// references a non-existent AM user); the next listing of the
    /// containing group would surface a member that is no longer
    /// a valid user.
    ///
    /// Production wiring in `gear.rs` always invokes this builder;
    /// tests that don't exercise the cleanup path skip it and leave
    /// `rg_client = None` so the cleanup branch short-circuits.
    #[must_use]
    pub fn with_rg_membership_cleanup(
        mut self,
        rg_client: Arc<dyn ResourceGroupClient + Send + Sync>,
    ) -> Self {
        self.rg_client = Some(rg_client);
        self
    }

    // ----------------------------------------------------------------
    // Provision user
    // ----------------------------------------------------------------

    /// Provision a user in `tenant_id` via the configured `IdP` plugin.
    ///
    /// Implements
    /// `cpt-cf-account-management-flow-idp-user-operations-contract-provision-user`.
    ///
    /// Guard ordering MUST match
    /// `cpt-cf-account-management-dod-idp-user-operations-contract-authenticated-tenant-scoped-invocation`:
    ///
    /// 1. Load tenant via `tenant_repo.find_by_id`.
    /// 2. Reject `None` with [`DomainError::NotFound`] -- no `IdP` call.
    /// 3. Reject any non-`Active` status with
    ///    [`DomainError::Validation`] -- no `IdP` call.
    /// 4. Forward to [`IdpPluginClient::provision_user`].
    ///
    /// The actor recorded on the outcome `am.events` line is
    /// `ctx.subject_id()`. AM does not re-validate it — platform
    /// `AuthN` is a precondition per
    /// `cpt-cf-account-management-nfr-authentication-context`.
    ///
    /// # Errors
    ///
    /// * [`DomainError::NotFound`] -- `tenant_id` does not resolve.
    /// * [`DomainError::Validation`] -- tenant exists but is not
    ///   [`TenantStatus::Active`] (provisioning, suspended, deleted).
    /// * [`DomainError::Validation`] -- payload rejected before the
    ///   `IdP` call: empty / whitespace-only username, username or
    ///   email / `display_name` exceeding the length cap, or GTS
    ///   schema rejection on a structural property.
    /// * [`DomainError::ServiceUnavailable`] -- GTS Types Registry
    ///   transport failure or `gts.cf.core.am.user.v1~` not yet
    ///   registered (fails closed rather than forwarding an
    ///   unvalidated payload to the `IdP`); also DB transport
    ///   failure inside `resolve_active_tenant`.
    /// * [`DomainError::IdpUnavailable`] -- transport failure or
    ///   timeout on the `IdP` call.
    /// * [`DomainError::UnsupportedOperation`] -- provider declined
    ///   the operation.
    /// * [`DomainError::Validation`] -- provider rejected the payload
    ///   (duplicate username, vendor-side checks, etc.).
    /// * [`DomainError::Internal`] -- provider returned `Uuid::nil()`
    ///   as the user id (plugin contract violation) or an unknown
    ///   SDK failure variant.
    // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-provision-user:p1:inst-flow-puser-service
    #[allow(
        clippy::cognitive_complexity,
        reason = "flat guard sequence (tenant scope -> actor precondition -> payload trim/cap -> GTS structural -> IdP call -> response nil-id guard) is the security-critical ordering reviewers eyeball-check; extracting helpers would fragment the audit chain and obscure the @cpt-* CPT markers anchored to each step"
    )]
    pub async fn create_user(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        mut payload: IdpNewUser,
    ) -> Result<IdpUser, DomainError> {
        // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-provision-user:p1:inst-flow-puser-resolve-tenant
        // @cpt-begin:cpt-cf-account-management-dod-idp-user-operations-contract-authenticated-tenant-scoped-invocation:p1:inst-dod-authenticated-tenant-scoped-invocation-puser
        // PEP gate FIRST: compiles the caller's `SecurityContext`
        // into an `AccessScope` (`InTenantSubtree` predicate rooted
        // at the caller's subtree). A denied caller surfaces as
        // `CrossTenantDenied` BEFORE any IdP / Types Registry round
        // trip. Mirrors the production posture in `TenantService`
        // and `MetadataService`.
        let scope = self.authorize(ctx, pep::actions::CREATE, tenant_id).await?;
        let actor = ctx.subject_id();

        // Tenant existence + status guard runs AFTER the PEP gate so
        // a request against a non-existent / soft-deleted / out-of-
        // scope tenant surfaces as `NotFound` / `Validation` without
        // leaking tenant topology through a payload-shape error.
        let tenant_context = self.resolve_active_tenant(&scope, tenant_id).await?;
        // @cpt-end:cpt-cf-account-management-dod-idp-user-operations-contract-authenticated-tenant-scoped-invocation:p1:inst-dod-authenticated-tenant-scoped-invocation-puser
        // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-provision-user:p1:inst-flow-puser-resolve-tenant

        // Trim username BEFORE the GTS schema check: whitespace-equivalence is AM
        // policy, not schema-level (the schema enforces structural shape only).
        // Cap at MAX_USERNAME_CHARS BEFORE the IdP round-trip so megabyte payloads
        // don't ride the wire just to be rejected upstream.
        let trimmed = payload.username.trim();
        if trimmed.is_empty() {
            return Err(DomainError::Validation {
                detail: "create_user: username MUST not be all-whitespace".to_owned(),
            });
        }
        if trimmed.chars().count() > MAX_USERNAME_CHARS {
            return Err(DomainError::Validation {
                detail: format!(
                    "create_user: username MUST be {MAX_USERNAME_CHARS} characters or fewer"
                ),
            });
        }
        if trimmed.len() != payload.username.len() {
            payload.username = trimmed.to_owned();
        }
        // Pre-flight char-count caps on email/display_name. Cuts megabyte-scale
        // payloads before the GTS validator runs, and stays fail-closed when the
        // schema is unregistered (ServiceUnavailable path).
        check_profile_field_bound("email", payload.email.as_deref())?;
        check_profile_field_bound("display_name", payload.display_name.as_deref())?;
        crate::domain::gts_validation::validate_new_user_payload_via_gts(
            &payload,
            &*self.types_registry,
        )
        .await?;

        // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-provision-user:p1:inst-flow-puser-invoke-contract
        // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-package-request-puser
        // Convert the AM-internal `TenantContext` to the SDK-facing
        // `IdpTenantContext` at the plugin-SPI boundary so internal
        // additions stay out of the public plugin contract.
        let req = IdpProvisionUserRequest::new(IdpTenantContext::from(&tenant_context), payload);
        // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-package-request-puser

        // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-invoke-puser
        let outcome = self.idp_user.provision_user(ctx, &req).await;
        // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-invoke-puser
        // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-provision-user:p1:inst-flow-puser-invoke-contract

        match outcome {
            Ok(projection) => {
                // Plugin-contract guard: a `Uuid::nil()` user id is a
                // contract violation. The id flows into `am.events`
                // as the authoritative IdP-issued identifier and into
                // any downstream membership write keyed on it; a nil
                // value would coalesce distinct users into one audit
                // / membership bucket the same way a nil
                // `requested_by` would. Mirrors the `id eq <uuid>`
                // contract-drift gate further down in `list_users`.
                if projection.id.is_nil() {
                    tracing::warn!(
                        target: "am.user.audit",
                        tenant_id = %tenant_id,
                        "create_user: provider returned Uuid::nil() as user id (plugin contract violation)"
                    );
                    return Err(DomainError::Internal {
                        diagnostic: format!(
                            "create_user: provider returned Uuid::nil() as user id for tenant {tenant_id} (plugin contract violation)"
                        ),
                        cause: None,
                    });
                }
                // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-provision-user:p1:inst-flow-puser-success-return
                // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-success-return-puser
                // @cpt-begin:cpt-cf-account-management-dod-idp-user-operations-contract-user-projection-schema:p1:inst-dod-user-projection-schema-puser
                // Response-side schema validation is intentionally NOT performed:
                // AM trusts the plugin's published projection contract. A
                // response-side fence would either break the listing on a single
                // drifted field or silently drop bad rows — both worse than the
                // contract-trust posture.
                // Only the Ok arm emits am.events; failure correlation lives on
                // am.idp warn lines so downstream consumers grouping by event
                // count successes, not attempts.
                tracing::info!(
                    target: "am.events",
                    event = "user_provisioned",
                    tenant_id = %tenant_id,
                    user_id = %projection.id,
                    actor_uuid = %actor,
                    outcome = "ok",
                    "am user provisioned"
                );
                Ok(projection)
                // @cpt-end:cpt-cf-account-management-dod-idp-user-operations-contract-user-projection-schema:p1:inst-dod-user-projection-schema-puser
                // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-success-return-puser
                // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-provision-user:p1:inst-flow-puser-success-return
            }
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-provision-user:p1:inst-flow-puser-provider-error-branch
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-provision-user:p1:inst-flow-puser-unavailable-branch
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-provision-user:p1:inst-flow-puser-unavailable-return
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-provision-user:p1:inst-flow-puser-provider-error-return
            Err(failure) => Err(failure.into_domain_error(tenant_id)),
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-provision-user:p1:inst-flow-puser-provider-error-return
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-provision-user:p1:inst-flow-puser-unavailable-return
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-provision-user:p1:inst-flow-puser-unavailable-branch
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-provision-user:p1:inst-flow-puser-provider-error-branch
        }
    }
    // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-provision-user:p1:inst-flow-puser-service

    // ----------------------------------------------------------------
    // Deprovision user
    // ----------------------------------------------------------------

    /// Deprovision `user_id` in `tenant_id` via the configured `IdP`
    /// plugin. Idempotent: an already-absent user returns `Ok(())`.
    ///
    /// Implements
    /// `cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user`
    /// and the
    /// `cpt-cf-account-management-algo-idp-user-operations-contract-deprovision-idempotency-guard`
    /// rule.
    ///
    /// The idempotency guard is fully delegated to the plugin: plugins
    /// MUST map vendor "user does not exist" responses to `Ok(())` so
    /// AM observes a uniform success regardless of whether the user
    /// was actually removed on this call or was already gone.
    /// `Unavailable` and `UnsupportedOperation` pass through unchanged
    /// per
    /// `cpt-cf-account-management-dod-idp-user-operations-contract-deprovision-idempotency`.
    ///
    /// # Errors
    ///
    /// * [`DomainError::NotFound`] -- `tenant_id` does not resolve.
    /// * [`DomainError::Validation`] -- tenant exists but is not
    ///   [`TenantStatus::Active`].
    /// * [`DomainError::ServiceUnavailable`] -- GTS Types Registry or
    ///   DB transport failure inside `resolve_active_tenant`, or
    ///   resource-group transport / overall-budget timeout during the
    ///   post-deprovision membership cleanup (the call returns `Err`
    ///   so a retry re-enters; `IdP` returns `Ok(())` idempotently,
    ///   cleanup retries).
    /// * [`DomainError::IdpUnavailable`] -- transport failure or
    ///   timeout on the `IdP` call.
    /// * [`DomainError::UnsupportedOperation`] -- provider declined
    ///   the operation.
    /// * [`DomainError::Validation`] -- provider rejected the request.
    /// * [`DomainError::Internal`] -- unknown SDK failure variant
    ///   (catch-all in the internal failure-mapping helper).
    // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-service
    pub async fn delete_user(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-resolve-tenant
        // @cpt-begin:cpt-cf-account-management-dod-idp-user-operations-contract-authenticated-tenant-scoped-invocation:p1:inst-dod-authenticated-tenant-scoped-invocation-duser
        let scope = self.authorize(ctx, pep::actions::DELETE, tenant_id).await?;
        let actor = ctx.subject_id();
        let tenant_context = self.resolve_active_tenant(&scope, tenant_id).await?;
        // @cpt-end:cpt-cf-account-management-dod-idp-user-operations-contract-authenticated-tenant-scoped-invocation:p1:inst-dod-authenticated-tenant-scoped-invocation-duser
        // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-resolve-tenant

        // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-invoke-contract
        // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-package-request-duser
        // Convert internal `TenantContext` → SDK `IdpTenantContext` at
        // the plugin-SPI boundary.
        let req = IdpDeprovisionUserRequest::new(IdpTenantContext::from(&tenant_context), user_id);
        // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-package-request-duser
        // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-invoke-duser
        let outcome = self.idp_user.deprovision_user(ctx, &req).await;
        // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-invoke-duser
        // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-invoke-contract

        match outcome {
            // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-deprovision-idempotency-guard:p1:inst-algo-dig-absent-branch
            // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-deprovision-idempotency-guard:p1:inst-algo-dig-absent-return
            // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-deprovision-idempotency-guard:p1:inst-algo-dig-other-branch-removed
            // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-deprovision-idempotency-guard:p1:inst-algo-dig-other-return-removed
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-absent-branch
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-idempotency-check
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-idempotent-return
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-success-return
            // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-success-return-duser
            // @cpt-begin:cpt-cf-account-management-dod-idp-user-operations-contract-deprovision-idempotency:p1:inst-dod-deprovision-idempotency-service
            // Plugin maps vendor "user does not exist" responses to
            // `Ok(())` itself; AM observes a single success arm
            // regardless of removed-vs-absent provenance. RG-membership
            // cleanup runs BEFORE emitting the success log so the
            // success event lands only after the user is truly gone
            // end-to-end. If cleanup fails, the call returns Err and a
            // retry re-enters the path; IdP returns `Ok(())` idempotently,
            // cleanup retries.
            Ok(()) => {
                self.cleanup_user_group_memberships(ctx, tenant_id, user_id)
                    .await?;
                tracing::info!(
                    target: "am.events",
                    event = "user_deprovisioned",
                    tenant_id = %tenant_id,
                    user_id = %user_id,
                    actor_uuid = %actor,
                    outcome = "ok",
                    "am user deprovisioned"
                );
                Ok(())
            }
            // @cpt-end:cpt-cf-account-management-dod-idp-user-operations-contract-deprovision-idempotency:p1:inst-dod-deprovision-idempotency-service
            // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-success-return-duser
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-success-return
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-idempotent-return
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-idempotency-check
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-absent-branch
            // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-deprovision-idempotency-guard:p1:inst-algo-dig-other-return-removed
            // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-deprovision-idempotency-guard:p1:inst-algo-dig-other-branch-removed
            // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-deprovision-idempotency-guard:p1:inst-algo-dig-absent-return
            // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-deprovision-idempotency-guard:p1:inst-algo-dig-absent-branch
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-unavailable-branch
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-provider-error-branch
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-unavailable-return
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-provider-error-return
            // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-deprovision-idempotency-guard:p1:inst-algo-dig-other-branch-failure
            // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-deprovision-idempotency-guard:p1:inst-algo-dig-other-return-failure
            // Pass-through "non-absent failure" arm of the
            // idempotency guard: error correlation lives on `am.idp`
            // warn lines emitted by [`UserOperationFailureExt::into_domain_error`].
            Err(failure) => Err(failure.into_domain_error(tenant_id)),
            // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-deprovision-idempotency-guard:p1:inst-algo-dig-other-return-failure
            // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-deprovision-idempotency-guard:p1:inst-algo-dig-other-branch-failure
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-provider-error-return
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-unavailable-return
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-provider-error-branch
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-unavailable-branch
        }
    }
    // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user:p1:inst-flow-duser-service

    // ----------------------------------------------------------------
    // Update user
    // ----------------------------------------------------------------

    /// Apply a JSON Merge Patch to `user_id`'s mutable attributes in
    /// `tenant_id` via the configured `IdP` plugin. The `IdP` remains
    /// the source of truth; AM orchestrates the saga (empty-patch guard
    /// -> PEP gate -> tenant scope -> username normalisation -> GTS
    /// structural validation of changed fields -> `IdP` call -> nil-id
    /// guard) and persists nothing per
    /// `cpt-cf-account-management-constraint-no-user-storage`.
    ///
    /// The patch semantics are documented on [`IdpUserPatch`]: omitted
    /// fields are unchanged, `Some(None)` clears a nullable profile
    /// field, and a `password` value sets a new credential. Unlike
    /// `delete_user`, an absent user is NOT folded into success — the
    /// provider surfaces [`account_management_sdk::IdpUserOperationFailure::NotFound`]
    /// which AM maps to [`DomainError::UserNotFound`] (HTTP 404).
    ///
    /// # Errors
    ///
    /// * [`DomainError::Validation`] -- empty patch; a username rename
    ///   that is all-whitespace or over the length cap; a profile field
    ///   over its cap or failing the GTS schema; a provider payload
    ///   rejection (incl. rejected password).
    /// * [`DomainError::UserAlreadyExists`] -- a username rename
    ///   collided with an existing login (HTTP 409).
    /// * [`DomainError::UserNotFound`] -- the `IdP` reports the target
    ///   user absent in this tenant scope (HTTP 404).
    /// * [`DomainError::NotFound`] -- `tenant_id` does not resolve.
    /// * [`DomainError::ServiceUnavailable`] -- GTS Types Registry / DB
    ///   transport failure inside `resolve_active_tenant` or the patch
    ///   validator.
    /// * [`DomainError::IdpUnavailable`] -- transport failure or timeout
    ///   on the `IdP` call.
    /// * [`DomainError::UnsupportedOperation`] -- provider declined the
    ///   operation.
    /// * [`DomainError::Internal`] -- provider returned `Uuid::nil()`
    ///   (plugin contract violation) or an unknown SDK failure variant.
    // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-update-user:p1:inst-flow-uuser-service
    #[allow(
        clippy::cognitive_complexity,
        reason = "flat guard sequence (empty-patch -> PEP gate -> tenant scope -> username normalisation -> profile caps -> GTS structural -> IdP call -> response nil-id guard) is the security-critical ordering reviewers eyeball-check; mirrors create_user and keeps the @cpt-* CPT markers anchored per step"
    )]
    pub async fn update_user(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        user_id: Uuid,
        mut patch: IdpUserPatch,
    ) -> Result<IdpUser, DomainError> {
        // Reject an all-`None` patch before any PEP / IdP round trip:
        // a no-field PATCH carries no intent and must not issue an IdP
        // call. Mirrors `TenantService::update_tenant`'s empty guard.
        if patch.is_empty() {
            return Err(DomainError::Validation {
                detail: "update_user: patch is empty; at least one field required".to_owned(),
            });
        }

        // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-update-user:p1:inst-flow-uuser-resolve-tenant
        // @cpt-begin:cpt-cf-account-management-dod-idp-user-operations-contract-authenticated-tenant-scoped-invocation:p1:inst-dod-authenticated-tenant-scoped-invocation-uuser
        // PEP gate FIRST, then tenant existence + status guard — same
        // security ordering as create_user / delete_user.
        let scope = self.authorize(ctx, pep::actions::UPDATE, tenant_id).await?;
        let actor = ctx.subject_id();
        let tenant_context = self.resolve_active_tenant(&scope, tenant_id).await?;
        // @cpt-end:cpt-cf-account-management-dod-idp-user-operations-contract-authenticated-tenant-scoped-invocation:p1:inst-dod-authenticated-tenant-scoped-invocation-uuser
        // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-update-user:p1:inst-flow-uuser-resolve-tenant

        // Normalise + bound a username rename exactly as create_user
        // treats the create-time username: trim (whitespace-equivalence
        // is AM policy, not schema-level), reject all-whitespace, cap
        // before the IdP round-trip.
        if let Some(username) = patch.username.as_ref() {
            let trimmed = username.trim();
            if trimmed.is_empty() {
                return Err(DomainError::Validation {
                    detail: "update_user: username MUST not be all-whitespace".to_owned(),
                });
            }
            if trimmed.chars().count() > MAX_USERNAME_CHARS {
                return Err(DomainError::Validation {
                    detail: format!(
                        "update_user: username MUST be {MAX_USERNAME_CHARS} characters or fewer"
                    ),
                });
            }
            if trimmed.len() != username.len() {
                patch.username = Some(trimmed.to_owned());
            }
        }

        // Pre-flight char-count caps on the profile fields being SET to
        // a value (`Some(Some(_))`); clearing (`Some(None)`) carries no
        // value to cap. Cuts megabyte-scale payloads before the GTS
        // validator's registry round-trip, mirroring create_user.
        check_profile_field_bound("email", patch.email.as_ref().and_then(|e| e.as_deref()))?;
        check_profile_field_bound(
            "display_name",
            patch.display_name.as_ref().and_then(|d| d.as_deref()),
        )?;
        crate::domain::gts_validation::validate_user_patch_via_gts(&patch, &*self.types_registry)
            .await?;

        // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-update-user:p1:inst-flow-uuser-invoke-contract
        // Convert internal `TenantContext` → SDK `IdpTenantContext` at
        // the plugin-SPI boundary.
        let req =
            IdpUpdateUserRequest::new(IdpTenantContext::from(&tenant_context), user_id, patch);
        let outcome = self.idp_user.update_user(ctx, &req).await;
        // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-update-user:p1:inst-flow-uuser-invoke-contract

        match outcome {
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-update-user:p1:inst-flow-uuser-success-return
            Ok(projection) => {
                // Plugin-contract guard: a `Uuid::nil()` user id is a
                // contract violation (mirrors create_user). The id
                // flows into `am.events` as the authoritative IdP-issued
                // identifier; a nil value would coalesce distinct users
                // into one audit bucket.
                if projection.id.is_nil() {
                    tracing::warn!(
                        target: "am.user.audit",
                        tenant_id = %tenant_id,
                        "update_user: provider returned Uuid::nil() as user id (plugin contract violation)"
                    );
                    return Err(DomainError::Internal {
                        diagnostic: format!(
                            "update_user: provider returned Uuid::nil() as user id for tenant {tenant_id} (plugin contract violation)"
                        ),
                        cause: None,
                    });
                }
                tracing::info!(
                    target: "am.events",
                    event = "user_updated",
                    tenant_id = %tenant_id,
                    user_id = %projection.id,
                    actor_uuid = %actor,
                    outcome = "ok",
                    "am user updated"
                );
                Ok(projection)
            }
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-update-user:p1:inst-flow-uuser-success-return
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-update-user:p1:inst-flow-uuser-provider-error-return
            // Absent target user is a 404 for update (NOT folded into
            // success the way `deprovision_user` treats an absent user).
            // Pre-empt the shared failure mapping here — where `user_id`
            // is in scope — so the canonical envelope carries the precise
            // resource identifier.
            Err(IdpUserOperationFailure::NotFound { .. }) => Err(DomainError::UserNotFound {
                detail: format!("user {user_id} not found in tenant {tenant_id}"),
                resource: user_id.to_string(),
            }),
            // All other failure categories share the redact + warn
            // mapping in [`UserOperationFailureExt::into_domain_error`].
            Err(failure) => Err(failure.into_domain_error(tenant_id)),
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-update-user:p1:inst-flow-uuser-provider-error-return
        }
    }
    // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-update-user:p1:inst-flow-uuser-service

    // ----------------------------------------------------------------
    // List users
    // ----------------------------------------------------------------

    /// Fetch a single user by id from `tenant_id` via the configured
    /// `IdP` plugin. Thin wrapper over [`Self::list_users`] with
    /// `$filter = id eq <user_id>`, `top = 1`, `cursor = None`
    /// (constructed via [`ListUsersQuery::with_id`]); the AM-level
    /// pagination disciplines documented on `list_users` (cursor MUST
    /// be absent, top MUST be 1 when filtering by `id eq`) make a
    /// one-shot filtered lookup the authoritative existence check for
    /// a specific user.
    ///
    /// User profile mutation is exposed through [`Self::update_user`]
    /// as a pure pass-through to the `IdP` (AM persists nothing per
    /// `cpt-cf-account-management-adr-idp-user-identity-source-of-truth`).
    /// This method is the read-side projection; the full user surface is
    /// the lifecycle saga (`create_user` / `update_user` / `delete_user`)
    /// plus the read side (`get_user` / `list_users`).
    ///
    /// # Errors
    ///
    /// * [`DomainError::NotFound`] -- `tenant_id` does not resolve OR
    ///   the user is not present in the plugin's response (empty page).
    /// * All other failure shapes documented on [`Self::list_users`]
    ///   (status precondition, `IdP` transport failure, unsupported
    ///   operation, provider validation).
    pub async fn get_user(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<IdpUser, DomainError> {
        // `ListUsersQuery::with_id` bakes in the canonical
        // existence-check shape (`$filter = id eq <user_id>`,
        // `top = 1`, `cursor = None`) via
        // `IdpUserPagination::for_existence_check()` which is `const`
        // and infallible — no error mapping needed.
        let query = ListUsersQuery::with_id(user_id);
        let page = self.list_users(ctx, tenant_id, query).await?;
        page.items
            .into_iter()
            .next()
            .ok_or_else(|| DomainError::UserNotFound {
                detail: format!("user {user_id} not found in tenant {tenant_id}"),
                resource: user_id.to_string(),
            })
    }

    /// List users in `tenant_id` via the configured `IdP` plugin.
    /// A top-level `$filter = id eq <uuid>` clause (the shape produced
    /// by [`ListUsersQuery::with_id`]) is the authoritative existence
    /// signal consumed by sibling features (e.g. `feature-user-groups`).
    ///
    /// Implements
    /// `cpt-cf-account-management-flow-idp-user-operations-contract-list-users`.
    ///
    /// # Errors
    ///
    /// * [`DomainError::Validation`] -- pagination shape: `cursor`
    ///   present or `top != 1` combined with the `id eq <uuid>` filter
    ///   shape (the filtered point-lookup is an authoritative existence
    ///   check, not a paginated query).
    /// * [`DomainError::NotFound`] -- `tenant_id` does not resolve.
    /// * [`DomainError::Validation`] -- tenant exists but is not
    ///   [`TenantStatus::Active`].
    /// * [`DomainError::ServiceUnavailable`] -- GTS Types Registry or
    ///   DB transport failure inside `resolve_active_tenant`.
    /// * [`DomainError::IdpUnavailable`] -- transport failure or
    ///   timeout. NO stale projection is served per
    ///   `cpt-cf-account-management-principle-idp-agnostic`.
    /// * [`DomainError::UnsupportedOperation`] -- provider declined
    ///   the operation.
    /// * [`DomainError::Validation`] -- provider rejected the request.
    /// * [`DomainError::Internal`] -- provider returned a row that
    ///   does not match the `id eq <uuid>` filter (contract-drift
    ///   guard) or an unknown SDK failure variant.
    // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-service
    pub async fn list_users(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, DomainError> {
        let ListUsersQuery {
            pagination,
            filter,
            order,
            ..
        } = query;
        let existence_check_id: Option<Uuid> = extract_top_level_id_eq(filter.as_ref());
        // PEP gate FIRST. The `LIST` action is used for both the
        // raw list and the `get_user` existence-check shape
        // (top-level `$filter = id eq <uuid>` AST clause) so the same
        // policy decision governs both surfaces. The compiled scope
        // clamps the tenant-existence guard below.
        let scope = self.authorize(ctx, pep::actions::LIST, tenant_id).await?;

        // Tenant existence + status guard runs after PEP so a request
        // against a non-existent / soft-deleted / out-of-scope tenant
        // surfaces as `NotFound` / `Validation` without leaking
        // tenant topology through a pagination-shape error.
        // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-resolve-tenant
        // @cpt-begin:cpt-cf-account-management-dod-idp-user-operations-contract-authenticated-tenant-scoped-invocation:p1:inst-dod-authenticated-tenant-scoped-invocation-luser
        let tenant_context = self.resolve_active_tenant(&scope, tenant_id).await?;
        // @cpt-end:cpt-cf-account-management-dod-idp-user-operations-contract-authenticated-tenant-scoped-invocation:p1:inst-dod-authenticated-tenant-scoped-invocation-luser
        // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-resolve-tenant

        // When the filter is exactly a top-level `id eq <uuid>` clause
        // the call is an existence check (empty page = authoritative
        // absent). Reject cursor/top>1: a continuation cursor turns
        // existence into a false negative; top>1 lets a vendor that
        // ignores the filter return unrelated rows and surface a caller
        // bug as Internal 500 instead of Validation 400.
        if existence_check_id.is_some() {
            if pagination.cursor().is_some() {
                return Err(DomainError::Validation {
                    detail: "list_users: cursor MUST be absent when filtering by `id eq` \
                        (filtered point-lookup is an authoritative existence check, not \
                        paginated)"
                        .to_owned(),
                });
            }
            if pagination.top() != 1 {
                return Err(DomainError::Validation {
                    detail: "list_users: top MUST be 1 when filtering by `id eq` (filtered \
                        point-lookup is an authoritative existence check, not paginated)"
                        .to_owned(),
                });
            }
        }

        // Inject default order + `id ASC` tiebreaker before SPI
        // dispatch. Plugins receive a deterministic ordering even when
        // the caller supplied none, and any caller-supplied order gets
        // a stable tiebreaker so cursor continuations are well-defined.
        let effective_order = order
            .unwrap_or_else(|| {
                ODataOrderBy(vec![OrderKey {
                    field: "username".into(),
                    dir: SortDir::Asc,
                }])
            })
            .ensure_tiebreaker("id", SortDir::Asc);

        // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-invoke-contract
        // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-package-request-luser
        // Convert internal `TenantContext` → SDK `IdpTenantContext` at
        // the plugin-SPI boundary.
        let req = {
            let mut base =
                IdpListUsersRequest::new(IdpTenantContext::from(&tenant_context), pagination);
            if let Some(f) = filter {
                base = base.with_filter(f);
            }
            base.with_order(effective_order)
        };
        // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-package-request-luser
        // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-invoke-luser
        let outcome = self.idp_user.list_users(ctx, &req).await;
        // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-invoke-luser
        // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-invoke-contract

        match outcome {
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-success-return
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-project
            // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-success-return-luser
            Ok(page) => {
                // Provider contract guard: surface `id eq` filter drift
                // as Internal 500 — downstream existence checks and
                // RBAC mapping treat this page as authoritative, so
                // silent drift is security-relevant.
                if let Some(filter_id) = existence_check_id
                    && (page.items.len() > 1 || page.items.iter().any(|u| u.id != filter_id))
                {
                    let bad_ids: Vec<Uuid> = page.items.iter().map(|u| u.id).collect();
                    tracing::warn!(
                        target: "am.user.audit",
                        tenant_id = %tenant_id,
                        requested_user_id = %filter_id,
                        returned_ids = ?bad_ids,
                        count = page.items.len(),
                        "list_users: provider violated `$filter=id eq <uuid>` contract"
                    );
                    return Err(DomainError::Internal {
                        diagnostic: format!(
                            "list_users: provider returned {} item(s) for \
                             `$filter=id eq {filter_id}`; expected at most 1 \
                             item with matching id",
                            page.items.len()
                        ),
                        cause: None,
                    });
                }
                // Response-side schema validation is intentionally NOT performed
                // (see `create_user` for the rationale).
                Ok(page)
            }
            // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-success-return-luser
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-project
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-success-return
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-unavailable-branch
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-unavailable-return
            Err(failure) => Err(failure.into_domain_error(tenant_id)),
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-unavailable-return
            // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-unavailable-branch
        }
    }
    // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-service

    // ----------------------------------------------------------------
    // Helpers
    // ----------------------------------------------------------------

    /// Resolve `tenant_id` to an [`TenantStatus::Active`] tenant and
    /// build the [`TenantContext`] forwarded to the `IdP` plugin.
    ///
    /// Centralised so each flow shares one tenant guard implementation
    /// and CPT review can verify the precondition once instead of
    /// three times.
    ///
    /// `tenant_type` on the returned context is mandatory: AM resolves
    /// the chained GTS identifier via `TypesRegistryClient` and
    /// surfaces an outage as [`DomainError::service_unavailable`]
    /// rather than leaking an `Option` through the plugin contract.
    /// The opaque `metadata` blob is loaded from `tenant_idp_metadata`
    /// (whatever the `IdP` plugin returned at `provision_tenant` time
    /// and AM persisted via `activate_tenant`) so every `IdP` call
    /// receives the plugin's own per-tenant state inline.
    async fn resolve_active_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<TenantContext, DomainError> {
        let tenant = self
            .tenant_repo
            .find_by_id(scope, tenant_id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                detail: format!("tenant {tenant_id} not found"),
                resource: tenant_id.to_string(),
            })?;

        if !matches!(tenant.status, TenantStatus::Active) {
            return Err(DomainError::Validation {
                detail: format!(
                    "tenant {} is not active (status={})",
                    tenant.id,
                    tenant.status.as_str()
                ),
            });
        }

        // Resolve the chained `tenant_type` mandatorily — the
        // plugin contract no longer accepts `None`. A registry
        // blip surfaces as `ServiceUnavailable` (HTTP 503): user
        // ops cannot proceed without the type because plugins may
        // route on it (Keycloak realm name, Zitadel organization
        // selection, vendor-side org id derivation).
        // `get_type_schema_by_uuid` returns the typed
        // `GtsTypeId` directly so no string round-trip is
        // needed.
        let tenant_type = match self
            .types_registry
            .get_type_schema_by_uuid(tenant.tenant_type_uuid)
            .await
        {
            Ok(schema) => schema.type_id,
            Err(err) => {
                tracing::warn!(
                    target: "am.user.service",
                    tenant_type_uuid = %tenant.tenant_type_uuid,
                    error = %err,
                    "tenant_type uuid -> chained-id resolution failed; surfacing ServiceUnavailable"
                );
                return Err(DomainError::service_unavailable(format!(
                    "tenant_type resolution failed for tenant {}: {err}",
                    tenant.id
                )));
            }
        };

        // Load the plugin-private metadata blob AM stamped at
        // `activate_tenant` time. AM does not interpret the shape;
        // the value flows verbatim into `TenantContext::metadata`
        // so the plugin sees its own state.
        let metadata = self.tenant_repo.find_idp_metadata(scope, tenant.id).await?;
        Ok(TenantContext::new(
            tenant.id,
            tenant.name,
            tenant_type,
            metadata,
        ))
    }

    // ----------------------------------------------------------------
    // RG-membership cleanup post user-deprovision
    // ----------------------------------------------------------------

    /// Remove every RG user-group membership row that references the
    /// deprovisioned user as a member. Closes the orphaned-row gap
    /// left by AM hard-deleting a user while RG still holds rows
    /// pointing at it.
    ///
    /// Short-circuits on `rg_client = None` (test fixtures that don't
    /// model membership scenarios). Runs under the CALLER's `ctx`
    /// (VHP-190): the caller that deleted the user authorizes clearing
    /// its RG memberships, which keeps this read off the elevated
    /// system-actor PDP path (background flows with no caller still use
    /// the `system_actor` factories).
    ///
    /// Two-step, tenant-scoped by construction:
    ///
    /// 1. `list_groups($filter = tenant_id eq T AND type eq USER_GROUP_RG_TYPE_CODE)`
    ///    — drain all pages → `Vec<group_id>`. Listing groups (not
    ///    memberships) is what clamps the cleanup to a single tenant:
    ///    `ResourceGroup` carries `tenant_id` directly, whereas
    ///    `ResourceGroupMembership` does not, so a membership-keyed
    ///    listing cannot express a tenant filter without joining
    ///    through `group_id`.
    /// 2. For each group: `remove_membership(group_id, USER_RG_TYPE_CODE,
    ///    user_id.to_string())`. `NotFound` is idempotent success —
    ///    the user simply isn't a member of that group, or a peer
    ///    cleanup tick has already removed the row.
    ///
    /// Symmetric with `domain::user_groups::cascade::cascade_inner`,
    /// which also lists tenant-scoped user-groups and then dispatches
    /// per-group cleanup. The whole body is wrapped in a single
    /// [`tokio::time::timeout`] (`RG_CLEANUP_BUDGET`) so a tenant with
    /// pathologically many user-groups cannot pin the deprovision
    /// request future indefinitely.
    ///
    /// # Errors
    ///
    /// * [`DomainError::ServiceUnavailable`] -- RG transport failure,
    ///   per-call timeout, or overall-budget exceeded. The user has
    ///   already been removed from `IdP`; the caller's retry path
    ///   returns `NotFoundInTenant` from `IdP` and retries cleanup
    ///   against the still-orphaned RG rows. The mapping to AIP-193
    ///   `ServiceUnavailable` (HTTP 503) matches other RG-failure
    ///   call sites (cascade hook, resource ownership checker) so
    ///   the dependency-health signal is uniform across AM.
    /// * [`DomainError::Internal`] -- RG returned an unexpected error
    ///   shape (e.g. an `OData` cursor it cannot decode). Surfaces
    ///   loud rather than silently swallowing.
    async fn cleanup_user_group_memberships(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), DomainError> {
        let Some(rg) = self.rg_client.as_ref() else {
            return Ok(());
        };
        let body = cleanup_inner(rg.as_ref(), ctx, tenant_id, user_id);
        match tokio::time::timeout(RG_CLEANUP_BUDGET, body).await {
            Ok(res) => res,
            Err(_elapsed) => {
                emit_metric(
                    AM_DEPENDENCY_HEALTH,
                    MetricKind::Counter,
                    &[
                        ("target", "resource_group"),
                        ("op", "user_cleanup_memberships"),
                        ("outcome", "budget_exceeded"),
                    ],
                );
                Err(DomainError::service_unavailable(format!(
                    "resource-group: cleanup of memberships for deprovisioned user {user_id} in \
                     tenant {tenant_id} exceeded overall budget of {}s",
                    RG_CLEANUP_BUDGET.as_secs()
                )))
            }
        }
    }
}

/// Inner cleanup body — extracted so the overall budget timeout in
/// [`UserService::cleanup_user_group_memberships`] can wrap it as one
/// future.
///
/// Lists every user-group in the tenant, then issues one
/// `remove_membership(group_id, USER_RG_TYPE_CODE, user_id)` per
/// listed group. `NotFound` on remove is idempotent (the user
/// wasn't a member of that group); transport errors surface as
/// [`DomainError::ServiceUnavailable`] so the caller's retry path
/// re-enters the flow.
async fn cleanup_inner(
    rg: &(dyn ResourceGroupClient + Send + Sync),
    sys_ctx: &SecurityContext,
    tenant_id: Uuid,
    user_id: Uuid,
) -> Result<(), DomainError> {
    let user_id_str = user_id.to_string();
    let groups = list_tenant_user_groups(rg, sys_ctx, tenant_id, user_id).await?;

    let mut removed: usize = 0;
    let mut already_gone: usize = 0;
    for group_id in &groups {
        // The trait boundary is `CanonicalError` (ADR 0005); project the
        // inner result into the typed SDK view so the `NotFound`
        // idempotent arm dispatches as before.
        let outcome = tokio::time::timeout(
            RG_CLEANUP_TIMEOUT,
            rg.remove_membership(sys_ctx, *group_id, USER_RG_TYPE_CODE, &user_id_str),
        )
        .await
        .map(|r| r.map_err(ResourceGroupError::from));
        match outcome {
            Err(_elapsed) => {
                emit_metric(
                    AM_DEPENDENCY_HEALTH,
                    MetricKind::Counter,
                    &[
                        ("target", "resource_group"),
                        ("op", "user_cleanup_remove_membership"),
                        ("outcome", "timeout"),
                    ],
                );
                return Err(DomainError::service_unavailable(format!(
                    "resource-group: timeout removing user {user_id} from group {group_id} \
                     during deprovision cleanup"
                )));
            }
            Ok(Err(ResourceGroupError::NotFound { .. })) => {
                // Either the user was never a member of this group,
                // or a peer cleanup tick removed the row between our
                // list and remove calls. Idempotent success — emit
                // a distinct outcome so the metric distinguishes "we
                // removed it" from "it wasn't there".
                already_gone += 1;
                emit_metric(
                    AM_DEPENDENCY_HEALTH,
                    MetricKind::Counter,
                    &[
                        ("target", "resource_group"),
                        ("op", "user_cleanup_remove_membership"),
                        ("outcome", "already_gone"),
                    ],
                );
            }
            Ok(Err(e)) => {
                emit_metric(
                    AM_DEPENDENCY_HEALTH,
                    MetricKind::Counter,
                    &[
                        ("target", "resource_group"),
                        ("op", "user_cleanup_remove_membership"),
                        ("outcome", "error"),
                    ],
                );
                tracing::warn!(
                    target: "am.events",
                    tenant_id = %tenant_id,
                    user_id = %user_id,
                    group_id = %group_id,
                    error = %e,
                    "RG user-group membership cleanup failed during user deprovision; \
                     orphaned membership row pending retry"
                );
                return Err(DomainError::service_unavailable(format!(
                    "resource-group: failed to remove user {user_id} from group {group_id} \
                     during deprovision cleanup: {e}"
                )));
            }
            Ok(Ok(())) => {
                removed += 1;
            }
        }
    }

    emit_metric(
        AM_DEPENDENCY_HEALTH,
        MetricKind::Counter,
        &[
            ("target", "resource_group"),
            ("op", "user_cleanup_memberships"),
            ("outcome", "success"),
        ],
    );
    tracing::debug!(
        target: "am.user_groups",
        tenant_id = %tenant_id,
        user_id = %user_id,
        groups_listed = groups.len(),
        memberships_removed = removed,
        already_gone,
        "deprovision cleanup completed"
    );

    Ok(())
}

/// Drain every user-group in `tenant_id`, paginated by `CursorV1`.
///
/// Filters `tenant_id eq T AND type eq USER_GROUP_RG_TYPE_CODE` so
/// the result set is tenant-clamped at the source. `user_id` is
/// included in error diagnostics only; the listing itself is
/// per-tenant, not per-user.
async fn list_tenant_user_groups(
    rg: &(dyn ResourceGroupClient + Send + Sync),
    sys_ctx: &SecurityContext,
    tenant_id: Uuid,
    user_id: Uuid,
) -> Result<Vec<Uuid>, DomainError> {
    let filter = Expr::And(
        Box::new(Expr::Compare(
            Box::new(Expr::Identifier("tenant_id".to_owned())),
            CompareOperator::Eq,
            Box::new(Expr::Value(Value::Uuid(tenant_id))),
        )),
        Box::new(Expr::Compare(
            Box::new(Expr::Identifier("type".to_owned())),
            CompareOperator::Eq,
            Box::new(Expr::Value(Value::String(
                USER_GROUP_RG_TYPE_CODE.to_owned(),
            ))),
        )),
    );

    let mut all_ids = Vec::new();
    let mut cursor: Option<CursorV1> = None;
    loop {
        let mut query = ODataQuery::default()
            .with_limit(RG_CLEANUP_PAGE_SIZE)
            .with_filter(filter.clone());
        if let Some(c) = cursor.take() {
            query = query.with_cursor(c);
        }

        let page =
            match tokio::time::timeout(RG_CLEANUP_TIMEOUT, rg.list_groups(sys_ctx, &query)).await {
                Err(_elapsed) => {
                    emit_metric(
                        AM_DEPENDENCY_HEALTH,
                        MetricKind::Counter,
                        &[
                            ("target", "resource_group"),
                            ("op", "user_cleanup_list_groups"),
                            ("outcome", "timeout"),
                        ],
                    );
                    return Err(DomainError::service_unavailable(format!(
                        "resource-group: timeout listing user-groups for deprovisioned user \
                         {user_id} in tenant {tenant_id}"
                    )));
                }
                Ok(Err(e)) => {
                    emit_metric(
                        AM_DEPENDENCY_HEALTH,
                        MetricKind::Counter,
                        &[
                            ("target", "resource_group"),
                            ("op", "user_cleanup_list_groups"),
                            ("outcome", "error"),
                        ],
                    );
                    return Err(DomainError::service_unavailable(format!(
                        "resource-group: failed to list user-groups for deprovisioned user \
                         {user_id} in tenant {tenant_id}: {e}"
                    )));
                }
                Ok(Ok(p)) => p,
            };

        all_ids.extend(page.items.into_iter().map(|g| g.id));

        match page.page_info.next_cursor {
            Some(token) => {
                cursor = Some(CursorV1::decode(&token).map_err(|e| DomainError::Internal {
                    diagnostic: format!(
                        "resource-group: invalid cursor from list_groups during \
                         user-deprovision cleanup ({user_id} in {tenant_id}): {e}"
                    ),
                    cause: None,
                })?);
            }
            None => break,
        }
    }

    Ok(all_ids)
}

/// AM-side fallback length cap for the optional profile fields
/// (`email`, `display_name`) on `create_user`. `None`
/// short-circuits to `Ok(())` because the public schema declares
/// the field optional and AM does not synthesise a value;
/// `Some(value)` is rejected with [`DomainError::Validation`] when
/// the **raw** char-count exceeds [`MAX_PROFILE_FIELD_CHARS`].
///
/// No trim normalisation is applied to these fields (unlike
/// `username`) — vendor profiles differ on whether `email`
/// uniqueness is whitespace-sensitive and `display_name` is a
/// user-visible label where surrounding whitespace can be
/// intentional. The cap is therefore on the raw value as supplied;
/// whitespace policy stays the provider's call. The cap alone is
/// enough to prevent megabyte-scale forwards to the `IdP` when the
/// GTS schema is not registered.
fn check_profile_field_bound(
    field_name: &'static str,
    value: Option<&str>,
) -> Result<(), DomainError> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.chars().count() > MAX_PROFILE_FIELD_CHARS {
        return Err(DomainError::Validation {
            detail: format!(
                "create_user: {field_name} MUST be {MAX_PROFILE_FIELD_CHARS} characters or fewer"
            ),
        });
    }
    Ok(())
}
// @cpt-end:cpt-cf-account-management-dod-idp-user-operations-contract-no-local-user-storage:p1:inst-dod-idp-user-operations-contract-no-local-user-storage-service
