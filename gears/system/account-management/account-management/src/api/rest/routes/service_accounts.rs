//! `OperationBuilder` route registration for the
//! `/account-management/v1/tenants/{tenant_id}/service-accounts*`
//! endpoints.

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use crate::api::rest::{dto, handlers};

const API_TAG: &str = "Tenant Service Accounts";

/// Collection path: `GET / POST /tenants/{tenant_id}/service-accounts`.
const COLLECTION_PATH: &str = "/account-management/v1/tenants/{tenant_id}/service-accounts";
/// Item path: `DELETE /tenants/{tenant_id}/service-accounts/{client_id}`.
const ENTRY_PATH: &str = "/account-management/v1/tenants/{tenant_id}/service-accounts/{client_id}";
/// Secret rotation is a named action on the item, not a state edit: it
/// mints a new credential and invalidates the old one, which no PUT /
/// PATCH body shape expresses honestly.
const ROTATE_PATH: &str =
    "/account-management/v1/tenants/{tenant_id}/service-accounts/{client_id}/rotate-secret";

const TENANT_PARAM: &str = "Tenant UUID. The caller must be authorized for this tenant \
     (its own or a descendant via a subtree grant).";
const CLIENT_ID_PARAM: &str = "Provider-assigned service-account client id. Opaque: \
     `svc-<tenant_id>-<name>` is a common provider convention, not a format callers may \
     rely on - correlate a name to its client id through the collection listing instead.";

pub(super) fn register_service_accounts_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    // POST /account-management/v1/tenants/{tenant_id}/service-accounts
    router = OperationBuilder::post(COLLECTION_PATH)
        .operation_id("account_management.create_tenant_service_account")
        .summary("Provision a service account in a tenant")
        .description(
            "Provision a confidential `client_credentials` machine identity owned by the \
             tenant, via the configured IdP plugin. AM persists no local account state -- \
             the IdP is the source of truth. Returns 201 Created with the credentials and a \
             `Location` header addressing the new account; the `client_secret` is returned \
             EXACTLY ONCE (there is no read-back endpoint) and the response carries \
             `Cache-Control: no-store`, so persist it immediately -- recovery from loss is \
             rotate-secret, not a re-read. `name` must be unique among the tenant's live \
             accounts: a name already in use is rejected with 400 and the existing \
             account is never resumed, revealed, or modified. A 409 with \
             `context.reason = AMBIGUOUS_OUTCOME` means the IdP outcome is uncertain and a \
             account MAY have been created: do NOT retry the same request (it would come \
             back as a 400 name collision) -- list the tenant's accounts, match the `name` \
             you submitted, and either rotate that entry's secret or revoke it and provision \
             again.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("tenant_id", TENANT_PARAM)
        .json_request::<dto::ServiceAccountCreateRequestDto>(
            openapi,
            "Service-account name and requested scopes",
        )
        .handler(handlers::create_service_account)
        .json_response_with_schema::<dto::ServiceAccountCredentialsDto>(
            openapi,
            http::StatusCode::CREATED,
            "Service account provisioned - credentials (secret returned once; \
             Cache-Control: no-store)",
        )
        .standard_errors(openapi)
        // 501 / 503 are outside the standard set: `idp_unsupported_operation`
        // and `idp_unavailable` surface from `IdpPluginClient`.
        .problem_response(
            openapi,
            http::StatusCode::NOT_IMPLEMENTED,
            "IdP plugin does not support service accounts",
        )
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "IdP unavailable (transport failure or timeout)",
        )
        .register(router, openapi);

    // GET /account-management/v1/tenants/{tenant_id}/service-accounts
    router = OperationBuilder::get(COLLECTION_PATH)
        .operation_id("account_management.list_tenant_service_accounts")
        .summary("List a tenant's service accounts")
        .description(
            "List the tenant's service accounts via the configured IdP plugin. Never \
             includes secrets. Unpaginated: the IdP returns the tenant's whole collection \
             (provider quotas bound it), because this listing is also the reconciliation \
             path after an ambiguous create -- each entry echoes the caller-supplied `name` \
             verbatim, which is the only contractual bridge from a name you submitted to the \
             provider-assigned `client_id`. AM persists no local account state; every read \
             is a pass-through to the IdP.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("tenant_id", TENANT_PARAM)
        .handler(handlers::list_service_accounts)
        .json_response_with_schema::<dto::ServiceAccountListDto>(
            openapi,
            http::StatusCode::OK,
            "The tenant's service accounts",
        )
        .standard_errors(openapi)
        .problem_response(
            openapi,
            http::StatusCode::NOT_IMPLEMENTED,
            "IdP plugin does not support service accounts",
        )
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "IdP unavailable (transport failure or timeout)",
        )
        .register(router, openapi);

    // POST …/service-accounts/{client_id}/rotate-secret
    router = OperationBuilder::post(ROTATE_PATH)
        .operation_id("account_management.rotate_tenant_service_account_secret")
        .summary("Rotate a service account's secret")
        .description(
            "Regenerate the account's `client_secret`; the previous one stops working \
             immediately. Returns 200 OK with the new credentials, again EXACTLY ONCE and \
             again with `Cache-Control: no-store`. This is the recovery path for a lost \
             secret. It also re-keys an account left behind by an ambiguous create, but \
             only one whose creation got far enough to be fully provisioned: a half-created \
             account cannot be repaired by a rotate, because its originally requested \
             scopes are not recoverable, so the provider rejects it as invalid input -- \
             revoke it and create it again. Unlike DELETE, an absent account is \
             NOT folded into success: a rotate that found nothing minted no credential, so \
             it returns 404. Requires the `rotate_secret` permission, which is grantable \
             independently of `create` -- re-keying an existing account need not imply the \
             power to mint new ones.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("tenant_id", TENANT_PARAM)
        .path_param("client_id", CLIENT_ID_PARAM)
        .handler(handlers::rotate_service_account_secret)
        .json_response_with_schema::<dto::ServiceAccountCredentialsDto>(
            openapi,
            http::StatusCode::OK,
            "Secret rotated - new credentials (secret returned once; \
             Cache-Control: no-store)",
        )
        .standard_errors(openapi)
        .problem_response(
            openapi,
            http::StatusCode::NOT_IMPLEMENTED,
            "IdP plugin does not support service accounts",
        )
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "IdP unavailable (transport failure or timeout)",
        )
        .register(router, openapi);

    // DELETE …/service-accounts/{client_id}
    //
    // NOTE: the item path deliberately registers no `GET`, so the URL
    // `create` returns in its `Location` header answers `DELETE` but
    // 405s a `GET`. This is a bounded seam rather than an oversight:
    //
    //  * the plugin contract exposes no by-id read — adding one means
    //    changing `IdpPluginClient` and every adapter;
    //  * RFC 9110 §10.2.2 has `Location` *identify* the created
    //    resource; it does not promise the URL is `GET`-able, and
    //    rotate / revoke both address the item without reading it;
    //  * nothing is write-only: the collection `GET` above enumerates
    //    the tenant's accounts in full.
    router = OperationBuilder::delete(ENTRY_PATH)
        .operation_id("account_management.revoke_tenant_service_account")
        .summary("Revoke a service account")
        .description(
            "Delete the service account via the configured IdP plugin; any credentials it \
             holds stop working. Idempotent: returns 204 No Content whether the IdP removed \
             the client or reported it already absent, so a retried DELETE is safe. Provider \
             implementations MUST NOT silently no-op a genuinely supported mutating \
             operation -- unsupported revocation surfaces as \
             `code=idp_unsupported_operation`. Accounts are also removed when their owning \
             tenant is deprovisioned.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("tenant_id", TENANT_PARAM)
        .path_param("client_id", CLIENT_ID_PARAM)
        .handler(handlers::revoke_service_account)
        .no_content_response(
            http::StatusCode::NO_CONTENT,
            "Service account revoked (or already absent)",
        )
        .standard_errors(openapi)
        .problem_response(
            openapi,
            http::StatusCode::NOT_IMPLEMENTED,
            "IdP plugin does not support service accounts",
        )
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "IdP unavailable (transport failure or timeout)",
        )
        .register(router, openapi);

    router
}
