# Refuse a request whose auth header a rule or transform set

## Setup

Apply [shared setup](README.md#shared-setup), then:

```json
{"headers": {"request": {"set": {"Authorization": "Bearer other"}}}}
```

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
HTTP/1.1 500 Internal Server Error
Content-Type: application/problem+json
X-OAGW-Error-Source: gateway
```

- `type` contains `internal`. No `context.reason`: this is a gateway misconfiguration, not a caller-side denial, so it does not use `SECRET_REF_POLICY_DENIED`.
- The provider receives no request.

## What to check

- OAGW strips the auth header before header rules and transforms run and injects the key after them, so they never see the key.
- A rule or transform that sets the auth header (SREF-HDR-002) or adds a value to it (SREF-HDR-003) is refused with 500, and the gateway warns with the upstream and route to fix.
- A rule or transform that removes it changes nothing: the key is still injected and the request is forwarded (SREF-HDR-001).
- A transform that copies the auth header into the query finds no key to copy (SREF-HDR-006).
- Rules and transforms that leave the auth header alone still apply and the request is forwarded (SREF-HDR-005).
