# Keep the debit on a CredStore outage

## Setup

Apply [shared setup](README.md#shared-setup), then:

The upstream has a rate limit of one request per minute; CredStore is down, or does not answer within the gateway request timeout.

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
HTTP/1.1 503 Service Unavailable
Content-Type: application/problem+json
X-OAGW-Error-Source: gateway
```

- `type` contains `service_unavailable`. No `context.reason`.
- The provider receives no request.

## What to check

- A second request gets 429 before any CredStore read: the first debit stays (SREF-ERR-004). With CredStore up, an exhausted limit gets 429 with no CredStore read (SREF-ERR-003).
- The SDK gets `Unavailable` (SREF-SDK-005).
- When CredStore's outage carries a retry hint, `context.retry_after_seconds` is that hint rounded up; otherwise it is the gateway default (SREF-ERR-004).
- A CredStore error only a write can cause (conflict, type violation, unsupported transition) returns 500 with no retry hint (SREF-ERR-004).
- The gateway log line names the caller tenant, upstream, route and request IDs, and not the reference (SREF-OBS-004).
