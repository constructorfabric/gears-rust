# Guardrail: SSE is not served from :stream

SSE (`text/event-stream`) is available only at the dedicated `:sse` endpoint. Requesting it from `:stream` returns `406` — the two transports are distinct paths, not content-negotiated variants of one endpoint.

## Setup

- Run [Cold JOIN](../subscriptions/positive-2.1-cold-join-fresh-group.md) + [SEEK](../positions/positive-3.1-pre-stream-seek-earliest.md) → seeded `{sub_id}`.

## Request

```http
GET /v1/events:stream?subscription_id={sub_id} HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Accept: text/event-stream
```

## Expected response

- `406 Not Acceptable` (`PD`)
- Body lists `multipart/mixed` as the supported type and points to `:sse` for SSE.

```json
{
  "type": "https://errors.cf.core/events/not-acceptable",
  "title": "NotAcceptable",
  "status": 406,
  "detail": "SSE is served only at /v1/events:sse; this endpoint serves multipart/mixed",
  "instance": "/v1/events:stream",
  "supported": ["multipart/mixed"]
}
```

## Side effects

- No stream established. For SSE, the client must use [`GET /v1/events:sse`](positive-6.9-sse-event-stream.md).
