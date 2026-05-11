# List active subscriptions

Returns a paged list of subscriptions visible to the caller — subscriptions belonging to consumer groups the caller's principal can see. Useful for observability and debugging (e.g., "how many consumers are active for this group?").

## Request

```http
GET /v1/subscriptions?limit=10 HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
```

## Expected response

- `200 OK`

```json
{
  "items": [
    {
      "id": "11111111-2222-3333-4444-555555555555",
      "consumer_group": "gts.cf.core.events.consumer_group.v1~{uuid}",
      "client_agent": "order-worker/1.4.0 cf-event-broker-sdk/0.1.0",
      "assigned": [
        { "topic": "gts.cf.core.events.topic.v1~acme.orders.v1", "partition": 0 },
        { "topic": "gts.cf.core.events.topic.v1~acme.orders.v1", "partition": 1 }
      ],
      "topology_version": 2,
      "expires_at": "2026-06-15T10:00:30Z"
    }
  ],
  "page_info": {
    "next_cursor": null,
    "limit": 10
  }
}
```

## Side effects

- Read-only. No state changes.
- Returns only subscriptions for groups visible to the caller's `AccessScope`. Cross-tenant subscriptions are not visible.
