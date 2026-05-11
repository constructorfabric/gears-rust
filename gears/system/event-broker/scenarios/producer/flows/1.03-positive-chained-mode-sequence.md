# Chained-mode publish advances the producer chain

A producer registered in chained mode supplies `meta.producer_id`, `meta.previous`, and `meta.sequence`. The broker validates `meta.previous` against its stored `last_sequence` for `(producer_id, topic, partition)`; on match, the event is admitted and `last_sequence` advances.

## Setup

- Producer `{producer_id}` is registered in chained mode. Its `last_sequence` for `(producer_id, "acme.orders.v1", 0)` is currently `7`.
- The event's `subject` hashes to partition `0`.

## Request

```http
POST /v1/events HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "id": "c0000000-0000-0000-0000-000000000008",
  "type": "gts.cf.core.events.event.v1~acme.orders.created.v1",
  "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
  "tenant_id": "<tenant-uuid>",
  "source": "order-service",
  "subject": "order-chain",
  "subject_type": "gts.cf.core.events.subject.v1~acme.order.v1",
  "occurred_at": "2026-05-29T10:07:00Z",
  "data": { "order_id": "order-chain", "total_cents": 700 },
  "meta": { "version": 1, "producer_id": "{producer_id}", "previous": 7, "sequence": 8 }
}
```

## Expected response

- `202 Accepted` — `meta.previous` (7) matches the broker's `last_sequence`; the event is admitted.

## Side effects

- The producer chain's `last_sequence` for `({producer_id}, "acme.orders.v1", 0)` advances from `7` to `8`.
- `meta` is stripped on read — consumers never see `producer_id` / `previous` / `sequence`.
- A re-send of the same `meta.previous=7` (duplicate) is deduped, not double-admitted.
