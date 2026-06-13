# Stream rejects when positions are not set

Opening `:stream` before SEEKing a partition with no committed cursor returns `409 PositionsNotSet`. This is the defensive backstop that enforces "the consumer must declare its starting position explicitly". A well-behaved SDK SEEKs before streaming and never observes this on the happy path.

> Covers the broker-observable half of consumer-seek-semantics §5.6 — the broker's `409 PositionsNotSet` reply. The end-to-end recovery call sequence is [flows/flow-7.3](../flows/flow-7.3-positions-not-set-recovery.md). (The SDK's recovery-loop control flow is out of scope here — see `event-broker-sdk-scenarios`.)

## Setup

- Run [Cold JOIN against a fresh group](../subscriptions/positive-2.1-cold-join-fresh-group.md). Subscription id `{sub_id}`; assigned `("acme.orders.v1", 0..3)`.
- **No** SEEK has been performed — every assigned partition lacks a committed cursor.

## Request

```http
GET /v1/events:stream?subscription_id={sub_id} HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Accept: multipart/mixed
```

## Expected response

- `409 Conflict` (`PD`)
- Body lists the unseeded `(topic, partition)` pairs and a recovery hint.

```json
{
  "type": "https://errors.cf.core/events/positions-not-set",
  "title": "PositionsNotSet",
  "status": 409,
  "detail": "4 assigned partition(s) have no committed cursor",
  "instance": "/v1/events:stream",
  "unseeded": [
    { "topic": "gts.cf.core.events.topic.v1~acme.orders.v1", "partition": 0 },
    { "topic": "gts.cf.core.events.topic.v1~acme.orders.v1", "partition": 1 },
    { "topic": "gts.cf.core.events.topic.v1~acme.orders.v1", "partition": 2 },
    { "topic": "gts.cf.core.events.topic.v1~acme.orders.v1", "partition": 3 }
  ],
  "recovery_hint": "Call POST /v1/subscriptions/{id}/positions for the unseeded partitions before re-opening the stream."
}
```

## Side effects

- No stream is established; no frames are emitted.
- `evbk_group_offsets` for the assigned partitions remains absent (the rejection changes no state).
- After the consumer SEEKs the listed partitions, `subsequent GET /v1/events:stream` returns `200` (see [positive-6.1](positive-6.1-stream-multipart-frames.md)).
