# Management API permission gates

## Scenario A: missing permission

### Request

```http
POST /api/oagw/v1/routes HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <token-without-route-create>
Content-Type: application/json

{
  "upstream_id": "gts.cf.core.oagw.upstream.v1~7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "match": {
    "http": {
      "methods": ["POST"],
      "path": "/post",
      "query_allowlist": ["tag"],
      "path_suffix_mode": "disabled"
    }
  }
}
```

### Expected response

- `403 Forbidden`
- `Content-Type: application/problem+json`

```json
{
  "type": "<stable-error-type>",
  "title": "Forbidden",
  "status": 403,
  "detail": "<missing permission or access denied>",
  "instance": "/api/oagw/v1/routes"
}
```

## Scenario B: permission present

- Repeat the same request with `Authorization: Bearer <token-with-route-create>`.
- Expect `201 Created` and a returned Route resource.

## Scenario C: every operation is gated

### Request (example: list without `read`)

```http
GET /api/oagw/v1/upstreams HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <token-without-upstream-read>
```

### Expected response

```http
HTTP/1.1 403 Forbidden
Content-Type: application/problem+json

{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
  "title": "Permission Denied",
  "status": 403,
  "context": { "resource_type": "gts.cf.core.oagw.upstream.v1~" }
}
```

The list is refused; it does not return `200` with an empty array.

### Every operation

Each upstream and route operation of the Management API and the SDK asks the policy engine for one action on `gts.cf.core.oagw.upstream.v1~` or `gts.cf.core.oagw.route.v1~`, with the caller's tenant as owner:

| Operation | Action |
|---|---|
| create | `create` |
| list, get | `read` |
| update | `override` (no `read` needed) |
| delete | `delete` |

Without the permission, each returns `403` as above, with `context.resource_type` naming the upstream or route type (not the proxy type).

A grant the policy constrains with `eq` or `in` on `owner_tenant_id` must include the caller's tenant; one that adds a row-level `eq` or `in` on `id` is refused with `403` and reason `AUTHZ_SCOPE_NOT_SUPPORTED`: the repositories filter by tenant only and cannot apply it. A subquery grant (`in_tenant_subtree`, group membership) cannot be compiled without a database and fails with `500`.

SDK `resolve_proxy_target` needs `read` on each upstream the answer is built from (the selected upstream and the ancestors' upstreams that share with it) and on the route. An ancestor's upstream on the alias that shares nothing is not asked about. An unknown alias or a path with no matching route needs `read` on every tenant of the caller's chain; without it the answer is `403`, not `404`, so the error does not reveal what the ancestors share. A policy that does not let a tenant read its ancestors' upstreams (tr-authz, static-authz) therefore refuses resolution through an ancestor and every resolution error of a non-root tenant with `403`.

Upstreams and routes provisioned from the types registry are written without these checks.
