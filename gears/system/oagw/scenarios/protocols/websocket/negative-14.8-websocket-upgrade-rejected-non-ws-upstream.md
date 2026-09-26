# WebSocket upgrade rejected for a non-WebSocket upstream endpoint

## Setup

Create a route pointing at a plain HTTP endpoint on the upstream (one that does not implement the WebSocket handshake) — e.g. `/echo`.

## Inbound request

A WebSocket upgrade handshake against that route:

```http
GET /api/oagw/v1/proxy/<alias>/echo HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Connection: Upgrade
Upgrade: websocket
Sec-WebSocket-Key: <key>
Sec-WebSocket-Version: 13
```

## Expected response

- The upstream responds to the upgrade attempt with a non-`101` status (typically its normal `404`/`405`/`200` for that path).
- OAGW does not propagate that response to the client. It discards the upstream's real status and body and returns its own `503 Service Unavailable` (`problem+json`, canonical `service_unavailable`, `ProtocolError`) with `X-OAGW-Error-Source: gateway` — the WebSocket handshake fails on the client side with a gateway-originated error, not the upstream's actual status.

## What to check

- OAGW does not assume every proxied route supports WebSocket; the handshake only succeeds end-to-end when the upstream itself completes it (`101`). See [positive-14.1](positive-14.1-websocket-upgrade-proxied.md) for the successful-upgrade case.
- The client never sees the upstream's real non-`101` status, headers, or body — only the fixed `503` shape. `X-OAGW-Error-Source` reads `gateway`, not `upstream`, for this response.
