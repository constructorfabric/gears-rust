# Maximum body size limit enforced

Two limits apply to a proxied request body, and the smaller one wins:

- the api-gateway's `body_limit_bytes` (16 MiB by default), checked before OAGW sees the request;
- OAGW's `max_body_size_bytes` (default and maximum 100 MiB).

With both defaults the api-gateway limit is the smaller one, so OAGW's own `413` happens only when `max_body_size_bytes` is set below `body_limit_bytes` (the E2E configuration sets it to 1 MiB).

## Scenario A: body over OAGW's limit

`max_body_size_bytes` is smaller than `body_limit_bytes`. Send a body, or a `Content-Length`, larger than `max_body_size_bytes`.

```http
POST /api/oagw/v1/proxy/httpbin.org/post HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/octet-stream
Content-Length: <greater-than-max_body_size_bytes>

<binary>
```

Expected:

- `413 Payload Too Large`
- `Content-Type: application/problem+json`
- `X-OAGW-Error-Source: gateway`
- `type` = `...out_of_range...` (the 413 is a transport status override), one field violation on `body` with `reason` = `PAYLOAD_TOO_LARGE`; `detail` names the limit.
- A `Content-Length` over the limit is rejected before the body is read; a body without one is rejected while it is read.

## Scenario B: body over the api-gateway limit

Send a body larger than `body_limit_bytes`.

Expected:

- `413 Payload Too Large` from the api-gateway, before OAGW runs.
- `Content-Type: application/problem+json` with `type` = `about:blank`.
- No `X-OAGW-Error-Source` header: OAGW did not produce the response.
