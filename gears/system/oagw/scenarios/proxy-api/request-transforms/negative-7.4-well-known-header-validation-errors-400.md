# Well-known header validation: Content-Length

## Scenario A: invalid Content-Length

```http
POST /api/oagw/v1/proxy/httpbin.org/post HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json
Content-Length: not-a-number

{"a":1}
```

Expected over REST: hyper rejects the framing before OAGW runs.
- `400 Bad Request` from hyper, with an empty body
- No `Content-Type: application/problem+json`, no `X-OAGW-Error-Source`

Expected for an in-process SDK request with the same header: a gateway `400` `invalid_argument` on field `content-length`.

## Scenario B: Content-Length mismatch

```http
POST /api/oagw/v1/proxy/httpbin.org/post HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json
Content-Length: 999

{"a":1}
```

Expected over REST (the client ends the body early, so reading it fails;
the request head may already have reached the upstream, which gets no
complete request):
- `400 Bad Request`, not `413`: only the size limit is `413 PayloadTooLarge`
- `Content-Type: application/problem+json`
- `X-OAGW-Error-Source: gateway`
- field violation reason `BODY_READ_FAILED`

Expected for an in-process SDK `Body::Stream` of another length than its
`Content-Length`: `400`, field `content-length`, reason
`INVALID_CONTENT_LENGTH`. A `Body::Bytes` is sent with its own length; the
header is corrected.
