# Register a monotonic-mode producer

Monotonic mode is simpler than chained: the producer supplies a monotonically increasing `meta.sequence` per `(producer_id, topic, partition)` but no `meta.previous` link. The broker deduplicates on `(producer_id, topic, partition, sequence)` without requiring a contiguous chain.

## Request

```http
POST /v1/producers HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "mode": "monotonic"
}
```

## Expected response

- `201 Created`

```json
{
  "producer_id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
  "mode": "monotonic",
  "created_at": "2026-06-15T10:00:00Z"
}
```

## Side effects

- `evbk_producer(aaaaaaaa-...)` row created with `mode = monotonic`.
- Publishes omitting `meta.previous` are accepted; publishes that include `meta.previous` are rejected with `400 Invalid Argument` (wrong shape for monotonic mode).
