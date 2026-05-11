# Guardrail: `Accept: application/json` on :stream is rejected

`:stream` serves only `multipart/mixed`. A client whose `Accept` header excludes it gets `406 Not Acceptable` with the supported types listed. (Guardrail: a caller reasonably expects content negotiation, so the boundary is documented explicitly.)

## Setup

- Run [Cold JOIN](../subscriptions/positive-2.1-cold-join-fresh-group.md) + [SEEK](../positions/positive-3.1-pre-stream-seek-earliest.md) → seeded `{sub_id}`.

## Request

```http
GET /v1/events:stream?subscription_id={sub_id} HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Accept: application/json
```

## Expected response

- `406 Not Acceptable` (`PD`)
- Body lists the supported media types.

```json
{
  "type": "https://errors.cf.core/events/not-acceptable",
  "title": "NotAcceptable",
  "status": 406,
  "detail": "this endpoint serves multipart/mixed only",
  "instance": "/v1/events:stream",
  "supported": ["multipart/mixed"]
}
```

## Side effects

- No stream established. (`Accept: */*` or `Accept: multipart/mixed` would succeed.)
