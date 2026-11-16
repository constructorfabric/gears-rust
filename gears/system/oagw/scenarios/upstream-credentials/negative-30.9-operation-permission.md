# Deny an operation before exposing items (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{
  "credential_action_use": false
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
  "status": 403
}
```

## What to check

- Existing operation permission context identifies a denial; do not add a credential-specific existence reason.
- Repeat with a missing name: same operation denial. No secret reads, debits or provider calls.
- For management, exercise create/read/override/delete independently, correct PDP resource type/UUID/owner and unsupported sub-tenant constraints. List scopes apply before pagination; unsupported scope returns AUTHZ_SCOPE_NOT_SUPPORTED.
