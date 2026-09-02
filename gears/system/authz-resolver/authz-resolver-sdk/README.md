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

### PolicyEnforcer (Recommended)

The `PolicyEnforcer` encapsulates the full PEP flow — most services should use this:

```rust
use authz_resolver_sdk::pep::{PolicyEnforcer, ResourceType};
use toolkit_security::pep_properties;

// Define resource type with supported constraint properties
const USER: ResourceType = ResourceType {
    name: "gts.cf.core.users.user.v1~",
    supported_properties: &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
};

// Create enforcer once during service init.
// In-process: resolve the client eagerly. Out-of-process consumers (the client
// is wired after `init`) should use `PolicyEnforcer::from_hub(hub)` instead.
let authz = hub.get::<dyn AuthZResolverApi>()?;
let enforcer = PolicyEnforcer::new(authz.clone());

// All CRUD operations return AccessScope for SecureORM
let scope = enforcer.access_scope(&ctx, &USER, "list", None).await?;
let scope = enforcer.access_scope(&ctx, &USER, "get", Some(resource_id)).await?;
let scope = enforcer.access_scope(&ctx, &USER, "create", None).await?;
```

### Advanced: AccessRequest Overrides

For non-default scenarios (cross-tenant, barrier bypass, ABAC properties):

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
```

### Low-Level: Direct Evaluation

For cases where `PolicyEnforcer` is not suitable:

```rust
use authz_resolver_sdk::{AuthZResolverApi, EvaluationRequest};

// `ctx` is the caller's `SecurityContext`; over REST the generated client
// attaches its bearer token so the PDP can authenticate the call.
let authz = hub.get::<dyn AuthZResolverApi>()?;
let response = authz.evaluate(ctx.clone(), request).await?;

if response.decision {
    // Access granted; optionally compile constraints
    let scope = compile_to_access_scope(&response, true, supported_properties)?;
} else {
    // Access denied
    let reason = response.context.deny_reason;
}
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

PDP returns row-level constraints when `decision=true`:

```rust
pub struct Constraint {
    pub predicates: Vec<Predicate>,  // ANDed within a constraint
}
// Multiple constraints are ORed

pub enum Predicate {
    Eq(EqPredicate),   // property = value
    In(InPredicate),   // property IN (values)
}
```

## PEP Compilation Matrix

| `require_constraints` | constraints | Result |
|---|---|---|
| `false` | empty | `AccessScope::allow_all()` |
| `false` | present | Compile to `AccessScope` |
| `true` | empty | Error (fail-closed) |
| `true` | present | Compile to `AccessScope` |

Unknown properties fail that constraint (fail-closed). If ALL constraints fail, access is denied.

## Error Handling

```rust
use authz_resolver_sdk::pep::enforcer::EnforcerError;

match enforcer.access_scope(&ctx, &USER, "get", Some(id)).await {
    Ok(scope) => { /* use with SecureORM */ },
    Err(EnforcerError::Denied { deny_reason }) => { /* PDP denied access */ },
    Err(EnforcerError::EvaluationFailed(e)) => { /* PDP call failed */ },
    Err(EnforcerError::CompileFailed(e)) => { /* constraint compilation failed */ },
}
```

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

## License

Apache-2.0
