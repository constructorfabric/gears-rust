---
status: accepted
date: 2026-10-20
description: The Application Manager gear runs every seed and uninstall as a background operation, admitted by a bounded synchronous phase that answers with the operation id, and brought to a final state within a run window sized by the section plugin count
---
<!-- Created: 2026-10-20 by Constructor Tech -->
# Seed and Uninstall Run as Background Operations with a Run Window

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Background operations with a synchronous admission phase and a run window](#background-operations-with-a-synchronous-admission-phase-and-a-run-window)
  - [Fully synchronous seed and uninstall within one HTTP request](#fully-synchronous-seed-and-uninstall-within-one-http-request)
  - [Background operations with a durable queue that resumes work after a restart](#background-operations-with-a-durable-queue-that-resumes-work-after-a-restart)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-async-operations`

## Context and Problem Statement

A seed fetches every plugin's schema and then calls `apply` on each plugin in turn; an uninstall calls `remove` on each. Each call may take up to `plugin_call_timeout`, so a request's time grows with the plugin count and can pass the gateway's 30s deadline. A call that exceeds its time limit is stopped, and each plugin keeps its own data consistent when that happens. How does the gear answer every request quickly and still bring every accepted request to a known end?

## Decision Drivers

* `cpt-cf-application-manager-nfr-bounded-request-time`
* `cpt-cf-application-manager-fr-async-operations`, `cpt-cf-application-manager-fr-one-operation-per-application`
* `cpt-cf-application-manager-fr-bounded-plugin-calls`
* `cpt-cf-application-manager-constraint-no-manifest-content`

## Considered Options

* Background operations with a synchronous admission phase and a run window
* Fully synchronous seed and uninstall within one HTTP request
* Background operations with a durable queue that resumes work after a restart

## Decision Outcome

Chosen option: "Background operations with a synchronous admission phase and a run window", because it answers every request within the gateway deadline whatever the plugin count, and gives every accepted request a known end. `cpt-cf-application-manager-fr-async-operations` forbids resuming, so the deciding comparison is against the synchronous option.

* A bounded synchronous phase makes the quick checks, with no plugin call, and either refuses or commits the operation and answers 202 with its id.
* A supervised request task, detached from the HTTP request, runs the operation.
* A run deadline set at acceptance ends every operation as completed or interrupted; an interrupted operation is never resumed.
* At the time limit the gear drops a plugin call and dispatches the next section.
* One open operation per application.

The steps, states, and formulas are in [DESIGN §3.6](../DESIGN.md#36-interactions--sequences), [§3.1](../DESIGN.md#31-domain-model), and [§4.2](../DESIGN.md#42-configuration).

### Consequences

* Callers read the operation for the result.
* The gear owns an operations table, a status guard, a supervisor, and a sweeper.
* Plugins keep their data consistent when a call is stopped, for example by doing the work in one transaction.
* Plugins tolerate repeated `apply` and `remove`, because callers retry with a new request.
* An operation whose replica stops blocks its application until its run deadline.

### Confirmation

* Every synchronous answer comes within the synchronous time budget, and `init` fails when the budget exceeds the gateway deadline.
* A table-driven test over the synchronous rows of the outcome table, including the 503 gate and the 202 shape.
* A run-deadline arithmetic test for seed and uninstall.
* An operation past its deadline ends interrupted, no call starts after it, and no replica resumes it.
* A timed-out call is dropped and dispatch proceeds.

## Pros and Cons of the Options

### Background operations with a synchronous admission phase and a run window

* Good, because the synchronous phase has a checked bound inside the gateway deadline.
* Good, because every operation ends by its run deadline, even when its replica stops.
* Bad, because callers must read the operation, and an interrupted operation can leave an application partly seeded.

### Fully synchronous seed and uninstall within one HTTP request

* Good, because the answer carries the result, with no operations table or sweeper.
* Bad, because (N + 1) × `plugin_call_timeout` must stay under the 30s deadline, which ties the plugin count and time limit to it.

### Background operations with a durable queue that resumes work after a restart

* Good, because an operation survives a restart.
* Bad, because resuming needs the manifest in durable storage and repeats calls with unknown outcome; `cpt-cf-application-manager-fr-async-operations` forbids it.
