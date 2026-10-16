# Overlapping route rejected (409)

## Setup

1. Create upstream `alias=my-service`.
2. Create route A: `GET, POST /v1/models`, `priority=0`, `enabled=true`.

## Create an overlapping route

```http
POST /api/oagw/v1/routes HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "upstream_id": "gts.cf.core.oagw.upstream.v1~<upstream-uuid>",
  "match": {
    "http": {
      "methods": ["POST", "PUT"],
      "path": "/v1/models"
    }
  },
  "priority": 0,
  "enabled": true
}
```

Two enabled routes overlap when they are on the same upstream and share the path, the priority, and at least one method.

## Expected response

- `409 Conflict`, `application/problem+json`.
- `type` = `...already_exists...`, `context.resource_type` = `gts.cf.core.oagw.route.v1~`.
- `detail` names the upstream, path, priority and the shared method.
- Route A is the only route stored on the upstream.
- A route with the same path and method at another `priority` does not overlap and is created (`201`).
- Callers that provision routes at startup rely on the 409 to treat an existing route as already done.
