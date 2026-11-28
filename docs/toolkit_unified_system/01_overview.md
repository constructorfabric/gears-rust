# ToolKit Overview

## What ToolKit provides

- **Gears** discovered through `inventory` and initialized in dependency order.
- **Gateway as a gear**: `api-gateway` owns the Axum router and the OpenAPI document.
- **Type-safe REST**: `OperationBuilder` refuses to register a route without auth and license decisions.
- **Server-Sent Events** through `SseBroadcaster`.
- **OpenAPI 3.1** generated with `utoipa`; DTO schemas are registered automatically.
- **Canonical errors**: handlers return `CanonicalError`; its `IntoResponse` renders an RFC 9457 `Problem`, and `canonical_error_middleware` fills `instance` and `trace_id`.
- **Typed ClientHub** for in-process clients, resolved by interface type and optional scope.
- **Plugins** through scoped ClientHub registration and GTS-based discovery ([ToolKit plugins](03_clienthub_and_plugins.md#plugins)).
- **Lifecycle** helpers for long-running tasks and graceful shutdown.

The rules that apply to every gear are in [README.md § Core invariants](README.md#core-invariants-apply-everywhere).

## Golden path

Simplified from `examples/toolkit/users-info`. Each fragment names the guide that explains it; the real GET flow adds a prefetch step (06 § GET — prefetch pattern).

```rust
// Gear registration — 02_gear_layout_and_sdk_pattern.md
#[toolkit::gear(
    name = "users-info",          // kebab-case
    deps = [authz_resolver],      // crate identifiers, not strings
    capabilities = [db, rest],
)]
pub struct UsersInfo { /* ... */ }

// init(): DB provider and clients — 03_clienthub_and_plugins.md
let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);
let authz = ctx.client_hub().get::<dyn AuthZResolverApi>()?;
ctx.client_hub().register::<dyn UsersInfoClientV1>(local_client);

// Domain service: scope from the PDP, data through the repository — 06_authn_authz_secure_orm.md
let conn = self.db.conn()?;
let scope = self
    .policy_enforcer
    .access_scope(ctx, &resources::CITY, actions::GET, Some(id))
    .await?;
let city = self.repo.get(&conn, &scope, id).await?;

// Repository (infra/storage): every query is scoped — 11_database_patterns.md
async fn get<C: DBRunner>(&self, conn: &C, scope: &AccessScope, id: Uuid) -> Result<Option<City>, DomainError> {
    let found = CityEntity::find_by_id(id).secure().scope_with(scope).one(conn).await.map_err(db_err)?;
    Ok(found.map(Into::into))
}

// Errors — 05_errors_rfc9457.md
impl From<DomainError> for CanonicalError { /* ... */ }
async fn get_city(/* ... */) -> ApiResult<Json<CityDto>> { /* ... */ }

// REST wiring — 04_rest_operation_builder.md
router = OperationBuilder::get("/users-info/v1/cities/{id}")
    .operation_id("users_info.get_city")
    .authenticated()
    .require_license_features::<License>([])
    .handler(handlers::get_city)
    .json_response_with_schema::<CityDto>(openapi, StatusCode::OK, "City")
    .standard_errors(openapi)
    .register(router, openapi);
```
