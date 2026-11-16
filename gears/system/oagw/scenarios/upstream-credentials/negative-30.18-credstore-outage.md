# Keep attempt debits on a secret failure (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{
  "credstore": "unavailable"
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
HTTP/1.1 503 Service Unavailable
Content-Type: application/problem+json
X-OAGW-Error-Source: gateway

{
  "status": 503,
  "error_domain": "oagw.credential.v1",
  "error_code": "CREDENTIAL_STORE_UNAVAILABLE"
}
```

## What to check

- Exactly one CredStore read and no provider call; research's admission token remains consumed.
- A second request while the clock is fixed gets gateway 429 before another CredStore read.
- A missing or inaccessible secret instead returns CREDENTIAL_SECRET_UNAVAILABLE (503), with repair guidance. None of these dependency failures becomes 401.
