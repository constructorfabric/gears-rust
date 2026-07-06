Created:  2026-06-30 by Constructor Tech
Updated:  2026-06-30 by Constructor Tech
# ADR-0027: Stream Resumability via Last-Event-ID


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Server-side resume buffer keyed by message_id, replayed by seq](#option-1-server-side-resume-buffer-keyed-by-message_id-replayed-by-seq)
  - [Option 2: Restart generation on reconnect](#option-2-restart-generation-on-reconnect)
  - [Option 3: No resume — client refetches the finished message](#option-3-no-resume--client-refetches-the-finished-message)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-06-30

**Status**: accepted

**Review**: Revisit if the resume buffer needs to survive process restarts (durable buffer) or when multi-replica fan-out of a single stream is required.

**ID**: `cpt-cf-chat-engine-adr-stream-resumability`

## Context and Problem Statement

Assistant responses stream over SSE (`cpt-cf-chat-engine-adr-sse-delta-streaming`) and can take seconds. Networks drop, tabs sleep, and clients reconnect. If a reconnect either lost the in-flight portion or replayed the whole stream from the start, the client would see gaps or duplicated text. The generation itself must also not die just because the client connection closed. How can a client reconnect to an in-progress (or just-finished) assistant message and continue exactly where it left off?

## Decision Drivers

* Resume an interrupted stream without gaps or duplicates
* Generation must outlive the client connection (a dropped client must not abort the backend turn)
* Reuse SSE's native reconnect signal (`Last-Event-ID`) rather than invent a new handshake
* Bounded memory — buffered events must not accumulate unboundedly
* Stateless-scaling-friendly within a replica's lifetime (`cpt-cf-chat-engine-adr-stateless-scaling`)

## Considered Options

* **Option 1: Server-side resume buffer keyed by `message_id`, replayed by `seq`** — tee every wire event into a per-message buffer; on reconnect replay only events with `seq` greater than the client's `Last-Event-ID`, then live-tail.
* **Option 2: Restart generation on reconnect** — re-run the backend turn from scratch.
* **Option 3: No resume — client refetches the finished message** — drop the stream on disconnect; the client polls the persisted message once complete.

## Decision Outcome

Chosen option: "Server-side resume buffer keyed by `message_id`, replayed by `seq`". The streaming driver runs decoupled from the client connection and tees each `WireStreamEvent` into a resume buffer as it is produced. A dedicated resume endpoint (`GET /messages/{id}/stream`) honors the SSE `Last-Event-ID` request header: it replays buffered events with `seq > Last-Event-ID`, then live-tails newly produced events until the terminal `message.complete`/`message.error`. Because every event already carries a monotonic per-message `seq` (mirrored in the SSE `id:` line), the cursor is exact — no gaps, no duplicates. The driver outlives the client connection, so a dropped client does not cancel generation; the buffer is swept on a TTL cadence to bound memory.

### Consequences

* Good, because a reconnecting client resumes precisely from its last applied `seq`.
* Good, because generation continues after a client disconnect — work is not wasted.
* Good, because it reuses the browser-native `Last-Event-ID` mechanism (no bespoke handshake).
* Good, because buffer entries are bounded by a TTL sweep.
* Bad, because the buffer is in-memory and per-replica — a resume must land on the same replica, and a process restart loses an in-flight buffer.
* Bad, because teeing and buffering add per-event bookkeeping and memory pressure for very long streams.

### Confirmation

Confirmed by the resume tests: a full replay of a finished message reproduces the same reconstructed text, and a `Last-Event-ID: N` reconnect replays only `seq > N`; plus the test asserting a dropped client stream still completes generation and persists the assistant message.

## Pros and Cons of the Options

### Option 1: Server-side resume buffer keyed by message_id, replayed by seq

* Good, because resume is exact (`seq` cursor) and gap/duplicate-free.
* Good, because it decouples generation lifetime from the client connection.
* Bad, because in-memory per-replica buffering does not survive restarts and pins resume to one replica.

### Option 2: Restart generation on reconnect

* Good, because no buffer is needed.
* Bad, because it wastes backend compute and tokens re-running the turn.
* Bad, because a non-deterministic backend produces different text on the rerun, so the client sees the message change.

### Option 3: No resume — client refetches the finished message

* Good, because it is the simplest to implement.
* Bad, because the user loses live progress on every transient disconnect.
* Bad, because there is no partial view until the whole turn finishes.

## Related Design Elements

**Actors**:
* `cpt-cf-chat-engine-actor-client` — reconnects with `Last-Event-ID` to resume the stream
* `cpt-cf-chat-engine-actor-backend-plugin` — produces the turn whose events are buffered

**Requirements**:
* `cpt-cf-chat-engine-fr-delta-streaming` — the delta stream being resumed
* `cpt-cf-chat-engine-fr-send-message` — the assistant turn whose generation outlives the client

**Design Elements**:
* `cpt-cf-chat-engine-design-streaming-protocol` — the `seq`-stamped wire vocabulary the cursor relies on

**Related ADRs**:
* ADR-0026 (SSE Delta Streaming) — the `seq`/`Last-Event-ID` wire this resume is built on
* ADR-0008 (Streaming Cancellation) — client-initiated cancel, the inverse of resume
* ADR-0009 (Stateless Horizontal Scaling) — the per-replica state caveat noted above
