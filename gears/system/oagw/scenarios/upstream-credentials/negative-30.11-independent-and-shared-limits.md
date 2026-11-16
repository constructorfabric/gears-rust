# Reject an exhausted credential before CredStore (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{
  "clock": "fixed",
  "research_requests_per_minute": 1
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
HTTP/1.1 429 Too Many Requests
Content-Type: application/problem+json
X-OAGW-Error-Source: gateway

{
  "status": 429
}
```

## What to check

- The first request gets 200 with key-A. The second gets gateway 429, quota details, rate headers and Retry-After; it makes no CredStore read or provider call.
- Production still works. Exhaust an upstream limit, then use production: its credential bucket keeps its unused capacity.
- Exhaust the route bucket separately: earlier upstream/credential buckets retain debits.
- Assert refill, smallest remaining count and first-bucket tie precedence. A provider 429 retains upstream error source and causes no automatic retry.
