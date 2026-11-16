# Select accounts on one route (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{
  "reset_between_calls": true
}
```

## Inbound request

```http
POST /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json
X-OAGW-Credential: research

{"messages":[]}
```

## Expected response

200 OK. The mock returns `{"ok":true}`.

## Expected outbound request

```http
POST /v1/chat/completions HTTP/1.1
Host: api.example.com
Content-Type: application/json
X-Api-Key: key-A

{"messages":[]}
```

## What to check

- Send the request with research, production and no selector. The mock receives key-A, key-B and key-D, in that order, on the same route with the same body.
- Compare the whole no-selector request and limiter behavior to the legacy baseline. Repeat named selection for SSE and WebSocket handshakes; each counts once.
- With passthrough all and a transform reintroducing the selector, the mock still sees no selector. A rule replacing, removing or duplicating the chosen auth value prevents forwarding.
