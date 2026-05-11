# Schema validation failure on publish

The event `data` is validated against the event type's `data_schema` at ingest; a payload that fails is rejected `400`. Supplying a server-stamped read-only field (`partition`, `sequence`, `sequence_time`) is likewise a `400 BadRequest`.

## Setup

- Event type `acme.orders.created.v1` has a `data_schema` requiring `order_id` (string) and `total_cents` (integer).

## Request

`data` is missing the required `total_cents`:

```http
POST /v1/events HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "id": "d0000000-0000-0000-0000-000000000001",
  "type": "gts.cf.core.events.event.v1~acme.orders.created.v1",
  "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
  "tenant_id": "<tenant-uuid>",
  "source": "order-service",
  "subject": "order-bad",
  "subject_type": "gts.cf.core.events.subject.v1~acme.order.v1",
  "occurred_at": "2026-05-29T10:09:00Z",
  "data": { "order_id": "order-bad" }
}
```

## Expected response

- `400 Bad Request` (`PD`)
- Body carries the validator's diagnostics.

```json
{
  "type": "https://errors.cf.core/events/event-data-invalid",
  "title": "EventDataInvalid",
  "status": 400,
  "detail": "payload failed schema validation for gts.cf.core.events.event.v1~acme.orders.created.v1",
  "instance": "/v1/events",
  "errors": ["missing required field: total_cents"]
}
```

## Side effects

- No event is admitted.

> Related `400` shapes share this envelope (different `title`): `InvalidPartition` / `InvalidTraceParent` (field-format), and `BadRequest` when a producer supplies a `readOnly` field (`partition`, `sequence`, `sequence_time`) on publish.
