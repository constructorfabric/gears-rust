# Stream emits a topology frame on rebalance

When a second consumer JOINs the same group, the broker rebalances partitions and notifies existing streams in-band via a `topology` frame carrying the new `topology_version` and the subscription's updated `assigned` list. The stream is NOT closed.

> Covers the broker-observable half of consumer-seek-semantics §5.5 — the broker *emits* a `topology` frame on the open stream. The new owner's subsequent SEEK calls are shown end-to-end in [flows/flow-7.2](../flows/flow-7.2-two-consumer-rebalance.md). (How a client *decides* which partitions to re-SEEK is SDK behavior — see `event-broker-sdk-scenarios`.)

## Setup

- Consumer A: group `{group_id}`, subscription `{sub_a}`, currently assigned all 4 partitions of `acme.orders.v1`, seeded and [streaming](positive-6.1-stream-multipart-frames.md).
- Consumer B JOINs `{group_id}` with the same interest (per [subscriptions/positive-2.1](../subscriptions/positive-2.1-cold-join-fresh-group.md)), triggering a rebalance to a 2/2 split.

## Request

The triggering call is consumer B's JOIN on the same group (it causes the rebalance that A observes as a frame):

```http
POST /v1/subscriptions HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{ "consumer_group": "{group_id}", "client_agent": "order-worker/1.4.0", "interests": [ { "topic": "gts.cf.core.events.topic.v1~acme.orders.v1", "tenant_id": "<tenant-uuid>", "types": ["gts.cf.core.events.event.v1~acme.orders.*"] } ] }
```

## Expected behavior on A's open stream

- A's stream receives a `topology` `MP` frame (the stream stays open):

```json
{
  "kind": "topology",
  "topology_version": 2,
  "assigned": [
    { "topic": "gts.cf.core.events.topic.v1~acme.orders.v1", "partition": 0 },
    { "topic": "gts.cf.core.events.topic.v1~acme.orders.v1", "partition": 1 }
  ]
}
```

- A keeps partitions `0`, `1`; partitions `2`, `3` move to B.

## Side effects

- Group `{group_id}` topology_version advances from `1` to `2`.
- `evbk_subscription({sub_a}).assigned` updates to `[0, 1]`; `evbk_subscription({sub_b}).assigned` is `[2, 3]`.
- `subscription {sub_a} next frame is topology with assigned=[0,1]` (no stream close).
- Partitions `2`, `3` are now owned by B and require B to SEEK before B's stream delivers them — B's `:stream` returns `409 PositionsNotSet` until B seeds them (see [negative-6.4](negative-6.4-stream-positions-not-set.md)).
- Cursors for the continuing partitions `0`, `1` are unchanged (only moved partitions are affected).
