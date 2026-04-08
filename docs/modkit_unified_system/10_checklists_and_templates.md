# Quick Checklists and Templates

This section provides minimal checklists and code templates for common ModKit tasks.

## Adding a New Module

### Checklist

- [ ] Create `<module>-sdk` crate with `api.rs`, `models.rs`, `errors.rs`, `lib.rs`
- [ ] Create `<module>` crate with `module.rs`, `api/rest/`, `domain/`, `infra/storage/`
- [ ] Implement SDK trait with `async_trait` and `SecurityContext` first param
- [ ] Add `#[domain_model]` on all `struct`/`enum` in `domain/` (import `modkit_macros::domain_model`)
- [ ] Add `#[derive(ODataFilterable)]` on REST DTOs (import `modkit_odata_macros::ODataFilterable`)
- [ ] Add `#[derive(Scopable)]` on SeaORM entities (import `modkit_db_macros::Scopable`)
- [ ] Use `SecureConn` + `SecurityContext` for all DB operations
- [ ] Register client in `init()`: `ctx.client_hub().register::<dyn MyModuleApi>(api)`
- [ ] Export SDK types from module crate `lib.rs`
- [ ] Add module to `Cargo.toml` workspace and `main.rs` type_name check

### Module `src/lib.rs` template

```rust
//! <YourModule> Module Implementation
//!
//! The public API is defined in `<your-module>-sdk` and re-exported here.

// === PUBLIC API (from SDK) ===
pub use <your_module>_sdk::{
    YourModuleClient, YourModuleError,
    User, NewUser, UserPatch, UpdateUserRequest,
};

// === MODULE DEFINITION ===
pub mod module;
pub use module::YourModule;

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

### Module registration template

```rust
#[modkit::module(
    name = "my_module",
    deps = ["foo", "bar"],
    capabilities = [db, rest, stateful],
    client = my_module_sdk::MyModuleApi,
    ctor = MyModule::new(),
    lifecycle(entry = "serve", stop_timeout = "30s", await_ready)
)]
pub struct MyModule {
    /* fields */
}
```

> The `client = ...` attribute validates the trait at compile time and exposes MODULE_NAME, but does not auto-register the client into ClientHub. You must still register it explicitly in your `init()` method using `ctx.client_hub().register::<dyn my_module_sdk::MyModuleApi>(client)`. 

## DB Access and Secure ORM

### Checklist

- [ ] Derive `Scopable` on SeaORM entities with `tenant_col` (required)
- [ ] Use `db.sea_secure()` for all DB access in handlers/services
- [ ] Pass `SecurityContext` to repository methods
- [ ] Use `secure_conn.find::<Entity>(&scope).all(&secure_conn)` for auto-scoped queries
- [ ] Use raw SQL only in `migrations/*.rs` (enforced later via dylint)
- [ ] Add indexes on security columns (tenant_id, resource_id)
- [ ] Test with `SecurityContext::test_tenant()` for unit tests

### Entity template

```rust
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "users")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub email: String,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

### Repository method template

```rust
pub async fn find_by_id(
    &self,
    ctx: &SecurityContext,
    id: Uuid,
) -> Result<Option<user::Model>, DomainError> {
    let secure_conn = self.db.sea_secure();
    let scope = modkit_db::secure::AccessScope::for_tenant(ctx.tenant_id());
    let user = secure_conn
        .find_by_id::<user::Entity>(&scope, id)?
        .one(&secure_conn)
        .await?;
    Ok(user)
}
```

## REST API with OperationBuilder

### Checklist

- [ ] Use `OperationBuilder` for every route
- [ ] Add `.authenticated()` + `.require_license_features::<License>([])` for protected endpoints
- [ ] Add `.standard_errors(openapi)` or specific errors
- [ ] Use `.json_response_with_schema()` for typed responses
- [ ] Use `Extension<Arc<Service>>` and attach once after all routes
- [ ] Use `Extension(ctx): Extension<SecurityContext>` to get `SecurityContext`
- [ ] Use `ApiResult<T>` and `?` for error propagation
- [ ] For OData: add `.with_odata_*()` helpers and use `OData(query)` extractor

### Route registration template

```rust
OperationBuilder::get("/users-info/v1/users")
    .operation_id("users_info.list_users")
    .authenticated()
    .require_license_features::<License>([])
    .handler(handlers::list_users)
    .json_response_with_schema::<modkit_odata::Page<dto::UserDto>>(
        openapi,
        StatusCode::OK,
        "Paginated list of users",
    )
    .with_odata_filter::<dto::UserDtoFilterField>()
    .with_odata_select()
    .with_odata_orderby::<dto::UserDtoFilterField>()
    .standard_errors(openapi)
    .register(router, openapi);
```

### Handler template

```rust
pub async fn list_users(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<Service>>,
    OData(query): OData,
) -> ApiResult<JsonPage<serde_json::Value>> {
    let page: modkit_odata::Page<user_info_sdk::User> =
        svc.users.list_users_page(&ctx, &query).await?;
    let page = page.map_items(UserDto::from);
    Ok(Json(page_to_projected_json(&page, query.selected_fields())))
}
```

## OData Support

### Checklist

- [ ] Add `#[derive(ODataFilterable)]` on DTOs with `#[odata(filter(kind = "..."))]`
- [ ] Import `modkit_odata_macros::ODataFilterable`
- [ ] Use `OperationBuilderODataExt` helpers (`.with_odata_*()`)
- [ ] Use `OData(query)` extractor in handlers
- [ ] Return `Page<T>` from domain services
- [ ] Use `page_to_projected_json()` for responses with $select
- [ ] Add `.standard_errors()` for OData error handling

### DTO template

```rust
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, ODataFilterable)]
pub struct UserDto {
    #[odata(filter(kind = "Uuid"))]
    pub id: Uuid,
    #[odata(filter(kind = "Uuid"))]
    pub tenant_id: Uuid,
    #[odata(filter(kind = "String"))]
    pub email: String,
    pub display_name: String,
    #[odata(filter(kind = "DateTimeUtc"))]
    pub created_at: DateTime<Utc>,
    #[odata(filter(kind = "DateTimeUtc"))]
    pub updated_at: DateTime<Utc>,
}
```

## Error Handling

### Checklist

- [ ] Define `DomainError` in `domain/error.rs` with `thiserror::Error`
- [ ] Define SDK error in `<module>-sdk/src/errors.rs` (transport-agnostic)
- [ ] Implement `From<DomainError> for <Sdk>Error` in module crate
- [ ] Implement `From<DomainError> for Problem` in `api/rest/error.rs`
- [ ] Use `ApiResult<T>` in handlers and `?` for error propagation
- [ ] Register relevant errors in OperationBuilder (`.error_*` or `.standard_errors()`)
- [ ] Do not use `ProblemResponse` (doesn't exist)

### Domain error template

```rust
use modkit_macros::domain_model;

#[domain_model]
#[derive(Error, Debug, Clone)]
pub enum DomainError {
    #[error("User not found: {id}")]
    UserNotFound { id: uuid::Uuid },
    #[error("Email already exists: {email}")]
    EmailAlreadyExists { email: String },
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("Internal error: {0}")]
    Internal(String),
}
```

## ClientHub and Inter-Module Communication

### Checklist

- [ ] Define SDK trait with `async_trait` and `SecurityContext` first param
- [ ] Implement local adapter in module crate
- [ ] Register client in `init()`: `ctx.client_hub().register::<dyn Trait>(api)`
- [ ] Consume client: `ctx.client_hub().get::<dyn Trait>()?`
- [ ] For plugins: use `ClientScope::gts_id()` and `register_scoped()`
- [ ] For OoP: use gRPC client utilities and register both local and remote clients

### Client registration template

```rust
// In init()
let api: std::sync::Arc<dyn my_module_sdk::MyModuleApi> =
    std::sync::Arc::new(crate::domain::local_client::MyModuleLocalClient::new(svc));
ctx.client_hub().register::<dyn my_module_sdk::MyModuleApi>(api);
```

### Client consumption template

```rust
// In consumer module
let api = ctx.client_hub().get::<dyn my_module_sdk::MyModuleApi>()?;
let result = api.do_something(&ctx, input).await?;
```

## Lifecycle and Background Tasks

### Checklist

- [ ] Add `lifecycle(entry = "...")` to `#[modkit::module(...)]` for background tasks
- [ ] Use `CancellationToken` for shutdown coordination
- [ ] Pass child tokens to background tasks
- [ ] Call `ready.notify()` after setup when using `await_ready`
- [ ] Use `tokio::select!` for cooperative shutdown
- [ ] Implement graceful shutdown with timeout handling
- [ ] Test lifecycle with manual cancellation

### Background task template

```rust
pub fn spawn_background_task(cancel: CancellationToken) {
    let child = cancel.child_token();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        
        loop {
            tokio::select! {
                _ = child.cancelled() => break,
                _ = interval.tick() => {
                    // Do periodic work
                }
            }
        }
    });
}
```

## Out-of-Process (OoP) Modules

### Checklist

- [ ] Create `*-sdk` crate with API trait, types, gRPC client, and wiring helpers
- [ ] Define `.proto` file and generate gRPC stubs in SDK
- [ ] Implement gRPC server in module crate
- [ ] Use `modkit_transport_grpc::client` utilities for connections
- [ ] Register both local and remote clients in module
- [ ] Use `CancellationToken` for coordinated shutdown
- [ ] Test with mock gRPC servers

### SDK wiring template

```rust
// In SDK crate
pub async fn wire_client(endpoint: &str) -> Result<Box<dyn MyModuleApi>, Box<dyn std::error::Error>> {
    let channel = connect_with_stack(endpoint).await?;
    let client = MyModuleGrpcClient::new(channel);
    Ok(Box::new(client))
}
```

### OoP bootstrap template

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts = OopRunOptions {
        module_name: "my_module".to_string(),
        instance_id: None,
        directory_endpoint: "http://127.0.0.1:50051".to_string(),
        config_path: None,
        verbose: 0,
        print_config: false,
        heartbeat_interval_secs: 5,
    };

    run_oop_with_options(opts).await
}
```

## Testing Templates

### Testing with SecurityContext

All service and repository tests need a `SecurityContext`. Use explicit tenant IDs for test contexts:

```rust
use modkit_security::SecurityContext;
use uuid::Uuid;

#[tokio::test]
async fn test_service_method() {
    let tenant_id = Uuid::new_v4();
    let subject_id = Uuid::new_v4();
    let test_user_id = Uuid::new_v4();
    let ctx = SecurityContext::builder()
        .tenant_id(tenant_id)
        .subject_id(subject_id)
        .build();
    let service = create_test_service().await;

    let result = service.get_user(&ctx, test_user_id).await;
    assert!(result.is_ok());
}
```

For multi-tenant tests, create separate contexts per tenant and verify isolation:

```rust
#[tokio::test]
async fn test_tenant_isolation() {
    let tenant_a = SecurityContext::builder()
        .tenant_id(Uuid::new_v4())
        .subject_id(Uuid::new_v4())
        .build();
    let tenant_b = SecurityContext::builder()
        .tenant_id(Uuid::new_v4())
        .subject_id(Uuid::new_v4())
        .build();

    let service = create_test_service().await;

    // Tenant A cannot see Tenant B's data
    let result = service.list_users(&tenant_a, Default::default()).await;
    assert!(result.is_ok());
}
```

### Integration Test Template (Router::oneshot)

Create `tests/integration_tests.rs`:

```rust
use axum::{body::Body, http::{Request, StatusCode}, Router};
use modkit::api::OpenApiRegistry;
use std::sync::Arc;
use tower::ServiceExt;
use api_gateway::ApiGateway;

async fn create_test_router() -> Router {
    let service = create_test_service().await;
    let router = Router::new();
    let openapi = ApiGateway::default();
    your_module::api::rest::routes::register_routes(router, &openapi, service).unwrap()
}

#[tokio::test]
async fn test_get_endpoint() {
    let router = create_test_router().await;

    let request = Request::builder()
        .uri("/your-module/v1/resources/00000000-0000-0000-0000-000000000001")
        .body(Body::empty())
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_post_endpoint() {
    let router = create_test_router().await;

    let body = serde_json::json!({
        "tenant_id": "00000000-0000-0000-0000-000000000001",
        "email": "test@example.com",
        "display_name": "Test User"
    });

    let request = Request::builder()
        .method("POST")
        .uri("/your-module/v1/resources")
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
}
```

### SSE Test Template

```rust
use futures::StreamExt;
use modkit::SseBroadcaster;
use tokio::time::{timeout, Duration};

#[tokio::test]
async fn test_sse_broadcaster() {
    let broadcaster = SseBroadcaster::<UserEvent>::new(10);
    let mut stream = Box::pin(broadcaster.subscribe_stream());

    let event = UserEvent {
        kind: "created".to_string(),
        id: Uuid::new_v4(),
        at: time::OffsetDateTime::now_utc(),
    };

    broadcaster.send(event.clone());

    let received = timeout(Duration::from_millis(100), stream.next())
        .await
        .expect("timeout")
        .expect("event received");

    assert_eq!(received.kind, "created");
}
```

### Error handling test template

```rust
#[tokio::test]
async fn test_error_handling() {
    let service = setup_test_service().await;
    let ctx = SecurityContext::builder()
        .tenant_id(Uuid::new_v4())
        .subject_id(Uuid::new_v4())
        .build();

    let result = service.get_nonexistent(&ctx, Uuid::new_v4()).await;
    assert!(matches!(result, Err(DomainError::UserNotFound { .. })));
}
```

### OData test template

```rust
#[tokio::test]
async fn test_odata_filter() {
    let query = ODataQuery::from_str("?$filter=email eq 'test@example.com'").unwrap();
    assert!(query.filter().is_some());

    let filter = query.filter().unwrap();
    let condition = filter.to_sea_condition::<user::Entity>();
    // Verify condition
}
```
