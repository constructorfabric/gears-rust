# Answer missing and invisible secrets the same

## Setup

Apply [shared setup](README.md#shared-setup).

## Inbound request

```http
POST /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json
X-OAGW-Secret-Ref: cred://tenant-missing

{"model":"gpt-4o","messages":[]}
```

## Expected response

```http
HTTP/1.1 403 Forbidden
Content-Type: application/problem+json
X-OAGW-Error-Source: gateway
```

- `type` contains `permission_denied`. `context.reason` = `SECRET_REF_NOT_ACCESSIBLE`.
- The provider receives no request.

## What to check

- A missing secret, one private to another subject, and one owned by the sibling Other give identical bodies (SREF-RES-005).
- With a default configured, the default is not read in its place (SREF-GUARD-005).
- The SDK gets `PermissionDenied { reason: SecretRefNotAccessible }` (SREF-SDK-005). The body names no reference (SREF-OBS-001).
