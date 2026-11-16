# Reject incompatible parent auth (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{
  "parent_auth": "change to noop after create",
  "routing_cache": "reload changed parent"
}
```

## Inbound request

```http
POST /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json
X-OAGW-Credential: research

{"messages":[]}
```

## Expected response

```http
HTTP/1.1 403 Forbidden
Content-Type: application/problem+json
X-OAGW-Error-Source: gateway

{
  "status": 403,
  "error_domain": "oagw.credential.v1",
  "error_code": "CREDENTIAL_POLICY_DENIED"
}
```

## What to check

- Proxy and preview fail with no secret read, debit or provider call.
- PUT/resume fails validation and keeps suspension. Suspend still works.
