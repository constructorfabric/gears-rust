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
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
  "title": "Not Found",
  "status": 404,
  "detail": "topic gts.cf.core.events.topic.v1~acme.nonexistent.v1 not found",
  "instance": "/v1/topics/segments",
  "trace_id": "<request-trace-id>",
  "context": {
    "resource_type": "gts.cf.core.events.topic.v1~",
    "resource_name": "gts.cf.core.events.topic.v1~acme.nonexistent.v1"
  }
}
```

<!-- No "## Side effects" — lookup miss changes nothing. (An invalid `partition` value would instead be 400 InvalidPartition.) -->
