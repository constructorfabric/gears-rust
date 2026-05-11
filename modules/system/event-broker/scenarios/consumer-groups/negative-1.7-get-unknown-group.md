# Get an unknown consumer group

Looking up an id that was never registered (or was deleted) returns `404`.

## Request

```http
GET /v1/consumer_groups/gts.cf.core.events.consumer_group.v1~00000000-0000-0000-0000-000000000000 HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
```

## Expected response

- `404 Not Found` (`PD`)

```json
{
  "type": "https://errors.cf.core/events/consumer-group-not-found",
  "title": "ConsumerGroupNotFound",
  "status": 404,
  "detail": "consumer group gts.cf.core.events.consumer_group.v1~00000000-0000-0000-0000-000000000000 not found",
  "instance": "/v1/consumer_groups/gts.cf.core.events.consumer_group.v1~00000000-0000-0000-0000-000000000000"
}
```

<!-- No "## Side effects" — lookup miss changes nothing. -->
