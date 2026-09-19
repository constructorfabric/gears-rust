# CORS disabled by default (no config → no CORS headers)

## Setup

Create an upstream with no `cors` field at all.

## Inbound request

```http
POST /api/oagw/v1/proxy/<alias>/echo HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json
Origin: https://app.example.com
```

## Expected response

- `200 OK` — the request proceeds normally; an `Origin` header alone does not trigger any CORS logic.
- No `Access-Control-*` response headers at all — not even a permissive default.

## What to check

- CORS is strictly opt-in per upstream/route (`cors.enabled: true`); there is no gateway-wide default policy.
- This applies to actual requests. A CORS **preflight** (`OPTIONS` with `Access-Control-Request-Method`) is still handled permissively at the handler level even without a `cors` config on the upstream — see [positive-10.2](positive-10.2-built-cors-handling.md) — because the preflight responder runs before upstream resolution and cannot yet see the upstream's config.
