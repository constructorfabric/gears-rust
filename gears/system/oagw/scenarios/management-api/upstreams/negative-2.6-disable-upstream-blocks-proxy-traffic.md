# Disable upstream blocks proxy traffic (including descendants)

## Step 1: Create upstream + route

Create upstream `alias=httpbin.org`, then route `POST /post`.

## Step 2: Disable upstream

```http
PUT /api/oagw/v1/upstreams/gts.cf.core.oagw.upstream.v1~<uuid> HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "id": "gts.cf.core.oagw.upstream.v1~<uuid>",
  "enabled": false,
  "alias": "httpbin.org",
  "server": {
    "endpoints": [
      { "scheme": "https", "host": "httpbin.org", "port": 443 }
    ]
  },
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1"
}
```

Expected: `200 OK`, `enabled=false`.

## Step 3: Invoke proxy

```http
POST /api/oagw/v1/proxy/httpbin.org/post HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{"name":"test"}
```

## Expected response

```http
HTTP/1.1 503 Service Unavailable
Content-Type: application/problem+json
Retry-After: 300
X-OAGW-Error-Source: gateway
```

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
  "title": "Service Unavailable",
  "status": 503,
  "detail": "Service temporarily unavailable",
  "instance": "/oagw/v1/proxy/httpbin.org/post",
  "context": { "retry_after_seconds": 300, "reason": "UPSTREAM_DISABLED" },
  "trace_id": "<trace id>"
}
```

- The upstream receives nothing.
- `context.reason` = `UPSTREAM_DISABLED` tells this 503 apart from a transport failure (`REQUEST_NOT_SENT`). The retry hint is long (300 s) because only an operator can enable the upstream again.
- The SDK gets `Unavailable { retry_after_secs: Some(300), reason: Some(UnavailableReason::UpstreamDisabled) }`.

## Descendant tenant variant

- Disable upstream at ancestor tenant.
- Descendant invoking the same alias is rejected as well.
