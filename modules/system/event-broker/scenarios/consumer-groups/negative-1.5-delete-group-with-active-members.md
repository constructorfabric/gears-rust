# Delete is rejected while the group has active members

A group with at least one active subscription cannot be deleted.

## Setup

- Run [Create an anonymous group](positive-1.1-create-anonymous-group.md). Group id `{group_id}`.
- Run [Cold JOIN against a fresh group](../subscriptions/positive-2.1-cold-join-fresh-group.md) against `{group_id}` — one active subscription exists.

## Request

```http
DELETE /v1/consumer_groups/{group_id} HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
```

## Expected response

- `409 Conflict` (`PD`)

```json
{
  "type": "https://errors.cf.core/events/consumer-group-has-active-members",
  "title": "ConsumerGroupHasActiveMembers",
  "status": 409,
  "detail": "consumer group {group_id} has 1 active subscription(s)",
  "instance": "/v1/consumer_groups/{group_id}"
}
```

## Side effects

- `evbk_consumer_group({group_id})` is unchanged (still present).
- After the active subscription LEAVEs (or is reaped), a retried `DELETE` returns `204` (see [positive-1.4](positive-1.4-delete-empty-group.md)).
