# Flow: chained producer desync recovery

A chained producer loses its local sequence state (DB restore, crash). The first publish after recovery fails with `412 SequenceViolation`. The producer reads the broker's cursor, reconciles its local counter, and successfully republishes.

Three exchanges shown inline. Topic: `acme.orders.v1`, partition 0. Broker's last accepted sequence for this producer on this partition: 7.

---

## Exchange 1 — Publish with stale sequence → 412

After a restart, the producer's local DB was restored from a backup and thinks `last_sequence = 3`. It sends the next event with `meta.previous = 3, meta.sequence = 4` — but the broker has already accepted through sequence 7.

```http
POST /v1/events HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json
Producer-Id: 550e8400-e29b-41d4-a716-446655440000

{
  "id": "aaaaaaaa-0000-0000-0000-000000000004",
  "type": "gts.cf.core.events.event.v1~acme.orders.created.v1",
  "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
  "tenant_id": "<tenant-uuid>",
  "source": "order-service",
  "subject": "order-4",
  "subject_type": "gts.cf.core.events.subject.v1~acme.order.v1",
  "occurred_at": "2026-06-15T10:00:00Z",
  "data": { "order_id": "order-4" },
  "meta": {
    "producer_id": "550e8400-e29b-41d4-a716-446655440000",
    "sequence": 4,
    "previous": 3
  }
}
```

```http
HTTP/1.1 412 Precondition Failed
Content-Type: application/problem+json

{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
  "title": "Failed Precondition",
  "status": 412,
  "detail": "chain broken on (550e8400-..., acme.orders.v1, 0): expected previous=7, got previous=3",
  "instance": "/v1/events",
  "context": {
    "expected_previous": 7
  }
}
```

---

## Exchange 2 — Read broker cursor → reconcile local state

The producer calls `GET /v1/producers/{id}/cursors` to learn the broker's authoritative `last_sequence` per `(topic, partition)`.

```http
GET /v1/producers/550e8400-e29b-41d4-a716-446655440000/cursors HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
```

```http
HTTP/1.1 200 OK
Content-Type: application/json

{
  "producer_id": "550e8400-e29b-41d4-a716-446655440000",
  "cursors": [
    { "topic": "gts.cf.core.events.topic.v1~acme.orders.v1", "partition": 0, "last_sequence": 7 }
  ]
}
```

> Producer updates its local DB: `last_sequence(acme.orders.v1, 0) = 7`. Next publish must use `meta.previous = 7, meta.sequence = 8`.

---

## Exchange 3 — Republish with correct sequence → 202

```http
POST /v1/events HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json
Producer-Id: 550e8400-e29b-41d4-a716-446655440000

{
  "id": "aaaaaaaa-0000-0000-0000-000000000008",
  "type": "gts.cf.core.events.event.v1~acme.orders.created.v1",
  "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
  "tenant_id": "<tenant-uuid>",
  "source": "order-service",
  "subject": "order-8",
  "subject_type": "gts.cf.core.events.subject.v1~acme.order.v1",
  "occurred_at": "2026-06-15T10:00:01Z",
  "data": { "order_id": "order-8" },
  "meta": {
    "producer_id": "550e8400-e29b-41d4-a716-446655440000",
    "sequence": 8,
    "previous": 7
  }
}
```

```http
HTTP/1.1 202 Accepted
Content-Type: application/json

{
  "id": "aaaaaaaa-0000-0000-0000-000000000008",
  "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
  "partition": 0,
  "accepted_at": "2026-06-15T10:00:01Z"
}
```

> `evbk_producer_state(550e8400-..., acme.orders.v1, 0).last_sequence` advances to `8`. Chain is healthy; the producer continues normally from sequence 9.
