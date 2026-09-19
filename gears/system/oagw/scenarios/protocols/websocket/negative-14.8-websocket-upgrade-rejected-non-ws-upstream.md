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
- OAGW propagates that non-`101` response rather than fabricating a `101 Switching Protocols` — the WebSocket handshake fails on the client side with the upstream's actual status.

## What to check

- OAGW does not assume every proxied route supports WebSocket; the handshake only succeeds end-to-end when the upstream itself completes it (`101`). See [positive-14.1](positive-14.1-websocket-upgrade-proxied.md) for the successful-upgrade case.
