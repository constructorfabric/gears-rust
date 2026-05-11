# SEEK rejects a partition not assigned to the subscription

A SEEK referencing a `(topic, partition)` not currently assigned to this subscription is rejected `409 PartitionNotAssigned`. Validation is per-request atomic — if any partition in the body is unassigned, nothing is applied. The response carries the current assignment so the client can self-heal.

## Setup

- Run [Cold JOIN against a fresh group](../subscriptions/positive-2.1-cold-join-fresh-group.md) on a group already split with another member, so `{sub_id}` is assigned only `("acme.orders.v1", 0)` and `("acme.orders.v1", 1)` (partitions `2`, `3` belong to another subscription).

## Request

```http
POST /v1/subscriptions/{sub_id}/positions HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "partition_positions": {
    "gts.cf.core.events.topic.v1~acme.orders.v1:0": "earliest",
    "gts.cf.core.events.topic.v1~acme.orders.v1:2": "earliest"
  }
}
```

## Expected response

- `409 Conflict` (`PD`)
- Body carries the current `topology_version` and the subscription's actual assignment.

```json
{
  "type": "https://errors.cf.core/events/partition-not-assigned",
  "title": "PartitionNotAssigned",
  "status": 409,
  "detail": "acme.orders.v1:2 is not assigned to subscription {sub_id}",
  "instance": "/v1/subscriptions/{sub_id}/positions",
  "topology_version": 2,
  "assigned": [
    { "topic": "gts.cf.core.events.topic.v1~acme.orders.v1", "partition": 0 },
    { "topic": "gts.cf.core.events.topic.v1~acme.orders.v1", "partition": 1 }
  ]
}
```

## Side effects

- Nothing is applied — neither partition `0` nor `2` is seeded (`evbk_group_offsets` unchanged for both). Atomic per-request: the assigned partition `0` is NOT seeded despite being valid, because partition `2` made the request invalid.
