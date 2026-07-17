# List event types

Returns a paged list of event types registered in `types_registry`. Producers use this to discover which event types are available on a given topic; consumers use it to understand the data schemas they'll receive.

## Request

```http
GET /v1/event_types?topic=gts.cf.core.events.topic.v1~acme.orders.v1&limit=20 HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
```

## Expected response

- `200 OK`

```json
{
  "items": [
    {
      "id": "gts.cf.core.events.event.v1~acme.orders.created.v1",
      "topic_id": "gts.cf.core.events.topic.v1~acme.orders.v1",
      "description": "An order was placed",
      "allowed_subject_types": ["gts.cf.core.events.subject.v1~acme.order.v1"],
      "created_at": "2026-01-01T00:00:00Z"
    },
    {
      "id": "gts.cf.core.events.event.v1~acme.orders.cancelled.v1",
      "topic_id": "gts.cf.core.events.topic.v1~acme.orders.v1",
      "description": "An order was cancelled",
      "allowed_subject_types": ["gts.cf.core.events.subject.v1~acme.order.v1"],
      "created_at": "2026-01-01T00:00:00Z"
    }
  ],
  "page_info": {
    "next_cursor": null,
    "limit": 20
  }
}
```

## Side effects

- Read-only. No state changes.
- `data_schema` is omitted from the list response to keep payloads manageable; retrieve full schema via the GTS registry if needed.
