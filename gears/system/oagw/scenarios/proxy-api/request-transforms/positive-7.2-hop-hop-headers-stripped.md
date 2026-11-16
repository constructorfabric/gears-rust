# Hop-by-hop headers are stripped

## Inbound request

```http
GET /api/oagw/v1/proxy/httpbin.org/get HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Connection: keep-alive, Upgrade
Upgrade: websocket
TE: trailers
Transfer-Encoding: chunked
Keep-Alive: timeout=5
Trailer: X-Checksum
Proxy-Authorization: Basic <credentials>

```

## Expected behavior

- Gateway strips hop-by-hop headers when building outbound request:
  - `Connection`
  - `Upgrade`
  - `TE`
  - `Transfer-Encoding`
  - `Keep-Alive`
  - `Trailer`
  - `Proxy-Authorization`
- Outbound request uses correct upstream scheme and does not attempt protocol upgrades.

## What to check

- Upstream echo endpoint does not receive the stripped headers.
- Response is successful (or a gateway validation error if request is malformed; lock expected behavior).
- The same names are stripped when an upstream header rule or a transform plugin sets them.
- Every `Connection` field line counts: a header nominated on a second `Connection` line is stripped too.
- Under `"passthrough": "allowlist"`, a header the allowlist names is still stripped when the client nominates it in `Connection`, even if the allowlist does not name `Connection` itself. A WebSocket upgrade keeps `Upgrade`.

## Scenario B: client forwarding headers under `passthrough: all`

### Setup

Create an upstream with `"headers": {"request": {"passthrough": "all"}}` and a route `GET /get`.

### Inbound request

```http
GET /api/oagw/v1/proxy/httpbin.org/get HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Forwarded: for=203.0.113.7
Via: 1.1 client-proxy
X-Forwarded-For: 203.0.113.7
X-Real-IP: 203.0.113.7
X-HTTP-Method-Override: DELETE
X-Custom: kept
```

### Expected behavior

- The upstream receives `X-Custom: kept`.
- It receives none of `Forwarded`, `Via`, `X-Forwarded-*`, `X-Real-IP`, `X-HTTP-Method-Override` (nor the other forwarding and override headers DESIGN lists).
- With `"passthrough": "allowlist"` and `"passthrough_allowlist": ["x-forwarded-for"]`, `X-Forwarded-For` is forwarded.
