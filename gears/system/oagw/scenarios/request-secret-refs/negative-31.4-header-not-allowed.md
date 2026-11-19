# Reject the header on an upstream that does not accept it

## Setup

Apply [shared setup](README.md#shared-setup), then:

```json
{"auth": {"config": {"secret_ref": "cred://tenant-openai"}}}
```

Remove `credential_mode`, so the upstream is in `config` mode.

## Inbound request

```http
POST /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json
X-OAGW-Secret-Ref: cred://tenant-openai

{"model":"gpt-4o","messages":[]}
```

## Expected response

```http
HTTP/1.1 400 Bad Request
Content-Type: application/problem+json
X-OAGW-Error-Source: gateway
```

- `type` contains `invalid_argument`. One field violation: `field` = `x-oagw-secret-ref`, `reason` = `SECRET_REF_NOT_ALLOWED`.
- The provider receives no request.

## What to check

- The same for an upstream whose auth uses another plugin, or that has no auth (SREF-CFG-006).
- An ancestor that enforces `config` mode wins over a descendant layer set to `request_ref` (SREF-HIER-002).
- Without the header the configured secret goes out exactly as before (SREF-CFG-001, SREF-REG-001); the stored config is unchanged.
- No CredStore read and no rate-limit debit (SREF-ERR-002). The SDK gets `InvalidSecretRef { code: NotAllowed }` (SREF-SDK-005).
