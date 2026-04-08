Created:  2026-02-04 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# ADR-0004: Zero Business Logic in Routing Layer


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Zero business logic (pure routing)](#option-1-zero-business-logic-pure-routing)
  - [Option 2: Enrichment layer](#option-2-enrichment-layer)
  - [Option 3: Smart routing](#option-3-smart-routing)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-02-04

**Status**: accepted

**ID**: `cpt-cf-chat-engine-adr-routing-layer`

## Context and Problem Statement

Chat Engine sits between clients and webhook backends as a proxy service. Should Chat Engine inspect, analyze, or transform message content, or should it remain a pure routing infrastructure focused on session management and message persistence?

## Decision Drivers

* Enable rapid backend experimentation without infrastructure changes
* Keep Chat Engine focused on infrastructure concerns (routing, persistence, scaling)
* Avoid coupling Chat Engine to specific backend implementations or processing logic
* Support diverse backend types (LLMs, rule-based, human-in-the-loop)
* Simplify Chat Engine codebase and reduce maintenance burden
* Enable backends to evolve independently
* Minimize latency overhead from proxying

## Considered Options

* **Option 1: Zero business logic (pure routing)** - Chat Engine only routes, persists, and manages message trees
* **Option 2: Enrichment layer** - Chat Engine adds metadata, moderation, logging before routing
* **Option 3: Smart routing** - Chat Engine analyzes content to select appropriate backend or transform messages

## Decision Outcome

Chosen option: "Zero business logic (pure routing)", because it decouples infrastructure from processing logic, enables backends to change without Chat Engine updates, keeps routing latency minimal, allows diverse backend implementations, and simplifies Chat Engine codebase focusing on reliability and scaling.

### Consequences

* Good, because backends can change processing logic without Chat Engine deployment
* Good, because new backend types require zero Chat Engine code changes
* Good, because routing layer remains simple, testable, and maintainable
* Good, because latency overhead is minimal (no content inspection/transformation)
* Good, because Chat Engine can focus on reliability, scaling, and message tree management
* Good, because content moderation, language detection, etc. can be backend-specific
* Bad, because common processing (moderation, logging enrichment) must be implemented per backend
* Bad, because Chat Engine cannot provide value-added services (e.g., automatic translation)
* Bad, because debugging requires looking at backend logs (Chat Engine doesn't inspect content)

### Confirmation

Confirmed via design review and alignment with DESIGN.md implementation.

## Pros and Cons of the Options

### Option 1: Zero business logic (pure routing)

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 2: Enrichment layer

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 3: Smart routing

See "Considered Options" and "Consequences" above for trade-off analysis.

## Related Design Elements

**Actors**:
* `cpt-cf-chat-engine-actor-backend-plugin` - Responsible for ALL message processing logic

**Design Elements**:
* `cpt-cf-chat-engine-component-webhook-integration` - Chat Engine's HTTP proxy functionality with timeout/circuit breaker

**Requirements**:
* All functional requirements assume Chat Engine routes without processing
* `cpt-cf-chat-engine-nfr-response-time` - Minimal overhead from routing (< 100ms)

* `cpt-cf-chat-engine-principle-zero-business-logic` - Design principle codifying this decision
* `cpt-cf-chat-engine-component-webhook-integration` - Chat Engine's HTTP client functionality for pure forwarding
* `cpt-cf-chat-engine-design-context-circuit-breaker` - Backend responsibility scope

**Related ADRs**:
* ADR-0002 (Capability Model) - Backends define capabilities, not Chat Engine
* ADR-0007 (Webhook Event Schema with Typed Events) - Events carry full context without interpretation
