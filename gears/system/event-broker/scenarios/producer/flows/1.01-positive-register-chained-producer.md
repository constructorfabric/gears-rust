# Register a chained-mode producer

A producer that wants exactly-once ingest-side deduplication registers with the broker to obtain a `producer_id` bound to its principal. Subsequent publishes supply this id in the `Producer-Id` header and include a `meta` block with chained sequence fields.

## Request

```http
POST /v1/producers HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "mode": "chained"
}
```

## Expected response

- `201 Created`
- Body:

```json
{
  "producer_id": "550e8400-e29b-41d4-a716-446655440000",
  "mode": "chained",
  "created_at": "2026-06-15T10:00:00Z"
}
```

## Side effects

- `evbk_producer(550e8400-...)` row created with `owner_principal_id` from `SecurityContext`, `mode = chained`, `last_seen_at = now()`.
- Subsequent `POST /v1/events` with `Producer-Id: 550e8400-...` from any other principal returns `403 Permission Denied`.
- The producer row is reaped by the Reaper worker after the platform-wide producer-registration TTL (default `P30D`) of inactivity.
