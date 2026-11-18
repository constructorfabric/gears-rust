# Refuse a secret of a type the upstream does not allow

## Setup

Apply [shared setup](README.md#shared-setup), then:

```json
{"auth": {"config": {"credential_mode": "request_ref", "request_secret_types": "<api-key GTS type id>"}}}
```

## Inbound request

```http
POST /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json
X-OAGW-Secret-Ref: cred://tenant-db

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

- `tenant-openai` (api-key) is injected; `tenant-db` (connection-string) is refused (SREF-GUARD-004).
- Without `request_secret_types` any type passes.
