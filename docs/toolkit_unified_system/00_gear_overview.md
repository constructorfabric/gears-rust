<!-- Updated: 2026-06-04 by Constructor Tech -->

# Gear Overview

**Design references:** [Architecture Manifest](../ARCHITECTURE_MANIFEST.md) · [authorization DESIGN](../arch/authorization/DESIGN.md) · [toolkit-oop DESIGN](../arch/toolkit-oop/DESIGN.md) · [errors DESIGN](../arch/errors/DESIGN.md) · [Gears inventory](../GEARS.md)

## What a Gear is

A Gear is a **vertically-sliced, self-contained capability** of the Constructor Fabric / Rust
(CF/Rust) platform, built on ToolKit (`libs/toolkit*`) and declared with `#[toolkit::gear]`
under `gears/<name>/`. Each Gear:

- **Owns its public API** through an SDK crate (`<name>-sdk`) — transport-agnostic
  traits, models, and errors. Consumers never depend on a Gear's internals.
- **Owns its data** behind the secure ORM layer of `toolkit-db`. Repositories take a
  `DbConn` / `DbTx` and build queries with the secure builders (`.secure().scope_with(&scope)`,
  `secure_insert`, `secure_update_with_scope`), which apply an `AccessScope`.
- **Is discovered at link time** via `inventory` and initialized in dependency
  order by the runtime (see [08_lifecycle_stateful_tasks.md](08_lifecycle_stateful_tasks.md)).
- **Composes with other Gears** through the typed `ClientHub`: an in-process
  implementation, or a REST (default) or gRPC client for out-of-process / remote
  Gears (`#[toolkit::consumes]`, [03](03_clienthub_and_plugins.md)).
- **Can be extended by plugins** — built-in plugins (compiled in) or external
  plugins (separate crates with their own API handler, business logic, and DB).
- **Can extend its API data schema** through GTS schema extensions.

A Gear is intentionally a **DDD-light** structure with clear layers:

| Layer | Path | Responsibility |
|-------|------|----------------|
| **API** | `src/api/rest/` | HTTP DTOs, Axum handlers, `OperationBuilder` route + OpenAPI wiring. Extracts `SecurityContext` and passes it to the domain service, which performs PEP enforcement. |
| **Domain** | `src/domain/` | Business logic, the SDK trait implementation (local client), `#[domain_model]` types, and **PEP enforcement** via `PolicyEnforcer`. |
| **Infra** | `src/infra/` | SeaORM entities, repositories, migrations, adapters — everything "low-level" (DB, IO, external clients). |

See [02_gear_layout_and_sdk_pattern.md](02_gear_layout_and_sdk_pattern.md) for the full canonical layout and the
SDK pattern. The reasons behind the layering are in
[Architecture Manifest § 3.2.1](../ARCHITECTURE_MANIFEST.md#321-ddd-light-layer-isolation).

## Core Architecture

The diagram below shows a typical Gear "X" with every option in play: an SDK for
external Gears, gRPC and REST APIs, a built-in plugin, an external plugin, and a
database. It also shows the shared **ToolKit libraries** (errors, security, GTS
types, HTTP client, cluster primitives, outbox) and the **SDKs for other system
Gears** (authz/authn resolver, tenant resolver, types registry, events, outbound
API gateway, flight control, cred store).

![CF/Rust Gear architecture](../img/gear_architecture.drawio.png)

**Key points from the architecture:**

- **Two inbound transports** — external clients reach a Gear via **REST API
  endpoints** (ToolKit REST handler with OData parser + `OperationBuilder`) or via
  the **gRPC interface** (ToolKit gRPC handler). Both feed into the same `api/`
  layer.
- **`api/` → `domain/` → `infra/`** is the one-directional dependency flow. The
  domain holds the "main Gear X business logic".
- **Plugins** attach to the domain:
  - A **built-in plugin** (e.g. plugin Z) ships its business-logic crate plus its
    own `toolkit-db` access and an optional dedicated DB.
  - An **external plugin** (e.g. plugin Y) is a separate deployable with its own
    REST API handler, business logic, `toolkit-db`, and DB.
- **Infra** uses `toolkit-db` (including OData filtering) to reach the Gear's own DB
  through the secure ORM layer.
- **ToolKit libraries** and **core-gear SDKs** are available to every Gear as
  shared, reusable building blocks.

### Gear categories

**Core gears** are all gears under `gears/system/`, including their plugins, such as
resolvers, gateways and registries. Domain gears provide business capabilities; plugins extend
their owning gears. Directory placement does not decide the startup group; see
[the system capability](08_lifecycle_stateful_tasks.md#the-system-capability).

![Gear categories](../img/gears_categories.drawio.png)

## Typical REST API Request Trajectory

This section maps a typical authenticated REST request onto ToolKit code, using the
`mini-chat` Gear as the golden-path example. The model it follows — who authenticates,
who decides, who enforces — is designed in
[authorization DESIGN § Request Flow](../arch/authorization/DESIGN.md#request-flow) and summarized in
[Architecture Manifest § 7.2](../ARCHITECTURE_MANIFEST.md#72-request-lifecycle); it is not
repeated here.

### Middleware stack (request order)

The API Gateway applies a Tower middleware stack. Request execution order
(outermost → innermost), as documented in `ApiGateway::apply_middleware_stack`
(`gears/system/api-gateway/src/gear.rs`):

```
SetRequestId → PropagateRequestId → Trace → push_req_id_to_extensions
  → Timeout → BodyLimit → CORS → MIME validation → PreAuthThrottling → ErrorMapping
  → Auth → PlatformGate → ScopeEnforcement → PostAuthThrottling → License → Router
```

- **PreAuthThrottling** — IP-keyed throttling zones; runs before authentication.
- **ErrorMapping** — `error_mapping_middleware` (`libs/toolkit/src/api/error_layer.rs`)
  is a passthrough kept so the layer stack stays unchanged. Errors are rendered by
  `IntoResponse for CanonicalError` (RFC 9457 `application/problem+json`);
  `canonical_error_middleware`, registered outside `CatchPanicLayer` and inside HTTP
  metrics (not shown in the list above), fills `instance` / `trace_id` and logs.
- **Auth (AuthN middleware)** — `authn_middleware` (`gears/system/api-gateway/src/middleware/auth.rs`)
  resolves the route's auth requirement (`OperationBuilder::authenticated()`
  registers the route as authenticated), extracts the bearer token, calls the
  AuthN Resolver, and injects `SecurityContext` as a request `Extension`.
- **PlatformGate** — platform-plane admission, always applied after auth and before
  tenant scope checks.
- **ScopeEnforcement** — optional coarse-grained `token_scopes` gate configured with
  `route_policies`; see
  [authorization DESIGN § Gateway Scope Enforcement](../arch/authorization/DESIGN.md#gateway-scope-enforcement-optional).
- **PostAuthThrottling** — identity-keyed throttling zones; reads `SecurityContext`.
- **License** — `require_license_features([...])` on the route is enforced here.

### Sequence diagram

```mermaid
sequenceDiagram
    autonumber
    participant Client
    participant GW as API Gateway<br/>(middleware stack)
    participant AuthN as AuthN Resolver<br/>(+ vendor plugin → IdP)
    participant Handler as Gear Handler<br/>(api/rest)
    participant Svc as Domain Service
    participant PEP as PolicyEnforcer (PEP)
    participant AuthZ as AuthZ Resolver (PDP)<br/>(+ vendor plugin)
    participant Repo as Repository (DbConn / DbTx)
    participant DB as Gear DB

    Client->>GW: HTTP request + Bearer token
    Note over GW: SetRequestId / Trace / Timeout /<br/>BodyLimit / CORS / MIME / PreAuthThrottling

    rect rgb(235, 244, 255)
    Note over GW,AuthN: Authentication (AuthN middleware)
    GW->>GW: resolve route auth requirement
    alt route is .authenticated()
        GW->>AuthN: authenticate(bearer_token)
        AuthN->>AuthN: vendor plugin validates / introspects token (IdP)
        AuthN-->>GW: AuthenticationResult { SecurityContext }
        GW->>GW: inject Extension<SecurityContext>
    else missing token
        GW-->>Client: 401 Unauthorized (MISSING_BEARER)
    end
    end

    GW->>GW: PlatformGate + ScopeEnforcement (token_scopes)<br/>+ PostAuthThrottling + License check
    GW->>Handler: dispatch to Axum handler + SecurityContext

    Handler->>Handler: deserialize DTO / extract Path & OData query
    Handler->>Svc: service.method(&ctx, args)

    rect rgb(240, 255, 240)
    Note over Svc,AuthZ: Authorization (PEP → PDP)
    Svc->>PEP: access_scope(ctx, ResourceType, action, resource_id)
    PEP->>AuthZ: EvaluationRequest (subject, action, resource, context, token_scopes)
    AuthZ->>AuthZ: vendor plugin (PDP) evaluates policies<br/>(+ tenant/RG hierarchy)
    AuthZ-->>PEP: decision + constraints
    alt decision = false
        PEP-->>Svc: EnforcerError::AccessDenied (fail-closed; recommended 403)
    else constraints required but missing
        PEP-->>Svc: EnforcerError::CompileFailed (fail-closed; recommended 500)
    else allowed
        PEP->>PEP: compile constraints → AccessScope
        PEP-->>Svc: AccessScope
    end
    end

    Svc->>Repo: repo.op(&conn, &scope, ...)
    Repo->>Repo: .secure().scope_with(&scope) → SQL WHERE (tenant + constraints)
    Repo->>DB: scoped query / insert / update
    DB-->>Repo: rows (0 rows on point op → 404)
    Repo-->>Svc: domain model(s)
    Svc-->>Handler: domain result (or DomainError)
    Handler-->>GW: ApiResult<T> → JSON / RFC-9457 Problem<br/>(IntoResponse for CanonicalError)
    GW->>GW: canonical_error_middleware (instance, trace_id), Trace span close
    GW-->>Client: HTTP response
```

### Where each step lives in code

| Step | Code |
|------|------|
| Pre-auth middleware (request ID, trace, timeout, body limit, CORS, MIME, IP-keyed throttling) | `ApiGateway::apply_middleware_stack` in `gears/system/api-gateway/src/gear.rs` |
| AuthN: route policy, bearer extraction, `AuthNResolverClient::authenticate`, `SecurityContext` injection | `authn_middleware` in `gears/system/api-gateway/src/middleware/auth.rs`; route policy from `OperationBuilder::authenticated()` ([04](04_rest_operation_builder.md)) |
| Post-auth gates: platform admission, `token_scopes`, identity-keyed throttling, `require_license_features` | same middleware stack; scope gate described in [authorization DESIGN § Gateway Scope Enforcement](../arch/authorization/DESIGN.md#gateway-scope-enforcement-optional) |
| Handler: extract `Extension<SecurityContext>`, services, path, body, `OData` query | `create_chat` / `list_chats` in `gears/mini-chat/mini-chat/src/api/rest/handlers/chats.rs` |
| Domain service + PEP: `PolicyEnforcer::access_scope` / `access_scope_with` | `gears/mini-chat/mini-chat/src/domain/service/chat_service.rs`; `gears/system/authz-resolver/authz-resolver-sdk/src/pep/enforcer.rs` |
| PDP decision and compilation to `AccessScope` | AuthZ Resolver + vendor plugin; outcomes and the recommended HTTP mapping in [06 § EnforcerError](06_authn_authz_secure_orm.md#enforcererror-authz-errors) (each gear maps them itself; mini-chat uses `CompileFailed` → `403`, `EvaluationFailed` → `503`); the rules in [authorization DESIGN § Fail-Closed Rules](../arch/authorization/DESIGN.md#fail-closed-rules) |
| Secure DB access: `.secure().scope_with(&scope)`, `secure_insert`, `secure_update_with_scope` | `get` / `list_page` / `create` / `update` in `gears/mini-chat/mini-chat/src/infra/db/repo/chat_repo.rs`; enforcement points in [authorization DESIGN § Scope Determination](../arch/authorization/DESIGN.md#scope-determination) |
| Response: `ApiResult<T>`, `IntoResponse for CanonicalError`, `canonical_error_middleware` | [05_errors_rfc9457.md](05_errors_rfc9457.md) |

For a point operation the gear maps "0 rows in scope" to `404`, so an unauthorized
caller cannot tell a forbidden row from a missing one.

### Plugins in the trajectory

- **Built-in plugin** — Invoked from the domain layer through its own
  business-logic crate; uses its own `toolkit-db` scope and optional dedicated
  DB (see plugin Z in the architecture diagram).
- **External plugin** — A separate deployable; the host Gear reaches it through
  the plugin's API handler (REST today). The plugin runs its own business logic
  and owns its own DB (plugin Y).

Plugin discovery and scoped clients are covered in
[03_clienthub_and_plugins.md § Plugins](03_clienthub_and_plugins.md#plugins).

## References

- **Gear layout & SDK pattern** — [02_gear_layout_and_sdk_pattern.md](02_gear_layout_and_sdk_pattern.md)
- **AuthN/AuthZ, PolicyEnforcer (PEP), SecureORM** — [06_authn_authz_secure_orm.md](06_authn_authz_secure_orm.md)
- **Authorization design (PDP/PEP, constraints, fail-closed rules)** — [docs/arch/authorization/DESIGN.md](../arch/authorization/DESIGN.md)
- **REST wiring & OperationBuilder** — [04_rest_operation_builder.md](04_rest_operation_builder.md)
- **Errors (RFC-9457)** — [05_errors_rfc9457.md](05_errors_rfc9457.md)
- **OData / pagination / $select / $filter** — [07_odata_pagination_select_filter.md](07_odata_pagination_select_filter.md)
- **Database patterns (connections, transactions, repos)** — [11_database_patterns.md](11_database_patterns.md)
- **Out-of-Process / gRPC / SDK pattern** — [09_oop_grpc_sdk_pattern.md](09_oop_grpc_sdk_pattern.md)
- **Startup groups and the system capability** — [08_lifecycle_stateful_tasks.md](08_lifecycle_stateful_tasks.md)
- **Platform gear inventory & high-level architecture** — [docs/GEARS.md](../GEARS.md), [docs/ARCHITECTURE_MANIFEST.md](../ARCHITECTURE_MANIFEST.md)
