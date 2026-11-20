# AuthZ Resolver SDK

SDK crate for the AuthZ Resolver gear, providing the authorization evaluation API, constraint model, and PEP (Policy Enforcement Point) helpers for Gears.

## Overview

This crate defines the transport-agnostic interface for the AuthZ Resolver gear:

- **`AuthZResolverApi`** — Transport-agnostic `#[toolkit::contract]` for evaluating authorization requests (in-process via `ClientHub`, or out-of-process via the generated directory-resolving REST client).
- **`AuthZResolverPluginClient`** — Async trait for PDP plugin implementations
- **`PolicyEnforcer`** — High-level PEP helper (build request → evaluate → compile to `AccessScope`)
- **`EvaluationRequest` / `EvaluationResponse`** — AuthZEN 1.0-based request/response models
- **Constraint types** — `Constraint`, `Predicate`, `EqPredicate`, `InPredicate`
- **PEP compiler** — `compile_to_access_scope()` converts PDP constraints to SecureORM `AccessScope`

## Usage

### Consumer Test Fixtures

Enable `test-util` in a dev-dependency for `test_support::EmptyScopePdp`.
It permits an empty tenant scope; `EmptyScopePdp { live: true }` adds a live
OR branch for the subject's tenant. Use it to test empty queries, refused gates,
and live alternatives.

### PolicyEnforcer (Recommended)

`PolicyEnforcer` builds the request, calls the resolver, checks the decision, and compiles the returned constraints into an `AccessScope`. Apply that scope through SecureORM.

```rust
use authz_resolver_sdk::AuthZResolverApi;
use authz_resolver_sdk::pep::{AccessRequest, PolicyEnforcer, ResourceType};
use sea_orm::EntityTrait;
use toolkit_db::secure::SecureEntityExt;
use toolkit_security::pep_properties;

// `user::Entity` is the gear's SeaORM entity; `conn` is its `SecureConn`
// (any `DBRunner`, e.g. from `db.conn()`).

// Define resource type with supported constraint properties
const USER: ResourceType = ResourceType::from_static(
    "gts.cf.core.users.user.v1~",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

// Create enforcer once during service init.
// In-process: resolve the client eagerly. Out-of-process consumers (the client
// is wired after `init`) should use `PolicyEnforcer::from_hub(hub)` instead.
let authz = hub.get::<dyn AuthZResolverApi>()?;
let enforcer = PolicyEnforcer::new(authz);

// LIST: opt in to empty scopes and apply the whole scope through SecureORM.
// An empty scope compiles to SQL `FALSE`, so the query returns no rows.
let list_scope = enforcer
    .access_scope_with(
        &ctx, &USER, "list", None,
        &AccessRequest::new().allow_empty_scope(true),
    )
    .await?;
let users = user::Entity::find()
    .secure()
    .scope_with(&list_scope)
    .all(&conn)
    .await?;

// Point operations keep the default and refuse an authorized but empty scope
// with `ScopeMatchesNothing`; a nonempty scope is still applied to the query.
let get_scope = enforcer.access_scope(&ctx, &USER, "get", Some(resource_id)).await?;
let user = user::Entity::find_by_id(resource_id)
    .secure()
    .scope_with(&get_scope)
    .one(&conn)
    .await?;
```

### Advanced: AccessRequest Overrides

For non-default scenarios (cross-tenant, barrier bypass, ABAC properties,
optional constraints, allowing empty scopes):

```rust
use authz_resolver_sdk::pep::AccessRequest;
use authz_resolver_sdk::models::TenantMode;

let scope = enforcer.access_scope_with(
    &ctx, &USER, "create", None,
    &AccessRequest::new()
        .context_tenant_id(target_tenant_id)
        .tenant_mode(TenantMode::RootOnly)
        .resource_property(pep_properties::OWNER_TENANT_ID, target_tenant_id),
).await?;

// new_user is a user::ActiveModel with its tenant set to target_tenant_id.
let created = toolkit_db::secure::secure_insert::<user::Entity>(new_user, &scope, &conn).await?;
```

Validate the proposed row against the returned scope before inserting. For a scoped entity, `secure_insert` checks populated columns and skips filters on unset columns in live branches. Materialize the values used for authorization first.

The example assumes the PDP preserves the requested tenant equality. The SDK forwards the property but does not verify `eq(owner_tenant_id, target_tenant_id)` in every allow branch. A custom PEP enforcing this requirement must check the decoded response before compilation. [Implementation status](../../../../docs/arch/authorization/DESIGN.md#implementation-status)

### Low-Level: Direct Evaluation

Direct `AuthZResolverApi::evaluate` calls return decision data. A custom PEP must perform [response validation](../../../../docs/arch/authorization/DESIGN.md#fail-closed-rules) and enforce the resulting scope on the operation.

```rust
use authz_resolver_sdk::pep::{
    CompileOptions, compile_to_access_scope, compile_to_access_scope_with_options,
};
use authz_resolver_sdk::{AccessDenial, AuthZResolverApi, EnforcerError};
use toolkit_security::PlatformSecurityContext;

// The caller's error type propagates both hub lookup and enforcer errors.
// Lift compile errors with `EnforcerError::from`, which classifies
// `ConstraintCompileError::ScopeMatchesNothing` as an access denial.

// Build a point-read request from the caller's SecurityContext.
let request = enforcer.build_request(&ctx, &USER, "get", Some(resource_id), true);
let supported_properties = USER.supported_properties();

// The resolver receives the subject in the request. For a remote call, the
// transport attaches the calling service's credential to the platform request.
let authz = hub.get::<dyn AuthZResolverApi>()?;
let response = authz
    .evaluate(PlatformSecurityContext::outbound_marker(), request)
    .await
    .map_err(EnforcerError::from)?;

// The low-level compiler does not check the decision: stop on a deny first.
if !response.decision {
    return Err(EnforcerError::AccessDenied(AccessDenial::Pdp {
        reason: response.context.deny_reason,
    })
    .into());
}
```

The following examples assume a request with no resource properties and a response containing only `eq` and `in` predicates. The low-level functions receive no negotiated capability set. A custom PEP must check native predicates against the capabilities sent in the request before compilation. Native group predicates also require the resource descriptor available through `PolicyEnforcer`. For requests with resource properties, verify each requested equality in every decoded branch before compilation.

```rust
// Point operations: the default refuses a scope whose
// every compiled branch is empty with `ConstraintCompileError::ScopeMatchesNothing`.
let scope = compile_to_access_scope(&response, true, supported_properties)
    .map_err(EnforcerError::from)?;
```

```rust
// Queries that apply the whole scope: opt in to receive an all-empty
// (deny-all-equivalent) scope instead of the error.
let scope = compile_to_access_scope_with_options(
    &response,
    true,
    supported_properties,
    CompileOptions::new().allow_empty_scope(true),
)
.map_err(EnforcerError::from)?;
```

## Models

### EvaluationRequest (AuthZEN 1.0)

```rust
pub struct EvaluationRequest {
    pub subject: Subject,                  // Who (id, type, properties)
    pub action: Action,                    // What (name: "list", "get", "create", ...)
    pub resource: Resource,                // On what (type, id, properties)
    pub context: EvaluationRequestContext, // Tenant context, token scopes, capabilities
}
```

### EvaluationResponse

```rust
pub struct EvaluationResponse {
    pub decision: bool,                      // Allow or deny
    pub context: EvaluationResponseContext,   // Constraints or deny reason
}
```

### Constraints

A PDP permit for a resource operation must carry row-level constraints.

```rust
pub struct Constraint {
    pub predicates: Vec<Predicate>,  // ANDed within a constraint
}
// Multiple constraints are ORed

pub enum Predicate {
    Eq(EqPredicate),   // property = value
    In(InPredicate),   // property IN (values)
    InGroup(InGroupPredicate),
    InGroupSubtree(InGroupSubtreePredicate),
    InTenantSubtree(InTenantSubtreePredicate),
}
```

## PEP Compilation Matrix

The table describes the current compiler API for a decoded `decision: true` response. Choose the flag by [operation semantics](../../../../docs/arch/authorization/DESIGN.md#require_constraints-flag), independently of the action name.

| `require_constraints` | constraints | Result |
|---|---|---|
| `false` | absent or empty array | `AccessScope::allow_all()` |
| `false` | nonempty | Compile to `AccessScope` |
| `true` | absent or empty array | Error (fail-closed) |
| `true` | nonempty | Compile to `AccessScope` |

An unsupported property invalidates its branch. If every branch is invalid, the compiler returns an error and the operation must stop. An empty outer `constraints` array differs from a predicate with `values: []`; see [empty-scope handling](../../../../docs/arch/authorization/DESIGN.md#permission-gates-and-empty-scopes).

Empty `In`, `InGroup`, and `InGroupSubtree` sets are valid and compile to SQL `FALSE`; other OR branches may match. Empty `predicates: []` is malformed. Property, capability, and tenant checks still apply. See [Permission gates and empty scopes](#permission-gates-and-empty-scopes) for handling an all-empty scope.

## Error Handling

```rust
use authz_resolver_sdk::{AccessDenial, EnforcerError};

match enforcer.access_scope(&ctx, &USER, "get", Some(id)).await {
    Ok(scope) => { /* use with SecureORM */ },
    Err(EnforcerError::AccessDenied(AccessDenial::Pdp { reason })) => { /* PDP denied access */ },
    Err(EnforcerError::AccessDenied(_)) => { /* PEP-local refusal, e.g. an empty scope: deny */ },
    Err(EnforcerError::EvaluationFailed(e)) => { /* PDP call failed */ },
    Err(EnforcerError::CompileFailed(e)) => { /* constraint compilation failed */ },
    Err(e) => { /* unknown outcome class: internal failure (HTTP 500), stop */ },
}
```

`EnforcerError` and `AccessDenial` are both `#[non_exhaustive]`. Handle the
outcome classes that need distinct treatment (`AccessDenied`,
`EvaluationFailed`, `CompileFailed`), then fall back to an internal failure for
the rest, an unknown class included: stop the operation, report a generic
internal error (HTTP 500) without its details, and never map an unknown class
to an access denial, to PDP unavailability, or to a skipped item.
Every refusal reason stays inside `AccessDenied`: keep an `AccessDenied(_)` arm
that maps any refusal as a denial carrying no PDP reason. Only
`AccessDenial::Pdp` holds the PDP's `deny_reason`.

Recommended HTTP mapping for new or updated handlers (existing consumers may
keep legacy mappings and are not migrated):

```rust
use authz_resolver_sdk::EnforcerError;

fn http_status(err: &EnforcerError) -> u16 {
    match err {
        EnforcerError::AccessDenied(_) => 403,
        EnforcerError::EvaluationFailed(_) if err.is_pdp_unavailable() => 503,
        // Other evaluation failures, compile failures and unknown outcome
        // classes: internal, without details on the wire.
        _ => 500,
    }
}
```

`is_pdp_unavailable()` is true only for an evaluation failure whose canonical
category is `ServiceUnavailable` or `DeadlineExceeded` (including the
enforcer's own evaluation deadline). It names the cause, not whether a retry is
safe: that depends on the operation and its idempotency.

## Implementing a Plugin

Implement `AuthZResolverPluginClient` and register with a GTS instance ID:

```rust
use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverPluginClient, EvaluationRequest, EvaluationResponse, AuthZResolverError,
};

struct MyPdpPlugin { /* ... */ }

#[async_trait]
impl AuthZResolverPluginClient for MyPdpPlugin {
    async fn evaluate(&self, request: EvaluationRequest)
        -> Result<EvaluationResponse, AuthZResolverError> {
        // Evaluate policies, return decision + constraints
    }
}
```

### Permission gates and empty scopes

`access_scope` and default `access_scope_with` refuse a permitted empty scope
with `EnforcerError::AccessDenied(AccessDenial::ScopeMatchesNothing)`; map it as an
access denial.
Enable `AccessRequest::allow_empty_scope(true)` only for queries that apply the
whole scope (preserving empty-input aggregate results), or catalog gates that
return an empty list when `matches_nothing()` holds. The flag is PEP-local and
independent of `require_constraints`; validation is unchanged.

The low-level `compile_to_access_scope` has the same fail-closed default
(`ConstraintCompileError::ScopeMatchesNothing`, which `EnforcerError::from`
maps to `AccessDenial::ScopeMatchesNothing`); opt in with
`compile_to_access_scope_with_options(.., CompileOptions::new().allow_empty_scope(true))`.

Every returned `AccessScope` is normalized:

- `constraints()` lists only branches that may match, in PDP order. A branch
  with an empty `In`/`InGroup`/`InGroupSubtree` set is removed whole, never
  narrowed. Nothing else is detected (no general contradictions); an empty
  `InTenantSubtree.descendant_status` disables the status filter and is not
  empty. Use `constraints()` for enforcement and grant inspection.
- `original_constraints()` replays every *compiled* branch, removed ones
  included, in PDP order — not the raw PDP JSON (branches that failed
  compilation are not there). It allocates nothing; each branch is stored
  once.
- A scope whose every branch was removed is operationally deny-all
  (`is_deny_all()` and `matches_nothing()` are `true`) but keeps its
  originals. Equality is structural, so it is not `==` to
  `AccessScope::deny_all()`.
- Projections (`tenant_only`, `tenant_and_owner`, `ensure_owner`) build a new
  scope from live branches only; the source's originals are not carried over.

The invalid-branch policy is unchanged: a branch that fails validation is
dropped, and the request fails only when no branch compiled. A valid empty
branch is not invalid, so `[valid-empty, invalid]` is decided by
`allow_empty_scope`. Strict rejection of any invalid branch is tracked
separately in [#5958](https://github.com/constructorfabric/gears-rust/issues/5958).
A nonempty scope still requires row-level enforcement.

See [the full contract](../../../../docs/arch/authorization/DESIGN.md#permission-gates-and-empty-scopes)
for branch semantics, projection helpers, INSERT checks, and consumer limitations.

## License

Apache-2.0
