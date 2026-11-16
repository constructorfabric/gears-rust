# Hide absent and inaccessible items (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{
  "variants": [
    "name is missing",
    "name exists in another tenant",
    "name exists only under a different local upstream"
  ]
}
```

## Inbound request

```http
POST /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json
X-OAGW-Credential: missing

{"messages":[]}
```

## Expected response

```http
HTTP/1.1 404 Not Found
Content-Type: application/problem+json
X-OAGW-Error-Source: gateway

{
  "status": 404
}
```

## What to check

- All three variants have indistinguishable not-found categories and safe details.
- Preview has the same result. No secret reads, debits or provider calls.
- GET and DELETE of foreign names cannot expose or change foreign rows.
