Created:  2026-02-04 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# ADR-0015: Session Type Switching with Capability Updates


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Update session_type_id, route next message to new plugin (chosen)](#option-1-update-sessiontypeid-route-next-message-to-new-plugin-chosen)
  - [Option 2: Create new session, copy history](#option-2-create-new-session-copy-history)
  - [Option 3: Message-level backend tracking](#option-3-message-level-backend-tracking)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-02-04

**Status**: accepted

**ID**: `cpt-cf-chat-engine-adr-session-switching`

## Context and Problem Statement

Chat Engine supports multiple session types (different webhook backends like GPT-4, Claude, human support). Users may want to switch backends mid-conversation (e.g., escalate from AI to human). How should Chat Engine handle session type switching while preserving conversation history and updating capabilities?

## Decision Drivers

* Preserve full conversation history when switching
* Update capabilities to reflect new backend features
* No message loss or data corruption
* Backend receives complete history (not just current type's messages)
* Simple client API for switching
* Session metadata remains consistent
* Support switching to/from any backend type
* Enable use cases like AI → human escalation

## Considered Options

* **Option 1: Update session_type_id, route next message to new backend** - Mutable session_type_id field, routing changes immediately
* **Option 2: Create new session, copy history** - New session record with duplicated messages
* **Option 3: Message-level backend tracking** - Each message stores backend used, no session-level type

## Decision Outcome

Chosen option: "Update session_type_id, route next message to new backend", because it preserves conversation history in single session, updates capabilities from new backend, enables simple client API (single field update), maintains referential integrity, and supports all switching use cases (AI ↔ AI, AI ↔ human).

### Consequences

* Good, because single session retains full conversation history
* Good, because new plugin receives complete history (all messages)
* Good, because client API simple (session.switch_type event)
* Good, because no message duplication or data migration
* Good, because `available_capabilities` refreshed from new plugin via `on_session_type_configured`
* Good, because session metadata (title, timestamps) preserved
* Bad, because history mixing plugins may confuse some plugin implementations
* Bad, because old capabilities become stale (stored but inactive)
* Bad, because cannot easily revert to previous plugin (no capability restoration)
* Bad, because plugin type history not tracked per message

### Confirmation

Confirmed when switching `session_type_id` causes the next message to be routed to the new plugin, `Session.enabled_capabilities` is updated from the new `SessionType.available_capabilities`, and full conversation history is passed to the new plugin's `on_message` call.

## Pros and Cons of the Options

### Option 1: Update session_type_id, route next message to new plugin (chosen)

* Good, because single session retains full history
* Good, because simple client API
* Bad, because history mixing plugins may cause unexpected behavior

### Option 2: Create new session, copy history

* Good, because clean separation between old and new plugin contexts
* Bad, because message duplication; breaks session continuity for the client
* Bad, because complex migration logic required

### Option 3: Message-level backend tracking

* Good, because precise tracking of which plugin handled each message
* Bad, because routing complexity grows as history spans multiple plugins
* Bad, because no single authoritative capability set for a session

## Related Design Elements

**Actors**:
* `cpt-cf-chat-engine-actor-client` - Initiates session type switching
* `cpt-cf-chat-engine-actor-backend-plugin` - New plugin receives full history
* `cpt-cf-chat-engine-component-session-management` - Updates session_type_id

**Requirements**:
* `cpt-cf-chat-engine-fr-switch-session-type` - Switch to different backend mid-conversation
* `cpt-cf-chat-engine-fr-send-message` - Routing uses current session_type_id

**Design Elements**:
* `cpt-cf-chat-engine-design-entity-session` - session_type_id field (mutable)
* `cpt-cf-chat-engine-design-entity-session-type` - References `plugin_instance_id` per backend type
* Sequence diagram S4 (Switch Session Type Mid-Conversation)

**Related ADRs**:
* ADR-0002 (Capability Model) - New plugin provides updated `available_capabilities` via `on_session_type_configured`
* ADR-0022 (Plugin Backend Integration) - Plugin trait methods; `on_message` receives full history
* ADR-0018 (Per-Request Capability Filtering) - Client can enable/disable capabilities per message
