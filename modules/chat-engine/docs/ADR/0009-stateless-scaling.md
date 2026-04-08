Created:  2026-02-04 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# ADR-0009: Stateless Horizontal Scaling with Database State


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Stateless instances with database state](#option-1-stateless-instances-with-database-state)
  - [Option 2: Stateful instances with sticky sessions](#option-2-stateful-instances-with-sticky-sessions)
  - [Option 3: Redis cache layer](#option-3-redis-cache-layer)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-02-04

**Status**: accepted

**ID**: `cpt-cf-chat-engine-adr-stateless-scaling`

## Context and Problem Statement

Chat Engine must support 10,000 concurrent sessions and handle traffic spikes. How should Chat Engine instances be designed to enable horizontal scaling (adding more instances) while maintaining session consistency and simplifying operational complexity?

## Decision Drivers

* Support 10K+ concurrent sessions via horizontal scaling
* Simplify deployment and operations (Kubernetes friendly)
* Eliminate stateful instance complexity (no session affinity)
* Any instance can handle any request (load balancing flexibility)
* Database provides consistency (ACID transactions)
* Fault tolerance via instance redundancy
* Auto-scaling based on load (CPU/memory)
* No shared memory or inter-instance coordination

## Considered Options

* **Option 1: Stateless instances with database state** - All session state in PostgreSQL, instances stateless
* **Option 2: Stateful instances with sticky sessions** - WebSocket connections pinned to specific instances
* **Option 3: Redis cache layer** - Session state cached in Redis, database as backup

## Decision Outcome

Chosen option: "Stateless instances with database state", because it enables simple horizontal scaling (add instances without coordination), eliminates session affinity complexity for load balancing, provides fault tolerance (any instance failure transparent), simplifies deployment (Kubernetes native), and leverages database ACID guarantees for consistency.

### Consequences

* Good, because any instance can handle any request (no session affinity)
* Good, because simple horizontal scaling (add pods, no state migration)
* Good, because instance failure transparent (no connection state lost)
* Good, because auto-scaling straightforward (scale on CPU/memory)
* Good, because deployment simple (stateless containers)
* Good, because database handles consistency (ACID transactions)
* Bad, because every request requires database queries (no in-memory state)
* Bad, because database becomes scaling bottleneck (write throughput limit)
* Bad, because no request coalescing or in-memory optimizations

### Confirmation

Confirmed via design review and alignment with DESIGN.md implementation.

## Pros and Cons of the Options

### Option 1: Stateless instances with database state

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 2: Stateful instances with sticky sessions

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 3: Redis cache layer

See "Considered Options" and "Consequences" above for trade-off analysis.

## Related Design Elements

**Actors**:
* Chat Engine instances (stateless pods) - HTTP servers with no persistent connection state
* `cpt-cf-chat-engine-actor-database` - Single source of truth for all state

**Requirements**:
* `cpt-cf-chat-engine-nfr-scalability` - 10K concurrent sessions, horizontal scaling
* `cpt-cf-chat-engine-nfr-availability` - Instance failures must not affect service
* `cpt-cf-chat-engine-nfr-response-time` - Routing latency < 100ms despite database queries

**Design Elements**:
* cpt-cf-chat-engine-topology-cloud - Kubernetes deployment with 3+ replicas
* `cpt-cf-chat-engine-constraint-single-database` - Database provides shared state
* All components designed as stateless services

**Related ADRs**:
* ADR-0009 (Stateless Horizontal Scaling with Database State) - Database provides all persistent state
* ADR-0006 (HTTP Client Protocol) - Stateless HTTP protocol enables true horizontal scaling
