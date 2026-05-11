# Publish with unknown or expired Producer-Id

A producer whose registration has been reaped (expired after `P30D` of inactivity) or was never registered sends a publish with a `Producer-Id` header. The broker rejects the request.

## Request

```http
POST /v1/events HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json
Producer-Id: deadbeef-0000-0000-0000-000000000000

{
  "id": "11111111-0000-0000-0000-000000000001",
  "type": "gts.cf.core.events.event.v1~acme.orders.created.v1",
  "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
  "tenant_id": "<tenant-uuid>",
  "source": "order-service",
  "subject": "order-1",
  "subject_type": "gts.cf.core.events.subject.v1~acme.order.v1",
  "occurred_at": "2026-06-15T10:00:00Z",
  "data": { "order_id": "order-1" },
  "meta": {
    "producer_id": "deadbeef-0000-0000-0000-000000000000",
    "sequence": 1,
    "previous": 0
  }
}
```

## Expected response

- `400 Invalid Argument`

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
  "title": "Invalid Argument",
  "status": 400,
  "detail": "Producer-Id deadbeef-0000-0000-0000-000000000000 is not registered",
  "instance": "/v1/events",
  "trace_id": "<request-trace-id>",
  "context": {
    "field_violations": [
      { "field": "Producer-Id", "description": "producer not registered or registration has expired", "reason": "unknown_producer" }
    ]
  }
}
```

## Side effects

- No event is written or enqueued.
- Recovery: call `POST /v1/producers` to obtain a fresh `producer_id` and restart the chain from sequence 1.
