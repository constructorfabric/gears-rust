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

## Scenario C: repeated client headers with an auth plugin

### Setup

Create an upstream with `"headers": {"request": {"passthrough": "all"}}` and apikey auth (`"header": "authorization"`, `"prefix": "Bearer "`, a `secret_ref` the credential store holds), and a route `POST /echo-lines`. Run it again with the request_id transform (`gts.cf.core.oagw.transform_plugin.v1~cf.core.oagw.request_id.v1`) bound on the upstream.

### Inbound request

```http
POST /api/oagw/v1/proxy/<alias>/echo-lines HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
X-Multi: a
X-Multi: b
```

### Expected behavior

- The upstream receives both `X-Multi` lines, `a` then `b`.
- It receives one `Authorization` line, the injected `Bearer <secret>`; the client's line is replaced.
- A non-UTF-8 header value passes through unchanged too (Rust test; the e2e client sends text headers).
- With the transform bound, the same holds, and the upstream also receives one generated `X-Request-ID`. A transform rewrites only the headers it changes: the credential keeps its exact bytes (an obs-text secret such as `sk-café` included) and stays marked sensitive, and repeated and non-UTF-8 lines it does not touch pass unchanged. The same holds for response headers (Rust tests).
- If the auth plugin produces a header name or value that is not valid, the request fails with `500` (plugin misconfiguration) instead of being sent without credentials.
