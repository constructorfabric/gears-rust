---
status: accepted
date: 2026-10-24
description: "Serve workers over the REST API with capped long-poll claims, batch heartbeats, and a per-node queue watcher for cross-node wake-up; streaming transports are deferred."
---

# Serve Workers over REST with Long-Poll Claims

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Short polling](#short-polling)
  - [REST long-poll claims](#rest-long-poll-claims)
  - [Streaming (gRPC bidirectional stream or WebSocket)](#streaming-grpc-bidirectional-stream-or-websocket)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-worker-transport`

## Context and Problem Statement

Workers need a way to get new tasks quickly, keep their leases alive, and report results. Workers run in many places and may be behind proxies, load balancers, or unstable networks. The engine runs as several instances behind a load balancer, and a task created on one instance must reach a worker connected to another.

Which transport should workers use to receive tasks and talk to the engine?

## Decision Drivers

* New work should reach an idle worker within about a second.
* Work through common proxies and load balancers without special setup.
* Survive dropped connections without losing tasks or leases.
* Use the same authentication, authorization, and error model as the rest of the API.
* No dependency on the Cluster gear or a message broker for correctness.
* Keep per-task request overhead low for workers that run many tasks.

## Considered Options

* Short polling
* REST long-poll claims
* Streaming (gRPC bidirectional stream or WebSocket)

## Decision Outcome

Chosen option: "REST long-poll claims", because it gives near-instant delivery with plain HTTP, works through standard infrastructure, and reuses the platform REST stack (`OperationBuilder`, AuthN/AuthZ, RFC 9457 errors).

How it works:

* A worker calls `POST /tasks/claim` with a timeout and may ask for up to `max_tasks` tasks (ADR-0008). If no task is ready, the request waits. The wait is capped by `claim_max_wait_secs` (default 50 seconds, below the common 60-second proxy idle timeout); a longer request is shortened, not rejected.
* A waiting request holds no database connection. It is woken by an in-process signal when work arrives on the same instance.
* For work created on another instance, each instance runs one queue watcher. It checks each queue that has waiting workers once per `claim_poll_interval_ms` (default 1 second) and, on a hit, wakes one claim per waiter group (node-local coalescing, ADR-0008). The cost depends on the number of queues with waiters, not on the number of workers.
* The claim response includes a `session_id` and `heartbeat_interval_secs` (one third of the session lease). A worker renews all its leases in a domain with one heartbeat of its worker session (ADR-0017).
* A dropped connection loses nothing: an unclaimed task stays queued, and a claimed task is recovered by lease expiry.

### Consequences

* Each instance must handle many idle connections (target 5,000 per node). The request path keeps waiting claims away from the database pool and admission permits.
* Cross-instance delivery can take up to one poll interval.
* Workers claim several tasks per request when they have free capacity (`max_tasks`) and send one heartbeat per worker session for all their leases.
* A streaming transport can be added later as an extra transport beside REST (DESIGN §4.8). That needs a new ADR.

### Confirmation

* Tests check that a task created on one instance is claimed by a worker waiting on another instance within the poll interval.
* Tests check that a requested timeout above the cap is shortened.
* Load tests check 5,000 waiting claims per node with the database pool in its normal range.

## Pros and Cons of the Options

### Short polling

The worker asks for work in a loop with a sleep between requests.

* Good, because it is the simplest client and server.
* Bad, because latency equals the sleep, or the request rate becomes very high.
* Bad, because idle workers create constant database load.

### REST long-poll claims

* Good, because delivery is near-instant on the same instance and within one poll interval across instances.
* Good, because plain HTTP works through proxies and reuses the platform REST stack.
* Good, because a lost connection has no state to clean up.
* Neutral, because each idle worker holds one open request.
* Bad, because each claim is a separate request.

### Streaming (gRPC bidirectional stream or WebSocket)

The engine pushes tasks over a long-lived stream.

* Good, because pushes are instant and per-message overhead is low.
* Bad, because streams need sticky routing, reconnect logic, and flow control on both sides.
* Bad, because some proxies and load balancers break long streams.
* Bad, because the platform REST stack (`OperationBuilder`, standard errors) does not cover streams, so auth and errors must be rebuilt.

## More Information

PRD §4.3 lists other client APIs, such as gRPC, as future scope.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-task-engine-fr-assignment` — long-poll claim with a capped wait and cross-node wake-up.
* `cpt-cf-task-engine-fr-heartbeat` — heartbeat interval and batch heartbeat.
* `cpt-cf-task-engine-fr-rest-api` — the REST API is the worker transport.
* `cpt-cf-task-engine-nfr-latency` — claim latency targets.
* `cpt-cf-task-engine-nfr-capacity` — 5,000 parallel client connections.
* `cpt-cf-task-engine-design-api-contracts` — the claim and heartbeat endpoints.
