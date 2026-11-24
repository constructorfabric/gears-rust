# Gear Layout and SDK Pattern

Reference implementation: `examples/toolkit/users-info/` (`users-info-sdk` + `users-info`).

## Gear Naming Convention

**All gear names MUST use kebab-case** (lowercase with hyphens).

- **Correct**: `file-parser`, `simple-user-settings`, `api-gateway`, `types-registry`
- **Incorrect**: `file_parser` (snake_case), `FileParser` (PascalCase), `fileParser` (camelCase)

This naming convention is **enforced at multiple levels**:
1. **Folder names**: Validated by `make validate-gear-names` (`tools/scripts/validate_gear_names.py`, also run in CI)
2. **Gear attribute**: `#[toolkit::gear(name = "...")]` rejects non-kebab-case names at compile time

Gear names:
- Must contain only lowercase letters (a-z), digits (0-9), and hyphens (-)
- Must start with a lowercase letter
- Must not end with a hyphen
- Must not contain consecutive hyphens or underscores

Crate names under `gears/` follow the gear name:

| Crate | `[package] name` | `[lib] name` |
|-------|------------------|--------------|
| Gear | `cf-gears-<name>` | `<name_snake>` (e.g. `authz_resolver`) |
| SDK | `cf-gears-<name>-sdk` | `<name_snake>_sdk` |

The `[lib]` name is what `deps = [...]` refers to (see [gear.rs](#gear-srcgearrs-registration--capabilities)).

## Canonical layout (DDD-light)

Place each gear under `gears/<name>/`:

```
gears/<name>/
  ├─ <name>-sdk/                     # public API surface for consumers
  │  ├─ Cargo.toml
  │  └─ src/
  │     ├─ lib.rs                    # re-exports; CanonicalError alias
  │     ├─ client.rs                 # ClientHub trait(s)
  │     ├─ models.rs                 # transport-agnostic models (no REST specifics)
  │     ├─ gts.rs                    # GTS resource-type constants
  │     ├─ odata/                    # ODataFilterable query structs (optional)
  │     └─ error.rs                  # optional typed projection of CanonicalError (ADR 0005)
  └─ <name>/                         # gear implementation
     ├─ Cargo.toml
     └─ src/
        ├─ lib.rs                    # re-exports SDK types + gear struct
        ├─ gear.rs                   # gear struct + Gear / DatabaseCapability / RestApiCapability impls
        ├─ config.rs                 # typed config (optional)
        ├─ api/
        │  └─ rest/
        │     ├─ dto.rs              # HTTP DTOs (#[api_dto]) — REST-only types
        │     ├─ error.rs            # From<DomainError> for CanonicalError
        │     ├─ handlers/           # Axum handlers
        │     └─ routes/             # OperationBuilder registration
        ├─ domain/
        │  ├─ error.rs               # DomainError
        │  ├─ service/               # business logic
        │  ├─ repos/                 # repository traits
        │  └─ local_client/          # SDK trait impl over the service
        └─ infra/
           └─ storage/
              ├─ entity/             # SeaORM entities (one file per entity)
              ├─ *_sea_repo.rs       # repository impls
              ├─ mapper.rs           # entity <-> SDK conversions (From impls)
              ├─ odata_mapper.rs     # OData field -> column mapping
              └─ migrations/
                 ├─ mod.rs
                 └─ m20260111_000001_initial.rs
```

### Notes

- Handlers call the domain service directly.
- Gear crates host local client adapters that implement SDK traits; consumers resolve them via `ClientHub`.
- DB access goes through `DBProvider` (`ctx.db_required()`), with every query scoped by an `AccessScope` from `PolicyEnforcer`: `Entity::find().secure().scope_with(&scope)`, `secure_insert::<Entity>(..)`. See [06_authn_authz_secure_orm.md](06_authn_authz_secure_orm.md) and [11_database_patterns.md](11_database_patterns.md). Gears provide migration definitions; the runtime executes them.

## Data Types Naming Matrix

Use the following naming matrix for data types across layers:

| Operation              | DB Layer (SeaORM)<br/>`src/infra/storage/entity/` | Domain Layer (SDK)<br/>`<gear>-sdk/src/models.rs` | API Request (in)<br/>`src/api/rest/dto.rs`      | API Response (out)<br/>`src/api/rest/dto.rs`                                                    |
|------------------------|----------------------------------------------------------|-----------------------------------------------------------|-------------------------------------------------|-------------------------------------------------------------------------------------------------|
| Create                 | ActiveModel                                              | NewUser                                                   | CreateUserRequest                               | UserResponse                                                                                    |
| Read/Get by id         | UserEntity                                               | User                                                      | Path params (id)                                | UserResponse                                                                                    |
| List/Query             | UserEntity (rows)                                        | User (Vec/iterator)                                       | ListUsersQuery (filter+page)                    | UserListResponse or Page\<UserView\>                                                            |
| Update (PUT, full)     | UserEntity (update query)                                | UpdatedUser (optional)                                    | UpdateUserRequest                               | UserResponse                                                                                    |
| Patch (PATCH, partial) | UserPatchEntity (optional)                               | UserPatch                                                 | PatchUserRequest                                | UserResponse                                                                                    |
| Delete                 | (no payload)                                             | DeleteUser (optional command)                             | Path params (id)                                | NoContent (204) or DeleteUserResponse (rare)                                                    |
| Search (text)          | UserSearchEntity (projection)                            | UserSearchHit                                             | SearchUsersQuery                                | SearchUsersResponse (hits + meta)                                                               |
| Projection/View        | UserAggEntity / UserSummaryEntity                        | UserSummary                                               | (n/a)                                           | UserSummaryView                                                                                 |

Notes:
- Keep all transport-agnostic types in the SDK crate. Handlers and DTOs must not leak into the SDK.
- SeaORM entities live in `src/infra/storage/entity/` (one file per entity). Repository traits live in `src/domain/repos/`, their SeaORM implementations in `src/infra/storage/`.
- All REST DTOs live in `src/api/rest/dto.rs`; provide `From` conversions in `dto.rs` or an optional `mapper.rs`.

## SDK Crate (`<gear>-sdk`)

**Purpose**: Transport-agnostic public API for consumers. Consumers depend only on the SDK.

### SDK `src/lib.rs`

From `users-info-sdk/src/lib.rs` (abridged):

```rust
#![forbid(unsafe_code)]

#[cfg(feature = "odata")]
pub mod client;
pub mod gts;
pub mod models;

#[cfg(feature = "odata")]
pub mod odata;

#[cfg(feature = "odata")]
pub use client::UsersInfoClientV1;
pub use gts::{ADDRESS_RESOURCE_TYPE, CITY_RESOURCE_TYPE, USER_RESOURCE_TYPE};
pub use models::{NewUser, UpdateUserRequest, User, UserPatch /* , ... */};
pub use toolkit_canonical_errors::CanonicalError as UsersInfoError;
pub use toolkit_canonical_errors::{self, CanonicalError, Problem};
```

### SDK `src/models.rs` (transport-agnostic)

```rust
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub email: String,
    pub display_name: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUser {
    pub id: Option<Uuid>,
    pub tenant_id: Uuid,
    pub email: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UserPatch {
    pub email: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateUserRequest {
    pub id: Uuid,
    pub patch: UserPatch,
}
```

### SDK errors

For new SDKs, trait methods return `Result<_, CanonicalError>` ([ADR 0005](../arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md)); users-info re-exports it as `UsersInfoError`. Some existing SDKs still define their own error enums in trait signatures (`TenantResolverError` in tenant-resolver-sdk, `AuthNResolverError` in authn-resolver-sdk, `UsageCollectorError` in usage-collector-sdk, `ManagementError` in policy-engine-sdk); do not copy that pattern. The gear crate maps `DomainError` to `CanonicalError` once, in `api/rest/error.rs`. An SDK may additionally ship a typed projection enum in `error.rs` (`From<CanonicalError>`, with an `Other { canonical }` catch-all) for consumers that want flat matching — see [ADR 0005](../arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md) and `gears/system/resource-group/resource-group-sdk/src/error.rs`. Details: [05_errors_rfc9457.md](05_errors_rfc9457.md).

### SDK `src/client.rs` (ClientHub trait)

The first parameter is the caller's `SecurityContext`. Most SDKs take `&SecurityContext`; users-info takes it by value. From `users-info-sdk/src/client.rs` (abridged):

```rust
use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::UsersInfoError;
use crate::models::{NewUser, UpdateUserRequest, User};

#[async_trait]
pub trait UsersInfoClientV1: Send + Sync {
    fn users(&self) -> Box<dyn UsersStreamingClientV1>;

    async fn get_user(&self, ctx: SecurityContext, id: Uuid) -> Result<User, UsersInfoError>;

    async fn create_user(
        &self,
        ctx: SecurityContext,
        new_user: NewUser,
    ) -> Result<User, UsersInfoError>;

    async fn update_user(
        &self,
        ctx: SecurityContext,
        req: UpdateUserRequest,
    ) -> Result<User, UsersInfoError>;

    async fn delete_user(&self, ctx: SecurityContext, id: Uuid) -> Result<(), UsersInfoError>;
}
```

Listing goes through the streaming facade (`users()` returning `UsersStreamingClientV1`) with typed OData queries; see the full trait.

### SDK `src/odata/` (filterable fields)

`#[derive(ODataFilterable)]` goes on a dedicated SDK query struct, not on the REST DTO. The generated `<Struct>FilterField` enum is used by the REST layer (`.with_odata_filter::<UserFilterField>()`) and by the infra OData mapper. From `users-info-sdk/src/odata/users.rs`:

```rust
use time::OffsetDateTime;
use toolkit_odata_macros::ODataFilterable;
use uuid::Uuid;

#[derive(ODataFilterable)]
pub struct UserQuery {
    #[odata(filter(kind = "Uuid"))]
    pub id: Uuid,

    #[odata(filter(kind = "String"))]
    pub email: String,

    #[odata(filter(kind = "DateTimeUtc"))]
    pub created_at: OffsetDateTime,
}

pub use UserQueryFilterField as UserFilterField;
```

Filter kinds, `$orderby`, `$select` and pagination: [07_odata_pagination_select_filter.md](07_odata_pagination_select_filter.md).

## Gear Crate (`<gear>`)

### Gear `src/lib.rs` template

```rust
// === PUBLIC API (from SDK) ===
pub use users_info_sdk::{
    NewUser, UpdateUserRequest, User, UserPatch, UsersInfoClientV1, UsersInfoError,
};

// === MODULE DEFINITION ===
pub mod gear;
pub use gear::UsersInfo;

// === INTERNAL MODULES ===
#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod infra;
```

### Gear `src/gear.rs` (registration + capabilities)

From `examples/toolkit/users-info/users-info/src/gear.rs`:

```rust
#[toolkit::gear(
    name = "users-info",
    deps = [authz_resolver],
    capabilities = [db, rest]
)]
pub struct UsersInfo {
    service: OnceLock<Arc<ConcreteAppServices>>,
    sse: SseBroadcaster<UserEvent>,
}
```

A gear with a background task and a consumed contract (`gears/system/account-management/account-management/src/gear.rs`):

```rust
#[toolkit::consumes(contract = types_registry_sdk::TypesRegistryApi, from = "types-registry")]
#[toolkit::gear(
    name = "account-management",
    deps = [authz_resolver, resource_group, tenant_resolver],
    capabilities = [db, rest, stateful],
    lifecycle(entry = "serve", stop_timeout = "30s", await_ready),
    gts_publication(
        crates = [crate, account_management_sdk],
        entities = Self::configured_entities,
    ),
)]
```

`#[toolkit::gear]` arguments (`libs/toolkit-macros/src/lib.rs`):

| Argument | Value |
|----------|-------|
| `name` | Required. Kebab-case string literal. |
| `deps` | Array of crate identifiers (`[lib]` names, snake_case) of co-located gears that must init first: `deps = [authz_resolver, types_registry]`. Strings are rejected. The runtime name is derived by replacing `_` with `-`. Each crate must be a Cargo dependency and be added to `[workspace.metadata.cargo-shear] ignored` in the workspace `Cargo.toml`. |
| `capabilities` | Any of `db`, `rest`, `rest_host`, `stateful`, `system`, `grpc_hub`, `grpc`. |
| `ctor` | Expression building the instance (`ctor = MyGear::new()`); defaults to `Default::default()`. |
| `client` | Trait path; only a compile-time object-safety check. Register clients yourself in `init()`. |
| `lifecycle(...)` | `entry = "serve"`, `stop_timeout = "30s"` or `stop_timeout_fn = ...`, `await_ready`, `ready_warn_after = "..."`. |
| `gts_publication(...)` | `crates = [...]` and/or `entities = ...` (at least one), optional `publisher = ...` (defaults to `::types_registry_sdk::publication`); see `libs/toolkit-macros/src/gts_publication.rs`. |
| `one_per_installation` | `true`/`false`; read by composition tooling, not the runtime. |

`#[toolkit::consumes(contract = <sdk::Trait>, from = "<gear-name>")]` (`libs/toolkit-contract-macros/src/consumes.rs`) declares a dependency on another gear's contract. At startup the runtime keeps a co-located local impl if one is in the `ClientHub`, otherwise it registers a directory-resolving REST client (`transport = grpc` for gRPC). The consuming crate needs the SDK built with its `rest-client` (or `grpc-client`) feature. It adds no init ordering, and the client is wired after `init()`, so resolve it from the hub at call time (e.g. `PolicyEnforcer::from_hub`), not in `init()`. Use `deps` only for co-located gears whose clients `init()` itself needs.

Register the gear's own client in `init()`: `ctx.client_hub().register::<dyn UsersInfoClientV1>(Arc::new(local));`.

### Domain types and `#[domain_model]` macro

All `struct` and `enum` types in `domain/` that are visible beyond their own module (`pub`, `pub(crate)`, `pub(super)`, `pub(in ...)`) **must** have the `#[domain_model]` attribute from `toolkit_macros`. Strictly module-private types (no `pub` keyword) are exempt — they never cross a layer boundary, and their fields are still checked for infrastructure leakage by the `DE0301`/`DE0308` domain-layer lints.

#### What it does

The `#[domain_model]` proc-macro attribute enforces Domain-Driven Design (DDD) boundaries at compile time:

1. **Validates field types** — scans all fields (including nested generics like `Option<T>`, `Vec<T>`, `Box<dyn Trait<T>>`) and rejects infrastructure types.
2. **Implements `DomainModel` trait** — marks the type as `impl toolkit::domain::DomainModel`, which can be used for downstream type constraints.
3. **Works on structs and enums** — supports named fields, tuple fields, unit variants, and generics.

#### Why it exists

Without compile-time enforcement, infrastructure types (database connections, HTTP extractors, file handles) can leak into the domain layer, coupling business logic to specific frameworks. The macro catches these violations immediately during `cargo check` / `cargo build`, before code reaches CI.

#### Usage

```rust
use toolkit_macros::domain_model;

#[domain_model]
pub struct Service {
    pub(super) repo: Box<dyn UserRepository>,
}

#[domain_model]
pub enum DomainError {
    NotFound { id: Uuid },
    Validation { message: String },
}
```

#### Forbidden types

The macro rejects the following infrastructure types in field positions:

| Category | Forbidden crates / paths | Examples |
|----------|-------------------------|----------|
| Database frameworks | `sqlx`, `sea_orm` | `sqlx::PgPool`, `sea_orm::DatabaseConnection` |
| HTTP / Web frameworks | `http`, `axum`, `hyper` | `http::StatusCode`, `axum::extract::Request` |
| External service clients | `reqwest`, `tonic` | `reqwest::Client`, `tonic::Request` |
| File system | `std::fs`, `tokio::fs` | `std::fs::File`, `tokio::fs::File` |
| DB-specific type names | — | `PgPool`, `MySqlPool`, `SqlitePool`, `DatabaseConnection` (any path) |

Forbidden types are also caught inside generics: `Option<http::StatusCode>`, `Vec<sea_orm::Value>`, `Box<dyn Iterator<Item = http::StatusCode>>` are all rejected.

#### Allowed types

Standard library types, domain types, and SDK types are allowed:

- `String`, `i32`, `bool`, `Uuid`, `OffsetDateTime`
- `Vec<T>`, `Option<T>`, `HashMap<K, V>`, `Arc<T>`, `Box<T>`
- `std::collections::*`, `std::sync::*` (only `std::fs` is forbidden)
- SDK trait objects: `Box<dyn UserRepository>`, `Arc<dyn MyClient>`
- Your own domain types: `domain::Request`, `domain::StatusCode`

#### Compile-time error example

If a forbidden type is used, the compiler produces a clear, actionable error:

```text
error: field 'pool' has type 'sqlx::PgPool' which is forbidden (crate 'sqlx').
       Domain models must be free of infrastructure dependencies like
       database types (sqlx, sea_orm) or HTTP types (http, axum, hyper).
       Move infrastructure types to the infra/ or api/ layers.
```

#### Where it applies

- **Required on**: `struct` and `enum` types in files under `*/domain/` paths, except strictly module-private ones (see above).
- **Not needed on**: SDK models (`<gear>-sdk/src/models.rs`), REST DTOs (`api/rest/dto.rs`), SeaORM entities (`infra/storage/entity/`).

#### CI enforcement

The DE0309 architecture lint (via `cargo gears lint`) runs in CI and **denies** any non-module-private `struct` or `enum` in `domain/` that is missing the `#[domain_model]` attribute. This ensures the macro cannot be accidentally omitted.

### Gear `src/api/rest/dto.rs` (REST DTOs)

`#[toolkit_macros::api_dto(request, response)]` adds the serde/utoipa derives and the `RequestApiDto` / `ResponseApiDto` markers that `OperationBuilder` requires. From `users-info/src/api/rest/dto.rs`:

```rust
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
#[schema(as = UsersInfoUserDto)]
pub struct UserDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub email: String,
    pub display_name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}
```

`#[schema(as = ...)]` is needed only when another gear in the same server already has a schema with the same name.

### Gear `src/infra/storage/entity/user.rs` (SeaORM)

```rust
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "users")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub email: String,
    pub display_name: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}
```

## Local Client Implementation

The local client adapter implements the SDK trait over the domain service.

**Location:** `src/domain/local_client/` (users-info: `client.rs` plus one file per streaming facade).

**Rules:**
- Implements the SDK trait (`users_info_sdk::UsersInfoClientV1`), using SDK types only
- Delegates every call to the domain service, passing `&ctx`
- Maps `DomainError` with `CanonicalError::from` (the SDK alias: `UsersInfoError::from`)

From `users-info/src/domain/local_client/client.rs`:

```rust
#[domain_model]
#[derive(Clone)]
pub struct UsersInfoLocalClient {
    services: Arc<ConcreteAppServices>,
}

#[async_trait]
impl UsersInfoClientV1 for UsersInfoLocalClient {
    async fn get_user(&self, ctx: SecurityContext, id: Uuid) -> Result<User, UsersInfoError> {
        self.services
            .users
            .get_user(&ctx, id)
            .await
            .map_err(UsersInfoError::from)
    }

    // ... other methods follow the same pattern
}
```

## Gear Registration in Gears Server

A gear is linked into `apps/cf-gears-example-server` through an optional dependency, a Cargo feature, and an import:

### 1. `apps/cf-gears-example-server/Cargo.toml`

```toml
[features]
file-parser = ["dep:file_parser"]

[dependencies]
file_parser = { package = "cf-gears-file-parser", path = "../../gears/file-parser/file-parser", optional = true }
```

### 2. `apps/cf-gears-example-server/src/registered_gears.rs`

```rust
#![allow(unused_imports)]

#[cfg(feature = "file-parser")]
use file_parser as _;  // keeps the gear's inventory registration linked
```

Core gears (`api_gateway`, `authn_resolver`, `types_registry`, ...) are imported unconditionally.

**Why this is required:**
- The `inventory` crate discovers gears at link time
- Without importing the gear, it won't be linked into the binary
- This results in missing API endpoints and the gear won't be initialized

After registration, build with the feature enabled and verify at `http://127.0.0.1:8087/cf/docs` in the example setup.
The `/cf` prefix comes from `gears.api-gateway.config.prefix_path` and is configurable.

## Gear Documentation: QUICKSTART.md

Every gear with REST endpoints SHOULD include a `QUICKSTART.md` file with:

1. **Gear description** — brief explanation of what the gear does
2. **Features/capabilities** — bulleted list of key functionality
3. **Link to /docs** — reference to full API documentation
4. **1-2 minimal examples** — basic curl commands showing typical usage

**Template:**

````md
# <Gear Name> - Quickstart

<2-3 sentence description of what the gear does and its purpose.>

**Features:**
- Key capability 1
- Key capability 2

Full API documentation: <http://127.0.0.1:8087/cf/docs>

## Examples

### List Resources

```bash
curl -s http://127.0.0.1:8087/cf/<gear>/v1/resource | python3 -m json.tool
```

For additional endpoints, see <http://127.0.0.1:8087/cf/docs>.
````

**Key principles:**
- Avoid duplication — `/docs` is auto-generated and always current
- Show, don't list — 1-2 working examples, not comprehensive tables
- Describe stable features — capabilities that won't change frequently

## Quick checklist

- [ ] Create `<gear>-sdk` (`cf-gears-<name>-sdk`) with `lib.rs`, `client.rs`, `models.rs`, and `odata/` if the gear lists resources.
- [ ] Create `<gear>` (`cf-gears-<name>`) with `gear.rs`, `api/rest/`, `domain/`, `infra/storage/`.
- [ ] SDK trait methods: `#[async_trait]`, the caller's `SecurityContext` first (most SDKs take `&SecurityContext`; users-info takes it by value), return `Result<_, CanonicalError>` for new SDKs (ADR 0005).
- [ ] `#[toolkit::gear(name = "<kebab-name>", deps = [<crate_ident>, ...], capabilities = [...])]`; other gears' contracts via `#[toolkit::consumes(contract = ..., from = "...")]`.
- [ ] Add `#[domain_model]` on non-module-private `struct`/`enum` types in `domain/` (import `toolkit_macros::domain_model`).
- [ ] `#[derive(ODataFilterable)]` on SDK query structs (`toolkit_odata_macros::ODataFilterable`); `#[toolkit_macros::api_dto(..)]` on REST DTOs.
- [ ] Add `#[derive(Scopable)]` on SeaORM entities (import `toolkit_db_macros::Scopable`).
- [ ] DB access via `DBProvider` with an `AccessScope` from `PolicyEnforcer`; queries through `.secure().scope_with(&scope)` / `secure_insert`.
- [ ] Register client in `init()`: `ctx.client_hub().register::<dyn MyGearApi>(api)`.
- [ ] Export SDK types from gear crate `lib.rs`.
- [ ] Register gear in `apps/cf-gears-example-server/Cargo.toml` (optional dep + feature) and `registered_gears.rs`.
