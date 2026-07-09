Created:  2026-06-30 by Constructor Tech
Updated:  2026-06-30 by Constructor Tech
# ADR-0026: SSE Delta Streaming to the Client


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: SSE carrying typed `(op, path, value)` deltas](#option-1-sse-carrying-typed-op-path-value-deltas)
  - [Option 2: Keep NDJSON chunk forwarding](#option-2-keep-ndjson-chunk-forwarding)
  - [Option 3: WebSocket bidirectional channel](#option-3-websocket-bidirectional-channel)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-06-30

**Status**: accepted

**Review**: Revisit if a non-text part type needs incremental (rather than whole-part) streaming, or if HTTP/3 / WebSocket transport is adopted.

**ID**: `cpt-cf-chat-engine-adr-sse-delta-streaming`

## Context and Problem Statement

The backend-plugin hop speaks a chunk-based NDJSON stream (`start` → `chunk*` → `complete`/`error`, see `cpt-cf-chat-engine-adr-http-client-protocol`). A message is no longer a single text blob but an ordered list of typed parts (`cpt-cf-chat-engine-adr-message-parts`) that may also carry citations (`cpt-cf-chat-engine-adr-citations`). Forwarding raw text chunks to the client cannot express "open a new part", "append to part N", or "attach citations to part N", and gives the client no stable way to apply, deduplicate, or resume updates. How should Chat Engine deliver an assistant message to the client so a structured, multi-part document can be reconstructed incrementally?

## Decision Drivers

* Reconstruct a structured message document (parts, text, citations) incrementally, not just concatenated text
* A stable, ordered, resumable wire so a dropped connection can resume (`cpt-cf-chat-engine-adr-stream-resumability`)
* Keep the plugin contract simple (chunk-based) — projection is Chat Engine's job, not the plugin's
* Minimize per-event payload overhead on a high-frequency event (one event per token-ish chunk)
* Browser-native transport with automatic reconnection semantics

## Considered Options

* **Option 1: SSE carrying typed `(op, path, value)` deltas** — Chat Engine projects the plugin's chunk stream into Server-Sent Events; each event mutates the client-held message document by path
* **Option 2: Keep NDJSON chunk forwarding** — forward the plugin's `start`/`chunk`/`complete` stream verbatim to the client
* **Option 3: WebSocket bidirectional channel** — push deltas over a WebSocket

## Decision Outcome

Chosen option: "SSE carrying typed `(op, path, value)` deltas". Chat Engine runs a stateful projector (`DeltaProjector`) that turns the plugin's `StreamingEvent`s into a typed wire vocabulary delivered over `text/event-stream`. Each event carries a per-message monotonic `seq` (mirrored in the SSE `id:` line) and a typed discriminator mirrored in the SSE `event:` line:

* `message.start` — opens an empty assistant message document.
* `message.part.add` — opens a new part at `parts/{n}`.
* `message.text.delta` — appends a text fragment to a part body.
* `message.file_citation.add` / `message.link_citation.add` / `message.reference.add` — attach citations/references to a part.
* `message.status.changed` / `message.state.changed` / `session.meta.updated` / `message.tool` — out-of-band signals that do not mutate the document.
* `message.complete` — terminal success; carries the `stop` marker plus optional plugin metadata (model, finish_reason, usage).
* `message.error` — terminal error.

The document-mutating events carry **terse patch keys** to keep the high-frequency text-delta event small: `o` (operation: `add` / `append` / `patch` / `remove` / `stop`), `p` (JSON-pointer-like path, e.g. `parts/0/content/text`), and `v` (value). The client applies each `(o, p, v)` to its in-memory message document; text is reconstructed by concatenating the `v` of every `message.text.delta` at `parts/{n}/content/text`.

### Consequences

* Good, because the client reconstructs a fully structured multi-part document (text, code, links, citations) rather than flat text.
* Good, because the monotonic `seq` gives ordering, de-duplication, and a resume cursor (`Last-Event-ID`).
* Good, because the plugin contract stays chunk-based; projection complexity lives in one place (`DeltaProjector`).
* Good, because SSE is browser-native (`EventSource`) with built-in reconnect.
* Bad, because the client must implement a small patch-applier instead of string concatenation.
* Bad, because the terse `o/p/v` keys trade self-documenting field names for payload size (mitigated by the typed `event:`/`type` discriminator).
* Bad, because SSE is unidirectional — client→server actions (e.g. cancel) use a separate request, not the stream.

### Confirmation

Confirmed by the `DeltaProjector` unit tests (start→part.add→text.delta→complete ordering, contiguous `seq` from 0, terse-key serialization, `stop` terminator, citation projection) and the end-to-end streaming tests that reconstruct assistant text from the SSE frames.

## Pros and Cons of the Options

### Option 1: SSE carrying typed `(op, path, value)` deltas

* Good, because it expresses structured document mutations (parts, text, citations), not just text.
* Good, because `seq` + `Last-Event-ID` make the stream resumable and de-duplicable.
* Good, because the plugin stays simple; Chat Engine owns projection.
* Bad, because the client needs a patch-applier and the terse keys are less self-describing.

### Option 2: Keep NDJSON chunk forwarding

* Good, because it is the simplest possible passthrough.
* Bad, because it cannot express multi-part documents or per-part citations.
* Bad, because it offers no stable resume cursor or de-duplication contract for the client.

### Option 3: WebSocket bidirectional channel

* Good, because it supports client→server messages on the same channel.
* Bad, because it adds connection-lifecycle and infrastructure complexity (no transparent proxy/replay).
* Bad, because it loses SSE's built-in `Last-Event-ID` reconnection semantics, which the resume design relies on.

## Related Design Elements

**Actors**:
* `cpt-cf-chat-engine-actor-client` — consumes the SSE delta stream and reconstructs the message document
* `cpt-cf-chat-engine-actor-backend-plugin` — emits the chunk-based stream Chat Engine projects from

**Requirements**:
* `cpt-cf-chat-engine-fr-delta-streaming` — typed delta streaming to the client
* `cpt-cf-chat-engine-fr-send-message` — streamed assistant response
* `cpt-cf-chat-engine-fr-stop-streaming` — cancellation of an in-flight stream

**Design Elements**:
* `cpt-cf-chat-engine-design-streaming-protocol` — the wire vocabulary and projector
* `cpt-cf-chat-engine-design-entity-message-part` — the part structure deltas mutate
* `cpt-cf-chat-engine-design-entity-citations` — citations attached via `*.add` deltas

**Related ADRs**:
* ADR-0003 (Streaming Architecture) — streaming-first decision this protocol implements
* ADR-0006 (HTTP Client Protocol) — the plugin→engine chunk contract being projected
* ADR-0024 (Message Parts) — the part model the deltas address
* ADR-0027 (Stream Resumability) — `seq`/`Last-Event-ID` resume built on this vocabulary
