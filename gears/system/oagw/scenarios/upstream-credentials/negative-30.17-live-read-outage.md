# Fail a live read without stale credential fallback (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{
  "routing_cache": "warm",
  "credential_repository": "timeout"
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
  "error_code": "CREDENTIAL_CONFIG_UNAVAILABLE"
}
```

## What to check

- No CredStore reads, limiter debits or provider calls, including preview.
- Omitted-selector traffic may continue using ADR-0020 stale routing. A warm routing cache is insufficient for named traffic when its live credential read fails.
