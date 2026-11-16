# Suspend during an outage with CRUD disabled (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{
  "management_api_enabled": false,
  "credstore": "unavailable",
  "run_for_managed_by": [
    "api",
    "registry"
  ]
}
```

## Inbound request

```http
PATCH /oagw/v1/upstreams/<upstream-id>/credentials/research HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json

{"suspended":true}
```

## Expected response

200 OK. Validate the resource response or operation result described below.

## What to check

- PATCH and SDK suspend return enabled=true, suspended=true and the original managed_by. Neither reads CredStore.
- Repeat with incompatible parent auth and ancestor enforce: suspend still succeeds. Repeat an already suspended PATCH: its internal version increases again.
- A fresh named request and preview on another replica return 503 CREDENTIAL_SUSPENDED after dependencies/policies are restored.
- Replay registry configuration and finish an overlapping PUT: neither resumes the credential.
- CRUD writes stay disabled in REST and the new SDK capability. Suspend/resume still require override; a foreign tenant or denied manager cannot write.
