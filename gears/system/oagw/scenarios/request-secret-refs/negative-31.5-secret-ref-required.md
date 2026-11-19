# Require a reference when there is no default

## Setup

Apply [shared setup](README.md#shared-setup).

## Inbound request

```http
POST /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json

{"model":"gpt-4o","messages":[]}
```

## Expected response

```http
HTTP/1.1 400 Bad Request
Content-Type: application/problem+json
X-OAGW-Error-Source: gateway
```

- `type` contains `invalid_argument`. One field violation: `field` = `x-oagw-secret-ref`, `reason` = `SECRET_REF_REQUIRED`.
- The provider receives no request.

## What to check

- No CredStore read and no rate-limit debit (SREF-ERR-002).
- The SDK gets `InvalidSecretRef { code: Required }` (SREF-SDK-005); REST and SDK give the same status, category, reason and provider call count (SREF-SDK-003).
