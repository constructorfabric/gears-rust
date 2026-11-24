# Transport failure before sending: `REQUEST_NOT_SENT`

A gateway 502, 503 or 504 from the upstream exchange says in `context.reason` whether the request may have reached the upstream. When nothing was sent, the reason is `REQUEST_NOT_SENT` and a retry is safe for any method.

## Setup

The upstream `api.example.com` has one endpoint on a port where nothing listens, and a route `GET /v1/models`.

## Inbound request

```http
GET /api/oagw/v1/proxy/api.example.com/v1/models HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

## Expected response

The connection is refused:

```http
HTTP/1.1 502 Bad Gateway
Content-Type: application/problem+json
Retry-After: 5
X-OAGW-Error-Source: gateway
```

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
  "title": "Service Unavailable",
  "status": 502,
  "detail": "Service temporarily unavailable",
  "instance": "/oagw/v1/proxy/api.example.com/v1/models",
  "context": { "retry_after_seconds": 5, "reason": "REQUEST_NOT_SENT" },
  "trace_id": "<trace id>"
}
```

- The 502 is the canonical `service_unavailable` category with the transport status override.
- The SDK gets `Unavailable { retry_after_secs: Some(5), reason: Some(UnavailableReason::RequestNotSent) }`.

## Same reason, other cases

| Case | Status | `Retry-After` |
|---|---|---|
| Connection refused | 502 | 5 |
| TLS handshake failure or timeout (for example an `https` endpoint that speaks cleartext HTTP), invalid certificate | 502 | 5 |
| DNS failure, every resolved address refused by the SSRF policy, no route, other connect error | 503 | 10 |
| Connect timeout | 504 `deadline_exceeded` | |
| Endpoint selection, resolution or signing timed out | 504 `deadline_exceeded` | |
| Circuit breaker open ([18.9](../../rate-limiting/negative-18.9-circuit-opens-after-consecutive-failures.md)) | 503 | until the probe |
| Upstream without endpoints, outbound request that cannot be built | 502 | 5 |
| Identity provider of an auth plugin answered with an invalid response | 502 | 5 |

A 504 body has `context.resource_type` = `gts.cf.core.oagw.proxy.v1~` and the same `reason`; the SDK gets `Timeout { reason: Some(TimeoutReason::RequestNotSent) }`.
