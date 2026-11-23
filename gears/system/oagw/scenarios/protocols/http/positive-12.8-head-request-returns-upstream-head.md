# HEAD returns the upstream's head without a body

## Setup

Create an HTTP upstream (example: `alias=api.example.com`) and a route that lists `HEAD`:

```json
{
  "match": {
    "http": {
      "methods": ["GET", "HEAD"],
      "path": "/v1/doc"
    }
  }
}
```

A route matches only the methods it lists: `GET` does not imply `HEAD`.

## Scenario A: HEAD is proxied

```http
HEAD /api/oagw/v1/proxy/api.example.com/v1/doc HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

### Expected behavior

- The upstream receives `HEAD /v1/doc`.
- The client gets the upstream's status and headers, `Content-Length` included (the length of the representation, not of an empty body), `X-OAGW-Error-Source: upstream`, and no body.
- The response is not held open waiting for a body.

## Scenario B: HEAD on an event stream

The route lists `HEAD` on a path whose upstream answers `Content-Type: text/event-stream`.

### Expected behavior

- `200` with `Content-Type: text/event-stream` and no body; the response ends at its head instead of waiting for events.

## Scenario C: no implicit HEAD

The route lists `GET` only.

```http
HEAD /api/oagw/v1/proxy/api.example.com/v1/doc HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

### Expected behavior

- `404` (route not found), `X-OAGW-Error-Source: gateway`; the upstream is not called.

## What to check

- A `HEAD` with WebSocket upgrade headers is refused with `400` (`WS_UPGRADE_REQUIRES_GET`), not tunnelled (Rust test).
- `oagw_requests_total` counts the request under `http.request.method="HEAD"`; no stream duration is recorded for its empty body.
