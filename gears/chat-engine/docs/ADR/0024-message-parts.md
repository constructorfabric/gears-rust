Created:  2026-06-30 by Constructor Tech
Updated:  2026-06-30 by Constructor Tech
# ADR-0024: Parts-Based Message Model


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Ordered list of typed parts in a child table](#option-1-ordered-list-of-typed-parts-in-a-child-table)
  - [Option 2: Single content blob with a type tag](#option-2-single-content-blob-with-a-type-tag)
  - [Option 3: Fixed columns per content kind](#option-3-fixed-columns-per-content-kind)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-06-30

**Status**: accepted

**Review**: Revisit when adding `audio` / `document` / `table` part types, or if per-part streaming of non-text content is required.

**ID**: `cpt-cf-chat-engine-adr-message-parts`

## Context and Problem Statement

An assistant answer is rarely just one blob of text: it can interleave prose, code blocks, image/video references, link cards, and status notes, and each fragment may need its own type, ordering, and (for text) its own citations. The original model stored a message as a single `content` field, which cannot represent a heterogeneous, ordered body or anchor citations to a specific span. How should a message body be modeled so it supports mixed, ordered, typed content and incremental streaming?

## Decision Drivers

* Represent a heterogeneous, ordered message body (text, code, images, videos, links, statuses)
* Per-part typing with structurally validated `content` shapes
* Stable ordering for streaming (open part N, append to part N) and for citation anchoring per text block
* Text-only full-text search without indexing binary/structured payloads
* Vendor extensibility of the part-type set via GTS (`cpt-cf-chat-engine-fr-schema-extensibility`)

## Considered Options

* **Option 1: Ordered list of typed parts in a child table** — a message owns 1..N `MessagePart` rows, each `{type, content, number}`, in their own table with CASCADE to `messages`.
* **Option 2: Single content blob with a type tag** — keep one `content` JSON column plus a `content_type`.
* **Option 3: Fixed columns per content kind** — dedicated columns/tables per kind (text, code, images…).

## Decision Outcome

Chosen option: "Ordered list of typed parts in a child table". A message body is an ordered list of `MessagePart` rows (`cpt-cf-chat-engine-design-entity-message-part`), each carrying a `type` (`text`, `code`, `images`, `videos`, `links`, `statuses`), a typed `content` JSON whose shape is determined by `type`, and a 0-based `number` that is unique per message (`UNIQUE(message_id, number)`). The former scalar `content` field/column is removed; on read, the SDK `Message` carries `parts: Vec<MessagePart>` ordered by `number`. Ordinals are assigned as `MAX(number)+1` within the insert transaction, reusing the SERIALIZABLE-retry machinery from variant indexing (`cpt-cf-chat-engine-adr-variant-indexing`). The streaming text part is filled as chunks arrive and frozen on completion; the part-type set is extensible by plugin vendors via GTS. Chat Engine validates `content` structurally but leaves semantics to plugins (`cpt-cf-chat-engine-principle-zero-business-logic`).

### Consequences

* Good, because a single answer can interleave typed fragments in a stable order.
* Good, because text parts can own per-block citations (`cpt-cf-chat-engine-adr-citations`).
* Good, because text-only parts are cleanly indexable for full-text search.
* Good, because the streaming protocol maps naturally onto "add part / append to part" deltas.
* Bad, because reading a message requires joining/loading a child table (more rows per message).
* Bad, because ordinal assignment needs the SERIALIZABLE-retry path under concurrent inserts.
* Bad, because the wire/persisted shapes diverge (`MessagePartInput` vs `MessagePart`).

### Confirmation

Confirmed by the persistence tests that round-trip multi-part messages preserving `number` order, and by the streaming tests where the projector opens `parts/{n}` and appends text to `parts/{n}/content/text`.

## Pros and Cons of the Options

### Option 1: Ordered list of typed parts in a child table

* Good, because it cleanly models heterogeneous, ordered, typed bodies.
* Good, because it enables per-part citations and per-part streaming deltas.
* Bad, because it adds a child table, a join on read, and ordinal-assignment concurrency handling.

### Option 2: Single content blob with a type tag

* Good, because reads are a single row with no join.
* Bad, because a single blob cannot represent an ordered mix of differently typed fragments.
* Bad, because citations cannot be anchored to a specific text block within the blob.

### Option 3: Fixed columns per content kind

* Good, because each kind has a strongly typed home.
* Bad, because adding a new kind requires a schema migration (no GTS-driven extensibility).
* Bad, because ordering across heterogeneous kinds becomes awkward and sparse.

## Related Design Elements

**Actors**:
* `cpt-cf-chat-engine-actor-client` — renders the ordered, typed parts
* `cpt-cf-chat-engine-actor-backend-plugin` — emits parts (and the streamed text part)

**Requirements**:
* `cpt-cf-chat-engine-fr-message-parts` — parts-based message body
* `cpt-cf-chat-engine-fr-send-message` — the turn whose body is composed of parts

**Design Elements**:
* `cpt-cf-chat-engine-design-entity-message-part` — the part entity and per-type `content` shapes
* `cpt-cf-chat-engine-design-entity-citations` — citations owned by a text part

**Related ADRs**:
* ADR-0001 (Message Tree Structure) — the message rows parts hang off of
* ADR-0012 (Variant Indexing) — the SERIALIZABLE-retry ordinal pattern reused for `number`
* ADR-0025 (Citations) — per-text-part citation model
* ADR-0026 (SSE Delta Streaming) — deltas that open and append to parts
