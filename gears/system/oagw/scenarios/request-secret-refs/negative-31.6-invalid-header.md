# Reject an empty, malformed, repeated or comma-separated header

## Setup

Apply [shared setup](README.md#shared-setup).

## Inbound request

```http
POST /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json
X-OAGW-Secret-Ref: cred://tenant-openai
X-OAGW-Secret-Ref: cred://tenant-openai

{"model":"gpt-4o","messages":[]}
```

## Expected response

```http
HTTP/1.1 400 Bad Request
Content-Type: application/problem+json
X-OAGW-Error-Source: gateway
```

- `type` contains `invalid_argument`. One field violation: `field` = `x-oagw-secret-ref`, `reason` = `INVALID_SECRET_REF`.
- The provider receives no request.

## What to check

- Each of these gives the same answer (SREF-REQ-003): an empty value, `cred://` alone, `has:colon`, `two words`, `a/b`, `a,b`, two different values, and two identical values (the request above).
- A caller without `proxy:invoke` gets the existing 403 first, and a bad header on an unknown alias gets the existing 404 (SREF-ERR-001).
- No CredStore read and no rate-limit debit (SREF-ERR-002).
- A reference in the query string is not read: it is forwarded or refused as any query parameter is (SREF-REQ-005).
