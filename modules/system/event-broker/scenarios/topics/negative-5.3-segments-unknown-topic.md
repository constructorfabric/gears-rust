# Segments for an unknown topic

Requesting the segment manifest for a topic that isn't registered returns `404`.

## Request

```http
GET /v1/topics/segments?topic=gts.cf.core.events.topic.v1~acme.nonexistent.v1&partition=0 HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
```

## Expected response

- `404 Not Found` (`PD`)

```json
{
  "type": "https://errors.cf.core/events/topic-not-found",
  "title": "TopicNotFound",
  "status": 404,
  "detail": "topic gts.cf.core.events.topic.v1~acme.nonexistent.v1 not found",
  "instance": "/v1/topics/segments"
}
```

<!-- No "## Side effects" — lookup miss changes nothing. (An invalid `partition` value would instead be 400 InvalidPartition.) -->
