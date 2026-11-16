# Reject duplicate selector (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{}
```

## Inbound request

```http
POST /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json
X-OAGW-Credential: research
X-OAGW-Credential: research

{"messages":[]}
```

## Expected response

```http
HTTP/1.1 400 Bad Request
Content-Type: application/problem+json
X-OAGW-Error-Source: gateway

{
  "status": 400,
  "error_domain": "oagw.credential.v1",
  "error_code": "INVALID_CREDENTIAL_SELECTOR"
}
```

## What to check

- Assert the invalid selector field violation as well as the stable error code. No secret reads, limiter debits or provider calls.
- The error never echoes raw selector input. The syntax check also applies through SDK proxy; preview rejects an invalid name.
