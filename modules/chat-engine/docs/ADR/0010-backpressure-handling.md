Created:  2026-02-04 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# ADR-0010: Streaming Backpressure with Buffer Limits


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Per-stream buffer with limit and pause](#option-1-per-stream-buffer-with-limit-and-pause)
  - [Option 2: Unbounded buffering](#option-2-unbounded-buffering)
  - [Option 3: Drop chunks](#option-3-drop-chunks)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-02-04

**Status**: accepted

**ID**: `cpt-cf-chat-engine-adr-backpressure-handling`

## Context and Problem Statement

Webhook backends may stream responses faster than clients can consume (slow network, slow device rendering). How should Chat Engine handle backpressure to prevent memory exhaustion while maintaining streaming responsiveness?

## Decision Drivers

* Prevent memory exhaustion from unbounded buffering
* Support slow clients without blocking fast backends entirely
* Graceful handling when client cannot keep up
* HTTP/2 flow control for backend requests
* Per-stream buffer limits (not global)
* Client disconnect cancels backend request
* Minimal latency when client is fast
* Observable buffer metrics for monitoring

## Considered Options

* **Option 1: Per-stream buffer with limit and pause** - Buffer up to 10MB, pause backend via HTTP/2 flow control
* **Option 2: Unbounded buffering** - Buffer all chunks until client catches up
* **Option 3: Drop chunks** - Discard chunks when buffer full

## Decision Outcome

Chosen option: "Per-stream buffer with limit and pause", because it prevents memory exhaustion via 10MB buffer limit, uses HTTP/2 flow control to pause backend when buffer fills, supports slow clients within buffer limit, enables client disconnect to immediately cancel backend request, and maintains low latency for fast clients.

### Consequences

* Good, because memory usage bounded (10MB max per stream)
* Good, because backend paused via HTTP/2 flow control (not cancelled)
* Good, because slow clients supported within buffer limit
* Good, because client disconnect immediately cancels backend (saves resources)
* Good, because fast clients see minimal latency (no buffering)
* Good, because per-stream limits prevent one slow client affecting others
* Bad, because extremely slow clients may exhaust buffer (stream cancellation)
* Bad, because HTTP/2 flow control complexity (not all backends support)
* Bad, because buffer management adds overhead (~5% CPU)
* Bad, because no prioritization (all streams treated equally)

### Confirmation

Confirmed via design review and alignment with DESIGN.md implementation.

## Pros and Cons of the Options

### Option 1: Per-stream buffer with limit and pause

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 2: Unbounded buffering

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 3: Drop chunks

See "Considered Options" and "Consequences" above for trade-off analysis.

## Related Design Elements

**Actors**:
* `cpt-cf-chat-engine-component-response-streaming` - Implements buffer and backpressure logic
* `cpt-cf-chat-engine-actor-backend-plugin` - Paused via HTTP/2 flow control
* `cpt-cf-chat-engine-actor-client` - Slow consumption triggers backpressure

**Requirements**:
* `cpt-cf-chat-engine-nfr-streaming` - Backpressure handling requirement
* `cpt-cf-chat-engine-fr-stop-streaming` - Client disconnect cancels backend

**Design Elements**:
* `cpt-cf-chat-engine-design-context-backpressure` - Implementation details (10MB limit, HTTP/2 flow control)
* `cpt-cf-chat-engine-component-response-streaming` - Buffer management per stream

**Related ADRs**:
* ADR-0003 (Streaming Architecture) - Streaming design depends on backpressure handling
* ADR-0008 (Client-Initiated Streaming Cancellation) - Client cancellation releases buffer
