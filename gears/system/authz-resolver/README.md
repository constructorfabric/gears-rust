<!-- Updated: 2026-04-07 by Constructor Tech -->

# AuthZ Resolver

Authorization resolution for Gears — evaluates access requests and produces SQL-compilable constraints.

## Overview

The authorization design that Gears operate on is described in [DESIGN.md](../../../docs/arch/authorization/DESIGN.md).
The decision to use AuthZEN 1.0 with constraint extensions is documented in
[ADR-0001](../../../docs/arch/authorization/ADR/0001-pdp-pep-authorization-model.md) and
the split from authentication in [ADR-0002](../../../docs/arch/authorization/ADR/0002-split-authn-authz-resolvers.md).

The **authz_resolver** gear is an integration point for authorization policy evaluation. It discovers and delegates to a vendor-specific plugin via the types-registry — the plugin acts as the **Policy Decision Point (PDP)**, evaluating Subject + Action + Resource requests and returning a decision with optional row-level constraints.

## Public API

The gear registers [`AuthZResolverApi`](authz-resolver-sdk/src/api.rs) in ClientHub:

- `evaluate(ctx, request)` — Evaluate an authorization request and return a decision with constraints. `ctx` is a `PlatformSecurityContext` plane marker that carries no identity and is ignored by the resolver; the PDP trusts the subject in `request.subject`

### Evaluation Model (AuthZEN-based)

**Request:**

```rust
EvaluationRequest {
    subject: Subject { id, subject_type, properties },
    action: Action { name },               // "list", "get", "create", "update", "delete"
    resource: Resource { resource_type, id, properties },
    context: EvaluationRequestContext {
        tenant_context,                    // Tenant hierarchy context
        token_scopes,                      // OAuth scopes from SecurityContext
        require_constraints,               // true → PDP must return constraints
        capabilities,                      // [TenantHierarchy, GroupMembership, ...]
        supported_properties,              // Properties the PEP can compile
        bearer_token,                      // Optional for PDP validation
    },
}
```

**Response:**

```rust
EvaluationResponse {
    decision: bool,
    context: EvaluationResponseContext {
        constraints: Vec<Constraint>,      // Row-level constraints (OR'd)
        deny_reason: Option<DenyReason>,
    },
}
```

### Constraints

Constraints are SQL-compilable row-level predicates returned by the PDP:

- **`Constraint`** — A set of predicates (AND'd together)
- **Multiple constraints** — OR'd to form the final scope
- **Predicates:** `Eq(property, value)`, `In(property, values)`, `InGroup`, `InGroupSubtree` and `InTenantSubtree`

See [`constraints.rs`](authz-resolver-sdk/src/constraints.rs) for types.

### Errors

`AuthZResolverApi::evaluate` returns `CanonicalError`. The plugin-side [`AuthZResolverError`](authz-resolver-sdk/src/error.rs) has `NoPluginAvailable`, `ServiceUnavailable` and `Internal`; there is no deny variant (a deny is `decision: false`)

## Policy Enforcement Point (PEP)

The SDK provides [`PolicyEnforcer`](authz-resolver-sdk/src/pep/enforcer.rs) — a high-level API that encapsulates the full PEP flow: build request, evaluate via PDP, compile constraints to `AccessScope`.

```rust
use authz_resolver_sdk::pep::{AccessRequest, PolicyEnforcer, ResourceType};
use sea_orm::EntityTrait;
use toolkit_db::secure::SecureEntityExt;
use toolkit_security::pep_properties;

const USER: ResourceType = ResourceType::from_static(
    "gts.cf.core.users.user.v1~",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

let enforcer = PolicyEnforcer::new(authz_client.clone());

// LIST — an authorized-but-empty scope must yield an empty page, not 403
let scope = enforcer
    .access_scope_with(&ctx, &USER, "list", None, &AccessRequest::new().allow_empty_scope(true))
    .await?;
let users = User::find().secure().scope_with(&scope).all(&conn).await?;

// CREATE — require constraints for tenant scoping
let scope = enforcer.access_scope(&ctx, &USER, "create", None).await?;
secure_conn.insert::<User>(&scope, user).await?;
```

### PEP Compiler

The [compiler](authz-resolver-sdk/src/pep/compiler.rs) converts PDP constraints into `AccessScope` for SecureORM with fail-closed guarantees:

| `decision` | `constraints` | `require_constraints` | Result |
|------------|---------------|----------------------|--------|
| `false` | any | any | `Err(EnforcerError::AccessDenied(AccessDenial::Pdp { .. }))` |
| `true` | empty | `false` | `Ok(allow_all)` |
| `true` | empty | `true` | `Err(EnforcerError::CompileFailed(ConstraintCompileError::ConstraintsRequiredButAbsent))` |
| `true` | present | any | Compile to `AccessScope` |

Unknown properties cause the containing constraint to fail. If all constraints fail, the request fails closed with `EnforcerError::CompileFailed` (`AllConstraintsFailed` or `UnadvertisedCapabilities`), not with `AccessDenied`.

## Plugin API

Plugins implement [`AuthZResolverPluginClient`](authz-resolver-sdk/src/plugin_api.rs) and register via GTS.

Gears include two plugins out of the box:
- [`static_authz_plugin`](plugins/static-authz-plugin/) — Tenant-scoped plugin for development and testing (denies access when no valid tenant is resolved)
- [`tr_authz_plugin`](plugins/tr-authz-plugin/) — Resolves the tenant hierarchy via `TenantResolverClient`

## Plugin catalogue

The resolver `#[toolkit::consumes]` the registry's `PlatformTypesRegistryApi`; neither it nor
its plugins depend on the registry gear crate or declare it in `deps`.

- First evaluation lists all platform catalogue pages, keeps locally bound scoped
  `AuthZResolverPluginClient`s and selects by vendor/lowest priority. Failures are
  uncached and yield PEP `EvaluationFailed`; legacy is never consulted.
- Ordinary `static-authz-plugin` and `tr-authz-plugin` gears publish
  `PluginV1<AuthZResolverPluginSpecV1>` through the `gts_publication(entities = …)`
  gear attribute; the runtime admits it before `init`, and `init` only binds.
  Refused, pending or cancelled publication fails boot without a client.
- Publication goes to the `types-registry` linked into the same process (its
  system-gear init runs first) or, when none is linked, to a remote registry. An
  in-process registry that cannot provide the contract (no
  `gears.types-registry.database`) fails boot.

## Configuration

### AuthZ Resolver Gear

See [`config.rs`](authz-resolver/src/config.rs)

```yaml
gears:
  authz-resolver:
    config:
      vendor: "constructorfabric"  # Selects plugin by matching vendor
```

### Static AuthZ Plugin

See [`config.rs`](plugins/static-authz-plugin/src/config.rs)

```yaml
gears:
  static-authz-plugin:
    config:
      vendor: "constructorfabric"
      priority: 100
```

## Usage

Most gears should use `PolicyEnforcer` (see PEP section above) rather than calling `evaluate` directly:

```rust
// Direct evaluation (low-level)
let authz = hub.get::<dyn AuthZResolverApi>()?;

// `EvaluationRequest`, `Subject`, `Resource`, `Action` and `EvaluationRequestContext`
// do not implement `Default` (only the optional `TenantContext` does): set every field.
let response = authz.evaluate(PlatformSecurityContext::outbound_marker(), EvaluationRequest {
    subject: Subject { id: subject_id, subject_type: None, properties: HashMap::new() },
    action: Action { name: "list".into() },
    resource: Resource { resource_type: "my_gear.entity".into(), id: None, properties: HashMap::new() },
    context: EvaluationRequestContext {
        tenant_context: None,
        token_scopes: Vec::new(),
        require_constraints: true,
        capabilities: Vec::new(),
        supported_properties: Vec::new(),
        bearer_token: None,
    },
}).await?;
```

## Implementation Phases

### Phase 1: Core (Current)

- AuthZEN-based `evaluate` API
- `PolicyEnforcer` with `access_scope` for one-step PEP flow
- PEP compiler with fail-closed constraint compilation
- Plugin discovery over the types-registry platform catalogue (see [Plugin catalogue](#plugin-catalogue))
- Static dev plugin with tenant scoping (denies on nil/missing tenant)
- ClientHub registration for in-process consumption

### Phase 2: Advanced Predicates and Production PDP Plugin (In Progress)

- Advanced predicates `in_tenant_subtree`, `in_group`, `in_group_subtree` already exist in the SDK; `static-authz-plugin` emits `in_tenant_subtree` and `tr-authz-plugin` emits `in_group` / `in_group_subtree`.
- Native `in_group` requires `ResourceType::with_native_group_predicates()`, an exact canonical GTS resource name, and local `resource_group_membership` and `gts_type` (RG's identity dictionary) tables; the same resource identity qualifies membership rows. Native `in_group_subtree` additionally requires `resource_group_closure` and advertises both `GroupMembership` and `GroupHierarchy`.
- A service MUST omit a group capability when any required projection is unavailable. Capability omission asks a capable PDP to return expanded resource-ID `in` predicates; degradation is not automatic in the PEP, so a PDP unable to expand must deny.
- Local projection tables for hierarchy-aware constraints (`tenant_closure`, `resource_group_closure`; `resource_group_membership` projection not recommended but not forbidden)
