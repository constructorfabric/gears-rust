# List topics visible to the caller

Read-only introspection. Returns the topics the caller's principal is authorized to see in the current tenant.

## Request

```http
GET /v1/topics HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
```

## Expected response

- `200 OK`
- Body is an array of topic records; only topics the principal may `consume` or otherwise see are included.

```json
[
  {
    "id": "gts.cf.core.events.topic.v1~acme.orders.v1",
    "partitions": 4
  },
  {
    "id": "gts.cf.core.events.topic.v1~acme.shipments.v1",
    "partitions": 2
  }
]
```

<!-- No "## Side effects" — pure-introspection endpoint, changes no broker state (see INDEX rules). -->
