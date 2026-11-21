# Transport failure after sending: `REQUEST_MAY_HAVE_BEEN_SENT`

When the exchange fails after the request, or part of it, may have reached the upstream, the gateway error carries `context.reason` = `REQUEST_MAY_HAVE_BEEN_SENT`. Retry only an idempotent request.

## Setup

The upstream `api.example.com` accepts the connection, reads the request and closes the connection without an answer. A route `GET /drop` points to it.

## Inbound request

```http
GET /api/oagw/v1/proxy/api.example.com/drop HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

## Expected response

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
  "instance": "/oagw/v1/proxy/api.example.com/drop",
  "context": { "retry_after_seconds": 5, "reason": "REQUEST_MAY_HAVE_BEEN_SENT" },
  "trace_id": "<trace id>"
}
```

- The gateway does not resend the request ([12.6](negative-12.6-no-automatic-retries-upstream-failures.md)).
- The SDK gets `Unavailable { retry_after_secs: Some(5), reason: Some(UnavailableReason::RequestMayHaveBeenSent) }`.

## Same reason, other cases

| Case | Status |
|---|---|
| Connection closed by the upstream before a complete response | 502 |
| Read or write error on the exchange | 502 |
| HTTP/2 error, invalid or over-limit response | 502 |
| Upstream rejected a WebSocket upgrade (no `101`) | 502 |
| Streamed request body failed mid-stream | 502 |
| Read or write timeout, request timeout, WebSocket upgrade timeout | 504 `deadline_exceeded` |
| Any engine error kind added later | 502 |

A read timeout (the upstream does not answer within `proxy_timeout_secs`) returns `504` with `type` `...deadline_exceeded...`, `context.resource_type` = `gts.cf.core.oagw.proxy.v1~` and `context.reason` = `REQUEST_MAY_HAVE_BEEN_SENT`; the SDK gets `Timeout { reason: Some(TimeoutReason::RequestMayHaveBeenSent) }`.
