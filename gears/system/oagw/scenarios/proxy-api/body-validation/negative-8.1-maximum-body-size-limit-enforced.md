# Maximum body size limit enforced (100MB)

## Inbound request

Send a request body larger than 100MB.

```http
POST /api/oagw/v1/proxy/httpbin.org/post HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/octet-stream
Content-Length: <greater-than-100mb>

<binary>
```

## Expected response

- `400 Bad Request` — **not** `413`. `DomainError::PayloadTooLarge` maps to the canonical `OutOfRange` category, which is wire status `400` (`title: "Out of Range"`), per an explicit migration (`oagw/src/api/rest/error.rs`, pinned by the unit test `payload_too_large_now_maps_to_400`: "wire change accepted in the migration plan: 413 → 400"). The domain concept is still "payload too large"; only the HTTP status code changed.
- `Content-Type: application/problem+json`
- `type: "gts://gts.cf.core.errors.err.v1~cf.core.err.out_of_range.v1~"`
- `X-OAGW-Error-Source: gateway`
- Field violation on `body` (`field.PAYLOAD_TOO_LARGE`).
- Rejection happens before buffering the entire body (lock via memory/latency instrumentation if available).

## Related

This is OAGW's own logical cap. In a full deployment fronted by an outer api-gateway, a lower outer-layer limit fires first and produces a different (plain-text, non-PD) `413` — see [negative-8.4](negative-8.4-body-size-limit-enforced-outer-gateway-layer.md).
