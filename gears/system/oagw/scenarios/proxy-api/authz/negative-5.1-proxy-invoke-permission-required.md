# Proxy invoke permission required

## Inbound request

```http
POST /api/oagw/v1/proxy/httpbin.org/post HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <token-without-proxy-invoke>
Content-Type: application/json

{"name":"test"}
```

## Expected response

- `403 Forbidden`
- If body is present:
  - `Content-Type: application/problem+json`
  - `X-OAGW-Error-Source: gateway`
- `detail` indicates missing proxy invoke permission.
- The permission is checked before the request is read or validated: an oversized `Content-Length`, a bad `Content-Type`, an aborted upload, or a WebSocket upgrade with a body header from a caller without the permission still gets `403`, not `413` or `400`.
