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
- Permissive CORS headers echoed from the request (not validated against upstream config — this short-circuit runs unconditionally for any `OPTIONS` request carrying both `Origin` and `Access-Control-Request-Method`, before upstream lookup, regardless of whether the target upstream has `cors` configured at all; see [positive-10.5](positive-10.5-cors-disabled-by-default.md)):
  - `Access-Control-Allow-Origin: https://app.example.com`
  - `Access-Control-Allow-Methods: POST`
  - `Access-Control-Allow-Headers: Content-Type, Authorization`
  - `Access-Control-Allow-Credentials: true` — always `true` here, unconditionally, since the resolved upstream's `allow_credentials` setting isn't known yet at this stage.
  - `Access-Control-Max-Age: 86400`
  - `Vary: Origin, Access-Control-Request-Method, Access-Control-Request-Headers`
- The preflight echoes back *any* requested method, even one the target route doesn't actually allow (e.g. `DELETE` on a `GET`-only route) — it is not validated against route/upstream config at this stage.

## Actual request with allowed origin

```http
POST /api/oagw/v1/proxy/<alias>/resource HTTP/1.1
Host: oagw.example.com
Origin: https://app.example.com
Authorization: Bearer <token>
```

Expected (with `cors.expose_headers: ["x-request-id"]` configured):

- `200 OK`
- `Access-Control-Allow-Origin: https://app.example.com` (the configured, non-wildcard origin echoed verbatim — see [positive-10.6](positive-10.6-cors-wildcard-origin-response.md) for the `allowed_origins: ["*"]` case, which returns the literal `*` instead)
- `Access-Control-Expose-Headers: x-request-id`
- `Access-Control-Allow-Credentials: true` — present here because *this* Setup's `allow_credentials: true`; unlike the preflight response above, this one is conditional on the resolved upstream's config (see [ADR-0006](../../../docs/ADR/0006-cors.md#actual-request-handling)) and would be absent entirely if `allow_credentials` were `false` or unset.
- `Vary: Origin`

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

- Preflight is handled at the handler level (no upstream resolution, no tenant context required).
- Origin enforcement happens on the actual request after upstream resolution.
- Disallowed origins never reach the upstream.
- Without any `cors` config on the upstream, actual requests get no `Access-Control-*` headers at all (only the preflight short-circuit above is unconditional) — see [positive-10.5](positive-10.5-cors-disabled-by-default.md).
