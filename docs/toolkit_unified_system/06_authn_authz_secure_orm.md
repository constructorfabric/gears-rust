# Security: AuthN, AuthZ & Secure Database Access

This document describes the complete security data-path in Gears: how requests are authenticated (AuthN), how authorization decisions are enforced (AuthZ / PEP), how `AccessScope` is produced, and how the Secure ORM layer (`SecureConn`) applies it to every database query.

For the full architectural design (AuthZEN model, predicate types, caching, deployment modes), see [`docs/arch/authorization/DESIGN.md`](../arch/authorization/DESIGN.md).

## Core invariants

- **Rule**: Authentication is handled by API Gateway middleware — gears receive a ready-to-use `SecurityContext`.
- **Rule**: Use the secure query builders with a `DBRunner` for database access in services and repositories. Gears cannot access raw database connections/pools.
- **Rule**: Derive `Scopable` on SeaORM entities with tenant/resource columns.
- **Rule**: Use `PolicyEnforcer` from `authz-resolver-sdk` to obtain the scope for resource access. Use explicit scopes in repository tests and in the approved prefetch step described below.
- **Rule**: Every sensitive DB access MUST be covered by a PDP decision. Exception: the approved prefetch-first flow for GET/UPDATE/DELETE may read with `AccessScope::allow_all()` before the PDP call, provided the required compensating checks are applied (see [GET prefetch pattern](#get--prefetch-pattern) and [UPDATE/DELETE prefetch + TOCTOU safety](#update--delete--prefetch--toctou-safety)).
- **Rule**: Apply the [PEP validation rules](../arch/authorization/DESIGN.md#fail-closed-rules). No `EnforcerError`, including an unknown outcome class, lets the operation proceed. See [EnforcerError](#enforcererror-authz-errors) for the recommended HTTP mapping; existing consumers may keep legacy mappings.
- **Rule**: Map `EnforcerError` to domain errors. Never expose PDP internals to the client.
- **Rule**: Use `.authenticated()` on `OperationBuilder` for protected endpoints, `.anonymous()` for unauthenticated ones.
- **Rule**: No plain SQL in handlers/services/repos. Raw SQL is allowed only in migration infrastructure. See [`11_database_patterns.md`](./11_database_patterns.md) for migration rules.

## Architecture overview

```text
Request → API Gateway (AuthN middleware) → SecurityContext
              ↓
         Gear Handler (PEP)
              ↓
         PolicyEnforcer → AuthZ Resolver (PDP) → decision + constraints
              ↓
         PEP Compiler → AccessScope
              ↓
         SecureConn (SQL WHERE) → Database
```

Three components work together:

1. **AuthN Resolver** — validates bearer tokens, produces `SecurityContext` (subject identity, tenant, token scopes). Uses the gateway + plugin pattern to delegate to vendor-specific IdPs.
2. **AuthZ Resolver (PDP)** — evaluates policies, returns `decision + constraints`. Uses the gateway + plugin pattern to delegate to vendor-specific authorization services.
3. **Domain gears (PEP)** — call PDP via `PolicyEnforcer`, compile constraints to `AccessScope`, pass to `SecureConn` for SQL-level enforcement.

Gear developers interact primarily with the PEP layer — the AuthN and AuthZ resolvers are infrastructure gears.

## AuthN: how requests get authenticated

### Route policies

When registering REST endpoints, declare whether each route requires authentication using the type-state builder pattern:

```rust
// Protected endpoint — requires valid bearer token + license check
OperationBuilder::get("/users-info/v1/users")
    .authenticated()
    .require_license_features::<License>([])
    .handler(handlers::list_users)
    // ...

// Public endpoint — no token required
OperationBuilder::get("/users-info/v1/health")
    .anonymous()
    .handler(handlers::health)
    // ...
```

The builder enforces at compile time that every route declares its auth posture before `.register()`:
- `.authenticated()` marks the route as protected, then requires `.require_license_features::<L>(features)` (or `.no_license_required()`) before registration.
- `.anonymous()` marks the route as unauthenticated and automatically satisfies the license requirement. Handlers on it receive an anonymous `SecurityContext`, on the gateway and on an out-of-process gear's own listener alike, and a presented bearer is ignored on both: an anonymous operation answers the same whatever bearer is presented. A presented `X-ToolKit-Internal-Token` is still validated there wherever `internal_auth` is configured, and an invalid one gets `401`.
- `.platform_authenticated()` admits a validated `X-ToolKit-Internal-Token` only, for platform-plane routes that gears in other processes call; a bearer alone gets `401` — on the gear's own listener and through the API Gateway alike. The gateway validates the token with its inbound `internal_auth` authenticator and, with none configured, warns at startup and refuses the route. Its `OpenAPI` operation requires the `internalToken` apiKey scheme, and gateway discovery never publishes it, since the edge strips the internal token. It still requires a license decision. A bearer is never validated on these routes: alone it is dropped and the call gets `401` `MISSING_INTERNAL_TOKEN`. Handlers read the caller with `Extension<PlatformSecurityContext>` only, never a tenant `SecurityContext`, and the gateway's `route_policies` scopes do not apply to platform routes. Listeners install the stack with `toolkit_http_middleware::layer_route_auth`. A plane is per route, never per request: an API offered on both planes gets two route sets on distinct paths (for example `/types-registry/v2/…` for tenants and `/types-registry/v2/platform/…` for platform callers). `#[toolkit::rest_contract]` registers every `PlatformSecurityContext` method with `.platform_authenticated()` and refuses `#[anonymous]` on one.

A listener never serves a plane it cannot check. An `OoP` gear that serves `.authenticated()` operations without a tenant authenticator refuses to start. One that serves `.platform_authenticated()` operations without `oop_http.internal_auth` warns at startup, and each of them answers `401`, as on the gateway: a linked gear's platform route that nobody calls does not stop the process. At request time a tenant or undeclared route on a listener without a tenant authenticator answers `401` `TENANT_AUTH_NOT_CONFIGURED` rather than reaching the handler unauthenticated, and a request no route matches answers `404` without running the router's fallback; such a gear warns at startup that routes not declared through `OperationBuilder` are refused. For development, `oop_http.auth_disabled` mirrors the gateway's `auth_disabled`: without a tenant authenticator, non-platform routes run with the default tenant `SecurityContext`, and platform routes still need a validated internal token. An installed tenant authenticator still wins. A worker advertised at a routable address with `auth_disabled` logs an error at startup, since every tenant's request reaching it runs as the default tenant.

A request carries one plane (ADR-0008). On routes covered by the auth stack of the gateway or an `OoP` listener, a request presenting both an `Authorization` header and an `X-ToolKit-Internal-Token` is refused with `401` `MIXED_CREDENTIALS` before either authenticator runs, whatever the route's declaration and whether either credential is valid. A lone credential of the other plane is dropped unvalidated, header and all: the bearer on a `.platform_authenticated()` route, the internal token on an `.authenticated()` one (and, on a listener, on a route without a declaration). So a tenant handler never sees a `PlatformSecurityContext`, and, when tenant authentication is enabled, an `.authenticated()` call carrying only a token gets `401` `MISSING_BEARER`. `.anonymous()` routes keep a lone credential of either plane.

The API Gateway middleware uses these declarations to decide how to handle each request:
- **Protected routes**: extract bearer token → call AuthN Resolver → inject `SecurityContext` into request extensions. Returns 401 if token is invalid.
- **Public routes**: inject `SecurityContext::anonymous()` (zero-UUID subject and tenant, empty scopes).
- **Unregistered routes**: behavior depends on `require_auth_by_default` config flag.

### Extracting SecurityContext in handlers

The API Gateway injects `SecurityContext` as an Axum `Extension`. Extract it with the standard `Extension` extractor:

```rust
use toolkit::api::prelude::*;
use toolkit_security::SecurityContext;

pub async fn list_users(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<Service>>,
    OData(query): OData,
) -> ApiResult<JsonPage<serde_json::Value>> {
    let page = svc.users.list_users_page(&ctx, &query).await?;
    Ok(Json(page))
}
```

### SecurityContext fields

| Field | Required | Description |
|-------|----------|-------------|
| `subject_id` | Yes | Unique subject identifier (from token `sub` claim) |
| `subject_tenant_id` | Yes | Tenant the subject belongs to |
| `subject_type` | No | GTS type identifier (e.g., `gts.cf.core.security.subject_user.v1~`) |
| `token_scopes` | Yes | Capability restrictions (`["*"]` for first-party apps) |
| `bearer_token` | No | Original token wrapped in `SecretString`, available to an in-process PDP. The evaluation request skips this field during serialization. |

## AuthZ: PolicyEnforcer (PEP)

### Wiring up

1. Enable the `rest-client` feature on the gear's `authz-resolver-sdk` dependency and declare the consumed contract. The runtime uses this declaration to wire a remote resolver client.

```rust
#[toolkit::consumes(contract = authz_resolver_sdk::AuthZResolverApi, from = "authz-resolver")]
#[toolkit::gear(
    name = "my-gear",
    capabilities = [db, rest],
)]
pub struct MyGear { /* ... */ }
```

2. Create the enforcer once during `init()` and clone it into the services that use it. Lazy lookup supports out-of-process deployments where the resolver proxy is wired after gear initialization.

```rust
use authz_resolver_sdk::PolicyEnforcer;

let enforcer = PolicyEnforcer::from_hub(ctx.client_hub());
```

For an in-process provider already registered in the hub during initialization, the gear can also resolve the client immediately:

```rust
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};

let authz = ctx.client_hub().get::<dyn AuthZResolverApi>()?;
let enforcer = PolicyEnforcer::new(authz);
```

### Defining resource types

Each resource type declares which properties the PEP can compile from PDP constraints into SQL:

```rust
use authz_resolver_sdk::pep::ResourceType;
use toolkit_security::pep_properties;

pub const USER: ResourceType = ResourceType::from_static(
    "gts.example.core.my_gear.user.v1~",
    &[
        pep_properties::OWNER_TENANT_ID,  // tenant scoping
        pep_properties::RESOURCE_ID,      // resource-level access
    ],
);

pub const DOCUMENT: ResourceType = ResourceType::from_static(
    "gts.example.core.my_gear.document.v1~",
    &[
        pep_properties::OWNER_TENANT_ID,
        pep_properties::RESOURCE_ID,
        pep_properties::OWNER_ID,  // ownership-based access
        "category_id",            // custom domain property
    ],
);
```

Well-known properties from `toolkit_security::pep_properties`:
- `OWNER_TENANT_ID` — tenant that owns the resource
- `RESOURCE_ID` — the resource's primary key
- `OWNER_ID` — the user who owns the resource

A resource that can execute native `InGroup`/`InGroupSubtree` predicates must
opt in explicitly and use its exact canonical GTS type path as the
`ResourceType` name:

```rust
pub const USER: ResourceType = ResourceType::from_static(
    "gts.example.core.my_gear.user.v1~",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
)
.with_native_group_predicates();
```

The PEP uses that same identity to qualify
`resource_group_membership.gts_type_id` through RG's `gts_type.schema_id`
(an identity dictionary: key ↔ GTS type identifier, no definitions);
consumers do not configure a second membership type. Without the per-resource
opt-in, or for a non-canonical GTS policy name, `PolicyEnforcer` suppresses
`GroupMembership` and `GroupHierarchy`, asking the PDP to expand the group scope
to explicit resource-ID `in` predicates; a PDP that cannot expand must deny (the
bundled plugins deny — neither implements expansion today).

The querying database must also contain or project the tables each predicate
executes against before the enforcer is configured with the corresponding
capability: native `in_group` needs `resource_group_membership` and RG's
`gts_type` dictionary; native `in_group_subtree` additionally needs
`resource_group_closure`. Membership types are defined in types-registry; RG
keys any admitted member type in its dictionary on first use, and the
membership rows must be recorded under the resource's canonical GTS type path.

### Defining actions

Define action constants used in PDP requests:

```rust
pub const GET: &str = "get";
pub const LIST: &str = "list";
pub const CREATE: &str = "create";
pub const UPDATE: &str = "update";
pub const DELETE: &str = "delete";
```

### PolicyEnforcer API

`access_scope` requires PDP constraints and refuses empty scopes.
`access_scope_with` accepts per-request options via `AccessRequest`:

```rust
// Point operations and permission gates refuse empty scopes by default.
let scope = enforcer
    .access_scope(&ctx, &resource_type, action, resource_id)
    .await?;
let scope = enforcer
    .access_scope_with(&ctx, &resource_type, action, resource_id, &access_request)
    .await?;

// LIST / search / aggregation that apply the whole scope to their query: an
// authorized but empty scope is returned, so the query runs over an empty input.
let scope = enforcer
    .access_scope_with(
        &ctx, &resource_type, action, None,
        &AccessRequest::new().allow_empty_scope(true),
    )
    .await?;
```

Both methods return `Result<AccessScope, EnforcerError>`. The `AccessScope` is then passed to `SecureConn` methods for SQL-level enforcement. See [Permission gates and empty scopes](#permission-gates-and-empty-scopes) for the exact difference.

### Permission gates and empty scopes

Choose empty-scope handling by how the caller uses the returned scope. The action name alone does not select the mode: `access_scope` always uses `allow_empty_scope(false)`, including for a LIST action.

| Caller | `allow_empty_scope` | Handling an empty scope |
|---|---|---|
| Point operation (GET / CREATE / UPDATE / DELETE) or ordinary action-level permission gate | Keep the default `false` | The enforcer returns `EnforcerError::AccessDenied(AccessDenial::ScopeMatchesNothing)`; no manual `matches_nothing()` check is needed. |
| LIST / search that applies the whole scope to its query | Set `true` | Pass the scope to the query; SQL `FALSE` (or a non-SQL contradiction) produces an empty result. No manual check is needed. |
| Aggregation that applies the whole scope to its query | Set `true` | Run the scoped aggregation over empty input. Preserve backend semantics: an ungrouped `COUNT(*)` returns one row with `0`, not an empty list. |
| Catalog gate for a global model or type catalog, where the scope is not applied to catalog rows | Set `true` | Explicitly check `scope.matches_nothing()` and return an empty list; otherwise apply the catalog's visibility rules. |

`allow_empty_scope(true)` changes only whether a successfully compiled, permitted empty scope is returned. It is PEP-local, never sent to the PDP, and independent of `require_constraints`. PDP denials and constraint validation still apply. Never replace an empty scope with `allow_all()`, caller tenant/IDs, or an outer `constraints: []` in a PDP response (allow-all under `require_constraints(false)`). A normalized scope with empty `constraints()` that is not unconstrained is the correct empty scope.

#### When to check `matches_nothing()`

Use an explicit check when the consumer does not apply the scope directly to its query, as with a catalog gate. Illustrative example for a catalog method returning `Result<Vec<CatalogEntry>, DomainError>`:

```rust
let scope = self.enforcer
    .access_scope_with(
        ctx, &resources::CATALOG, actions::LIST, None,
        &AccessRequest::new().allow_empty_scope(true),
    )
    .await?;

if scope.matches_nothing() {
    return Ok(Vec::new());
}

// Apply this global catalog's visibility rules.
self.catalog.list_visible(ctx).await
```

For paginated catalogs, preserve the endpoint's empty-page format and query validation. For row-scoped queries, pass the whole scope to `SecureConn` instead of adding an early return; this also preserves aggregate semantics.

`matches_nothing()` detects statically known emptiness: deny-all, or a scope whose every OR branch contained an empty `In` / `InGroup` / `InGroupSubtree` set. Such a scope is normalized to no live branches, so `matches_nothing()` and `is_deny_all()` give the same answer. It does not query the database or detect general logical contradictions. A `false` result does not prove that rows exist or that a particular row is authorized.

A standalone decision-only gate may proceed on an unconstrained permit. If the PDP supplies constraints, enforce them or refuse the operation. Row access must enforce the scope even with the default `allow_empty_scope(false)`: a nonempty scope may cover another tenant, owner, or resource.

#### Inspecting scope branches

Every `AccessScope` is normalized, whether compiled or built by hand:

- `constraints()` lists only the branches that may match, in order. A branch with an empty `In` / `InGroup` / `InGroupSubtree` set is removed whole, never narrowed to its other filters; general contradictions are not detected, and an empty `InTenantSubtree.descendant_status` disables the status filter rather than emptying the branch. Use it for enforcement and grant inspection (projections, tenant/ID checks, shape checks). If one OR branch is empty but another may match, the scope is not statically empty.
- `original_constraints()` replays every compiled branch, removed ones included, in the original order (not the raw PDP JSON). Use it only for diagnostics or an explicit structural check; never to enforce access.
- An all-empty scope is operationally deny-all but keeps its originals, and equality is structural: it is not `==` to `AccessScope::deny_all()`. Check `is_unconstrained()` separately: both allow-all and deny-all have no live branches.
- `tenant_only()`, `tenant_and_owner()`, `ensure_owner()` and gear-local projections build a new scope from the live branches only; the source's originals are not carried over.

The PEP compiler validates supported properties and predicate shapes before normalization. Downstream checks (SecureORM property resolution, sibling placement, OData projection) see only live branches, so an all-empty scope lowers to `FALSE` without their entity-specific diagnostics. This is an accepted loss of diagnostics, never a widening of access. The invalid-branch policy is unchanged (an invalid branch is dropped; the request fails only if no branch compiles); strict rejection is tracked in [#5958](https://github.com/constructorfabric/gears-rust/issues/5958).

The low-level `compile_to_access_scope` refuses an all-empty scope by default with `ConstraintCompileError::ScopeMatchesNothing`; opt in with `compile_to_access_scope_with_options(.., CompileOptions::new().allow_empty_scope(true))`.

See [the full contract](../arch/authorization/DESIGN.md#permission-gates-and-empty-scopes) for projection helpers, INSERT checks, and consumer limitations.

### AccessRequest builder

Use `AccessRequest` for per-call overrides. Choose `require_constraints` by [operation semantics](../arch/authorization/DESIGN.md#require_constraints-flag), not by the action name.

```rust
use authz_resolver_sdk::pep::AccessRequest;
use toolkit_security::pep_properties;

let request = AccessRequest::new()
    // Pass resource properties to PDP for narrow constraints
    .resource_property(pep_properties::OWNER_TENANT_ID, tenant_id)
    // Require a scope to enforce on the resource (the builder default)
    .require_constraints(true)
    // Override tenant context
    .context_tenant_id(specific_tenant_id);

// Scoped queries may preserve empty scopes; ordinary permission gates keep false.
// Catalog gates may opt in with an explicit matches_nothing() check (see above).
let list_request = AccessRequest::new().allow_empty_scope(true);
```

### Empty constraints versus empty scopes

With `decision: true`, missing `constraints` and `constraints: []` both mean no row-level restrictions were supplied. With the default `require_constraints(true)`, they cause `EnforcerError::CompileFailed`. Explicitly setting `require_constraints(false)` accepts them and produces an unconstrained scope (`allow_all()`) for a standalone decision-only gate. If constraints are supplied, the SDK compiles them and the caller must enforce them or refuse the operation. `predicates: []` inside a constraint remains malformed.

This differs from a valid scope that matches no rows. `allow_empty_scope(true)` preserves that empty scope; it does not make missing constraints acceptable. Conversely, `require_constraints(false)` does not prevent `ScopeMatchesNothing` when supplied constraints compile to an all-empty scope and `allow_empty_scope` remains false.

`in(id, [])`, `in_group(id, [])`, and `in_group_subtree(id, [])` are valid predicates that compile to SQL `FALSE`, never `IN ()`. Property, capability, and tenant checks still apply. The PDP must verify that the selected groups belong to the authorized tenant. Other OR branches may match. [Group ownership and enforcement](../arch/authorization/DESIGN.md#group-tenant-scoping) See [Permission gates and empty scopes](#permission-gates-and-empty-scopes) for PEP options and non-SQL handling.

### Optional resource conditions

For an owner selector, validate the UUID and add `.resource_property(pep_properties::OWNER_ID, owner_id)` only when supplied. Advertise `owner_id` in the resource descriptor and map it to the entity’s owner column. Keep the caller in `SecurityContext`. [Owner scenarios](../arch/authorization/AUTHZ_USAGE_SCENARIOS.md#subject-owner-based-access)

The SDK forwards the requested `owner_id` without checking the returned predicates, and the bundled static and TR plugins ignore it. When a request specifies an owner, the PEP must reject an allow response before compilation if any branch omits the equality for that owner. [Implementation status](../arch/authorization/DESIGN.md#implementation-status)

## How AuthZ connects to the database: pep_prop

The bridge between PDP authorization constraints and SQL `WHERE` clauses is **property resolution** — mapping abstract PEP property names (like `"owner_tenant_id"`) to concrete database columns (like `Column::TenantId`).

### Scopable entity attributes

Every entity that participates in row-level security derives `Scopable` with a `#[secure(...)]` attribute declaring its security dimensions:

```rust
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "users")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "id",
    no_owner,
    no_type
)]
pub struct Model { /* ... */ }
```

Available dimension attributes:

| Attribute | PEP property auto-mapped | Description |
|-----------|--------------------------|-------------|
| `tenant_col = "col"` | `owner_tenant_id` → `Column::Col` | Tenant ownership column |
| `resource_col = "col"` | `id` → `Column::Col` | Resource identity (usually PK) |
| `owner_col = "col"` | `owner_id` → `Column::Col` | User ownership column |
| `type_col = "col"` / `no_type` | *(no auto-map)* | GTS type column |
| `no_tenant` / `no_resource` / `no_owner` | *(dimension absent)* | Opt out of a dimension |
| `unrestricted` | *(no scoping at all)* | Global tables without scoping columns |

**Rule**: All four dimensions must be declared (either `*_col` or `no_*`), unless `unrestricted` is used.

### Standard property auto-mapping

The `Scopable` derive generates one constant, `ScopableEntity::SCOPE: EntityScope<Column>`, with a field per dimension. `EntityScope::resolve_property()` maps the well-known PEP property names onto those fields:

| Dimension attribute | PEP property name | Constant |
|---------------------|-------------------|----------|
| `tenant_col = "tenant_id"` | `"owner_tenant_id"` | `pep_properties::OWNER_TENANT_ID` |
| `resource_col = "id"` | `"id"` | `pep_properties::RESOURCE_ID` |
| `owner_col = "user_id"` | `"owner_id"` | `pep_properties::OWNER_ID` |

`type_col` has no property name, so no scope can address it. For `tenant_col = "tenant_id", resource_col = "id", no_owner, no_type` the macro generates:

```rust
impl ScopableEntity for Entity {
    const SCOPE: EntityScope<Self::Column> = EntityScope::Scoped(Dimensions {
        tenant: Some(Column::TenantId),
        resource: Some(Column::Id),
        owner: None,
        type_col: None,
        custom: &[],
    });
}
```

and `#[secure(unrestricted)]` generates `EntityScope::Unrestricted`. `Dimensions` has no `Default`, so a hand-written scope must name every dimension too.

When the PDP returns a constraint like `In("owner_tenant_id", [uuid1, uuid2])`, `SecureConn` calls `E::SCOPE.resolve_property("owner_tenant_id")`, gets `Column::TenantId`, and generates `WHERE tenant_id IN (uuid1, uuid2)`.

### Custom properties with `pep_prop`

For domain-specific properties beyond the three standard ones, use `pep_prop(property = "column")`:

```rust
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "addresses")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "id",
    owner_col = "user_id",
    no_type,
    pep_prop(city_id = "city_id")
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub city_id: Uuid,
    pub street: String,
    // ...
}
```

This adds `("city_id", Column::CityId)` to the generated `custom` list, so `resolve_property("city_id")` returns `Column::CityId`.

To use the custom property, include it in both `ResourceType.supported_properties` and the PDP policy:

```rust
pub const ADDRESS: ResourceType = ResourceType::from_static(
    "my_gear.address",
    &[
        pep_properties::OWNER_TENANT_ID,
        pep_properties::RESOURCE_ID,
        pep_properties::OWNER_ID,
        "city_id",  // matches the pep_prop key
    ],
);
```

When the PDP returns constraints referencing `"city_id"`, the PEP compiler includes them in the `AccessScope`, and `SecureConn` resolves them to `Column::CityId` for the SQL `WHERE` clause.

### The full data flow

```text
ResourceType.supported_properties  ← declares what PEP can compile
         ↓
PDP constraint: In("city_id", [uuid]) ← PDP returns constraint on this property
         ↓
PEP compiler: ScopeFilter::In("city_id", [uuid]) → AccessScope
         ↓
SecureConn → build_scope_condition::<E>() → E::SCOPE.resolve_property("city_id")
         ↓
Column::CityId → WHERE city_id IN (uuid)
```

If `resolve_property()` returns `None`, SecureORM drops that OR branch. It produces a deny-all condition if no branch survives.

### pep_prop validation rules

The macro enforces at compile time:
- **Reserved names rejected** — `owner_tenant_id`, `id`, `owner_id` cannot be used in `pep_prop()` (use `tenant_col`, `resource_col`, `owner_col` instead).
- **No duplicates** — each property name may appear only once.
- **Empty names rejected** — property and column names must be non-empty.
- **Forbidden with `unrestricted`** — `pep_prop()` cannot be combined with `unrestricted`.

### Unrestricted entities (`#[secure(unrestricted)]`)

Use `#[secure(unrestricted)]` only for truly global tables where the entity has **no scoping columns**. Notes:

- `secure_insert` skips scope validation entirely for an unrestricted entity, even for a deny-all scope. The caller must authorize insertion before using this path.
- Queries with a scope that contains tenant IDs will be denied (by policy: tenants requested but entity has no `tenant_col`).
- If you need to read/write a global table within a tenant-scoped request, do not use `unrestricted`. Model it with explicit columns and use an appropriate scope shape (often `resources_only`).

## CRUD authorization patterns

The examples show how a service calls `authz-resolver` through `PolicyEnforcer::access_scope_with` and passes the returned `AccessScope` to storage. The enforcer evaluates the request, stops on a denial, and compiles the constraints on a permit.

The examples using `resource.properties` assume a PDP that preserves every requested equality in every allowed branch. The current enforcer does not check this before compilation. [Implementation status](../arch/authorization/DESIGN.md#implementation-status)

### LIST — simple scope

The simplest case: no resource ID, constraints required by default.

```rust
pub async fn list_users_page(
    &self,
    ctx: &SecurityContext,
    query: &ODataQuery,
) -> Result<Page<User>, DomainError> {
    let conn = self.db.conn()?;

    // Preserve empty scopes so an authorized LIST can return an empty page.
    let scope = self.policy_enforcer
        .access_scope_with(
            ctx, &resources::USER, actions::LIST, None,
            &AccessRequest::new().allow_empty_scope(true),
        )
        .await?;

    // SecureConn applies scope as SQL WHERE clause
    let page = self.repo.list_page(&conn, &scope, query).await?;
    Ok(page)
}
```

### GET — prefetch pattern

A PDP may need the target tenant before it can evaluate a point read. For example, the TR plugin requires `resource.properties.owner_tenant_id` whenever `resource.id` is present.

Prefetch by ID to build the evaluation request. Here `requested_tenant_id: Option<Uuid>` is the caller’s parsed tenant condition. Check it against the prefetched row before evaluation.

```rust
pub async fn get_user(
    &self,
    ctx: &SecurityContext,
    id: Uuid,
    requested_tenant_id: Option<Uuid>,
) -> Result<User, DomainError> {
    let conn = self.db.conn()?;
    let prefetched = self.repo
        .get(&conn, &AccessScope::allow_all(), id)
        .await?
        .ok_or_else(|| DomainError::user_not_found(id))?;
    if requested_tenant_id.is_some_and(|tenant| tenant != prefetched.tenant_id) {
        return Err(DomainError::user_not_found(id));
    }

    let conditions = AccessRequest::new()
        .resource_property(pep_properties::OWNER_TENANT_ID, prefetched.tenant_id);
    let scope = self.policy_enforcer
        .access_scope_with(ctx, &resources::USER, actions::GET, Some(id), &conditions)
        .await?;

    self.repo.get(&conn, &scope, id)
        .await?
        .ok_or_else(|| DomainError::user_not_found(id))
}
```

This example keeps the default `require_constraints: true` and returns the scoped read. The current `users-info` GET uses `require_constraints(false)` and can return the prefetched row directly. [Current GET implementation](../../examples/toolkit/users-info/users-info/src/domain/service/users.rs)

### CREATE — resource properties

Here `user: User` is the proposed row, constructed from validated input with its ID, tenant and timestamps set. Pass the same tenant to the PDP and the same row to the repository.

```rust
let conditions = AccessRequest::new()
    .resource_property(pep_properties::OWNER_TENANT_ID, user.tenant_id);
let scope = self.policy_enforcer
    .access_scope_with(ctx, &resources::USER, actions::CREATE, None, &conditions)
    .await?;

let conn = self.db.conn()?;
let created = self.repo.create(&conn, &scope, user).await?;
```

The repository receives `scope: &AccessScope` and sets all columns before calling `secure_insert`, so the helper can check the inserted values against the returned scope. [INSERT validation](#insert-secure_insert--secureconninsert)

```rust
let model = UserAM {
    id: Set(user.id),
    tenant_id: Set(user.tenant_id),
    email: Set(user.email.clone()),
    display_name: Set(user.display_name.clone()),
    created_at: Set(user.created_at),
    updated_at: Set(user.updated_at),
};
secure_insert::<UserEntity>(model, scope, conn).await.map_err(db_err)?;
```

### UPDATE / DELETE — prefetch + TOCTOU safety

After prefetching the current row and checking any explicit caller condition as in GET, request permission for the specific mutation. For a display-name update, `current.tenant_id` is the tenant of the existing user:

```rust
let conditions = AccessRequest::new()
    .resource_property(pep_properties::OWNER_TENANT_ID, current.tenant_id);
let scope = self.policy_enforcer
    .access_scope_with(ctx, &resources::USER, actions::UPDATE, Some(id), &conditions)
    .await?;
```

Pass `&scope` and `id` to the repository. To check the scope at write time, apply both in the UPDATE statement:

```rust
use sea_orm::{EntityTrait, ExprTrait, QueryFilter, sea_query::Expr};
use toolkit_db::secure::SecureUpdateExt;

let result = UserEntity::update_many()
    .filter(Expr::col(Column::Id).eq(id))
    .secure()
    .scope_with(scope)
    .col_expr(Column::DisplayName, Expr::value(display_name))
    .exec(conn)
    .await?;
if result.rows_affected == 0 {
    return Err(DomainError::user_not_found(id));
}
```

For DELETE, evaluate the delete action with the same target ID and current tenant condition:

```rust
let scope = self.policy_enforcer
    .access_scope_with(ctx, &resources::USER, actions::DELETE, Some(id), &conditions)
    .await?;
if !self.repo.delete(&conn, &scope, id).await? {
    return Err(DomainError::user_not_found(id));
}
```

The repository applies the ID and scope in its DELETE statement. A concurrent change that takes the row out of scope then prevents the write. The current `users-info` update delegates to `secure_update_with_scope`, which checks scope on a SELECT and subsequently updates by primary key. That helper does not provide this atomic guarantee.

## SecureConn usage

### Scoped access in a repository

The service passes `db.conn()?` and the scope to the repository. A `DBRunner` parameter accepts a `DbConn`, `SecureConn`, or transaction runner. For updates, `secure_conn.update_many::<E>(scope)` wraps `E::update_many().secure().scope_with(scope)`. [Users repository](../../examples/toolkit/users-info/users-info/src/infra/storage/users_sea_repo.rs)

### Implicit security policy (how `AccessScope` becomes SQL)

| Scope | Entity has column? | Result |
|------|---------------------|--------|
| Empty (`deny_all`) | N/A | deny all (`WHERE false`) |
| Unconstrained (`allow_all`) | N/A | no filtering (`WHERE true`) |
| Tenants only | has `tenant_col` | `tenant_col IN (tenant_ids)` |
| Tenants only | no `tenant_col` | deny all |
| Resources only | has `resource_col` | `resource_col IN (resource_ids)` |
| Resources only | no `resource_col` | deny all |
| Tenants + resources | has both | AND them |
| Tenants + resources | missing either column | deny all |

This is enforced inside `toolkit-db` when you call `.scope_with(&scope)` / `SecureConn::find*` / `SecureConn::update_many` / `SecureConn::delete_many`.

### OR/AND semantics

- Multiple constraints are OR-ed (alternative access paths)
- Filters within a constraint are AND-ed (all must match)
- Unknown properties fail that constraint (fail-closed)
- If all constraints fail resolution → deny-all

### Scoped queries

```rust
use sea_orm::EntityTrait;
use toolkit_db::secure::SecureEntityExt;

// scope: &AccessScope, runner: &impl DBRunner
let users = user::Entity::find()
    .secure()
    .scope_with(scope)
    .all(runner)
    .await?;

let user = user::Entity::find()
    .secure()
    .scope_with(scope)
    .and_id(user_id)?
    .one(runner)
    .await?;
```

To combine a business filter with authorization, apply both to the same query before executing it:

```rust
use sea_orm::{EntityTrait, ExprTrait, QueryFilter, sea_query::Expr};
use toolkit_db::secure::SecureEntityExt;

let count = UserEntity::find()
    .filter(Expr::col(Column::Email).eq(email))
    .secure()
    .scope_with(scope)
    .count(conn)
    .await
    .map_err(db_err)?;
```

### Advanced scoping for joins / related entities

Use these when the base entity cannot be tenant-filtered directly:

- `SecureSelect::and_scope_for::<J>(&scope)` — apply tenant scoping on a joined entity `J`.
- `SecureSelect::scope_via_exists::<J>(&scope)` — apply tenant scoping via an `EXISTS` subquery on `J`.

## Mutations (security rules)

### Insert (`secure_insert` / `SecureConn::insert`)

- For an entity with `tenant_col`, leaving that column `NotSet` returns `ScopeError::Invalid("tenant_id is required")`. On a scoped entity, failure to match any live branch returns `ScopeError::Denied`.
- For constrained, scoped entities, all filters in at least one scope branch must match the inserted values; checking tenant membership alone is insufficient.
- SecureORM rejects an all-empty scope on a scoped entity, including when a column is `NotSet`. Within a live branch, validation skips filters on `NotSet` columns. To satisfy the [resource-condition contract](../arch/authorization/DESIGN.md#scope-determination), materialize and check authorization-relevant values before inserting, or refuse the operation. The in-memory comparison supports `eq` and `in`. With a populated column it cannot satisfy a group or tenant-subtree predicate, which requires a database lookup.

### Update one record (`SecureConn::update_with_ctx`)

- There is no public unscoped update-one API.
- `update_with_ctx(scope, id, am)` first checks the row exists in scope.
- For tenant-scoped entities, `tenant_id` is immutable. Attempts to change it are denied.

### Update many (`SecureConn::update_many`)

- Must be scoped via `scope_with` / `SecureConn::update_many(scope)`.
- Attempts to set the `tenant_id` column are denied at runtime (`Denied("tenant_id is immutable")`).

## Executors, transactions, repository pattern, and migrations

See [`11_database_patterns.md`](./11_database_patterns.md) for `DBRunner`/`SecureTx`, transaction patterns (`in_transaction_mapped`), the repository pattern, and database migrations.

## Error handling

### EnforcerError (AuthZ errors)

`EnforcerError` has three known outcome classes: `AccessDenied(AccessDenial)`, `EvaluationFailed` and `CompileFailed`. It is `#[non_exhaustive]`: handle the classes that need distinct treatment, then fall back to an internal failure for the rest, an unknown class included (HTTP 500, no details on the wire, the operation stops; an unknown class is never a denial, PDP unavailability or a skipped item). `AccessDenial` is `#[non_exhaustive]` and says why access was refused: `AccessDenial::Pdp { reason }` is the PDP's `decision: false` with its optional deny reason, and any other reason is a PEP-local refusal that carries no PDP reason. Keep an `AccessDenied(_)` arm for those.

`AccessDenial::ScopeMatchesNothing` means the PDP returned `decision: true` and constraint compilation succeeded, but the scope is statically known to match no resources and the caller kept `allow_empty_scope(false)`. This is a PEP-local access refusal, distinct from a PDP denial (`decision: false`), and maps to 403 rather than an internal error. Do not catch it and continue the operation without a scope. Queries that should accept an empty scope opt in with `allow_empty_scope(true)` at the call site.

**Recommended mapping** for new or updated handlers. Existing consumers may keep legacy mappings (for example compile failures as 403, or every evaluation failure as 503) and are not migrated by this guidance.

| Outcome | HTTP |
|---|---|
| `AccessDenied(_)` (any refusal reason) | 403 |
| `EvaluationFailed(_)` where `is_pdp_unavailable()` (canonical `ServiceUnavailable` or `DeadlineExceeded`, including the enforcer's own evaluation deadline) | 503 |
| Any other evaluation failure (`Internal` among them), `CompileFailed(_)`, an unknown outcome class | 500, no details on the wire |

`EnforcerError::is_pdp_unavailable()` classifies the cause once in the SDK, so handlers do not inspect `CanonicalError` themselves. It is not `is_retryable`: whether a retry is safe depends on the operation and its idempotency.

A generic example at the canonical error boundary (`MyResourceError` stands for your gear's `#[resource_error]` type):

```rust
use authz_resolver_sdk::EnforcerError;
use toolkit_canonical_errors::CanonicalError;

fn authz_failure(err: &EnforcerError) -> CanonicalError {
    tracing::warn!(error = %err, "authorization failed");
    match err {
        EnforcerError::AccessDenied(_) => MyResourceError::permission_denied()
            .with_reason("ACCESS_DENIED")
            .create(),
        EnforcerError::EvaluationFailed(_) if err.is_pdp_unavailable() => {
            CanonicalError::service_unavailable().create()
        }
        // Other evaluation failures, compile failures and unknown outcome
        // classes: the detail stays in the log, never on the wire.
        _ => CanonicalError::internal("authorization failed").create(),
    }
}
```

Every mapping keeps the fail-closed principle: no outcome other than `Ok(scope)` lets the operation proceed.

### ScopeError (ORM errors)

`ScopeError` is returned by `SecureConn` / `SecureTx` methods when scope violations occur (e.g., inserting into a tenant not in scope, attempting to change `tenant_id`). Map these to appropriate domain errors (typically 403 for denied, 500 for DB errors).

## Development setup

### Static plugins

For development and testing, Gears provide static AuthN and AuthZ plugins:

- **Static AuthN plugin** — accepts all tokens or maps configured tokens to identities. Modes:
  - `accept_all`: any non-empty token maps to a default identity
  - `static_tokens`: specific tokens map to specific identities (subject_id, tenant_id, scopes)

- **Static AuthZ plugin** — uses the requested context root, falling back to the subject tenant. It denies an absent or nil tenant and requests containing group selectors. On a permit it emits an `in` branch for supported `owner_tenant_id`. With `tenant_hierarchy` it also emits subtree branches for supported `owner_tenant_id` and `id`. If none apply, the response has empty constraints. Use a PDP with owner-based policies for endpoints that require an `owner_id` constraint. [Plugin implementation](../../gears/system/authz-resolver/plugins/static-authz-plugin/src/domain/service.rs)

### Feature flags

The server binary uses feature flags to include static plugins:

```bash
# Run with static AuthN + AuthZ plugins (development mode)
cargo run --bin cf-gears-server --features static-authn,static-authz -- --config config.yaml run

# Makefile target includes these features
make example
```

### Config: auth_disabled

For the simplest local development (no auth at all), configure the API Gateway:

```yaml
gears:
  api-gateway:
    auth_disabled: true
```

This injects a default `SecurityContext` for all requests without calling any AuthN resolver.

**Important**: `auth_disabled` only skips AuthN. If your gear depends on `authz-resolver`, the AuthZ call still happens (using the default `SecurityContext` from the gateway). Use static AuthZ plugin to provide predictable authorization responses.

## Testing with SecureConn

For repository tests, construct an explicit scope with `AccessScope::for_tenant(tenant_id)`, `for_tenants(tenant_ids)`, or `for_resources(resource_ids)` and pass it with the test database runner. For service tests, configure the mock PDP and exercise the public operation. The `users-info` tests seed two tenants and assert that a scoped LIST returns only the permitted tenant’s row. [Tenant-scoping tests](../../examples/toolkit/users-info/users-info/src/domain/service/tests_tenant_scoping.rs)

```rust
#[tokio::test]
async fn tenant_scope_only_sees_its_tenant() {
    let db = inmem_db().await;
    let tenant1 = Uuid::new_v4();
    let tenant2 = Uuid::new_v4();

    let user1 = Uuid::new_v4();
    let user2 = Uuid::new_v4();
    let conn = db.conn().unwrap();
    seed_user(&conn, user1, tenant1, "u1@example.com", "U1").await;
    seed_user(&conn, user2, tenant2, "u2@example.com", "U2").await;

    let services = build_services(db.clone(), ServiceConfig::default());
    let ctx_t1 = ctx_allow_tenants(&[tenant1]);

    let page = services
        .users
        .list_users_page(&ctx_t1, &toolkit_odata::ODataQuery::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].tenant_id, tenant1);
}
```

## Quick checklist

### AuthZ wiring
- [ ] Declare `AuthZResolverApi` with `#[toolkit::consumes]` and enable the SDK’s `rest-client` feature.
- [ ] Create `PolicyEnforcer::from_hub(ctx.client_hub())` once in `init()` and clone it into sub-services.
- [ ] Define `ResourceType` constants with `supported_properties` for each resource.
- [ ] Define action constants (`get`, `list`, `create`, `update`, `delete`).
- [ ] Call `policy_enforcer.access_scope()` / `.access_scope_with()` in every point operation and ordinary action-level permission gate, keeping the default `allow_empty_scope(false)`.
- [ ] Use `allow_empty_scope(true)` only for queries that apply the whole scope or catalog gates that return an empty list for an empty scope.
- [ ] For catalog gates that opt in, explicitly check `scope.matches_nothing()` before returning catalog entries.
- [ ] Implement `From<EnforcerError>` for your domain error type; map every `AccessDenied(_)`, including `AccessDenial::ScopeMatchesNothing`, to access denial (403).
- [ ] Use `.authenticated()` + `.require_license_features::<License>([])` on protected `OperationBuilder` routes.
- [ ] Use `.anonymous()` only for truly unauthenticated routes (health checks, OpenAPI spec).

### SecureConn / database
- [ ] Derive `Scopable` on SeaORM entities with `tenant_col` (required).
- [ ] Declare all four dimensions or use `unrestricted`.
- [ ] Add `pep_prop(name = "column")` for custom domain properties.
- [ ] Obtain the connection runner with `db.conn()?` or use the active transaction runner.
- [ ] Pass the `AccessScope` from `PolicyEnforcer` to `SecureConn` methods.
- [ ] Apply `.secure().scope_with(&scope)` to the entity query before executing it with the runner.
- [ ] For atomic single-record changes, apply both the resource ID and the full scope to the UPDATE statement.
- [ ] See [`11_database_patterns.md`](./11_database_patterns.md) for DBRunner, transaction, repository, and migration checklists.

### CRUD patterns
- [ ] Require constraints for resource CRUD. If the PDP needs a prefetched attribute, preserve any explicit caller condition and enforce the result on the read or write.
- [ ] Validate every requested equality in every allow-response branch before compilation.
- [ ] For CREATE, verify the actual insert values satisfy all predicates of at least one branch.
- [ ] In repository tests, build scopes explicitly. Exercise service authorization through the test PDP.

## Related docs

- Full authorization architecture: [`docs/arch/authorization/DESIGN.md`](../arch/authorization/DESIGN.md)
- Usage scenarios: [`docs/arch/authorization/AUTHZ_USAGE_SCENARIOS.md`](../arch/authorization/AUTHZ_USAGE_SCENARIOS.md)
- Database execution patterns (DBRunner, transactions, repos, migrations): [`11_database_patterns.md`](./11_database_patterns.md)
- REST OperationBuilder (`.authenticated()`, `.anonymous()`): [`04_rest_operation_builder.md`](./04_rest_operation_builder.md)
- OData pagination / filtering: [`07_odata_pagination_select_filter.md`](./07_odata_pagination_select_filter.md)
- Canonical example: `examples/toolkit/users-info/users-info/src/`
