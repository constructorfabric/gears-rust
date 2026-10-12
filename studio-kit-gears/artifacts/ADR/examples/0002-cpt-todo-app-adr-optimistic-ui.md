---
status: accepted
date: 2026-02-07
--- 

# ADR-0002: Optimistic UI Updates


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Always wait for server confirmation](#always-wait-for-server-confirmation)
  - [Optimistically update UI and reconcile in background](#optimistically-update-ui-and-reconcile-in-background)
  - [Hybrid approach per action type](#hybrid-approach-per-action-type)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-examples-todo-app-adr-optimistic-ui`

## Context and Problem Statement

The Todo App should feel fast and responsive even when network conditions are poor or intermittent. If the UI waits for server confirmation for every action, perceived latency increases and offline-first behavior becomes inconsistent.

## Decision Drivers

- Maintain a fast perceived response time for core actions
- Preserve offline-first behavior without blocking on network
- Provide a consistent UX for create/complete/delete actions
- Allow safe rollback on server-side rejections or conflicts

## Considered Options

- Always wait for server confirmation before updating UI
- Optimistically update UI and reconcile in background
- Hybrid approach per action type

## Decision Outcome

Chosen option: **Optimistically update UI and reconcile in background**.

### Consequences

- Good, because UI stays responsive and consistent with offline-first flow
- Good, because actions work without network connectivity
- Bad, because we need reconciliation/rollback logic for rare rejection/conflict cases

### Confirmation

Confirmed via:

- UI performance benchmarks demonstrating perceived latency stays within `cpt-examples-todo-app-nfr-response-time`
- Integration tests that cover offline mode + background sync reconciliation
- Code review ensuring rollback/retry logic exists for failed sync events

## Pros and Cons of the Options

### Always wait for server confirmation

* Good, because simplest mental model — no rollback needed
* Bad, because slow perceived latency on every action
* Bad, because completely blocks offline usage

### Optimistically update UI and reconcile in background

* Good, because instant perceived response
* Good, because works seamlessly offline
* Bad, because requires reconciliation/rollback logic for conflicts

### Hybrid approach per action type

* Good, because critical actions get server confirmation
* Bad, because inconsistent UX across actions
* Bad, because added complexity to decide which actions are "critical"

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

- `cpt-examples-todo-app-nfr-response-time`
- `cpt-examples-todo-app-nfr-offline-support`
- `cpt-examples-todo-app-principle-optimistic-updates`
- `cpt-examples-todo-app-principle-offline-first`
- `cpt-examples-todo-app-flow-core-create-task`
- `cpt-examples-todo-app-flow-core-delete-task`
- `cpt-examples-todo-app-actor-user`
- `cpt-examples-todo-app-actor-sync-service`
