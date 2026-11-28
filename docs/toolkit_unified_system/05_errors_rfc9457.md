# Errors: CanonicalError and RFC 9457 Problem

Gears report every failure as a `CanonicalError` (crate `libs/toolkit-canonical-errors`). The error system — the 16 categories, their HTTP statuses, GTS type ids, context schemas and wire examples — is designed in [`docs/arch/errors/DESIGN.md`](../arch/errors/DESIGN.md), with one page per category in [`docs/arch/errors/categories/`](../arch/errors/categories/). This page covers only how gear code uses it.

**Design references:** [errors DESIGN](../arch/errors/DESIGN.md) · [errors PRD](../arch/errors/PRD.md) · [ADR 0001 categories](../arch/errors/ADR/0001-cpt-cf-adr-canonical-error-categories.md) · [ADR 0003 RFC 9457 wire format](../arch/errors/ADR/0003-cpt-cf-adr-rfc9457-wire-format.md) · [ADR 0005 SDK canonical projection](../arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md) · [ADR 0006 middleware catch-all](../arch/errors/ADR/0006-cpt-cf-adr-error-middleware-catchall.md)

```
DomainError (domain/error.rs)
     ↓ impl From<DomainError> for CanonicalError   (api/rest/error.rs)
CanonicalError ── handler: ApiResult<T> = Result<T, CanonicalError>
     ↓ IntoResponse
Problem (application/problem+json) ── canonical_error_middleware fills instance / trace_id
```

## Core invariants

- **Rule**: Handlers return `ApiResult<T>` from `toolkit::api::canonical_prelude` (`Result<T, CanonicalError>`). Never build a `Problem` in a handler.
- **Rule**: Each gear has exactly one `impl From<DomainError> for CanonicalError`, in `<gear>/src/api/rest/error.rs`. The REST handlers and the ClientHub local client both go through it.
- **Rule** (new SDKs, per [ADR 0005](../arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md)): SDK traits return `Result<_, CanonicalError>`. An SDK may alias it (`pub use toolkit_canonical_errors::CanonicalError as UsersInfoError;` in `users-info-sdk/src/lib.rs`) and may add an opt-in typed projection with `From<CanonicalError>` (see [ADR 0005](../arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md), e.g. `gears/system/resource-group/resource-group-sdk/src/error.rs`). A new SDK has no parallel `From<DomainError> for SdkError`. Some existing SDKs still define their own error enums (tenant-resolver-sdk `TenantResolverError`, authn-resolver-sdk `AuthNResolverError`, usage-collector-sdk `UsageCollectorError`, policy-engine-sdk `ManagementError`); do not copy that pattern.
- **Rule**: Use `toolkit::api::rest::extract::{Json, Path, Query}` instead of the axum extractors so request rejections render as `Problem` too.
- **Rule**: The string passed to `CanonicalError::internal(..)` (and the macro's `unknown(..)`) is a server-side diagnostic: it is logged by the middleware and never serialized. Every other category's `detail` is sent to the client, so keep internals out of it.

## Resource errors and the mapping

`#[resource_error(...)]` generates a unit struct whose constructors return a typestate builder bound to a GTS resource type. `gts_id!("...")` inside the attribute prepends the configured GTS prefix; it needs no import. From `examples/toolkit/users-info/users-info/src/api/rest/error.rs`:

```rust
use toolkit::api::canonical_prelude::*;

use crate::domain::error::DomainError;

#[resource_error(gts_id!("cf.example1.users.user.v1~"))]
struct UserResourceError;

impl From<DomainError> for CanonicalError {
    fn from(e: DomainError) -> Self {
        match &e {
            DomainError::UserNotFound { id } => {
                UserResourceError::not_found(format!("User with id {id} was not found"))
                    .with_resource(id.to_string())
                    .create()
            }
            DomainError::EmailAlreadyExists { email } => {
                UserResourceError::already_exists(format!("Email '{email}' is already in use"))
                    .with_resource(email.clone())
                    .create()
            }
            DomainError::InvalidEmail { email } => UserResourceError::invalid_argument()
                .with_field_violation("email", format!("Email '{email}' is invalid"), "INVALID_FORMAT")
                .create(),
            DomainError::Forbidden => UserResourceError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create(),
            DomainError::Database { .. } => {
                tracing::error!(error = ?e, "Database error occurred");
                CanonicalError::internal("An internal database error occurred").create()
            }
            // ...
        }
    }
}
```

`create()` only compiles once the category's required data is set:

| Builder | Required before `create()` |
|---------|----------------------------|
| `X::not_found(detail)`, `X::already_exists(detail)`, `X::data_loss(detail)` | `.with_resource(name)` |
| `X::invalid_argument()`, `X::out_of_range(detail)` | `.with_field_violation(field, description, reason)`, `.with_format(..)` or `.with_constraint(..)` |
| `X::failed_precondition()` | `.with_precondition_violation(..)` |
| `X::resource_exhausted(detail)` | `.with_quota_violation(..)` |
| `X::permission_denied()`, `X::aborted(detail)` | `.with_reason(reason)` |
| `CanonicalError::internal(detail)` | nothing (no resource type) |
| `CanonicalError::unauthenticated()` | `.with_reason(reason)` |
| `CanonicalError::service_unavailable()` | nothing; `.with_retry_after_seconds(..)` optional |
| `X::deadline_exceeded(detail)` | nothing; `.with_reason(reason)` optional |

The macro also generates `unknown`, `unimplemented` and `cancelled`. Exact signatures: `libs/toolkit-canonical-errors/src/builder.rs` and `libs/toolkit-canonical-errors-macro/src/lib.rs`. A per-occurrence status override goes through `.with_override(TransportOverride)` (`libs/toolkit-canonical-errors/src/transport.rs`); its rules (same status class, `type` stays authoritative) are in [DESIGN § Transport Overrides](../arch/errors/DESIGN.md#transport-overrides).

## Handlers

```rust
pub async fn get_user(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    extract::Path(id): extract::Path<Uuid>,
    OData(query): OData,
) -> ApiResult<JsonBody<serde_json::Value>> {
    let user_full = svc.users.get_user_full(&ctx, id).await?; // DomainError -> CanonicalError
    Ok(Json(apply_select(&UserFullDto::from(user_full), query.selected_fields())))
}
```

Declare the statuses an operation can return on its `OperationBuilder` (`.error_404(openapi)`, `.standard_errors(openapi)`, ...); see [04_rest_operation_builder.md](04_rest_operation_builder.md#error-responses).

## Wire format

`IntoResponse for CanonicalError` (`libs/toolkit-canonical-errors/src/problem.rs`) responds with `Content-Type: application/problem+json`, the category's status, and `Retry-After` when the context carries a retry hint. The serialized fields are defined in [DESIGN § RFC 9457 Problem Wire Format](../arch/errors/DESIGN.md#rfc-9457-problem-wire-format). Typed contract errors (`#[derive(ContractError)]`) also carry `error_code` and `error_domain` ([contract binding DESIGN § D4](../arch/toolkit-contract-binding/DESIGN.md#d4-contracterror-derive)).

`toolkit::api::canonical_error_middleware` (`libs/toolkit/src/api/canonical_error_layer.rs`) wraps every gear router — the api-gateway layers it in `gears/system/api-gateway/src/gear.rs`, OoP gears in `libs/toolkit/src/runtime/oop_serve.rs` — so a gear does not install it. It fills `instance` and `trace_id` when they are missing, logs 4xx at `warn` and 5xx at `error` (including the unredacted diagnostic of `internal` / `unknown`), and turns error responses with a `text/plain` body or no `Content-Type` into a `Problem`; error responses with any other content type, such as hand-built JSON, pass through unchanged. Catch-all and `ForeignPassthrough` rules: [DESIGN § Error Middleware](../arch/errors/DESIGN.md#error-middleware).

## Testing the mapping

```rust
#[test]
fn user_not_found_maps_to_404() {
    let err = CanonicalError::from(DomainError::user_not_found(Uuid::now_v7()));
    assert_eq!(err.status_code(), 404);
}
```

## Quick checklist

- [ ] `DomainError` in `domain/error.rs` (`thiserror`, `#[domain_model]`), no transport types.
- [ ] One `#[resource_error(gts_id!(..))]` struct per resource type and one `impl From<DomainError> for CanonicalError` in `api/rest/error.rs`.
- [ ] Handlers return `ApiResult<T>` (`toolkit::api::canonical_prelude::*`) and use `?`.
- [ ] Body/path/query extractors from `toolkit::api::rest::extract`.
- [ ] New SDK trait methods return `Result<_, CanonicalError>` (ADR 0005); the local client maps with `CanonicalError::from`.
- [ ] Error statuses declared on each `OperationBuilder` route.
