# ADR-0006: Offset Authority — Client-Side Tracking, No Broker-Side Durability

<!-- toc -->

- [Status](#status)
- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Client-Side Offset Tracking With SEEK-Owned Runtime Cursors](#client-side-offset-tracking-with-seek-owned-runtime-cursors)
  - [Broker-Side Durable ACK Endpoint](#broker-side-durable-ack-endpoint)
  - [Separate Timestamp-To-Offset Lookup Endpoint](#separate-timestamp-to-offset-lookup-endpoint)
- [More Information](#more-information)

<!-- /toc -->

**ID**: `cpt-cf-evbk-adr-offset-authority`

## Status

Accepted

## Context and Problem Statement

Early design drafts included a `POST /v1/subscriptions/{id}/ack` endpoint for consumers to commit processed offsets back to the broker. The intent was to support consumers that have no persistent store of their own.

In practice, every meaningful consumer type already has a natural persistence medium:

| Consumer type | Natural offset store |
|---|---|
| Long-running service | Application database |
| Browser | IndexedDB / localStorage |
| Script / CLI | Local filesystem |
| Date-anchored reader | Timestamp → offset lookup (see below) |
| In-process module | toolkit-db transactional outbox (producer side); own DB (consumer side) |

There is no consumer archetype that genuinely lacks a durable store. A broker-managed ACK endpoint does not eliminate the durability problem — it shifts responsibility to the broker and adds a write-path operation (ACK round-trip per processed batch) with no net reduction in system complexity.

## Decision Drivers

- Keep consumer progress ownership with the consumer, where the natural durable store already exists.
- Avoid adding broker write-path load for every processed batch.
- Preserve SEEK as the single broker operation that establishes an active subscription cursor.
- Support date-anchored readers without introducing a second offset-resolution endpoint.

## Considered Options

- Client-side offset tracking with SEEK-owned runtime cursors and timestamp sentinel support.
- Broker-side durable ACK endpoint for committed consumer offsets.
- Separate timestamp-to-offset lookup endpoint plus SEEK by integer offset.

## Decision Outcome

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

### Consequences

**Removed:**
- `POST /v1/subscriptions/{id}/ack` endpoint (and all associated broker-side cursor durability logic)
- `cursor.acked` position (renamed to `cursor.offset` — the ephemeral session cursor set by SEEK)
- `PartitionNotAssigned` error type (was only raised by ACK)
- `acknowledge()` method from `DeliveryService`

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

### Confirmation

Confirm the design and implementation by checking that consumer progress durability lives outside the broker, that active subscription cursors remain cache/runtime state, and that `POST /v1/subscriptions/{id}:seek` is the only broker operation that sets the stream cursor.

## Pros and Cons of the Options

### Client-Side Offset Tracking With SEEK-Owned Runtime Cursors

Pros:

- Matches the natural durable store of real consumers.
- Keeps broker ingest and delivery free of per-batch ACK writes.
- Keeps reconnect behavior explicit: consumers re-SEEK from their own stored position.

Cons:

- SDKs must provide good offset-manager helpers so application authors do not reimplement this poorly.

### Broker-Side Durable ACK Endpoint

Pros:

- Gives simple consumers an apparent central offset store.

Cons:

- Adds broker write-path load and durability requirements.
- Does not remove the need for consumer idempotency.
- Blurs ownership of processing success between the broker and application.

### Separate Timestamp-To-Offset Lookup Endpoint

Pros:

- Makes timestamp resolution independently callable.

Cons:

- Adds an endpoint for behavior that SEEK can perform atomically.
- Forces clients to handle a lookup-then-SEEK race.

## More Information

- ADR-0001 (offset-semantics) — defines the broker-logical sequence space used by cursors.
- `features/0002-consumer-subscription-lifecycle.md` — describes JOIN, SEEK, stream, and re-JOIN flows.
- DESIGN.md §3.1 "Offset Semantics" — summarizes the consumer-visible cursor model.
