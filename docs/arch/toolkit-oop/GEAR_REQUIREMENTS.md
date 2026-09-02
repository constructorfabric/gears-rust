# Requirements for a Gear to Run Out-of-Process (OoP)

This checklist captures every condition a gear must satisfy before it can be
deployed as its own Kubernetes pod in the Profile-3 OoP architecture.

The order is **practical importance**: architecture-level blockers first,
verification last. Some items apply only to specific gear types (authenticated,
DB-backed, contract providers).

- [1. No hard-linked dependencies](#1-no-hard-linked-dependencies)
- [2. The gear must have an OoP binary](#2-the-gear-must-have-an-oop-binary)
- [3. The gear must serve its own REST surface](#3-the-gear-must-serve-its-own-rest-surface)
- [4. The gear must participate in platform-plane auth](#4-the-gear-must-participate-in-platform-plane-auth)
- [5. Authenticated gears need an embedded tenant-plane authn stack](#5-authenticated-gears-need-an-embedded-tenant-plane-authn-stack)
- [6. Contract providers must be discoverable cross-pod](#6-contract-providers-must-be-discoverable-cross-pod)
- [7. There must be a standalone Helm chart](#7-there-must-be-a-standalone-helm-chart)
- [8. Database-backed gears need isolated storage](#8-database-backed-gears-need-isolated-storage)

---

## 1. No hard-linked dependencies

This is the single most common blocker. Any form of hard coupling forces gears
into the same process.

What to eliminate:

- `#[toolkit::gear(deps = [...])]` at the gear-macro level.
- Direct `Cargo.toml` dependencies on another gear's implementation crate.
- Direct SQL reads/writes against another gear's tables.

What to do instead:

- Declare remote dependencies with `#[toolkit::consumes(contract = ..., from = "...")]`.
- Resolve the dependency lazily from `ClientHub`
  (`PolicyEnforcer::from_hub`, a resolving REST client, etc.).
- Let the provider expose a `#[toolkit::contract]` / `#[toolkit::rest_contract]`
  surface and advertise it in the DirectoryService.
- Route all cross-gear data access through the SDK contract.

**Example:**

```rust
// src/gear.rs — declare the remote dependency with a standalone attribute on
// the gear struct.
#[toolkit::consumes(contract = some_gear_sdk::SomeApi, from = "some-gear")]
pub struct MyGear { ... }

// Resolve it lazily from the ClientHub (works whether the provider is local or
// in another pod). The macro registers a resolving client for the trait.
let client = ctx.client_hub().get::<dyn some_gear_sdk::SomeApi>()?;
```

Until this is true, the gear literally cannot run in a separate process.

## 2. The gear must have an OoP binary

The gear crate needs a feature-gated `[[bin]]` target that enables the bootstrap
runtime.

- `Cargo.toml`:
  - `oop_module = ["dep:tokio", "toolkit/bootstrap"]`.
  - `k8s-auth = ["toolkit/k8s-auth"]` for the platform plane.
  - `[[bin]]` with `required-features = ["oop_module"]`.
- `src/main.rs`: call `toolkit::bootstrap::oop::run_oop_with_options(...)`.
- `src/registered_gears.rs`: link the gear crate with `use <crate> as _;`.
- Container image must be built with `oop_module` and `k8s-auth` enabled. The
  generic `deploy/docker/oop-gear.Dockerfile` takes these as `GEAR_FEATURES`;
  use `BUILD_PROFILE=release` for production.

**Example:**

```toml
# Cargo.toml
[features]
oop_module = ["dep:tokio", "toolkit/bootstrap"]
k8s-auth = ["toolkit/k8s-auth"]

[[bin]]
name = "my-gear-oop"
required-features = ["oop_module"]
```

```rust
// src/main.rs — the essential part is building OopRunOptions (from your CLI /
// clap args) and handing off to the bootstrap runtime:
use toolkit::bootstrap::oop::{OopRunOptions, run_oop_with_options};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts = OopRunOptions {
        gear_name: "my-gear".to_owned(),
        config_path,
        verbose,
        version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        ..Default::default()
    };
    run_oop_with_options(opts).await
}
```

## 3. The gear must serve its own REST surface

In host mode the gear's REST routes are mounted onto the shared api-gateway.
In OoP mode the gear runs its own HTTP server on `oop_http.listen_addr` and
advertises `oop_http.advertise_uri` to the edge.

- Register routes on the local router from `register_rest()`.
- Any edge-reachable route must call `.exposed()`. Visibility is independent of
  auth: `.authenticated()` routes and `.anonymous()` routes both need
  `.exposed()` to be reachable through the edge.

**Example:**

```rust
// src/api/rest/routes.rs
router = OperationBuilder::get("/my-gear/v1/things")
    ...
    .authenticated()
    .exposed()
    .require_license_features::<License>([])
    .handler(handlers::list_things)
    ...
```

```yaml
# config/oop-my-gear.yaml
oop_http:
  listen_addr: "0.0.0.0:9090"
  advertise_uri: "http://my-gear:9090"
```

## 4. The gear must participate in platform-plane auth

Every DirectoryService caller and receiver must authenticate gRPC traffic with
`X-ToolKit-Internal-Token`. Without this, the platform-host rejects
registration.

- Enable `k8s-auth` in the gear crate and in the platform-host (`toolkit/k8s-auth`).
- Configure `oop_http.internal_auth.provider = kube` with the projected SA-token
  path in the gear's Helm values.
- Configure the platform-host `grpc-hub` to enforce internal auth
  (`provider: kube`, `internal_auth_enforcement: required`,
  `audiences: [toolkit-internal]`).
- Project a ServiceAccount token with audience `toolkit-internal`.
- Do not use `PlatformSecurityContext` / `PeerAuthenticated` / `PlatformIdentity`
  for tenant policy; they are workload-only identities.

**Context:** `PlatformSecurityContext` is the *platform-plane* identity, carried
by `X-ToolKit-Internal-Token` (the projected SA token in Profile 3). Use it for
DirectoryService registration, GTS registration, and heartbeats.

**Example:**

```yaml
# deploy/helm/my-gear/values.yaml
oop_http:
  internal_auth:
    provider: kube
    token_path: /var/run/secrets/tokens/toolkit-internal/token
```

```yaml
# deploy/helm/platform-host/values.yaml
grpc-hub:
  internal_auth:
    provider: kube
    audiences: [toolkit-internal]
  internal_auth_enforcement: required
```

## 5. Authenticated gears need an embedded tenant-plane authn stack

The OoP pod has no gateway security-context middleware; it must reconstruct
`SecurityContext` from the bearer token itself.

- Add `authn-resolver` + your production authn plugin (e.g. `oidc-authn-plugin`,
  or `static-authn-plugin` only for non-production acceptance testing) +
  `types-registry` as optional deps behind `oop_module`.
- Wire them in `registered_gears.rs` so the OoP binary links them.
- `types-registry` is required because authn-resolver discovers its plugins
  (static, OIDC, etc.) through the GTS registry. In host mode that registry is
  shared; in OoP mode the gear must embed a local one so the configured authn
  plugin can register itself and authn-resolver can resolve it.
- **Future state:** once `types-registry` runs OoP as its own pod, the tenant
  authn stack will consume it remotely instead of embedding it. Until then,
  every authenticated OoP gear must carry its own copy.
- Middleware re-validates the JWT via `AuthNResolverClient` and builds a
  `SecurityContext` for every tenant-scoped route.
- Generated REST clients forward the original bearer token automatically, so
  tenant context propagates across gear-to-gear calls.
- System/background jobs acting inside a tenant must obtain a real S2S
  client-credentials JWT (`exchange_client_credentials()`), not a synthetic or
  anonymous `SecurityContext`.
- Anonymous gears skip this requirement.

**Context:** `SecurityContext` is the *tenant-plane* identity, carried by
`Authorization: Bearer <jwt>`. Use it for user requests, gear-to-gear tenant
calls, and S2S tenant-scoped jobs.

- `SecurityContext` is never used for platform-level calls (DirectoryService,
  GTS registration, heartbeats); those use `PlatformSecurityContext` from §4.
- `x-secctx-bin` is not used over HTTP in OoP.

**Example:**

```toml
# Cargo.toml
[features]
oop_module = [
  "dep:tokio",
  "toolkit/bootstrap",
  "dep:authn-resolver",
  "dep:oidc-authn-plugin",      # or static-authn-plugin for testing
  "dep:types-registry",
]
k8s-auth = ["toolkit/k8s-auth"]
```

```rust
// src/registered_gears.rs
use authn_resolver as _;
use oidc_authn_plugin as _;   // or static_authn_plugin for testing
use types_registry as _;
```

## 6. Contract providers must be discoverable cross-pod

Applies only to gears that expose a contract consumed by other gears.

- Declare the contract with `#[toolkit::rest_contract]` and include OpenAPI
  metadata.
- Register `rest_endpoint` + `openapi_spec` in the DirectoryService. OoP
  bootstrap does this automatically once the contract is wired.

**Example:**

```rust
// SDK crate that defines the contract.
#[toolkit::rest_contract(base_path = "/my-gear/v1")]
pub trait MyApiRest: MyApi {
    #[post("/do-thing")]
    async fn do_thing(
        &self,
        ctx: SecurityContext,
        req: DoThingRequest,
    ) -> Result<DoThingResponse, CanonicalError>;
}
```

```rust
// Consumer gear declares the dependency with the standalone `consumes`
// attribute.
#[toolkit::consumes(contract = my_gear_sdk::MyApi, from = "my-gear")]
pub struct ConsumerGear { ... }
```

## 7. There must be a standalone Helm chart

The gear needs a deployable unit.

- Base the chart on `deploy/helm/toolkit-common` for Deployment/Service/ConfigMap
  /SA projection.
- Set `directoryEndpoint` to the platform-host's grpc-hub Service DNS.
- Configure `oop_http` with the gear's listen port and `advertise_uri`.
- Add the chart as a dependency of the umbrella `toolkit-platform` chart.

**Example:**

```text
deploy/helm/my-gear/
├── Chart.yaml            # depends on toolkit-common
├── values.yaml           # image, oop_http, directoryEndpoint, postgres block
└── templates/            # uses toolkit-common templates
```

```yaml
# umbrella values (production example)
my-gear:
  enabled: true
  fullnameOverride: my-gear
  image:
    repository: ghcr.io/constructorfabric/my-gear
    tag: "1.0.0"
    pullPolicy: IfNotPresent
  directoryEndpoint: "http://platform-host:50051"
  postgres:
    enabled: true
    host: shared-postgres
    database: mygear
    user: platform
    # Pull the password from a Kubernetes secret in production.
    password: "${POSTGRES_PASSWORD}"
```

## 8. Database-backed gears need isolated storage

Applies only to gears that persist data.

- In Kubernetes, use the shared Postgres with its own database
  (`postgres.databases` in the umbrella values).
- Locally, use a separate SQLite file per gear.
- The gear crate must be able to run its migrations against that isolated
  database.
- Never read or write another gear's tables directly.

**Example:**

```yaml
# umbrella values
postgres:
  databases:
    - mygear
    - othergear
```

```yaml
# local development config (do not use SQLite in production)
database:
  uri: "sqlite:///app/data/my-gear.db?mode=rwc"
```

## Notes for special cases

- **Plugins** (`static-authn-plugin`, `static-authz-plugin`, etc.) are always
  embedded in their parent gear's process and inherit that gear's OoP status.
  They are not independently assessed.
- **Anonymous gears** only need requirements 1–4 and 6–8; they skip the
  tenant-plane authn stack (requirement 5).
