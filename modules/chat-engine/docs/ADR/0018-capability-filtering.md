Created:  2026-02-04 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# ADR-0018: Per-Request Capability Filtering


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: enabled_capabilities array per message](#option-1-enabledcapabilities-array-per-message)
  - [Option 2: Session-level toggle](#option-2-session-level-toggle)
  - [Option 3: Implicit capabilities](#option-3-implicit-capabilities)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-02-04

**Status**: accepted

**ID**: `cpt-cf-chat-engine-adr-capability-filtering`

## Context and Problem Statement

Sessions have `enabled_capabilities` — typed `Capability` definitions (bool/enum/str/int) returned by the backend plugin on session creation (e.g., web_search: bool, response_style: enum, max_length: int). Users may want to selectively configure capabilities per message rather than using all defaults. How should clients pass capability settings for specific messages?

## Decision Drivers

* User control over expensive features (disable web_search to save costs)
* Backend receives explicit capability intent per message
* Capabilities available at session level, enabled at message level
* Client validates capabilities against available set
* Backend can optimize based on enabled capabilities
* Support for capability subsets (enable only web_search, not code_execution)
* Future-proof for new capability types
* Clear error messaging for unsupported capabilities

## Considered Options

* **Option 1: enabled_capabilities array per message** - Client sends array of `CapabilityValue` objects (`{id, value}`) with each message
* **Option 2: Session-level toggle** - Update session to enable/disable capabilities globally
* **Option 3: Implicit capabilities** - Backend infers from message content

## Decision Outcome

Chosen option: "enabled_capabilities array per message", because it provides per-message granularity for capability control, enables user cost optimization, gives backends explicit capability values (typed: bool/enum/str/int), supports capability subsets, maintains session `enabled_capabilities` as the authoritative capability registry, and supports all value types without protocol changes.

### Consequences

* Good, because users disable expensive capabilities per message (cost optimization)
* Good, because backend receives explicit intent (no capability inference needed)
* Good, because supports capability subsets (enable some, disable others)
* Good, because future capabilities work without protocol changes
* Good, because session `enabled_capabilities` remains authoritative (typed Capability definitions)
* Good, because client can validate before sending (check id + value type against session's `enabled_capabilities`)
* Bad, because client must send `CapabilityValue[]` with every message
* Bad, because value type validation (id exists in session's `enabled_capabilities`, value matches declared type) adds overhead
* Bad, because invalid capability IDs or type mismatches rejected (error handling complexity)
* Bad, because capability defaults not enforced (client must specify values explicitly)

### Confirmation

Confirmed via design review and alignment with DESIGN.md implementation.

## Pros and Cons of the Options

### Option 1: enabled_capabilities array per message

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 2: Session-level toggle

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 3: Implicit capabilities

See "Considered Options" and "Consequences" above for trade-off analysis.

## Related Design Elements

**Actors**:
* `cpt-cf-chat-engine-actor-client` - Sends `CapabilityValue[]` per message
* `cpt-cf-chat-engine-actor-backend-plugin` - Receives `CapabilityValue[]`, optimizes processing accordingly

**Design Elements**:
* Chat Engine validates `CapabilityValue.id` against session's `enabled_capabilities` and value type against `Capability.type`

**Requirements**:
* `cpt-cf-chat-engine-fr-send-message` - Message includes `enabled_capabilities: CapabilityValue[]`
* `cpt-cf-chat-engine-fr-create-session` - Session stores `enabled_capabilities: Capability[]`

**Design Elements**:
* `cpt-cf-chat-engine-design-entity-session` - `enabled_capabilities: Capability[]` (authoritative type registry)
* HTTP POST /messages/send — `enabled_capabilities: CapabilityValue[]`
* Webhook `message.new` event — `enabled_capabilities: CapabilityValue[]`

**Related ADRs**:
* ADR-0002 (Capability Model) - Backend defines `enabled_capabilities` (Capability definitions)
* ADR-0015 (Session Type Switching with Capability Updates) - Capabilities update when switching backends
