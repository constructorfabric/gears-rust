# List storage segments for a (topic, partition)

Read-only introspection of the storage backend's segment manifest for a specific `(topic, partition)`. Segment shapes are backend-specific; consumers treat `segments[]` entries as opaque except the documented envelope.

## Request

```http
GET /v1/topics/segments?topic=gts.cf.core.events.topic.v1~acme.orders.v1&partition=0 HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
```

## Expected response

- `200 OK`

```json
{
  "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
  "partition": 0,
  "start_sequence": 100,
  "end_sequence": 4999,
  "start_time": "2026-05-20T00:00:00Z",
  "end_time": "2026-05-29T10:00:00Z",
  "segments": [
    { "id": "seg-0001", "start_sequence": 100, "end_sequence": 2000 },
    { "id": "seg-0002", "start_sequence": 2001, "end_sequence": 4999 }
  ]
}
```

<!-- No "## Side effects" — pure-introspection read. -->
