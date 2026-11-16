# Keep a newer suspension (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{
  "suspended": true,
  "pause_resume": "after external validation",
  "concurrent_operation": "repeat PATCH suspended=true"
}
```

## Inbound request

```http
PATCH /oagw/v1/upstreams/<upstream-id>/credentials/research HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json

{"suspended":false}
```

## Expected response

```http
HTTP/1.1 409 Conflict
Content-Type: application/problem+json

{
  "status": 409,
  "error_domain": "oagw.credential.v1",
  "error_code": "CREDENTIAL_VERSION_CONFLICT"
}
```

## What to check

- The repeated suspend returns 200 and increases the version. Release the paused resume: it fails and suspension remains.
- Run through REST and SDK with CRUD disabled. No provider call.
- Fresh resume fails without clearing suspension for disabled configuration, missing override grant, incompatible parent or pin mismatch. With all prerequisites restored, a new resume succeeds.
