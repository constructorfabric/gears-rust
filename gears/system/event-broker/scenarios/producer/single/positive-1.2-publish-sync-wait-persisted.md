# Publish synchronously (wait for backend persistence)

Opting into synchronous persistence makes the broker hold the response until the storage backend has durably persisted the event, returning `201` instead of the default `202`.

## Setup

- Topic `acme.orders.v1` and event type `acme.orders.created.v1` registered.

## Request

```http
POST /v1/events?wait=persisted HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "id": "a1b2c3d4-0000-0000-0000-0000000000a2",
  "type": "gts.cf.core.events.event.v1~acme.orders.created.v1",
  "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
  "tenant_id": "<tenant-uuid>",
  "source": "order-service",
  "subject": "order-900",
  "subject_type": "gts.cf.core.events.subject.v1~acme.order.v1",
  "occurred_at": "2026-05-29T10:05:00Z",
  "data": { "order_id": "order-900", "total_cents": 200 }
}
```

(Equivalent to sending the header `Sync-Wait: true`.)

## Expected response

- `201 Created` — the event is persisted to the backend before the response returns.
- Offset/partition/sequence are still NOT returned inline (assigned by the backend; surfaced only on read).

## Side effects

- `evbk_event` on the resolved `(topic, partition)` holds the event durably before the `201` is sent (vs. `202` which only guarantees outbox enqueue).
