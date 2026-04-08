Created:  2026-02-04 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# ADR-0008: Client-Initiated Streaming Cancellation


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Close HTTP connection](#option-1-close-http-connection)
  - [Option 2: HTTP DELETE request](#option-2-http-delete-request)
  - [Option 3: HTTP timeout](#option-3-http-timeout)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-02-04

**Status**: accepted

**ID**: `cpt-cf-chat-engine-adr-streaming-cancellation`

## Context and Problem Statement

Users may want to stop assistant responses mid-generation (too slow, wrong direction, changing question). How should clients cancel ongoing streaming responses to save compute resources and provide responsive "stop" button UX?

## Decision Drivers

* User control over generation (stop button in UI)
* Compute resource conservation (cancel backend processing)
* Partial response preservation (save incomplete response)
* Responsive cancellation (immediate UI feedback)
* Simple cancellation mechanism
* Backend cleanup (cancel backend request)
* Database persistence of partial responses

## Considered Options

* **Option 1: Close HTTP connection** - Abort HTTP request to cancel stream
* **Option 2: HTTP DELETE request** - Separate HTTP endpoint to cancel by message_id
* **Option 3: HTTP timeout** - Set aggressive timeout to limit long operations

## Decision Outcome

Chosen option: "Close HTTP connection", because it provides immediate cancellation by aborting the HTTP request (using AbortController in browsers or request cancellation in other clients), saves backend resources, preserves partial responses with is_complete=false flag, aligns with standard HTTP patterns, and requires no separate cancellation endpoint.

### Consequences

* Good, because standard HTTP cancellation pattern (AbortController)
* Good, because immediate resource cleanup (no lingering connections)
* Good, because simple client implementation (abort request)
* Good, because backend detects disconnection immediately
* Good, because partial response preserved with is_complete=false flag
* Good, because no separate cancellation endpoint needed
* Bad, because connection close terminates stream (by design)
* Bad, because no explicit acknowledgment (implicit via disconnection)
* Bad, because backend must handle connection close gracefully

### Confirmation

Confirmed via design review and alignment with DESIGN.md implementation.

## Pros and Cons of the Options

### Option 1: Close HTTP connection

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 2: HTTP DELETE request

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 3: HTTP timeout

See "Considered Options" and "Consequences" above for trade-off analysis.

## Related Design Elements

**Actors**:
* `cpt-cf-chat-engine-actor-client` - Closes HTTP connection on user action
* `cpt-cf-chat-engine-component-response-streaming` - Detects connection close, saves partial response
* `cpt-cf-chat-engine-component-webhook-integration` - Cancels HTTP request to backend

**Requirements**:
* `cpt-cf-chat-engine-fr-stop-streaming` - Cancel streaming, save partial response with incomplete flag
* `cpt-cf-chat-engine-nfr-streaming` - Minimal latency for cancellation response

**Design Elements**:
* `cpt-cf-chat-engine-design-entity-message` - is_complete field indicates cancelled messages
* HTTP connection close mechanism (Section 3.3.1 of DESIGN.md)
* Sequence diagram S11 (Stop Streaming Response)

**Related ADRs**:
* ADR-0003 (Streaming Architecture) - Depends on this for complete streaming lifecycle
* ADR-0006 (HTTP Client Protocol) - HTTP streaming client protocol with cancellation
