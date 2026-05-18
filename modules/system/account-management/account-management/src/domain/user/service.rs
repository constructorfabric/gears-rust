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
    IdpProvisionUserRequest, IdpTenantContext, IdpUser, IdpUserPagination, ListUsersQuery,
};
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use modkit_macros::domain_model;
use modkit_odata::Page;
use modkit_odata::ast::{CompareOperator, Expr, Value};
use modkit_odata::{CursorV1, ODataQuery};
use modkit_security::AccessScope;
use modkit_security::{SecurityContext, pep_properties};
use resource_group_sdk::{ResourceGroupClient, ResourceGroupError};
use std::time::Duration;
use types_registry_sdk::TypesRegistryClient;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::idp::UserOperationFailureExt;
use crate::domain::metrics::{AM_DEPENDENCY_HEALTH, MetricKind, emit_metric};
use crate::domain::tenant::TenantContext;
use crate::domain::tenant::model::TenantStatus;
use crate::domain::tenant::repo::TenantRepo;
use crate::domain::user_groups::am_system_context;

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
/// A missing `gts.cf.core.am.user.v1~` schema is NOT a fallback
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

/// Central AM domain service for the `IdP` user-operations contract.
///
/// Construction mirrors [`crate::domain::conversion::service::ConversionService`]
/// -- every dependency is passed in as `Arc<dyn ...>` so production
/// wiring (`module.rs`) and tests (`FakeTenantRepo` /
/// `FakeIdpUserProvisioner`) share the same constructor surface. The
/// service holds no clock seam and no batch-size knobs because the
/// FEATURE doc state model is empty (no AM-side lifecycle here -- see
/// the section "States (CDSL): Not applicable").
/// PEP descriptors for the tenant-scoped `IdP` user resource.
///
/// Mirrors the `pep::TENANT` / `pep::METADATA` declarations on
/// sibling services. The resource type name pins
/// [`account_management_sdk::USER_RESOURCE_TYPE`]; the impl-side
/// duplication is required because `ResourceType.name` is a
/// `&'static str` consumed at compile time. Cross-checks live in
/// `domain::error_tests`.
pub(super) mod pep {
    use super::{ResourceType, pep_properties};

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
    pub const USER: ResourceType = ResourceType {
        name: "gts.cf.core.am.user.v1~",
        supported_properties: &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    };

    /// Action vocabulary. `get_user` is internally a single-row
    /// `list_users` projection so it shares the `LIST` policy bucket;
    /// fine-grained per-user `READ` is reserved for the future
    /// (e.g. policies that gate per-user-id metadata reads behind a
    /// separate decision).
    pub mod actions {
        pub const CREATE: &str = "create";
        pub const LIST: &str = "list";
        pub const DELETE: &str = "delete";
    }
}

#[domain_model]
pub struct UserService {
    tenant_repo: Arc<dyn TenantRepo>,
    idp_user: Arc<dyn IdpPluginClient>,
    /// GTS Types Registry client used to fetch the
    /// `gts.cf.core.am.user.v1~` schema at runtime so the structural
    /// contract (length bounds, formats) is enforced from the
    /// published JSON Schema rather than re-hardcoded here. Mirrors
    /// the wiring in `cf-resource-group::validate_metadata_via_gts`.
    types_registry: Arc<dyn TypesRegistryClient>,
    /// PEP gate. Mirrors `TenantService` / `MetadataService`: every
    /// public user-ops method PEP-gates via [`Self::authorize`]
    /// before any `IdP` / RG round trip. The `PolicyEnforcer` is owned
    /// by-value (it is `Clone`); the module wiring clones it from
    /// the shared instance used by sibling services.
    enforcer: PolicyEnforcer,
    /// Optional `ResourceGroupClient` used by
    /// [`Self::delete_user`] to remove dangling user-group
    /// memberships referencing the deleted user from RG's
    /// `resource_group_membership` table. `None` in tests that don't
    /// exercise the cleanup path; production wiring in `module.rs`
    /// always passes `Some(...)`.
    rg_client: Option<Arc<dyn ResourceGroupClient + Send + Sync>>,
}

/// Per-call RG timeout for the cleanup helper.
///
/// Matches `CASCADE_TIMEOUT` in `domain::user_groups::cascade` so the
/// two pipelines share an operator-tunable upper bound. Lifted into
/// a constant so the `delete_user` test fixtures can construct
/// it without re-deriving the literal.
#[allow(
    clippy::duration_suboptimal_units,
    reason = "from_mins is unstable on workspace MSRV; keep from_secs"
)]
const RG_CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum user-group rows fetched per `list_groups` page during cleanup.
///
/// Matches `CASCADE_PAGE_SIZE` in `domain::user_groups::cascade`.
const RG_CLEANUP_PAGE_SIZE: u64 = 100;

/// Overall budget for one `cleanup_user_group_memberships` invocation.
///
/// Caps the worst-case time the deprovision request can spend on RG
/// cleanup before surfacing [`DomainError::ServiceUnavailable`].
/// Without this cap a tenant with N user-groups would compound
/// `RG_CLEANUP_TIMEOUT` × pages × N remove-calls into a request future
/// that the caller cannot cancel.
///
/// Tighter sibling of `CASCADE_BUDGET` in
/// `domain::user_groups::cascade`: cleanup runs on the synchronous
/// `delete_user` request path (60s -- a user-visible response
/// budget), whereas cascade runs on the retention pipeline's tenant
/// hard-delete tick (120s -- a background-job budget that can yield
/// cheaply). The two are intentionally asymmetric on purpose.
#[allow(
    clippy::duration_suboptimal_units,
    reason = "from_mins is unstable on workspace MSRV; keep from_secs"
)]
const RG_CLEANUP_BUDGET: Duration = Duration::from_secs(60);

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
        }
    }

    /// PEP gate. Calls the platform-side `PolicyEnforcer`, returns
    /// the [`AccessScope`] the tenant-existence guard
    /// (`resolve_active_tenant`) forwards through `modkit_db`'s
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
        let request = AccessRequest::new()
            .resource_property(pep_properties::OWNER_TENANT_ID, tenant_id)
            .resource_property(pep_properties::RESOURCE_ID, tenant_id)
            .require_constraints(true);
        let scope = self
            .enforcer
            .access_scope_with(ctx, &pep::USER, action, Some(tenant_id), &request)
            .await?;
        Ok(scope)
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
    /// Production wiring in `module.rs` always invokes this builder;
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
    /// * [`DomainError::IdpUnavailable`] -- transport failure or
    ///   timeout on the `IdP` call.
    /// * [`DomainError::UnsupportedOperation`] -- provider declined
    ///   the operation.
    /// * [`DomainError::Validation`] -- provider rejected the payload
    ///   (duplicate username, malformed email, etc.).
    /// * [`DomainError::ServiceUnavailable`] -- GTS Types Registry
    ///   transport failure while fetching the
    ///   `gts.cf.core.am.user.v1~` schema; AM cannot validate the
    ///   payload structurally, so the call fails closed rather than
    ///   forwarding an unvalidated payload to the `IdP`.
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

        // AM business invariants on the username, enforced AFTER the
        // tenant guard so payload-shape feedback never leaks tenant
        // existence, and BEFORE the GTS schema round-trip so the
        // structural validator sees the canonical (trimmed) value:
        //   * Reject all-whitespace / empty — the schema's
        //     `minLength: 1` does not catch `"  "` which is
        //     semantically empty for a login identifier.
        //   * Reject lengths past 255 chars before the IdP round-trip
        //     — matches the `child_tenant_name` bound (`m0004` line
        //     45) so AM caps payload size at the boundary instead of
        //     forwarding megabyte-scale strings to the provider only
        //     to be rejected as `Validation` with a redacted detail.
        //   * Normalise surrounding whitespace — `" alice "` and
        //     `"alice"` MUST resolve to the same provider identity
        //     regardless of the vendor's storage semantics (literal /
        //     auto-trim / hard-reject).
        //
        // # Trim-before-schema ordering is deliberate
        //
        // AM-side trim is the **canonical normalisation** for the
        // username field — the published GTS schema declares
        // structural shape (`minLength` / `maxLength` / format) but
        // delegates whitespace-and-equivalence policy to the AM
        // service layer. If a future schema revision wants to
        // enforce a no-surrounding-whitespace pattern via JSON
        // Schema `pattern`, mirror it as an AM-side pre-trim
        // assertion here instead of inverting the order; the AM-side
        // normalisation MUST stay authoritative because the
        // schema-validation seam is structural-shape only.
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
        // Defence-in-depth caps on the secondary profile fields.
        // Runs BEFORE the GTS round-trip as belt-and-suspenders:
        // the helper below fails closed with `ServiceUnavailable`
        // when the `gts.cf.core.am.user.v1~` schema is not yet
        // registered (so a missing schema can never silently bypass
        // `format` / `pattern` rules), but these cheap pre-flight
        // caps cut megabyte-scale payloads before they ever hit the
        // JSON-Schema validator. When the schema IS registered the
        // validator below ALSO enforces `maxLength` / `format`; the
        // duplication is intentional, not load-bearing.
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
        let outcome = self.idp_user.provision_user(&req).await;
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
                // `requested_by` would. Mirrors the `user_id_filter`
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
                // Response-side schema validation is NOT performed.
                // AM trusts the plugin's published `IdpPluginClient`
                // contract: a successful `Ok(IdpUser)` is, by
                // contract, a schema-conformant projection. Adding an
                // AM-side `validate_user_projection_via_gts` fence
                // here would either (a) fail closed on the entire
                // response when a drifted plugin emits a single bad
                // field — breaking the public surface that callers
                // depend on for existence checks — or (b) silently
                // drop bad rows, which would invent phantom-absent
                // users and corrupt downstream membership state. The
                // input-side gate (`validate_new_user_payload_via_gts`
                // above) keeps AM from forwarding bad payloads; the
                // output-side contract is owned by the plugin.
                // Audit-success line is the ONLY `am.events` emission
                // for this flow; failure-side correlation lives on
                // `am.idp` warn lines emitted by the redaction pipeline
                // in [`UserOperationFailureExt::into_domain_error`]
                // (digest + len + tenant_id). Mirrors the conversion
                // service which also emits `am.events` only on the Ok
                // arm so a downstream consumer grouping by `event`
                // counts successes, not attempts.
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
    /// * [`DomainError::IdpUnavailable`] -- transport failure or
    ///   timeout on the `IdP` call.
    /// * [`DomainError::UnsupportedOperation`] -- provider declined
    ///   the operation.
    /// * [`DomainError::Validation`] -- provider rejected the request.
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
        let outcome = self.idp_user.deprovision_user(&req).await;
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
                self.cleanup_user_group_memberships(tenant_id, user_id)
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
    // List users
    // ----------------------------------------------------------------

    /// Fetch a single user by id from `tenant_id` via the configured
    /// `IdP` plugin. Thin wrapper over [`Self::list_users`] with
    /// `user_id_filter = Some(user_id)`, `top = 1`, `cursor = None`;
    /// the AM-level pagination disciplines documented on `list_users`
    /// (cursor MUST be absent, top MUST be 1 when `user_id_filter` is
    /// set) make a one-shot filtered lookup the authoritative
    /// existence check for a specific user.
    ///
    /// User profile mutation is intentionally **not** exposed by AM —
    /// `email` / `display_name` / `username` live in the `IdP` and
    /// SCIM-style edits go directly to the provider's admin API per
    /// `cpt-cf-account-management-adr-idp-user-identity-source-of-truth`.
    /// AM exposes only the lifecycle saga (`create_user` /
    /// `delete_user`) and read-side projection (`get_user` /
    /// `list_users`).
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
        // `IdpUserPagination::new(1, None)` is statically valid (`top=1`
        // is `>= 1` and `<= MAX_TOP`, no cursor to validate), so the
        // `Err` arm is unreachable. Route the impossible failure
        // through `Internal` rather than `expect()` to keep the
        // workspace clippy `expect_used` lint clean and surface any
        // future regression (e.g. a stricter constructor contract
        // that rejects `top=1`) as a structured error.
        let pagination = IdpUserPagination::new(1, None).map_err(|e| DomainError::Internal {
            diagnostic: format!(
                "get_user: failed to build single-row pagination (top=1, no cursor): {e}"
            ),
            cause: None,
        })?;
        let query = ListUsersQuery::new(pagination).with_user_id_filter(user_id);
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
    /// `user_id_filter = Some(_)` is the authoritative existence
    /// signal consumed by sibling features (e.g. `feature-user-groups`).
    ///
    /// Implements
    /// `cpt-cf-account-management-flow-idp-user-operations-contract-list-users`.
    ///
    /// # Errors
    ///
    /// * [`DomainError::NotFound`] -- `tenant_id` does not resolve.
    /// * [`DomainError::Validation`] -- tenant exists but is not
    ///   [`TenantStatus::Active`].
    /// * [`DomainError::IdpUnavailable`] -- transport failure or
    ///   timeout. NO stale projection is served per
    ///   `cpt-cf-account-management-principle-idp-agnostic`.
    /// * [`DomainError::UnsupportedOperation`] -- provider declined
    ///   the operation.
    /// * [`DomainError::Validation`] -- provider rejected the request.
    // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-service
    pub async fn list_users(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, DomainError> {
        let ListUsersQuery {
            pagination,
            user_id_filter,
            ..
        } = query;
        // PEP gate FIRST. The `LIST` action is used for both the
        // raw list and the `get_user` existence-check shape
        // (`user_id_filter = Some(_)`) so the same policy decision
        // governs both surfaces. The compiled scope clamps the
        // tenant-existence guard below.
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

        // When `user_id_filter` is set the call is an authoritative
        // existence check per the SDK doc on
        // `IdpListUsersRequest::user_id_filter` — both a one-element page
        // and an empty page are success and downstream callers
        // (feature-user-groups membership writes, RBAC mapping) treat
        // the empty page as authoritative absence. Enforce two
        // pagination disciplines at the AM boundary so the
        // existence-check semantics cannot be bypassed:
        //   * `cursor` MUST be absent — a continuation cursor would
        //     let the provider step past the matching row and return
        //     an empty page, turning the lookup into a false negative.
        //   * `top` MUST be 1 — an oversized `top` (e.g. 1000) plus a
        //     filter forwards both to a vendor that ignores the
        //     filter, returning up to `top` unrelated rows; the
        //     downstream `user_id_filter` contract check would then
        //     surface a caller-side bug as `Internal` (HTTP 500)
        //     instead of catching it at the AM seam.
        if user_id_filter.is_some() {
            if pagination.cursor().is_some() {
                return Err(DomainError::Validation {
                    detail: "list_users: cursor MUST be absent when user_id_filter is set \
                        (filtered lookup is an authoritative existence check, not a paginated \
                        query)"
                        .to_owned(),
                });
            }
            if pagination.top() != 1 {
                return Err(DomainError::Validation {
                    detail: "list_users: top MUST be 1 when user_id_filter is set (filtered \
                        lookup returns at most one row)"
                        .to_owned(),
                });
            }
        }

        // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-invoke-contract
        // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-package-request-luser
        // Convert internal `TenantContext` → SDK `IdpTenantContext` at
        // the plugin-SPI boundary.
        let req = {
            let base =
                IdpListUsersRequest::new(IdpTenantContext::from(&tenant_context), pagination);
            if let Some(uid) = user_id_filter {
                base.with_user_id_filter(uid)
            } else {
                base
            }
        };
        // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-package-request-luser
        // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-invoke-luser
        let outcome = self.idp_user.list_users(&req).await;
        // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-invoke-luser
        // @cpt-end:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-invoke-contract

        match outcome {
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-success-return
            // @cpt-begin:cpt-cf-account-management-flow-idp-user-operations-contract-list-users:p1:inst-flow-luser-project
            // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-success-return-luser
            Ok(page) => {
                // Plugin-contract validation when `user_id_filter` is set:
                // a buggy provider could echo back unrelated users or
                // multiple rows under a single-user lookup. Downstream
                // callers (existence checks, RBAC mapping) treat the
                // returned page as authoritative, so a contract drift
                // here is a security-relevant correctness bug. Surface
                // any drift as `Internal` rather than silently passing
                // wrong data through.
                if let Some(filter_id) = user_id_filter
                    && (page.items.len() > 1 || page.items.iter().any(|u| u.id != filter_id))
                {
                    let bad_ids: Vec<Uuid> = page.items.iter().map(|u| u.id).collect();
                    tracing::warn!(
                        target: "am.user.audit",
                        tenant_id = %tenant_id,
                        requested_user_id = %filter_id,
                        returned_ids = ?bad_ids,
                        count = page.items.len(),
                        "list_users: provider violated user_id_filter contract"
                    );
                    return Err(DomainError::Internal {
                        diagnostic: format!(
                            "list_users: provider returned {} item(s) for user_id_filter={filter_id}; \
                             expected at most 1 item with matching id",
                            page.items.len()
                        ),
                        cause: None,
                    });
                }
                // Response-side schema validation is NOT performed
                // (mirrors the `create_user` rationale above): AM
                // trusts the plugin's published projection contract
                // and only enforces the `user_id_filter` discipline
                // here. Failing closed on a single drifted row would
                // break the entire listing for read-only consumers,
                // and user projections are pass-through (never
                // persisted), so a stale projection cannot lock in
                // the way a persisted bad row could.
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
        // `GtsSchemaId` directly so no string round-trip is
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
    /// model membership scenarios). System-actor context is built via
    /// [`am_system_context`] so the call goes through cross-module
    /// authz with the stable AM subject UUID, mirroring the
    /// user-group cascade hook.
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
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), DomainError> {
        let Some(rg) = self.rg_client.as_ref() else {
            return Ok(());
        };
        let sys_ctx = am_system_context(Some(tenant_id));

        let body = cleanup_inner(rg.as_ref(), &sys_ctx, tenant_id, user_id);
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
        match tokio::time::timeout(
            RG_CLEANUP_TIMEOUT,
            rg.remove_membership(sys_ctx, *group_id, USER_RG_TYPE_CODE, &user_id_str),
        )
        .await
        {
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
