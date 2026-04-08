# All backends unreachable returns 502 or 504

## Setup

- Upstream `my-service` with multiple endpoints, all pointing to unreachable addresses:
  - `endpoints`: `[{"scheme": "https", "host": "10.0.0.1", "port": 19999}, {"scheme": "https", "host": "10.0.0.2", "port": 19999}]`
  - `alias`: `my-service`
- Route: `POST /v1/chat`

## Request

```http
POST /api/oagw/v1/proxy/my-service/v1/chat HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <token>
Content-Type: application/json

{"prompt": "hello"}
```

## Expected behavior

- All connection attempts fail (connection refused or timeout)
- Response: `502 Bad Gateway` or `504 Gateway Timeout`
- Error source: `X-OAGW-Error-Source: gateway`
- Body: RFC 9457 Problem Details

```http
HTTP/1.1 502 Bad Gateway
X-OAGW-Error-Source: gateway
Content-Type: application/problem+json

{
  "type": "gts.x.core.errors.err.v1~x.oagw.proxy.downstream_error.v1",
  "title": "Downstream Error",
  "status": 502,
  "detail": "Connection refused or timed out to all configured endpoints",
  "instance": "/api/oagw/v1/proxy/my-service/v1/chat"
}
```

## Validation

- Multi-endpoint upstream with all endpoints unreachable returns gateway error
- The selected endpoint's connection attempt fails and the error is returned immediately (OAGW does not retry)
- No partial or hanging response — client receives a clear error
