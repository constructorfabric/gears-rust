# Request without Authorization header → 401

All event-broker endpoints require a valid Bearer token. A request with no `Authorization` header is rejected before any resource lookup.

## Request

```http
POST /v1/events HTTP/1.1
Host: broker.example.com
Content-Type: application/json

{ "id": "...", "type": "...", "topic": "...", "tenant_id": "...", "source": "x", "subject": "y", "subject_type": "...", "occurred_at": "2026-06-15T10:00:00Z", "data": {} }
```

## Expected response

- `401 Unauthenticated`

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~",
  "title": "Unauthenticated",
  "status": 401,
  "detail": "Authorization header is missing",
  "instance": "/v1/events",
  "trace_id": "<request-trace-id>",
  "context": {}
}
```

## Side effects

- No event is processed or stored.
- Applies identically to every event-broker endpoint (`GET /v1/topics`, `POST /v1/consumer_groups`, `POST /v1/subscriptions`, etc.).
