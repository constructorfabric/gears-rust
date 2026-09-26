# Wildcard origin returns a literal '*' (actual request)

## Setup

```json
{
  "cors": {
    "enabled": true,
    "allowed_origins": ["*"],
    "allowed_methods": ["GET", "POST"],
    "allow_credentials": false
  }
}
```

## Inbound request

```http
POST /api/oagw/v1/proxy/<alias>/echo HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json
Origin: https://any-origin.com
```

## Expected response

- `200 OK`
- `Access-Control-Allow-Origin: *` — the literal wildcard, **not** the request's echoed origin (`https://any-origin.com`).
- `Vary: Origin` is still present, unconditionally, even though the `Allow-Origin` value itself (`*`) doesn't actually vary by origin in this case — `apply_cors_headers` pushes it regardless of which `Allow-Origin` branch was taken (see [positive-10.2](positive-10.2-built-cors-handling.md), same header for the non-wildcard case).

## What to check

- This differs from the non-wildcard case ([positive-10.2](positive-10.2-built-cors-handling.md)), where an allowed, explicit origin is echoed back verbatim as `Access-Control-Allow-Origin`.
- Wildcard `allowed_origins` cannot be combined with `allow_credentials: true` — rejected at config-validation time, see [negative-10.3](negative-10.3-cors-credentials-wildcard-rejected-config-validation.md).
