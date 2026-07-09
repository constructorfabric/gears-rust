Created:  2026-06-30 by Constructor Tech
Updated:  2026-06-30 by Constructor Tech
# ADR-0025: Per-Text-Part Citations and References


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Plugin-provided citations anchored to a text part](#option-1-plugin-provided-citations-anchored-to-a-text-part)
  - [Option 2: Chat Engine scans text and computes anchors](#option-2-chat-engine-scans-text-and-computes-anchors)
  - [Option 3: Message-level citation list](#option-3-message-level-citation-list)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-06-30

**Status**: accepted

**Review**: Revisit if citations must attach to non-text part types, or if streamed (incremental) citation delivery is required rather than attach-on-finalize.

**ID**: `cpt-cf-chat-engine-adr-citations`

## Context and Problem Statement

Retrieval-augmented answers must show their sources: which document or web page backs a given claim, and where in the answer text the `[N]` marker sits. Sources come in distinct kinds (a quote from a retrieved document, a citation into a web page, a lightweight URL badge), each with different fields. Chat Engine is a transport that must not invent or recompute source positions. How should citations and references be modeled, anchored to the answer, and persisted — without Chat Engine doing business logic over the text?

## Decision Drivers

* Anchor a source to a specific span of a specific text block, not vaguely to the message
* Support three distinct kinds with different field sets (document citation, web-page citation, URL badge)
* Keep Chat Engine a pure transport — positions are authored by the plugin, forwarded verbatim (`cpt-cf-chat-engine-principle-zero-business-logic`)
* Persist citations so a re-read of a finished message shows the same sources
* Stable positional mapping so `[N]` in the text resolves to the same source on every read

## Considered Options

* **Option 1: Plugin-provided citations anchored to a text part** — three sibling entities, each CASCADE-linked to a `message_part`; positions provided by the plugin and forwarded verbatim.
* **Option 2: Chat Engine scans text and computes anchors** — the engine parses `[N]` markers and computes character offsets.
* **Option 3: Message-level citation list** — one flat citation list per message, not per text block.

## Decision Outcome

Chosen option: "Plugin-provided citations anchored to a text part". Citations and references attach to a single `text` `MessagePart` (`cpt-cf-chat-engine-adr-message-parts`), so a multi-part answer cites per text block. Three sibling kinds (`cpt-cf-chat-engine-design-entity-citations`), each a row with a CASCADE foreign key to `message_parts(id)`:

* **FileCitation** (`cpt-cf-chat-engine-design-entity-file-citation`) — a citation into a retrieved document (document id/name, quote, char range, chunk info, reference type, position anchors).
* **LinkCitation** (`cpt-cf-chat-engine-design-entity-link-citation`) — a citation into a web page (url, title, preview, quote, char range).
* **LinkReference** (`cpt-cf-chat-engine-design-entity-link-reference`) — a lightweight URL badge with no quote/anchor, with a per-part `idx` so positional `[N]` → `refs[N-1]` is stable.

`FileCitation` and `LinkCitation` share one `[N]` marker namespace within a part. `text_positions[i]` is the character offset where the `[index]` marker appears and `text_position_anchors[i]` is the matching source location (parallel arrays). **Chat Engine forwards positions/anchors verbatim from the plugin — it never scans the text or computes offsets** (`cpt-cf-chat-engine-principle-zero-business-logic`). Citations are provided by the backend on its terminal text part and persisted when the assistant's text part is finalized (attach-on-finalize, not streamed incrementally); they CASCADE-delete with their part and are immutable once written. On read, each `text` part surfaces its `file_citations`, `link_citations`, and `references` arrays.

### Consequences

* Good, because every source anchors to an exact text span of an exact block.
* Good, because the three kinds capture genuinely different shapes without a lowest-common-denominator schema.
* Good, because Chat Engine stays a pure transport — no fragile text parsing or offset math.
* Good, because CASCADE + immutability keep citation lifecycle tied to the part with no orphan cleanup.
* Bad, because the plugin bears full responsibility for correct positions; a buggy plugin yields wrong anchors and Chat Engine cannot detect it.
* Bad, because attach-on-finalize means citations are not visible mid-stream (they arrive with completion).

### Confirmation

Confirmed by the projector tests that turn completion-time citation lists into `*.add` deltas targeting the correct `parts/{n}/...` path, and by persistence tests round-tripping the three citation kinds on a text part.

## Pros and Cons of the Options

### Option 1: Plugin-provided citations anchored to a text part

* Good, because anchoring is per-block and positions are authoritative (plugin-owned).
* Good, because it keeps Chat Engine free of text business logic.
* Bad, because correctness depends entirely on the plugin; the engine cannot validate anchors.

### Option 2: Chat Engine scans text and computes anchors

* Good, because the client gets anchors even from a plugin that omits positions.
* Bad, because it puts business logic (text parsing, offset computation) into the transport, violating the zero-business-logic principle.
* Bad, because marker conventions vary per plugin; a generic scanner would be wrong often.

### Option 3: Message-level citation list

* Good, because the model is flat and simple.
* Bad, because it cannot say which text block a source backs in a multi-part answer.
* Bad, because `[N]` markers in different blocks would collide in one namespace.

## Related Design Elements

**Actors**:
* `cpt-cf-chat-engine-actor-client` — renders citations/references against text spans
* `cpt-cf-chat-engine-actor-backend-plugin` — authors citations and their positions

**Requirements**:
* `cpt-cf-chat-engine-fr-citations` — per-answer source citations
* `cpt-cf-chat-engine-fr-message-parts` — the text parts citations attach to

**Design Elements**:
* `cpt-cf-chat-engine-design-entity-citations` — the umbrella citation/reference model
* `cpt-cf-chat-engine-design-entity-file-citation` — document citation entity
* `cpt-cf-chat-engine-design-entity-link-citation` — web-page citation entity
* `cpt-cf-chat-engine-design-entity-link-reference` — URL-badge reference entity
* `cpt-cf-chat-engine-principle-zero-business-logic` — positions forwarded verbatim

**Related ADRs**:
* ADR-0024 (Message Parts) — the text part a citation anchors to
* ADR-0026 (SSE Delta Streaming) — citation `*.add` deltas on the wire
* ADR-0005 (File Handling) — external storage for the documents citations point at
