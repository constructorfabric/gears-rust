# OData: $filter, $orderby, $select, and Pagination

ToolKit provides OData query support with type-safe filtering, ordering, field selection, and cursor-based pagination.

## Core invariants

- **Rule**: Use `toolkit_odata_macros::ODataFilterable` to declare the filterable fields of a resource.
- **Rule**: Use `OperationBuilderODataExt` helpers instead of manual `.query_param(...)`.
- **Rule**: Use `apply_select()` for single-resource field projection in handlers.
- **Rule**: Use `page_to_projected_json()` for paginated JSON responses with $select.
- **Rule**: Return `Page<T>` from domain services.

## Filterable fields

### Define a filter schema

`#[derive(ODataFilterable)]` on a struct generates a `<Struct>FilterField` enum that implements `toolkit_odata::filter::FilterField`. Only fields with `#[odata(filter(kind = "..."))]` become filterable; the field's Rust type is not inspected. `users-info` keeps the schema in its SDK crate so clients and the server share it ([`users_info_sdk::odata::users`](../../examples/toolkit/users-info/users-info-sdk/src/odata/users.rs)):

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

### Filter field kinds

The `kind` string names a `toolkit_odata::filter::FieldKind` variant. The SQL value is built in `toolkit_db::odata` (`core.rs`).

| Kind | SQL value | Example |
|------|-----------|---------|
| `String` | string | `email eq 'test@example.com'` |
| `I64` | 64-bit integer | `count ge 100` |
| `F64` | double | `score gt 0.5` |
| `Decimal` | decimal | `price le 19.99` |
| `Bool` | boolean | `is_active eq true` |
| `Uuid` | UUID | `id eq 550e8400-e29b-41d4-a716-446655440000` |
| `DateTimeUtc` | UTC timestamp | `created_at gt 2024-01-01T00:00:00Z` |
| `Date` | date | `birth_date eq 2024-01-01` |
| `Time` | time of day | `opens_at lt 09:00:00` |

## OperationBuilder with OData

### OData-enabled list endpoint

```rust
use toolkit::api::operation_builder::{OperationBuilder, OperationBuilderODataExt};
use users_info_sdk::odata::UserFilterField;

OperationBuilder::get("/users-info/v1/users")
    .operation_id("users_info.list_users")
    .authenticated()
    .require_license_features::<License>([])
    .handler(handlers::list_users)
    .json_response_with_schema::<toolkit_odata::Page<dto::UserDto>>(
        openapi,
        StatusCode::OK,
        "Paginated list of users",
    )
    .with_odata_filter::<UserFilterField>() // not .query_param("$filter", ...)
    .with_odata_select() // not .query_param("$select", ...)
    .with_odata_orderby::<UserFilterField>() // not .query_param("$orderby", ...)
    .standard_errors(openapi)
    .register(router, openapi);
```

## Handler with OData

### List handler (paginated with $select)

```rust
use axum::Extension;
use toolkit::api::canonical_prelude::*;
use toolkit::api::select::page_to_projected_json;
use toolkit_security::SecurityContext;

pub async fn list_users(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    OData(query): OData,
) -> ApiResult<JsonPage<serde_json::Value>> {
    let page: toolkit_odata::Page<users_info_sdk::User> =
        svc.users.list_users_page(&ctx, &query).await?;
    let page = page.map_items(UserDto::from);
    Ok(Json(page_to_projected_json(&page, query.selected_fields())))
}
```

### Single-resource handler with $select

```rust
use toolkit::api::canonical_prelude::*;

pub async fn get_user(
    OData(query): OData,
    // ... other extractors
) -> ApiResult<JsonBody<serde_json::Value>> {
    let user = fetch_user().await?;
    let projected = apply_select(&user, query.selected_fields());
    Ok(Json(projected))
}
```

### Domain service and repository with OData

The service obtains a connection runner and an `AccessScope` from `PolicyEnforcer`, then delegates to the repository ([service](../../examples/toolkit/users-info/users-info/src/domain/service/users.rs)):

```rust
pub async fn list_users_page(
    &self,
    ctx: &SecurityContext,
    query: &ODataQuery,
) -> Result<Page<User>, DomainError> {
    let conn = self.db.conn().map_err(DomainError::from)?;

    let scope = self
        .policy_enforcer
        .access_scope_with(
            ctx,
            &resources::USER,
            actions::LIST,
            None,
            &AccessRequest::new().allow_empty_scope(true),
        )
        .await?;

    self.repo.list_page(&conn, &scope, query).await
}
```

The repository builds a scoped select and passes it to `paginate_odata`, which applies `$filter`, `$orderby`, the cursor and the limit ([repository](../../examples/toolkit/users-info/users-info/src/infra/storage/users_sea_repo.rs)):

```rust
use toolkit_db::odata::{LimitCfg, paginate_odata};
use toolkit_db::secure::{DBRunner, SecureEntityExt};
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
use users_info_sdk::odata::UserFilterField;

async fn list_page<C: DBRunner>(
    &self,
    conn: &C,
    scope: &AccessScope,
    query: &ODataQuery,
) -> Result<Page<User>, DomainError> {
    let base_query = UserEntity::find().secure().scope_with(scope);

    let page = paginate_odata::<UserFilterField, UserODataMapper, _, _, _, _>(
        base_query,
        conn,
        query,
        ("id", SortDir::Desc),
        self.limit_cfg,
        Into::into,
    )
    .await
    .map_err(db_err)?;

    Ok(page)
}
```

`paginate_odata` takes six generic parameters: the filter field enum, the `ODataFieldMapping` implementation that maps fields to entity columns ([`UserODataMapper`](../../examples/toolkit/users-info/users-info/src/infra/storage/odata_mapper.rs)), the entity, the domain type, the model mapper and the runner. The last four are inferred. It returns `Result<Page<D>, toolkit_odata::Error>`. For a fallible model mapper use `toolkit_db::odata::sea_orm_filter::paginate_odata_try`; it is not re-exported at `toolkit_db::odata`.

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
use toolkit::api::select::page_to_projected_json;

let projected_page = page_to_projected_json(&page, query.selected_fields());
```

Automatically serializes each item, applies `$select` projection, preserves `page_info`, and returns `Page<Value>`.

#### `apply_select` (for single resources)

```rust
use toolkit::api::select::apply_select;

let projected = apply_select(&user, query.selected_fields());
```

#### `project_json` (advanced: manual projection)

For custom projection logic, use `project_json` directly:

```rust
use toolkit::api::select::project_json;
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
    page: &toolkit_odata::Page<T>,
    selected_fields: Option<&[String]>,
) -> toolkit_odata::Page<Value>
```

### $select limitations

- Field projection happens at the application layer, not the database layer
- `$select` is not pushed down to SQL; `paginate_odata` always fetches full rows. Projection is applied to the serialized JSON in the handler
- Nested object projection includes the entire nested object if the parent field is selected
- Computed or derived fields cannot be selectively excluded
- Dot notation requires exact field path matching (e.g., `access_control.read` won't match `access_control.permissions.read`)

## Cursor-based pagination

### Page size (`$top` / `limit`)

The page size binds off the query string in exactly one place —
`toolkit::api::odata::ODataParams` — and lands in `ODataQuery.limit`. Two
spellings reach that slot:

| Spelling | Notes |
|----------|-------|
| `$top` | Canonical OData (OASIS OData 4.01 Part 2: URL Conventions, §5.1.6). Serde alias. |
| `limit` | The unprefixed spelling most gears publish in their OpenAPI documents. |

```bash
# Equivalent
/users-info/v1/users?$top=20
/users-info/v1/users?limit=20

# Ambiguous — 400 InvalidArgument (duplicate field)
/users-info/v1/users?$top=20&limit=50
```

Both spellings are one parameter, so a gear needs no per-endpoint handling to
honor either. Two consequences worth knowing:

- OpenAPI cannot express one parameter under two names. Publish the spelling
  your gear treats as canonical and mention the other in its `description`.
- `$top=0` is rejected (`InvalidLimit` → `400`, `field_violations[0].field` is
  `$top`). Enforcing an upper bound is the handler's job — the extractor caps
  filter/orderby/select size, not page size.
- **`$skip` is not supported.** Offset paging is not the platform's model, and
  a request that pairs `$top` with `$skip` is refused rather than served with a
  page size it asked for at an offset it did not get. Continue a page with
  `$skiptoken` (alias `cursor`); see [Unsupported system query
  options](#unsupported-system-query-options).

### Continuation token (`$skiptoken` / `cursor`)

The same one-slot-two-spellings rule applies to the continuation token: both
`$skiptoken` and `cursor` land in `ODataQuery.cursor`. `$skiptoken` is OData's
opaque continuation token for server-driven paging, which is exactly what
`PageInfo.next_cursor` is — pass the previous page's `next_cursor` back under
either spelling.

```bash
# Equivalent
/users-info/v1/users?$top=20&$skiptoken=eyJ2IjoxLC…
/users-info/v1/users?limit=20&cursor=eyJ2IjoxLC…
```

### Unsupported system query options

Every `$`-prefixed query key the extractor does not bind is **rejected**, not
ignored:

```bash
# 400 InvalidArgument, field_violations[0] = { field: "$skip",
#   reason: "UNSUPPORTED_QUERY_PARAM" }
/users-info/v1/users?$top=20&$skip=20
```

The accepted set is `toolkit::api::odata::ACCEPTED_SYSTEM_QUERY_OPTIONS` —
`$filter`, `$orderby`, `$select`, `$top`, `$skiptoken`. Anything else in the
`$` namespace answers `400` with one `UNSUPPORTED_QUERY_PARAM` violation per
offending key, so one round trip names every problem:

| Option | Why it is refused |
|--------|-------------------|
| `$skip` | Offset paging is not the model. Use `$skiptoken` / `cursor`. |
| `$count` | A page carries no total; `PageInfo` is `{next_cursor, prev_cursor, limit}`. Serving a total would mean a `COUNT(*)` per page, which keyset pagination exists to avoid. |
| `$expand`, `$search`, `$compute`, `$index`, `$schemaversion` | Not implemented. |
| `$format` | Responses are JSON; negotiate the media type with `Accept`. |
| `$filtre`, `$topp`, … | Not OData options at all, so the violation reads `unknown query option` rather than `unsupported`. A dropped `$filtre` returns the whole unfiltered collection with a `200`. |

This is required behavior, not a house rule — OASIS OData 4.01 Part 1:
Protocol, §6.1 "Query Option Extensibility":

> OData services SHOULD NOT require any query options to be specified in a
> request. Services SHOULD fail any request that contains query options that
> they do not understand and MUST fail any request that contains unsupported
> OData query options defined in the version of this specification supported by
> the service.

Two boundaries follow from the same section:

- **Unprefixed keys are out of scope.** §6.1 reserves the `$` prefix for
  OData, so a non-`$` key belongs to the handler's own params struct — the
  extractor never rejects one. A gear that wants `?status=approved` refused
  instead of ignored needs its own guard; account-management has one
  (`reject_non_odata_params`).
- **Spelling is case-sensitive, deliberately.** OData 4.01 Part 2 §5.1 asks a
  4.01 service to accept system query option names case-insensitively and with
  or without the `$` prefix. This platform accepts neither `$Top` nor the
  unprefixed canonical `top`; it answers `400` naming the spelling that binds
  rather than dropping the request. Closing that gap is a separate conformance
  change.

### Page structure

```rust
use toolkit_odata::{Page, PageInfo};

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

`paginate_odata` decodes the incoming cursor, validates it against the query, and fills `PageInfo.next_cursor` / `prev_cursor`. The service does not handle cursors itself; see [Domain service and repository with OData](#domain-service-and-repository-with-odata).

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

OData errors are defined in `toolkit_odata::Error`. Crates in this workspace commonly import it as `use toolkit_odata::Error as ODataError;`; that alias is local to each file, not an exported type. Key variants:

| Variant | Description | HTTP status |
|---------|-------------|-------------|
| `InvalidFilter(String)` | Malformed `$filter` expression | 400 |
| `InvalidOrderByField(String)` | Unsupported `$orderby` field | 400 |
| `InvalidCursor` / `CursorInvalid*` | Malformed or expired cursor | 400 |
| `OrderWithCursor` | `$orderby` sent together with a `cursor` (the cursor fixes the order) | 400 |
| `OrderMismatch` | Server-derived order differs from the cursor's (`validate_cursor_against`) | 400 |
| `FilterMismatch` | Cursor/query filter conflict | 400 |
| `InvalidLimit` | Invalid page size parameter (`$top`, alias `limit`) | 400 |
| `Db(String)` | Database error (logged, generic message returned) | 500 |
| `ParsingUnavailable(&'static str)` | Configuration error ("OData parsing unavailable") | 500 |

Every variant above except `Db` and `ParsingUnavailable` maps through `OdataError::invalid_argument()`, and
`CanonicalError::InvalidArgument` renders as `400`. The canonical error taxonomy has no `422`
status, so no `OData` error can produce one.

`$select` validation errors (too long, too many fields, duplicates) are caught during parsing in the `OData` extractor and returned as `400 Bad Request` with RFC 9457 Problem Details before reaching the handler.

Unsupported system query options are not `toolkit_odata::Error` variants — the extractor builds the canonical `InvalidArgument` itself, before any value parsing, with one `UNSUPPORTED_QUERY_PARAM` field violation per offending key. See [Unsupported system query options](#unsupported-system-query-options).

### Error conversion

`From<toolkit_odata::Error> for CanonicalError` ([`toolkit-odata/src/problem_mapping.rs`](../../libs/toolkit-odata/src/problem_mapping.rs)) is the single mapping from OData errors to the canonical error catalog. The `OData` extractor and handlers return `CanonicalError`; `canonical_error_middleware` (`toolkit::api`) renders the wire `Problem` and fills `instance` and `trace_id`. A repository that wraps the error in its own domain error (as `users-info` does with `db_err`) owns that mapping instead.

## Testing OData

### Test field projection

```rust
use toolkit::api::select::apply_select;
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
use toolkit::api::select::page_to_projected_json;
use toolkit_odata::{Page, PageInfo};

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

- [ ] Add `#[derive(ODataFilterable)]` on a filter schema struct with `#[odata(filter(kind = "..."))]` fields.
- [ ] Import `toolkit_odata_macros::ODataFilterable`.
- [ ] Use `OperationBuilderODataExt` helpers (`.with_odata_*()`).
- [ ] Use `OData(query)` extractor in handlers.
- [ ] Return `Page<T>` from domain services.
- [ ] Use `page_to_projected_json()` for list responses with $select.
- [ ] Use `apply_select()` for single-resource responses with $select.
- [ ] Add `.standard_errors()` for OData error handling.
