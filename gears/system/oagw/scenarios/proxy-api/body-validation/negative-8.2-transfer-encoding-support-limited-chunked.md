# Transfer-Encoding validation

## Scenario: unsupported transfer encoding

```http
POST /api/oagw/v1/proxy/httpbin.org/post HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json
Transfer-Encoding: gzip

{"a":1}
```

## Expected response

Over REST hyper rejects the framing before OAGW runs:

- `400 Bad Request` from hyper, with an empty body
- No `Content-Type: application/problem+json`, no `X-OAGW-Error-Source`

For an in-process SDK request with the same header: a gateway `400`
`invalid_argument` on field `transfer-encoding`, reason `UNSUPPORTED_VALUE`,
whose `detail` mentions the unsupported transfer encoding.

## Scenario: chunked supported

```http
POST /api/oagw/v1/proxy/httpbin.org/post HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json
Transfer-Encoding: chunked

<chunked-body>
```

Expected:
- Request is accepted and streamed to the upstream as it arrives, chunked
  (HTTP/1.1) or as DATA frames (HTTP/2).
