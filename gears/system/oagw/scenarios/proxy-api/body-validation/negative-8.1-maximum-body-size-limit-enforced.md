# Maximum body size limit enforced

Two limits apply to a proxied request body, and the smaller one wins:

- the api-gateway's `body_limit_bytes` (16 MiB by default): a declared `Content-Length` is checked before OAGW sees the request, a body without one is cut as it streams;
- OAGW's `max_body_size_bytes` (default and maximum 100 MiB).

With both defaults the api-gateway limit is the smaller one. A declared length over it gets the api-gateway's `413`; a body without a declared length gets OAGW's own `413` at either limit (Scenario C). The E2E configuration sets `max_body_size_bytes` to 1 MiB.

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

## Scenario B: declared length over the api-gateway limit

Send a body with a `Content-Length` larger than `body_limit_bytes`.

Expected:

- `413 Payload Too Large` from the api-gateway, before OAGW runs.
- `Content-Type: application/problem+json` with `type` = `about:blank`.
- No `X-OAGW-Error-Source` header: OAGW did not produce the response.

## Scenario C: streamed body over the api-gateway limit

Send a chunked body (no `Content-Length`) that grows past `body_limit_bytes`, which is smaller than `max_body_size_bytes`.

Expected:

- The api-gateway cuts the body as it streams; OAGW reads the cut and answers as in Scenario A: `413`, `X-OAGW-Error-Source: gateway`, reason `PAYLOAD_TOO_LARGE`.
- If the upstream's response head arrived before the cut, that response is relayed instead and the cut is logged.
