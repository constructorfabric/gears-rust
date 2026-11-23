# Built-in CORS handling (preflight + actual request)

## Setup

Enable CORS on upstream or route:

```json
{
  "cors": {
    "enabled": true,
    "allowed_origins": ["https://app.example.com"],
    "allowed_methods": ["GET", "POST"],
    "allow_credentials": true
  }
}
```

## Preflight request

```http
OPTIONS /api/oagw/v1/proxy/<alias>/resource HTTP/1.1
Host: oagw.example.com
Origin: https://app.example.com
Access-Control-Request-Method: POST
Access-Control-Request-Headers: Content-Type, Authorization
```

## Expected preflight response

- `204 No Content`
- Permissive CORS headers echoed from the request (not validated against upstream config):
  - `Access-Control-Allow-Origin: https://app.example.com`
  - `Access-Control-Allow-Methods: POST`
  - `Access-Control-Allow-Headers: Content-Type, Authorization`
  - `Access-Control-Max-Age: 86400`
  - `Vary: Origin, Access-Control-Request-Method, Access-Control-Request-Headers`

## Actual request with disallowed origin

```http
POST /api/oagw/v1/proxy/<alias>/resource HTTP/1.1
Host: oagw.example.com
Origin: https://evil.com
Authorization: Bearer <token>
```

## Expected actual request response

- `403 Forbidden` — origin rejected before reaching upstream.

## What to check

- Preflight is handled at the handler level (no upstream resolution, no tenant context required). A preflight is an `OPTIONS` with both `Origin` and `Access-Control-Request-Method`; any other `OPTIONS` is routed and proxied when the route lists `OPTIONS` ([12.9](../../protocols/http/positive-12.9-options-routed-preflight-answered-gateway.md)).
- Origin enforcement happens on the actual request after upstream resolution.
- Disallowed origins never reach the upstream.
