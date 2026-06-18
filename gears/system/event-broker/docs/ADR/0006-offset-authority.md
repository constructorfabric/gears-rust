# ADR-0006: Offset Authority — Client-Side Tracking, No Broker-Side Durability

## Status

Accepted

## Context

Early design drafts included a `POST /v1/subscriptions/{id}/ack` endpoint for consumers to commit processed offsets back to the broker, with a future `/v1/groups/{group}/offsets` KV endpoint for durable group-scoped cursor storage. The intent was to support consumers that have no persistent store of their own.

In practice, every meaningful consumer type already has a natural persistence medium:

| Consumer type | Natural offset store |
|---|---|
| Long-running service | Application database |
| Browser | IndexedDB / localStorage |
| Script / CLI | Local filesystem |
| Date-anchored reader | Timestamp → offset lookup (see below) |
| In-process module | Modkit-db transactional outbox (producer side); own DB (consumer side) |

There is no consumer archetype that genuinely lacks a durable store. A broker-managed ACK endpoint does not eliminate the durability problem — it shifts responsibility to the broker and adds a write-path operation (ACK round-trip per processed batch) with no net reduction in system complexity.

## Decision

**Drop the ACK endpoint.** The broker does not durably store consumer progress.

**Extend the SEEK endpoint with a timestamp sentinel.** Date-anchored readers need a way to start from a point in time without resolving offsets separately. The existing `POST /v1/subscriptions/{id}:seek` sentinel vocabulary is extended with `"at:<ISO-8601>"`:

```
POST /v1/subscriptions/{id}:seek
{
  "partition_positions": {
    "<topic>:<partition>": "at:2026-06-14T10:00:00Z"
  }
}
```

The broker resolves the timestamp to the offset of the first event whose `occurred_at ≥ timestamp` and sets the cursor in one step. The response returns the resolved integer offset per partition, which the consumer may persist for future re-seeks. No separate endpoint is introduced.

## Consequences

**Removed:**
- `POST /v1/subscriptions/{id}/ack` endpoint (and all associated broker-side cursor durability logic)
- `cursor.acked` position (renamed to `cursor.offset` — the ephemeral session cursor set by SEEK)
- `PartitionNotAssigned` error type (was only raised by ACK)
- `acknowledge()` method from `DeliveryService`
- Any future `/v1/groups/{group}/offsets` KV endpoint

**Added:**
- `"at:<ISO-8601>"` sentinel on `POST /v1/subscriptions/{id}:seek` — resolves to the offset of the first event whose `occurred_at ≥ timestamp`. Boundary behaviour:
  - `timestamp` before retention floor → retention floor offset (first available event)
  - `timestamp` beyond current HWM → HWM (consumer streams only future events, equivalent to `"latest"`)
  - Response returns the resolved integer offset per partition; consumer may persist it for future re-seeks
  - Malformed timestamp → `400 InvalidTimestamp`

**Unchanged:**
- `cursor` inside the broker is still tracked during an active subscription session (ephemeral, cache-only), seeded by `POST /v1/subscriptions/{id}:seek` (SEEK). The SEEK position is the broker's reference for "emit from here" during the stream. It is NOT persisted across sessions — on reconnect, the consumer re-SEEKs from its own store.
- The `"earliest"` and `"latest"` sentinels on the SEEK endpoint are unchanged.

**Consumer contract:**
Every consumer is responsible for persisting the last offset it has successfully processed, using whatever storage is natural for its deployment context. The broker makes this easy: `offset` appears on every delivered event; persisting it is a single field write.

**Trade-off acknowledged:**
Consumers that are stateless by design (one-shot scripts, fire-and-forget readers) can use `"latest"` SEEK and simply accept reprocessing on restart — the at-least-once guarantee is preserved by the consumer's own idempotency, not by the broker's cursor.
