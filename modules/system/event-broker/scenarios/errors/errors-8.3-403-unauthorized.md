# 403 — authenticated but not authorized

A valid token whose principal lacks the required permission is rejected `403`. The SDK rolls up the wire-level fine-grained codes (`TopicNotAuthorized`, `EventTypeNotAuthorized`, `TenantIdNotAuthorized`) into one `Unauthorized`; the HTTP layer keeps the specific `title`.

## Request

```http
POST /v1/subscriptions HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "consumer_group": "{group_id}",
  "client_agent": "worker/1.0.0",
  "interests": [ { "topic": "gts.cf.core.events.topic.v1~acme.restricted.v1", "tenant_id": "<tenant-uuid>", "types": ["gts.cf.core.events.event.v1~acme.restricted.*"] } ]
}
```

## Expected response

- `403 Forbidden` (`PD`); `title` is the specific wire code.

```json
{
  "type": "https://errors.cf.core/events/topic-not-authorized",
  "title": "TopicNotAuthorized",
  "status": 403,
  "detail": "principal lacks 'consume' on gts.cf.core.events.topic.v1~acme.restricted.v1",
  "instance": "/v1/subscriptions",
  "topic": "gts.cf.core.events.topic.v1~acme.restricted.v1"
}
```

## Notes

- Sibling `403` titles share this envelope: `EventTypeNotAuthorized` (offending type in body), `TenantIdNotAuthorized` (tenant resolver denial), and producer-side `Forbidden` on publish.
- See the endpoint-specific instance: [subscriptions/negative-2.6](../subscriptions/negative-2.6-join-unauthorized-topic.md).

<!-- No "## Side effects" — authorization denial changes no state. -->
