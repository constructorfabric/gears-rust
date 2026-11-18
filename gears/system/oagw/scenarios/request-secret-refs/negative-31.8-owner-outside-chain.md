# Refuse a secret owned above the auth owner

## Setup

Apply [shared setup](README.md#shared-setup).

## Inbound request

```http
POST /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json
X-OAGW-Secret-Ref: cred://partner-openai

{"model":"gpt-4o","messages":[]}
```

## Expected response

```http
HTTP/1.1 403 Forbidden
Content-Type: application/problem+json
X-OAGW-Error-Source: gateway
```

- `type` contains `permission_denied`. `context.reason` = `SECRET_REF_POLICY_DENIED`.
- The provider receives no request.

## What to check

- Tenant owns the upstream, so only Tenant's secrets pass: `partner-openai` and `platform-openai` are refused and `tenant-openai` is injected (SREF-GUARD-002).
- With a default configured, the default is not read in its place (SREF-GUARD-005).
- The SDK gets `PermissionDenied { reason: SecretRefPolicyDenied }` (SREF-SDK-005).
