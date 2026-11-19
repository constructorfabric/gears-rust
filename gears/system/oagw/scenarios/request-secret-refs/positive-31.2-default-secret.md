# Use the configured default without a header

## Setup

Apply [shared setup](README.md#shared-setup), then:

```json
{"auth": {"config": {"credential_mode": "request_ref", "secret_ref": "cred://tenant-openai"}}}
```

## Inbound request

```http
POST /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json

{"model":"gpt-4o","messages":[]}
```

## Expected response

200 OK.

## Expected outbound request

```http
POST /v1/chat/completions HTTP/1.1
Host: api.example.com
Content-Type: application/json
Authorization: Bearer key-T

{"model":"gpt-4o","messages":[]}
```

## What to check

- The mode table holds row by row (SREF-REQ-001): with a header the requested secret goes out; without one, the default.
- Writing the upstream checks the default as any `secret_ref` is checked: a default that does not resolve is refused with 400 `...failed_precondition...` on `auth.config.secret_ref` (SREF-CFG-005).
- `request_ref` without `secret_ref` is accepted on write (SREF-CFG-004).
- The owner-chain and type guards apply to the default too (SREF-GUARD-006): a default owned by Root on Partner's upstream, or of a type outside `request_secret_types`, is refused with 403 `SECRET_REF_POLICY_DENIED`.
