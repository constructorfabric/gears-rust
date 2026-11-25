# AuthZ Resolver

Main gear for authorization in Gears. Discovers AuthZ plugins via GTS types-registry and routes policy evaluation to the selected plugin (PDP).

## Overview

The `cf-gears-authz-resolver` gear provides:

- **Plugin discovery** — Finds AuthZ plugins via GTS types-registry
- **Vendor-based selection** — Selects plugin by vendor and priority
- **Policy evaluation routing** — Delegates AuthZEN-based evaluation requests to the active PDP plugin
- **ClientHub integration** — Registers `AuthZResolverApi` for inter-gear use

This is a **main gear** — it contains no authorization logic itself. All operations are delegated to the active plugin (e.g., `cf-gears-static-authz-plugin` for development, or a custom implementation).

## Architecture

```
Consumer Gear (PEP)
    │
    ▼
PolicyEnforcer  (SDK helper — builds request, compiles response)
    │
    ▼
AuthZResolverApi  (SDK trait, registered in ClientHub)
    │
    ▼
authz-resolver gateway  (this crate — discovers & routes)
    │
    ▼
AuthZResolverPluginClient  (SDK trait, scoped by GTS instance ID)
    │
    ▼
Plugin implementation  (PDP — evaluates policies, returns constraints)
```

## Usage

Services act as Policy Enforcement Points (PEPs) using the `PolicyEnforcer` from the SDK:

```rust
use authz_resolver_sdk::AuthZResolverApi;
use authz_resolver_sdk::pep::{PolicyEnforcer, ResourceType};
use toolkit_security::pep_properties;

const USER: ResourceType = ResourceType::from_static(
    "gts.cf.core.users.user.v1~",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

let authz = hub.get::<dyn AuthZResolverApi>()?;
let enforcer = PolicyEnforcer::new(authz.clone());

// Get access scope for a single-resource operation (`access_scope` refuses an empty scope)
let scope = enforcer.access_scope(&ctx, &USER, "get", Some(user_id)).await?;
// Use scope with SecureORM for row-level filtering
```

## Configuration

The gear is configured via the server's YAML config. Plugin selection is automatic based on GTS registration. The development plugin is compiled into the example server by the `static-authz` feature of `apps/cf-gears-example-server` (this crate has no such feature).

## Writing a Plugin

Implement the `AuthZResolverPluginClient` trait from `cf-gears-authz-resolver-sdk`, publish a `PluginV1<AuthZResolverPluginSpecV1>` instance into the platform types-registry catalogue, and bind the client scoped to that instance's GTS ID. The resolver selects only from the platform catalogue; instances registered through the legacy `TypesRegistryClient` are never consulted.

## Testing

```bash
cargo test -p cf-gears-authz-resolver
```

## License

Apache-2.0
