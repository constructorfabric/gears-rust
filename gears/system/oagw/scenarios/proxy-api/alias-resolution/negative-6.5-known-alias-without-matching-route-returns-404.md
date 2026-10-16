# Known alias without a matching route returns 404

## Setup

1. Create upstream `alias=my-service`.
2. Create route `GET /v1/models`.

## Inbound request

```http
GET /api/oagw/v1/proxy/my-service/v2/unrouted HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

## Expected response

- `404 Not Found`
- `X-OAGW-Error-Source: gateway`
- `Content-Type: application/problem+json`
- `type` = `...not_found...`, `context.resource_type` = `gts.cf.core.oagw.route.v1~`, and `detail` indicates the route was not found.
- The resource type tells this apart from an unknown alias ([6.4](negative-6.4-alias-not-found-returns-stable-404.md)), which names the upstream (`gts.cf.core.oagw.upstream.v1~`).
- `GET /v1/models` on the same alias still succeeds.
