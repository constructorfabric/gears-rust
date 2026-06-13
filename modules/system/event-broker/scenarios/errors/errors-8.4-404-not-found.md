# 404 — resource not found

A request referencing a resource that doesn't exist returns `404`. The `title` names the specific resource kind.

## Request

```http
GET /v1/events:stream?subscription_id=99999999-8888-7777-6666-555555555555 HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Accept: multipart/mixed
```

## Expected response

- `404 Not Found` (`PD`)

```json
{
  "type": "https://errors.cf.core/events/subscription-not-found",
  "title": "SubscriptionNotFound",
  "status": 404,
  "detail": "subscription 99999999-8888-7777-6666-555555555555 not found",
  "instance": "/v1/events:stream"
}
```

## Notes

Sibling `404` titles across the API: `ConsumerGroupNotFound`, `TopicNotFound`, `SubscriptionNotFound`. Endpoint-specific instances: [consumer-groups/negative-1.7](../consumer-groups/negative-1.7-get-unknown-group.md), [topics/negative-5.3](../topics/negative-5.3-segments-unknown-topic.md), [transports/negative-6.5](../transports/negative-6.5-stream-unknown-subscription.md).

<!-- No "## Side effects" — lookup miss changes nothing. -->
