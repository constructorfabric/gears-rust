# JOIN with a typed filter expression

An interest may carry a paired `expression_type` + `expression` (both present or both absent). The broker compiles the expression at JOIN and stores it on the member; only matching events are delivered to this subscription.

## Setup

- Group `{group_id}` exists. Topic `acme.orders.v1` registered.

## Request

```http
POST /v1/subscriptions HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "consumer_group": "{group_id}",
  "client_agent": "high-value-worker/1.0.0",
  "interests": [
    {
      "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
      "tenant_id": "<tenant-uuid>",
      "types": ["gts.cf.core.events.event.v1~acme.orders.created.v1"],
      "expression_type": "gts.cf.core.events.filter.v1~cf.events.expression.cel.v1",
      "expression": "event.data.total_cents > 100000"
    }
  ]
}
```

## Expected response

- `201 Created`
- The subscription is created with the filter compiled and stored on the member.

```json
{
  "id": "<sub_id>",
  "assigned": [
    { "topic": "gts.cf.core.events.topic.v1~acme.orders.v1", "partition": 0 },
    { "topic": "gts.cf.core.events.topic.v1~acme.orders.v1", "partition": 1 }
  ],
  "topology_version": 1
}
```

## Side effects

- `evbk_subscription(<sub_id>)` carries the compiled CEL filter for the interest.
- On a later stream, only events whose `data.total_cents > 100000` are delivered; non-matching events advance the broker's per-member `last_examined` without delivery (per DESIGN R57).
