---
title: Pagination & filtering (OData)
description: OData $filter / $orderby / $select and cursor-based pagination for list endpoints.
sidebar:
  label: Pagination & OData
  order: 4
---

List endpoints in Gears speak a typed subset of OData — `$filter`, `$orderby`, `$select` —
with cursor-based pagination. Filtering is type-checked at compile time from a schema you
declare in the SDK.

## Declare filterable fields (SDK)

In the SDK crate, declare which fields are filterable with `#[derive(ODataFilterable)]`. The
macro generates a filter-field enum:

```rust title="<gear>-sdk/src/odata/users.rs"
#[derive(ODataFilterable)]
pub struct UserQuery {
    #[odata(filter(kind = "Uuid"))]
    pub id: Uuid,
    #[odata(filter(kind = "String"))]
    pub email: String,
    #[odata(filter(kind = "DateTimeUtc"))]
    pub created_at: OffsetDateTime,
}
// generated enum, aliased for ergonomics:
pub use UserQueryFilterField as UserFilterField;
```

Supported `kind`s include `Uuid`, `String`, `DateTimeUtc`, `I32`, `I64`, `Bool`.

## Enable OData on the route

Attach the filter/select/orderby capabilities to the `OperationBuilder` declaration; the
response schema is `Page<Dto>`:

```rust title="api/rest/routes/users.rs"
OperationBuilder::get("/users-info/v1/users")
    .operation_id("users_info.list_users")
    .authenticated()
    .handler(handlers::list_users)
    .json_response_with_schema::<toolkit_odata::Page<dto::UserDto>>(
        openapi, http::StatusCode::OK, "Paginated list of users")
    .with_odata_filter::<UserFilterField>()
    .with_odata_select()
    .with_odata_orderby::<UserFilterField>()
    .error_400(openapi).error_500(openapi)
    .register(router, openapi);
```

## Extract the query in the handler

The `OData` extractor parses the query string into an `ODataQuery`. After mapping items to
the DTO, apply `$select` projection:

```rust title="api/rest/handlers/users.rs"
pub async fn list_users(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    OData(query): OData,
) -> ApiResult<JsonPage<serde_json::Value>> {
    let page = svc.users.list_users_page(&ctx, &query).await?;
    let page = page.map_items(UserDto::from);
    Ok(Json(page_to_projected_json(&page, query.selected_fields())))
}
```

## Paginate in the repository

`paginate_odata` applies the filter/orderby to a **scoped** base query and returns a
cursor-paginated `Page<T>`. It takes the filter-field type, an OData mapper (mapping fields to
columns), a default sort, the page-size limits, and a row-mapping function:

```rust title="infra/storage/users_sea_repo.rs"
let base = UserEntity::find().secure().scope_with(scope);
let page = paginate_odata::<UserFilterField, UserODataMapper, _, _, _, _>(
    base,
    conn,
    query,
    ("id", SortDir::Desc),   // default sort column + direction
    self.limit_cfg,          // LimitCfg { default, max }
    Into::into,              // entity → SDK model
).await?;
```

`Page<T>` carries `items` plus a `page_info { next_cursor, prev_cursor, limit }`; cursors are
opaque and stateless.

## Call it

```sh
curl -s "http://127.0.0.1:8087/cf/users-info/v1/users?\$filter=email eq 'a@b.com'&\$orderby=created_at desc&\$select=id,email"
```

:::note[`$select` is applied after fetch]
Field projection happens at the JSON layer, not pushed down to SQL — full rows are fetched,
then projected. Keep that in mind for very wide rows.
:::

## See also

- [Database patterns](../database/) — the scoped base query `paginate_odata` runs over.
- Full code: the `odata/` module in `users-info-sdk` and `odata_mapper.rs` in the gear.
