# 409 — state conflict

A request that conflicts with current broker state returns `409`. Several distinct conditions share the status; the `title` disambiguates.

## Request

Representative `409` trigger (opening a stream whose assigned partitions have no committed cursor):

```http
GET /v1/events:stream?subscription_id={sub_id} HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Accept: multipart/mixed
```

(Opening a stream whose assigned partitions have no committed cursor.)

## Expected response

- `409 Conflict` (`PD`)

```json
{
  "type": "https://errors.cf.core/events/positions-not-set",
  "title": "PositionsNotSet",
  "status": 409,
  "detail": "1 assigned partition(s) have no committed cursor",
  "instance": "/v1/events:stream",
  "unseeded": [ { "topic": "gts.cf.core.events.topic.v1~acme.orders.v1", "partition": 0 } ],
  "recovery_hint": "Call POST /v1/subscriptions/{id}/positions for the unseeded partitions before re-opening the stream."
}
```

## The `409` family

| `title` | Condition | Endpoint-specific scenario |
|---|---|---|
| `PositionsNotSet` | assigned partition has no cursor on stream-open | [transports/negative-6.4](../transports/negative-6.4-stream-positions-not-set.md) |
| `SeekBackwardNotAllowed` | mid-stream backward SEEK | [positions/negative-3.7](../positions/negative-3.7-mid-stream-backward-seek.md) |
| `PartitionNotAssigned` | SEEK references an unassigned partition | [positions/negative-3.9](../positions/negative-3.9-seek-unassigned-partition.md) |
| `ConsumerGroupHasActiveMembers` | DELETE group with active subscriptions | [consumer-groups/negative-1.5](../consumer-groups/negative-1.5-delete-group-with-active-members.md) |

<!-- No "## Side effects" — conflicts reject without changing state. -->
