# 412 — sequence violation

The only producer-protocol error that escapes to callers. A chained-mode publish whose `meta.previous` doesn't match the broker's `last_sequence` is rejected `412`, with the broker's current value for resync.

## Request

```http
POST /v1/events HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "id": "c0000000-0000-0000-0000-000000000099",
  "type": "gts.cf.core.events.event.v1~acme.orders.created.v1",
  "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
  "tenant_id": "<tenant-uuid>",
  "source": "order-service",
  "subject": "order-chain",
  "subject_type": "gts.cf.core.events.subject.v1~acme.order.v1",
  "occurred_at": "2026-05-29T10:08:00Z",
  "data": { "order_id": "order-chain", "total_cents": 700 },
  "meta": { "version": 1, "producer_id": "{producer_id}", "previous": 3, "sequence": 4 }
}
```

## Expected response

- `412 Precondition Failed` (`PD`)
- `expected_previous` carries the broker's `last_sequence`.

```json
{
  "type": "https://errors.cf.core/events/sequence-violation",
  "title": "SequenceViolation",
  "status": 412,
  "detail": "meta.previous=3 does not match broker last_sequence=7",
  "instance": "/v1/events",
  "expected_previous": 7
}
```

## Notes

Endpoint-specific instance: [events/negative-4.6](../events/negative-4.6-chained-sequence-violation.md). For `:batch`, a violation on any event rejects the whole batch atomically.

<!-- No "## Side effects" — rejected publish admits nothing. -->
