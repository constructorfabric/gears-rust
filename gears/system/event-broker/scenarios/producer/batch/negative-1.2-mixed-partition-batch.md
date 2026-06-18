# Batch rejected for mixing topics

`POST /v1/events:batch` is atomic per topic — all events in one batch must target the same topic. A batch mixing topics is rejected `400`.

## Setup

- Topics `acme.orders.v1` and `acme.shipments.v1` both registered.

## Request

```http
POST /v1/events:batch HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "events": [
    {
      "id": "e0000000-0000-0000-0000-000000000001",
      "type": "gts.cf.core.events.event.v1~acme.orders.created.v1",
      "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
      "tenant_id": "<tenant-uuid>", "source": "svc", "subject": "o-1",
      "subject_type": "gts.cf.core.events.subject.v1~acme.order.v1",
      "occurred_at": "2026-05-29T10:10:00Z", "data": { "order_id": "o-1", "total_cents": 1 }
    },
    {
      "id": "e0000000-0000-0000-0000-000000000002",
      "type": "gts.cf.core.events.event.v1~acme.shipments.dispatched.v1",
      "topic": "gts.cf.core.events.topic.v1~acme.shipments.v1",
      "tenant_id": "<tenant-uuid>", "source": "svc", "subject": "s-1",
      "subject_type": "gts.cf.core.events.subject.v1~acme.shipment.v1",
      "occurred_at": "2026-05-29T10:10:01Z", "data": { "shipment_id": "s-1" }
    }
  ]
}
```

## Expected response

- `400 Bad Request` (`PD`)

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
  "title": "Invalid Argument",
  "status": 400,
  "detail": "batch mixes topics: acme.orders.v1, acme.shipments.v1 — a batch is atomic per topic",
  "instance": "/v1/events:batch",
  "trace_id": "<request-trace-id>",
  "context": {
    "constraint": "batch mixes topics; a batch is atomic per topic"
  }
}
```

## Side effects

- No event from the batch is admitted (all-or-nothing).
