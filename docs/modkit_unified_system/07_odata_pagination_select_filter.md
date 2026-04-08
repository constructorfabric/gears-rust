# OData: $filter, $orderby, $select, and Pagination

ModKit provides OData query support with type-safe filtering, ordering, field selection, and cursor-based pagination.

## Core invariants

- **Rule**: Use `modkit_odata_macros::ODataFilterable` for DTO filtering.
- **Rule**: Use `OperationBuilderODataExt` helpers instead of manual `.query_param(...)`.
- **Rule**: Use `apply_select()` for single-resource field projection in handlers.
- **Rule**: Use `page_to_projected_json()` for paginated JSON responses with $select.
- **Rule**: Return `Page<T>` from domain services.

## OData macro migration

### Before (old)

```rust
use modkit_db_macros::ODataFilterable;
```

### After (current)

```rust
use modkit_odata_macros::ODataFilterable;
```

## DTO with OData filtering

### Define filterable DTO

```rust
use modkit_odata_macros::ODataFilterable;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// REST DTO for user representation with OData filtering
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
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[odata(filter(kind = "DateTimeUtc"))]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
```

### Filter field kinds

| Kind | Type | Example |
|------|------|---------|
| `String` | `String` | `email eq 'test@example.com'` |
| `Uuid` | `uuid::Uuid` | `id eq 550e8400-e29b-41d4-a716-446655440000` |
| `DateTimeUtc` | `chrono::DateTime<chrono::Utc>` | `created_at gt 2024-01-01T00:00:00Z` |
| `I32` | `i32` | `age gt 18` |
| `I64` | `i64` | `count ge 100` |
| `Bool` | `bool` | `is_active eq true` |

## OperationBuilder with OData

### OData-enabled list endpoint

```rust
use modkit::api::operation_builder::{OperationBuilderODataExt};

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
    .with_odata_filter::<dto::UserDtoFilterField>() // not .query_param("$filter", ...)
    .with_odata_select() // not .query_param("$select", ...)
    .with_odata_orderby::<dto::UserDtoFilterField>() // not .query_param("$orderby", ...)
    .standard_errors(openapi)
    .register(router, openapi);
```

## Handler with OData

### List handler (paginated with $select)

```rust
use modkit::api::prelude::*;
use modkit::api::odata::OData;
use modkit::api::select::page_to_projected_json;
use axum::Extension;

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

### Single-resource handler with $select

```rust
use modkit::api::prelude::*;
use modkit::api::odata::OData;
use modkit::api::select::apply_select;

pub async fn get_user(
    OData(query): OData,
    // ... other extractors
) -> ApiResult<JsonBody<serde_json::Value>> {
    let user = fetch_user().await?;
    let projected = apply_select(&user, query.selected_fields());
    Ok(Json(projected))
}
```

### Domain service with OData

```rust
impl UserService {
    pub async fn list_users_page(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<User>, DomainError> {
        let secure_conn = self.db.sea_secure();
        let scope = modkit_db::secure::AccessScope::for_tenant(ctx.tenant_id());

        // Recommended: compose security + OData in one call, without raw connection access.
        use modkit_db::odata::sea_orm_filter::{paginate_odata, LimitCfg};
        use modkit_odata::SortDir;
        use crate::infra::storage::odata_mapper::UserODataMapper;
        use crate::api::rest::dto::UserDtoFilterField;

        let base_query = secure_conn.find::<user::Entity>(&scope);
        let page = paginate_odata::<UserDtoFilterField, UserODataMapper>(
            base_query,
            &secure_conn,
            query,
            ("id", SortDir::Desc),
            LimitCfg { default: 50, max: 500 },
            |model| model.into(),
        )
        .await?;

        Ok(page)
    }
}
```

## Field projection ($select)

### Format

```text
$select=field1,field2,field3
```

Field names are case-insensitive and whitespace is trimmed. Multiple fields are separated by commas.

### Dot notation for nested fields

Use dot notation to select specific nested fields:

```
$select=access_control.read,access_control.write
```

This includes only the `read` and `write` fields within `access_control`, filtering out other nested fields like `delete`.

### $select validation constraints

| Constraint | Value | Error |
|-----------|-------|-------|
| Maximum length | 2048 characters | `$select too long` |
| Maximum fields | 100 fields | `$select contains too many fields` |
| Empty check | Must contain at least one field | `$select must contain at least one field` |
| Duplicates | Field names must be unique | `duplicate field in $select: {field}` |

### $select examples

Request only `id` and `name` fields:
```
GET /api/users?$select=id,name
```

Response:
```json
{
  "items": [
    {"id": "123", "name": "John"},
    {"id": "456", "name": "Jane"}
  ],
  "page_info": { ... }
}
```

Combine `$select` with `$filter` and `$orderby`:
```
GET /api/users?$filter=email eq 'john@example.com'&$orderby=created_at desc&$select=id,email,created_at
```

Single resource:
```
GET /api/users/123?$select=id,email,display_name
```

Select entire nested object:
```
GET /api/users?$select=id,access_control
```

Select specific nested fields using dot notation:
```
GET /api/users?$select=id,access_control.read,access_control.write
```

Response:
```json
{
  "items": [
    {
      "id": "123",
      "access_control": {
        "read": true,
        "write": false
      }
    }
  ]
}
```

Deeply nested selection:
```
GET /api/users?$select=id,user.profile.name,user.profile.email
```

Response:
```json
{
  "items": [
    {
      "id": "123",
      "user": {
        "profile": {
          "name": "John Doe",
          "email": "john@example.com"
        }
      }
    }
  ]
}
```

### Dot notation behavior

1. **Entire Parent Selection**: If you select `access_control` without dot notation, the entire nested object is included with all its fields.
2. **Specific Nested Fields**: If you select `access_control.read` and `access_control.write`, only those specific fields are included in the nested object.
3. **Deep Nesting**: Dot notation works at any depth: `user.profile.name`, `user.profile.settings.notifications`, etc.
4. **Case Insensitivity**: Matching uses `.to_lowercase()` on both sides, so `Access_Control.READ` matches `access_control.read`. Note: this is not camelCase↔snake_case conversion — `AccessControl` will not match `access_control`.
5. **Array Projection**: When projecting arrays, the dot notation is applied to each element in the array.
6. **Mixed Selection**: You can mix top-level and nested selections: `$select=id,access_control,profile.bio` will include the entire `access_control` object and only the `bio` field from `profile`.

### Helper functions

#### `page_to_projected_json` (recommended for list endpoints)

```rust
use modkit::api::select::page_to_projected_json;

let projected_page = page_to_projected_json(&page, query.selected_fields());
```

Automatically serializes each item, applies `$select` projection, preserves `page_info`, and returns `Page<Value>`.

#### `apply_select` (for single resources)

```rust
use modkit::api::select::apply_select;

let projected = apply_select(&user, query.selected_fields());
```

#### `project_json` (advanced: manual projection)

For custom projection logic, use `project_json` directly:

```rust
use modkit::api::select::project_json;
use std::collections::HashSet;

let fields_set: HashSet<String> = query
    .selected_fields()
    .map(|fields| fields.iter().map(|f| f.to_lowercase()).collect())
    .unwrap_or_default();

let projected = project_json(&value, &fields_set);
```

### $select API reference

#### ODataQuery methods

```rust
// Check if field selection is present
pub fn has_select(&self) -> bool

// Get selected fields as a slice
pub fn selected_fields(&self) -> Option<&[String]>

// Set selected fields (builder pattern)
pub fn with_select(mut self, fields: Vec<String>) -> Self
```

#### Field projection utilities

```rust
// Project a JSON value to include only selected fields (supports dot notation, case-insensitive)
pub fn project_json(value: &Value, selected_fields: &HashSet<String>) -> Value

// Serialize and project a value; returns original if no fields selected
pub fn apply_select<T: serde::Serialize>(value: T, selected_fields: Option<&[String]>) -> Value

// Project all items in a page; preserves page_info
pub fn page_to_projected_json<T: serde::Serialize>(
    page: &modkit_odata::Page<T>,
    selected_fields: Option<&[String]>,
) -> modkit_odata::Page<Value>
```

### $select limitations

- Field projection happens at the application layer, not the database layer
- `$select` is not pushed down to SQL; `paginate_odata` always fetches full rows. Projection is applied to the serialized JSON in the handler
- Nested object projection includes the entire nested object if the parent field is selected
- Computed or derived fields cannot be selectively excluded
- Dot notation requires exact field path matching (e.g., `access_control.read` won't match `access_control.permissions.read`)

## Cursor-based pagination

### Page structure

```rust
use modkit_odata::{Page, PageInfo};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PageInfo {
    pub next_cursor: Option<String>,
    pub prev_cursor: Option<String>,
    pub limit: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub page_info: PageInfo,
}
```

`Page<T>` also provides:
- `Page::new(items, page_info)` — create a page
- `Page::empty(limit)` — create an empty page with default page_info
- `page.map_items(|item| ...)` — transform items while preserving `page_info`

### Cursor handling

```rust
// In domain service
impl UserService {
    pub async fn list_users_page(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<User>, DomainError> {
        let secure_conn = self.db.sea_secure();
        let scope = modkit_db::secure::AccessScope::for_tenant(ctx.tenant_id());
        use modkit_db::odata::sea_orm_filter::{paginate_odata, LimitCfg};
        use modkit_odata::SortDir;
        use crate::infra::storage::odata_mapper::UserODataMapper;
        use crate::api::rest::dto::UserDtoFilterField;

        let base_query = secure_conn.find::<user::Entity>(&scope);
        let page = paginate_odata::<UserDtoFilterField, UserODataMapper>(
            base_query,
            &secure_conn,
            query,
            ("id", SortDir::Desc),
            LimitCfg { default: 50, max: 500 },
            |model| model.into(),
        )
        .await?;

        Ok(page)
    }
}
```

## Common OData queries

### Filter examples

```bash
# String equality
$filter=email eq 'test@example.com'

# String contains
$filter=contains(email, 'test')

# UUID comparison
$filter=id eq 550e8400-e29b-41d4-a716-446655440000

# DateTime comparison
$filter=created_at gt 2024-01-01T00:00:00Z

# Logical operators
$filter=email eq 'test@example.com' and created_at gt 2024-01-01T00:00:00Z
$filter=age gt 18 or age lt 65
```

### Order examples

```bash
# Single field
$orderby=email

# Multiple fields
$orderby=created_at desc,email

# With direction
$orderby=created_at asc
```

### Select examples

```bash
# Single field
$select=id

# Multiple fields
$select=id,email,created_at

# Nested fields (dot notation)
$select=id,access_control.read,access_control.write

# Deeply nested
$select=id,user.profile.name,user.profile.email
```

### Combined examples

```bash
# Full query
/users-info/v1/users?$filter=email eq 'test@example.com'&$orderby=created_at desc&$select=id,email,created_at&limit=20

# With cursor
/users-info/v1/users?cursor=eyJpZCI6IjU1MGU4NDAwLWUyOWItNDFkNC1hNzE2LTQ0NjY1NTQ0MDAwMCJ9&limit=20
```

## Error handling

### OData error type

OData errors are defined in `modkit_odata::Error` (aliased as `ODataError` in `modkit`). Key variants:

| Variant | Description | HTTP status |
|---------|-------------|-------------|
| `InvalidFilter(String)` | Malformed `$filter` expression | 422 |
| `InvalidOrderByField(String)` | Unsupported `$orderby` field | 422 |
| `InvalidCursor` / `CursorInvalid*` | Malformed or expired cursor | 422 |
| `OrderMismatch` | Cursor/query order conflict | 422 |
| `FilterMismatch` | Cursor/query filter conflict | 422 |
| `InvalidLimit` | Invalid limit parameter | 422 |
| `Db(String)` | Database error (logged, generic message returned) | 500 |

`$select` validation errors (too long, too many fields, duplicates) are caught during parsing in the `OData` extractor and returned as `400 Bad Request` with RFC 9457 Problem Details before reaching the handler.

### Error conversion

The `From<modkit_odata::Error> for Problem` impl (in `modkit-odata/src/problem_mapping.rs`) maps each variant to a GTS error code. The HTTP layer in `modkit` adds instance paths and trace IDs via `odata_error_to_problem()`:

```rust
use modkit::api::odata::error::odata_error_to_problem;

// Automatically used by the OData extractor; manual usage:
let problem = odata_error_to_problem(&err, "/users-info/v1/users", None);
```

## Testing OData

### Test field projection

```rust
use modkit::api::select::apply_select;
use serde_json::json;

#[test]
fn test_field_projection() {
    #[derive(serde::Serialize)]
    struct UserDto {
        id: String,
        email: String,
        display_name: String,
    }

    let dto = UserDto {
        id: "123".to_owned(),
        email: "test@example.com".to_owned(),
        display_name: "Test User".to_owned(),
    };

    let fields = vec!["id".to_owned(), "email".to_owned()];
    let projected = apply_select(&dto, Some(&fields));

    assert_eq!(projected.get("id").and_then(|v| v.as_str()), Some("123"));
    assert_eq!(
        projected.get("email").and_then(|v| v.as_str()),
        Some("test@example.com"),
    );
    assert!(projected.get("display_name").is_none());
}
```

### Test page projection

```rust
use modkit::api::select::page_to_projected_json;
use modkit_odata::{Page, PageInfo};

#[test]
fn test_page_projection() {
    let page = Page {
        items: vec![
            serde_json::json!({"id": "1", "name": "Alice", "email": "a@example.com"}),
            serde_json::json!({"id": "2", "name": "Bob", "email": "b@example.com"}),
        ],
        page_info: PageInfo {
            next_cursor: Some("cursor123".to_owned()),
            prev_cursor: None,
            limit: 50,
        },
    };

    let fields = vec!["id".to_owned(), "name".to_owned()];
    let projected = page_to_projected_json(&page, Some(&fields));

    assert_eq!(projected.items.len(), 2);
    assert!(projected.items[0].get("email").is_none());
    assert_eq!(projected.page_info.limit, 50); // page_info preserved
}
```

## Quick checklist

- [ ] Add `#[derive(ODataFilterable)]` on DTOs with `#[odata(filter(kind = "..."))]`.
- [ ] Import `modkit_odata_macros::ODataFilterable`.
- [ ] Use `OperationBuilderODataExt` helpers (`.with_odata_*()`).
- [ ] Use `OData(query)` extractor in handlers.
- [ ] Return `Page<T>` from domain services.
- [ ] Use `page_to_projected_json()` for list responses with $select.
- [ ] Use `apply_select()` for single-resource responses with $select.
- [ ] Add `.standard_errors()` for OData error handling.
