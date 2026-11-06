Created:  2026-02-04 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# ADR-0007: Webhook Event Schema with Typed Events

> **Note**: the dotted-event design below (`message.new`, `history`, `timestamp`) is superseded by the implemented wire (`session_created`, `message`, `messages`, ...). See [WEBHOOK-PROTOCOL.md](../WEBHOOK-PROTOCOL.md). The typed `event` field decision itself still holds.


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Typed events with event field](#option-1-typed-events-with-event-field)
  - [Option 2: Separate endpoints per event](#option-2-separate-endpoints-per-event)
  - [Option 3: Generic events with action hints](#option-3-generic-events-with-action-hints)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-02-04

**Status**: accepted

**Review**: Revisit if event schema versioning issues arise.

**ID**: `cpt-cf-chat-engine-adr-webhook-event-types`

## Context and Problem Statement

Chat Engine needs to communicate different types of events to webhook backends (session created, new message, message recreated, session deleted, summarization request). How should these events be structured to enable backends to handle different scenarios while maintaining protocol extensibility?

## Decision Drivers

* Clear differentiation between event types (creation vs recreation vs deletion)
* Extensibility for new event types without breaking changes
* Type safety for backend implementations
* Context completeness (backends need full session context)
* Backward compatibility when adding new event types
* Debugging and logging clarity (event type visible in logs)
* Support for different backend responses based on event type

## Considered Options

* **Option 1: Typed events with event field** - JSON payload with "event" field discriminating type
* **Option 2: Separate endpoints per event** - Different URLs for different event types
* **Option 3: Generic events with action hints** - Single event structure with optional action metadata

## Decision Outcome

Chosen option: "Typed events with event field", because it provides clear type discrimination via "event" field, enables single webhook URL handling multiple event types, supports extensibility by adding new event values, maintains protocol simplicity, and allows backends to route internally based on event type.

### Consequences

* Good, because event type explicit in payload (event: "message.new" vs "message.recreate")
* Good, because single webhook URL can handle all event types (simpler configuration)
* Good, because new event types addable without backend changes (unknown events ignored)
* Good, because event schemas can evolve per type (message.new can differ from session.created)
* Good, because debugging clear (event type visible in logs and traces)
* Good, because backend routing straightforward (switch on event field)
* Bad, because backends must handle multiple event types (cannot specialize per endpoint)
* Bad, because event schema validation more complex (discriminated union)
* Bad, because misrouted events not caught at URL routing level

### Confirmation

Confirmed by reviewing webhook payload schemas and consumer integration tests.

## Pros and Cons of the Options

### Option 1: Typed events with event field

JSON payload with an "event" discriminator field (e.g., "message.new", "session.created"). Single webhook URL receives all events.

* Good, because event type is explicit in the payload, enabling clear routing and logging
* Good, because single webhook URL simplifies backend configuration and management
* Good, because new event types can be added without changing webhook registration or URLs
* Good, because each event type can have its own tailored schema while sharing a common envelope
* Bad, because backends must implement dispatching logic to handle multiple event types
* Bad, because schema validation is more complex (discriminated union rather than per-endpoint schema)
* Bad, because a misconfigured handler for one event type can affect processing of others on the same endpoint

### Option 2: Separate endpoints per event

Each event type is sent to a distinct webhook URL (e.g., /webhooks/message-new, /webhooks/session-created).

* Good, because each endpoint has a single responsibility, simplifying per-event handler implementation
* Good, because URL-level routing enables independent scaling and monitoring per event type
* Good, because schema validation is straightforward — each endpoint has exactly one expected payload shape
* Bad, because webhook registration becomes complex — must configure and maintain a URL per event type
* Bad, because adding new event types requires registering new endpoints on both Chat Engine and backend
* Bad, because operational overhead increases with endpoint count (monitoring, alerting, documentation per URL)
* Bad, because cross-event concerns (authentication, logging, error handling) must be duplicated across endpoints

### Option 3: Generic events with action hints

Single event structure with a generic payload and optional action metadata fields. Backends interpret the metadata to determine behavior.

* Good, because a single, stable schema reduces protocol versioning concerns
* Good, because backends can ignore unrecognized action hints, providing natural forward compatibility
* Good, because simple to implement initially — one event structure fits all cases
* Bad, because loose typing makes backend implementation error-prone (action hints are advisory, not enforced)
* Bad, because debugging is harder — generic payloads lack semantic clarity in logs and traces
* Bad, because backends must inspect metadata to determine event semantics, adding implicit coupling
* Bad, because schema evolution is constrained — the generic structure may not accommodate future event types cleanly

## Related Design Elements

**Actors**:
* `cpt-cf-chat-engine-actor-backend-plugin` - Receives typed events, routes internally
* `cpt-cf-chat-engine-component-webhook-integration` - Constructs event payloads with correct type

**Requirements**:
* `cpt-cf-chat-engine-fr-create-session` - session.created event
* `cpt-cf-chat-engine-fr-send-message` - message.new event
* `cpt-cf-chat-engine-fr-recreate-response` - message.recreate event
* `cpt-cf-chat-engine-fr-delete-session` - session.deleted event
* `cpt-cf-chat-engine-fr-session-summary` - session.summary event
* `cpt-cf-chat-engine-fr-stop-streaming` - message.aborted event

**Design Elements**:
* Webhook API specification (Section 3.3.2 of DESIGN.md) defines all event schemas
* `cpt-cf-chat-engine-component-webhook-integration` - Event payload construction

**Related ADRs**:
* ADR-0013 (Recreation Creates Variants, Branching Creates Children) - message.recreate event semantics
