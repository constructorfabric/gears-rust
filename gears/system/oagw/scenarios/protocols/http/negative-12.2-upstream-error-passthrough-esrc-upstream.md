# Upstream error passthrough (`X-OAGW-Error-Source: upstream`)

## Setup

Route points to an upstream endpoint that returns an error (example: `/fail` returns `500` with JSON body).

## Inbound request

```http
GET /api/oagw/v1/proxy/<alias>/fail HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

## Upstream response

```http
HTTP/1.1 500 Internal Server Error
Content-Type: application/json

{"error":"upstream_failed"}
```

## Expected response to client

- Status `500`.
- Body forwarded unchanged.
- `X-OAGW-Error-Source: upstream`.
- Content-Type preserved from upstream (not rewritten to Problem Details).
- Headers forwarded unchanged, including the upstream's own `Retry-After` (for example `503` with `Retry-After: 120`) and `WWW-Authenticate` (for example `401` with `WWW-Authenticate: Bearer realm="upstream", error="invalid_token"`). The gateway does not add or rewrite them.
- An upstream `429` or `503` differs from a gateway one only by `X-OAGW-Error-Source: upstream` and the body.
