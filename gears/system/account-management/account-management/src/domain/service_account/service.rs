//! `ServiceAccountService` — domain orchestrator for tenant-scoped
//! machine identities.
//!
//! Composes the shared tenant guard
//! ([`crate::domain::tenant::resolve::resolve_active_tenant`]) with the
//! resolved [`IdpPluginClient`] plugin to deliver four flows:
//!
//! * `create`        — `POST   /tenants/{tenant_id}/service-accounts`
//! * `list`          — `GET    /tenants/{tenant_id}/service-accounts`
//! * `rotate_secret` — `POST   /tenants/{tenant_id}/service-accounts/{client_id}/rotate-secret`
//! * `revoke`        — `DELETE /tenants/{tenant_id}/service-accounts/{client_id}`
//!
//! Every method follows the same ordering as the user pass-through, and
//! for the same reasons:
//!
//! 1. PEP gate FIRST, so a denied caller is refused before any tenant
//!    read or provider round-trip.
//! 2. Resolve the tenant through the compiled [`AccessScope`], rejecting
//!    a miss with `NotFound` and a non-`Active` tenant with
//!    `Validation` — still before the provider is touched.
//! 3. Bound the caller's inputs (see [`MAX_NAME_CHARS`]) so a
//!    megabyte-scale payload never rides the wire.
//! 4. Forward a tenant-scope-bound request to the plugin and map the
//!    failure through [`ServiceAccountFailureExt`].
//!
//! # Authorization is tenant-granular, not object-granular
//!
//! Every gate on this surface resolves to a tenant and only to a tenant:
//! `pep::SERVICE_ACCOUNT` passes `tenant_id` as `OWNER_TENANT_ID`, as
//! `RESOURCE_ID`, and as the `access_scope_with` resource argument, so
//! the `client_id` is never an authorization input. That is the same
//! shape as `pep::USER` — an inherited AM convention rather than a
//! posture chosen here.
//!
//! **What the tenant clamp does cover.** There is no separate "does the
//! compiled scope cover this tenant?" assertion because step 2 is one:
//! the tenant read runs under the PDP-emitted `InTenantSubtree`
//! predicate, so a caller whose grant does not cover the target tenant
//! cannot resolve it at all and gets a `404` — the tenant is invisible
//! rather than merely forbidden, which also keeps tenant topology from
//! leaking through a `403`. The clamp is evaluated against AM's
//! `tenant_closure` table, so it is strictly stronger than comparing the
//! target against a scope's uuid set, and it is the same posture
//! `UserService` relies on.
//!
//! **What it does not cover** is the `(tenant_id, client_id)` coupling
//! *below* the tenant. Once the tenant clamp passes, nothing on the AM
//! side constrains which `client_id` the caller names: the address is
//! forwarded verbatim and its ownership is the adapter's to enforce. A
//! `client_id` not owned by the addressed tenant MUST come back as
//! [`IdpServiceAccountFailure::NotFound`], never acted upon.
//!
//! Both in-tree adapters do enforce that — keycloak's
//! `lookup_owned_client` requires the `svc-<tenant_id>-` client-id prefix
//! *and* a matching `tenant_id` attribute marker before it will act, and
//! the static plugin keys its store per tenant scope — but a third-party
//! adapter that omits the check has **no platform-side backstop**.
//! Sub-tenant ownership is therefore a contract obligation on the
//! adapter, not a guarantee AM makes on its behalf.
//!
//! # Secrets
//!
//! A secret exists in this process only inside
//! [`IdpServiceAccountCredentials`] (a
//! [`secrecy::SecretString`]: redacted `Debug`, zeroize-on-drop, no
//! Serde) on its way from the provider to the public boundary. REST
//! serialises it only in the one-time credential response; an in-process
//! `AccountManagementClient` caller receives the wrapper unchanged for
//! immediate transfer to its credential custodian. Nothing here logs,
//! stores, or re-reads it — the `am.events` lines below deliberately carry
//! the `client_id` and never the credential, and a lost secret is recovered
//! by rotating, not by reading.

use std::sync::Arc;

use account_management_sdk::{
    IdpListServiceAccountsRequest, IdpPluginClient, IdpProvisionServiceAccountRequest,
    IdpRevokeServiceAccountRequest, IdpRotateServiceAccountSecretRequest,
    IdpServiceAccountCredentials, IdpServiceAccountFailure, IdpServiceAccountSummary,
    SERVICE_ACCOUNT_RESOURCE_TYPE,
};
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::ResourceType;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use types_registry_sdk::TypesRegistryClient;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::idp::ServiceAccountFailureExt;
use crate::domain::tenant::TenantContext;
use crate::domain::tenant::repo::TenantRepo;

/// Upper bound on the caller-supplied `name`, enforced at the AM
/// boundary before the provider round-trip.
///
/// This is a **cap, not the charset contract**: which characters a name
/// may contain, and whether a name is already taken, are the adapter's
/// to decide (reported as
/// [`IdpServiceAccountFailure::InvalidInput`]), because a deployment's
/// `IdP` owns client-id derivation. AM only refuses to forward payloads
/// that no conforming adapter could accept, which keeps a megabyte-scale
/// name from riding the wire just to be rejected upstream. The bound is
/// generous against the ~40-character convention adapters typically
/// document, so tightening that convention never requires an AM release.
///
/// Counts Unicode scalars (`chars().count()`), not bytes — matching how
/// AM bounds `username` on the user surface.
pub(super) const MAX_NAME_CHARS: usize = 64;

/// Upper bound on the number of scopes accepted on one request. Scopes
/// are validated against the adapter's allowlist provider-side; this
/// only bounds the list AM is willing to forward.
const MAX_SCOPES: usize = 32;

/// Upper bound on a single scope string, in Unicode scalars. Same
/// rationale as [`MAX_SCOPES`].
const MAX_SCOPE_CHARS: usize = 256;

/// PEP descriptors. The resource-type id comes from the SDK's single
/// source of truth so the enforcement gate, the permission catalog
/// (`crate::gts::permissions`), and the canonical-error marker
/// (`crate::infra::sdk_error_mapping::ServiceAccountResource`) cannot
/// drift.
pub(crate) mod pep {
    use super::{ResourceType, SERVICE_ACCOUNT_RESOURCE_TYPE, pep_properties};

    /// Resource declaration for a service account. AM persists no
    /// account table, so the compiled `AccessScope` clamps no AM-owned
    /// rows of its own — its role is the PEP allow/deny gate plus the
    /// `InTenantSubtree` predicate the tenant guard
    /// (`resolve_active_tenant`) consults on the `tenants` read.
    ///
    /// Supported PEP properties:
    ///
    /// * `OWNER_TENANT_ID` — the tenant that owns the account;
    ///   ownership-style policies consume this.
    /// * `RESOURCE_ID` — set to the tenant id. The per-account
    ///   `client_id` is deliberately not policy-visible: it is an
    ///   `IdP`-side identifier in an adapter-chosen format, so a policy
    ///   keyed on it could not be written portably. Matching the
    ///   `tenants` entity's `resource_col = "id"` is also what lets the
    ///   compiled subtree clamp resolve through this property.
    ///
    /// Both properties therefore carry the same tenant id, which makes
    /// every decision on this surface tenant-granular; see the module
    /// docs for what that leaves to the adapter.
    pub const SERVICE_ACCOUNT: ResourceType = ResourceType::from_static(
        SERVICE_ACCOUNT_RESOURCE_TYPE,
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    /// Action vocabulary — per-verb rather than the coarse
    /// read/write/delete triad, so credential minting can be granted
    /// independently of reading the inventory. `ROTATE_SECRET` is
    /// separate from `CREATE` for the same reason: an operator who may
    /// re-key an existing account need not be able to mint new ones.
    pub mod actions {
        pub const CREATE: &str = "create";
        pub const LIST: &str = "list";
        pub const ROTATE_SECRET: &str = "rotate_secret";
        pub const REVOKE: &str = "revoke";

        /// Every action in this vocabulary, in declaration order. The
        /// permission-catalog test asserts one `AuthzPermissionV1` per
        /// `(resource_type, action)` pair drawn from these slices, so an
        /// action that is enforced at a PEP gate but never declared as a
        /// grantable permission fails the test instead of silently
        /// becoming ungrantable. Add new actions here as well as above.
        /// Test-only: nothing in the production path enumerates actions.
        #[cfg(test)]
        pub const ALL: &[&str] = &[CREATE, LIST, ROTATE_SECRET, REVOKE];
    }
}

/// Central AM domain service for tenant-scoped machine identities.
/// Pure pass-through: no clock seam, no batch knobs, no storage handles
/// — the contract has no AM-side lifecycle state to keep.
#[domain_model]
pub struct ServiceAccountService {
    tenant_repo: Arc<dyn TenantRepo>,
    idp: Arc<dyn IdpPluginClient>,
    /// Resolves a tenant's chained `tenant_type` for the plugin-facing
    /// context; an outage surfaces as `ServiceUnavailable` rather than
    /// leaking an `Option` through the contract.
    types_registry: Arc<dyn TypesRegistryClient>,
    /// PEP gate run before any tenant read or `IdP` round trip.
    enforcer: PolicyEnforcer,
}

impl ServiceAccountService {
    /// Construct a fully-wired service.
    #[must_use]
    pub const fn new(
        tenant_repo: Arc<dyn TenantRepo>,
        idp: Arc<dyn IdpPluginClient>,
        types_registry: Arc<dyn TypesRegistryClient>,
        enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            tenant_repo,
            idp,
            types_registry,
            enforcer,
        }
    }

    // ----------------------------------------------------------------
    // Provision
    // ----------------------------------------------------------------

    /// Provision a `client_credentials` service account owned by
    /// `tenant_id` and return its live credentials.
    ///
    /// The returned secret is surfaced exactly once — there is no
    /// read-back path anywhere in AM or the contract, so the caller
    /// persists it immediately (credstore) and recovers a loss with
    /// [`Self::rotate_secret`].
    ///
    /// # Errors
    ///
    /// * [`DomainError::CrossTenantDenied`] — the PDP denied the caller
    ///   (or returned constraints AM cannot compile; fail-closed).
    /// * [`DomainError::NotFound`] — `tenant_id` does not resolve within
    ///   the caller's subtree.
    /// * [`DomainError::Validation`] — the tenant is not `Active`, or the
    ///   request exceeded an AM boundary cap (empty / oversized `name`,
    ///   too many scopes, oversized scope).
    /// * [`DomainError::ServiceAccountInvalidInput`] — the provider
    ///   rejected the request with nothing retained (name already live
    ///   in the tenant, charset violation, scope outside the allowlist,
    ///   provider quota).
    /// * [`DomainError::ServiceAccountAmbiguous`] — transport
    ///   uncertainty; the provider may hold a half-created account, so
    ///   the caller reconciles via [`Self::list`] rather than retrying.
    /// * [`DomainError::IdpUnavailable`] — clean provider failure;
    ///   nothing retained, retry is safe.
    /// * [`DomainError::UnsupportedOperation`] — the deployment's `IdP`
    ///   adapter ships no service-account support.
    /// * [`DomainError::ServiceUnavailable`] — Types Registry or DB
    ///   transport failure while resolving the tenant.
    pub async fn create(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        name: String,
        scopes: Vec<String>,
    ) -> Result<IdpServiceAccountCredentials, DomainError> {
        // @cpt-begin:cpt-cf-account-management-flow-service-accounts-provision:p1:inst-flow-sa-provision-authorize
        // @cpt-begin:cpt-cf-account-management-dod-service-accounts-authenticated-tenant-scoped-invocation:p1:inst-dod-sa-authenticated-tenant-scoped-invocation-provision
        // PEP gate FIRST: a denied caller is refused before any tenant
        // read or provider round-trip.
        let scope = self.authorize(ctx, pep::actions::CREATE, tenant_id).await?;
        let actor = ctx.subject_id();
        // @cpt-end:cpt-cf-account-management-flow-service-accounts-provision:p1:inst-flow-sa-provision-authorize
        // @cpt-begin:cpt-cf-account-management-flow-service-accounts-provision:p1:inst-flow-sa-provision-resolve-tenant
        let tenant_context = self.resolve_active_tenant(&scope, tenant_id).await?;
        // @cpt-end:cpt-cf-account-management-flow-service-accounts-provision:p1:inst-flow-sa-provision-resolve-tenant

        // @cpt-begin:cpt-cf-account-management-flow-service-accounts-provision:p1:inst-flow-sa-provision-bound-inputs
        let name = check_name(name)?;
        check_scopes(&scopes)?;
        // @cpt-end:cpt-cf-account-management-flow-service-accounts-provision:p1:inst-flow-sa-provision-bound-inputs
        // @cpt-end:cpt-cf-account-management-dod-service-accounts-authenticated-tenant-scoped-invocation:p1:inst-dod-sa-authenticated-tenant-scoped-invocation-provision

        // @cpt-begin:cpt-cf-account-management-flow-service-accounts-provision:p1:inst-flow-sa-provision-invoke-contract
        // @cpt-begin:cpt-cf-account-management-algo-service-accounts-contract-invocation:p1:inst-algo-sa-contract-invocation-package-request
        let req =
            IdpProvisionServiceAccountRequest::new((&tenant_context).into(), name.clone(), scopes);
        // @cpt-end:cpt-cf-account-management-algo-service-accounts-contract-invocation:p1:inst-algo-sa-contract-invocation-package-request
        // @cpt-begin:cpt-cf-account-management-algo-service-accounts-contract-invocation:p1:inst-algo-sa-contract-invocation-invoke
        match self.idp.provision_service_account(ctx, &req).await {
            // @cpt-end:cpt-cf-account-management-algo-service-accounts-contract-invocation:p1:inst-algo-sa-contract-invocation-invoke
            // @cpt-end:cpt-cf-account-management-flow-service-accounts-provision:p1:inst-flow-sa-provision-invoke-contract
            Ok(credentials) => {
                // @cpt-begin:cpt-cf-account-management-flow-service-accounts-provision:p1:inst-flow-sa-provision-audit
                // Audit the identity, never the credential: `client_id`
                // is the address a later rotate / revoke uses, and the
                // submitted `name` is what a caller reconciling an
                // earlier ambiguous outcome searches on.
                tracing::info!(
                    target: "am.events",
                    event = "service_account_provisioned",
                    tenant_id = %tenant_id,
                    client_id = %credentials.client_id,
                    name = %name,
                    actor_uuid = %actor,
                    outcome = "ok",
                    "am service account provisioned"
                );
                // @cpt-end:cpt-cf-account-management-flow-service-accounts-provision:p1:inst-flow-sa-provision-audit
                // @cpt-begin:cpt-cf-account-management-flow-service-accounts-provision:p1:inst-flow-sa-provision-success-return
                Ok(credentials)
                // @cpt-end:cpt-cf-account-management-flow-service-accounts-provision:p1:inst-flow-sa-provision-success-return
            }
            // @cpt-begin:cpt-cf-account-management-flow-service-accounts-provision:p1:inst-flow-sa-provision-failure-return
            // The submitted `name` is the address this call had: no
            // `client_id` exists yet on a failed create, and `name` is the
            // correlation key the ambiguous-create recovery path searches on.
            Err(failure) => Err(failure.into_domain_error(tenant_id, &name)),
            // @cpt-end:cpt-cf-account-management-flow-service-accounts-provision:p1:inst-flow-sa-provision-failure-return
        }
    }

    // ----------------------------------------------------------------
    // List
    // ----------------------------------------------------------------

    /// List the tenant's service accounts (no secrets).
    ///
    /// Unpaginated: the provider returns the tenant's whole collection.
    /// Besides audit and management surfaces this is the reconciliation
    /// path after a [`DomainError::ServiceAccountAmbiguous`] create —
    /// the caller matches
    /// [`IdpServiceAccountSummary::name`] against the name it
    /// submitted to learn whether the uncertain call landed.
    ///
    /// # Errors
    ///
    /// As [`Self::create`], minus the input caps and the ambiguous
    /// outcome (a listing retains nothing, so an uncertain transport
    /// failure is simply a clean failure to the caller).
    pub async fn list(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<Vec<IdpServiceAccountSummary>, DomainError> {
        // @cpt-begin:cpt-cf-account-management-flow-service-accounts-list:p1:inst-flow-sa-list-authorize
        // @cpt-begin:cpt-cf-account-management-dod-service-accounts-authenticated-tenant-scoped-invocation:p1:inst-dod-sa-authenticated-tenant-scoped-invocation-list
        let scope = self.authorize(ctx, pep::actions::LIST, tenant_id).await?;
        // @cpt-end:cpt-cf-account-management-flow-service-accounts-list:p1:inst-flow-sa-list-authorize
        // @cpt-begin:cpt-cf-account-management-flow-service-accounts-list:p1:inst-flow-sa-list-resolve-tenant
        let tenant_context = self.resolve_active_tenant(&scope, tenant_id).await?;
        // @cpt-end:cpt-cf-account-management-flow-service-accounts-list:p1:inst-flow-sa-list-resolve-tenant
        // @cpt-end:cpt-cf-account-management-dod-service-accounts-authenticated-tenant-scoped-invocation:p1:inst-dod-sa-authenticated-tenant-scoped-invocation-list
        // @cpt-begin:cpt-cf-account-management-flow-service-accounts-list:p1:inst-flow-sa-list-invoke-contract
        // @cpt-begin:cpt-cf-account-management-dod-service-accounts-no-local-storage:p1:inst-dod-sa-no-local-storage-list
        // Live pass-through: no AM-side inventory exists to serve, so a
        // provider outage becomes the failure return below rather than a
        // stale listing.
        let req = IdpListServiceAccountsRequest::new((&tenant_context).into());
        match self.idp.list_service_accounts(ctx, &req).await {
            // @cpt-end:cpt-cf-account-management-flow-service-accounts-list:p1:inst-flow-sa-list-invoke-contract
            Ok(summaries) => Ok(summaries),
            // A listing retains nothing, so provider uncertainty is retry-safe
            // and maps to 503 rather than the mutation-oriented 409
            // reconciliation steer. Keep that exception inside the failure
            // boundary so it still records the category and discarded-detail
            // length without exposing adapter text.
            // @cpt-begin:cpt-cf-account-management-flow-service-accounts-list:p1:inst-flow-sa-list-failure-return
            Err(failure) => Err(failure.into_list_domain_error(tenant_id)),
        }
        // @cpt-end:cpt-cf-account-management-flow-service-accounts-list:p1:inst-flow-sa-list-failure-return
        // @cpt-end:cpt-cf-account-management-dod-service-accounts-no-local-storage:p1:inst-dod-sa-no-local-storage-list
        // The `Ok` arm needs no projection step — the summaries cross
        // unchanged, which is what makes the verbatim `name` a usable
        // correlation key: `inst-flow-sa-list-success-return`.
    }

    // ----------------------------------------------------------------
    // Rotate secret
    // ----------------------------------------------------------------

    /// Rotate the account's secret; the previous one stops working.
    ///
    /// Unlike [`Self::revoke`], an absent account is NOT folded into
    /// success: a rotate that found nothing minted no credential the
    /// caller could use, so it answers `404`.
    ///
    /// # Errors
    ///
    /// As [`Self::create`], plus
    /// [`DomainError::ServiceAccountNotFound`] when
    /// `(tenant_id, client_id)` does not resolve.
    pub async fn rotate_secret(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        client_id: &str,
    ) -> Result<IdpServiceAccountCredentials, DomainError> {
        // @cpt-begin:cpt-cf-account-management-flow-service-accounts-rotate:p1:inst-flow-sa-rotate-authorize
        // @cpt-begin:cpt-cf-account-management-dod-service-accounts-independent-permissions:p1:inst-dod-sa-independent-permissions-rotate
        // A distinct action from `create`: re-keying an existing
        // account is grantable without the power to mint new ones.
        let scope = self
            .authorize(ctx, pep::actions::ROTATE_SECRET, tenant_id)
            .await?;
        // @cpt-end:cpt-cf-account-management-dod-service-accounts-independent-permissions:p1:inst-dod-sa-independent-permissions-rotate
        let actor = ctx.subject_id();
        // @cpt-end:cpt-cf-account-management-flow-service-accounts-rotate:p1:inst-flow-sa-rotate-authorize
        // @cpt-begin:cpt-cf-account-management-flow-service-accounts-rotate:p1:inst-flow-sa-rotate-resolve-tenant
        let tenant_context = self.resolve_active_tenant(&scope, tenant_id).await?;
        // @cpt-end:cpt-cf-account-management-flow-service-accounts-rotate:p1:inst-flow-sa-rotate-resolve-tenant
        // @cpt-begin:cpt-cf-account-management-flow-service-accounts-rotate:p1:inst-flow-sa-rotate-invoke-contract
        let req = IdpRotateServiceAccountSecretRequest::new(
            (&tenant_context).into(),
            client_id.to_owned(),
        );
        match self.idp.rotate_service_account_secret(ctx, &req).await {
            // @cpt-end:cpt-cf-account-management-flow-service-accounts-rotate:p1:inst-flow-sa-rotate-invoke-contract
            Ok(credentials) => {
                // @cpt-begin:cpt-cf-account-management-flow-service-accounts-rotate:p1:inst-flow-sa-rotate-audit
                tracing::info!(
                    target: "am.events",
                    event = "service_account_secret_rotated",
                    tenant_id = %tenant_id,
                    client_id = %credentials.client_id,
                    actor_uuid = %actor,
                    outcome = "ok",
                    "am service account secret rotated"
                );
                // @cpt-end:cpt-cf-account-management-flow-service-accounts-rotate:p1:inst-flow-sa-rotate-audit
                // @cpt-begin:cpt-cf-account-management-flow-service-accounts-rotate:p1:inst-flow-sa-rotate-success-return
                Ok(credentials)
                // @cpt-end:cpt-cf-account-management-flow-service-accounts-rotate:p1:inst-flow-sa-rotate-success-return
            }
            // One arm now carries all three instructions. `NotFound` needs no
            // pre-empt: the shared mapping takes the addressed resource, so it
            // emits the same 404-with-`client_id` the dedicated arm used to
            // hand-build. Absence is still NOT folded into success here
            // (unlike revoke) — a rotate that found nothing minted no
            // credential the caller could use.
            // @cpt-begin:cpt-cf-account-management-flow-service-accounts-rotate:p1:inst-flow-sa-rotate-not-found-return
            // @cpt-begin:cpt-cf-account-management-dod-service-accounts-revoke-idempotency:p1:inst-dod-sa-revoke-idempotency-rotate-not-folded
            // @cpt-begin:cpt-cf-account-management-flow-service-accounts-rotate:p1:inst-flow-sa-rotate-failure-return
            Err(failure) => Err(failure.into_domain_error(tenant_id, client_id)),
            // @cpt-end:cpt-cf-account-management-flow-service-accounts-rotate:p1:inst-flow-sa-rotate-failure-return
            // @cpt-end:cpt-cf-account-management-dod-service-accounts-revoke-idempotency:p1:inst-dod-sa-revoke-idempotency-rotate-not-folded
            // @cpt-end:cpt-cf-account-management-flow-service-accounts-rotate:p1:inst-flow-sa-rotate-not-found-return
        }
    }

    // ----------------------------------------------------------------
    // Revoke
    // ----------------------------------------------------------------

    /// Revoke (delete) the account. Idempotent: an account the
    /// provider reports absent is success, so a repeated `DELETE` still
    /// answers `204`.
    ///
    /// Idempotency lives here rather than in the adapter (which maps
    /// vendor "client does not exist" 1:1 to
    /// [`IdpServiceAccountFailure::NotFound`]) for the same reason as
    /// the tenant side: plugins map vendor errors, AM business logic
    /// decides what each one means.
    ///
    /// # Errors
    ///
    /// As [`Self::create`], except that
    /// [`IdpServiceAccountFailure::NotFound`] is not an error.
    pub async fn revoke(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        client_id: &str,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-account-management-flow-service-accounts-revoke:p1:inst-flow-sa-revoke-authorize
        let scope = self.authorize(ctx, pep::actions::REVOKE, tenant_id).await?;
        let actor = ctx.subject_id();
        // @cpt-end:cpt-cf-account-management-flow-service-accounts-revoke:p1:inst-flow-sa-revoke-authorize
        // @cpt-begin:cpt-cf-account-management-flow-service-accounts-revoke:p1:inst-flow-sa-revoke-resolve-tenant
        let tenant_context = self.resolve_active_tenant(&scope, tenant_id).await?;
        // @cpt-end:cpt-cf-account-management-flow-service-accounts-revoke:p1:inst-flow-sa-revoke-resolve-tenant
        // @cpt-begin:cpt-cf-account-management-flow-service-accounts-revoke:p1:inst-flow-sa-revoke-invoke-contract
        let req =
            IdpRevokeServiceAccountRequest::new((&tenant_context).into(), client_id.to_owned());
        match self.idp.revoke_service_account(ctx, &req).await {
            // @cpt-end:cpt-cf-account-management-flow-service-accounts-revoke:p1:inst-flow-sa-revoke-invoke-contract
            // @cpt-begin:cpt-cf-account-management-flow-service-accounts-revoke:p1:inst-flow-sa-revoke-idempotency-check
            // @cpt-begin:cpt-cf-account-management-algo-service-accounts-revoke-idempotency-guard:p1:inst-algo-sa-revoke-idempotency-removed-return
            // @cpt-begin:cpt-cf-account-management-algo-service-accounts-revoke-idempotency-guard:p1:inst-algo-sa-revoke-idempotency-absent-return
            // @cpt-begin:cpt-cf-account-management-dod-service-accounts-revoke-idempotency:p1:inst-dod-sa-revoke-idempotency-fold
            // Both arms are "the account is gone in this tenant scope
            // after the call", which is the whole contract of a revoke.
            Ok(()) | Err(IdpServiceAccountFailure::NotFound { .. }) => {
                // @cpt-begin:cpt-cf-account-management-flow-service-accounts-revoke:p1:inst-flow-sa-revoke-audit
                tracing::info!(
                    target: "am.events",
                    event = "service_account_revoked",
                    tenant_id = %tenant_id,
                    client_id = %client_id,
                    actor_uuid = %actor,
                    outcome = "ok",
                    "am service account revoked"
                );
                // @cpt-end:cpt-cf-account-management-flow-service-accounts-revoke:p1:inst-flow-sa-revoke-audit
                // @cpt-begin:cpt-cf-account-management-flow-service-accounts-revoke:p1:inst-flow-sa-revoke-success-return
                Ok(())
                // @cpt-end:cpt-cf-account-management-flow-service-accounts-revoke:p1:inst-flow-sa-revoke-success-return
            }
            // @cpt-end:cpt-cf-account-management-dod-service-accounts-revoke-idempotency:p1:inst-dod-sa-revoke-idempotency-fold
            // @cpt-end:cpt-cf-account-management-algo-service-accounts-revoke-idempotency-guard:p1:inst-algo-sa-revoke-idempotency-absent-return
            // @cpt-end:cpt-cf-account-management-algo-service-accounts-revoke-idempotency-guard:p1:inst-algo-sa-revoke-idempotency-removed-return
            // @cpt-end:cpt-cf-account-management-flow-service-accounts-revoke:p1:inst-flow-sa-revoke-idempotency-check
            // @cpt-begin:cpt-cf-account-management-algo-service-accounts-revoke-idempotency-guard:p1:inst-algo-sa-revoke-idempotency-other-return
            // @cpt-begin:cpt-cf-account-management-flow-service-accounts-revoke:p1:inst-flow-sa-revoke-failure-return
            // A clean failure or an ambiguous outcome is NOT
            // absence-equivalent and must not report a successful revoke.
            Err(failure) => Err(failure.into_domain_error(tenant_id, client_id)),
            // @cpt-end:cpt-cf-account-management-flow-service-accounts-revoke:p1:inst-flow-sa-revoke-failure-return
            // @cpt-end:cpt-cf-account-management-algo-service-accounts-revoke-idempotency-guard:p1:inst-algo-sa-revoke-idempotency-other-return
        }
    }

    // ----------------------------------------------------------------
    // Helpers
    // ----------------------------------------------------------------

    /// PEP gate. Delegates to [`crate::domain::authz::authz_scope`] for
    /// the uniform gate shape; the service account keys both
    /// `OWNER_TENANT_ID` and `RESOURCE_ID` on the owning tenant (see
    /// [`pep::SERVICE_ACCOUNT`]).
    ///
    /// Takes no `client_id`: the decision is tenant-granular, and the
    /// account address is checked only by the adapter — see the module
    /// docs, "Authorization is tenant-granular, not object-granular".
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        tenant_id: Uuid,
    ) -> Result<AccessScope, DomainError> {
        crate::domain::authz::authz_scope(
            &self.enforcer,
            ctx,
            &pep::SERVICE_ACCOUNT,
            action,
            tenant_id,
            Some(tenant_id),
            |req| req,
        )
        .await
    }

    /// Tenant existence + `Active` guard, shared verbatim with the user
    /// pass-through so both surfaces cannot drift apart.
    async fn resolve_active_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<TenantContext, DomainError> {
        crate::domain::tenant::resolve::resolve_active_tenant(
            &*self.tenant_repo,
            &*self.types_registry,
            scope,
            tenant_id,
        )
        .await
    }
}

/// Trim and bound the caller-supplied `name`.
///
/// Whitespace-equivalence is AM policy (as it is for `username` on the
/// user surface), so the name is trimmed before the cap is applied and
/// an all-whitespace name is rejected outright: it would otherwise reach
/// the provider as an empty correlation key, and the whole
/// ambiguous-create recovery path depends on that key being meaningful.
fn check_name(name: String) -> Result<String, DomainError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(DomainError::Validation {
            detail: "create service account: name MUST not be empty or all-whitespace".to_owned(),
        });
    }
    if trimmed.chars().count() > MAX_NAME_CHARS {
        return Err(DomainError::Validation {
            detail: format!(
                "create service account: name MUST be {MAX_NAME_CHARS} characters or fewer"
            ),
        });
    }
    if trimmed.len() == name.len() {
        return Ok(name);
    }
    Ok(trimmed.to_owned())
}

/// Bound the requested scope list. Membership in the adapter's allowlist
/// stays a provider decision; only the shape is capped here.
fn check_scopes(scopes: &[String]) -> Result<(), DomainError> {
    if scopes.len() > MAX_SCOPES {
        return Err(DomainError::Validation {
            detail: format!("create service account: at most {MAX_SCOPES} scopes may be requested"),
        });
    }
    if let Some(offending) = scopes.iter().find(|s| s.chars().count() > MAX_SCOPE_CHARS) {
        // The offending value is the caller's own input, so reporting
        // its length (not its content) keeps the message actionable
        // without echoing an unbounded string back into the envelope.
        return Err(DomainError::Validation {
            detail: format!(
                "create service account: each scope MUST be {MAX_SCOPE_CHARS} characters or \
                 fewer (received one of {} characters)",
                offending.chars().count()
            ),
        });
    }
    Ok(())
}
