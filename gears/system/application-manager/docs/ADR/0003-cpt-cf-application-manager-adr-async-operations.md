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
  - [Background operations that cancel plugin calls at timeout](#background-operations-that-cancel-plugin-calls-at-timeout)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-async-operations`

## Context and Problem Statement

A seed fetches the live schema of every section plugin and then calls `apply` on each one in priority order. An uninstall calls `remove` on each one. Every call may take up to `plugin_call_timeout`, so the time a request needs grows with the number of section plugins and can pass the gateway's fixed 30s request deadline. Section plugin work must never be cancelled or undone, and every request must still get an answer within the platform deadline. How does the gear admit, run, bound and report a seed or uninstall so that callers get a quick answer and every accepted request reaches a known end?

## Decision Drivers

* Every seed and uninstall request is answered within the platform request deadline, with a refusal or with an operation id (`cpt-cf-application-manager-nfr-bounded-request-time`).
* A request that fails a quick check is refused at once, with no operation and no section plugin call, and an accepted request gets its operation id before any section plugin call (`cpt-cf-application-manager-fr-async-operations`).
* Every operation reaches a final state, completed or interrupted, within a bounded run window, and an interrupted operation is never resumed (`cpt-cf-application-manager-fr-async-operations`).
* The gear stops waiting for a section plugin call at one configured time limit and never cancels or undoes the plugin's work (`cpt-cf-application-manager-fr-bounded-plugin-calls`, `cpt-cf-application-manager-nfr-bounded-call-time`).
* One application has at most one open operation, and operations for different applications never wait for each other (`cpt-cf-application-manager-fr-one-operation-per-application`).
* Nothing is dispatched before the operation record and its accepted event have committed ([ADR 0004](./0004-cpt-cf-application-manager-adr-audit-ledger.md)).
* The gear stores no manifest or section content and keeps operation records only for `operation_retention` (`cpt-cf-application-manager-constraint-no-manifest-content`, `cpt-cf-application-manager-fr-operation-read`).
* Resuming and cancelling operations are outside the PRD scope.

## Considered Options

* Background operations with a synchronous admission phase and a run window
* Fully synchronous seed and uninstall within one HTTP request
* Background operations with a durable queue that resumes work after a restart
* Background operations that cancel plugin calls at timeout

## Decision Outcome

Chosen option: "Background operations with a synchronous admission phase and a run window", because it is the only option that answers every request within the platform deadline, gives every accepted request a known end, and never cuts section plugin work short, while it keeps no manifest content beyond the request task's memory.

The PRD constrains two of the options: `cpt-cf-application-manager-fr-async-operations` forbids resuming an interrupted operation, which rules out the durable queue that resumes work, and `cpt-cf-application-manager-fr-bounded-plugin-calls` forbids cancelling plugin work, which rules out cancelling plugin calls at timeout. The comparison that decides is the one against a fully synchronous seed and uninstall.

The decision has these parts:

* **Bounded synchronous phase.** The synchronous phase makes the quick checks that refuse a request at ranks 1 to 4 of the outcome table in [DESIGN §3.3](../DESIGN.md#33-api-contracts): the readiness and capacity gate, the envelope or id check, the gear-level PDP decision ([ADR 0005](./0005-cpt-cf-application-manager-adr-platform-level-access.md)), and the read of the application's open operation. It makes no section plugin call. It ends with a refusal, or with the acceptance transaction that commits the operation record and its accepted event together ([ADR 0004](./0004-cpt-cf-application-manager-adr-audit-ledger.md)) and answers 202 with the operation id and a `Location` header for the operation read. Its time is bounded by `pdp_call_timeout` and `storage_call_timeout`, and `init` fails with `invalid_setting` when that time budget exceeds the gateway's 30s request deadline.
* **Background run.** After acceptance, the same request task runs the seed or uninstall. The task is supervised and detached from the HTTP request, so a caller disconnect cuts no step short. When the task fails before it answers, the caller gets 500 `internal`. When it fails after acceptance, the supervisor completes the operation as internal failure.
* **Run window anchored at acceptance.** The acceptance transaction sets the run deadline from the acceptance commit time, sized by the number of section plugins, `plugin_call_timeout` and `run_window_margin`. No plugin call starts after the run deadline.
* **Abandon, never cancel.** The plugin call guard stops waiting at `plugin_call_timeout`, records `timed_out`, and abandons the call without cancelling it, so the plugin may still finish. A cap on calls in flight per plugin instance, `plugin_max_in_flight`, counts abandoned calls, and a call over the cap is never sent (`not_sent`). Dispatch continues with the next section plugin after any failure.
* **Interrupt, never resume.** Every write to an open operation matches only an open record before its run deadline, so the first final write wins. An operation still open at its run deadline is recorded as interrupted with its section outcomes so far, by the operation sweeper, the operation read, or the next acceptance for the application, whichever meets it first. The gear never resumes an operation, and callers retry by sending a new request.
* **One open operation per application.** A unique index on open operations admits one open operation per exact application id across replicas. A second request is refused as conflict, 409 `aborted` with the reason `application_busy`, naming the open operation. An open operation past its run deadline is interrupted by the next acceptance in the same transaction.

The DESIGN holds the detail: the steps of the synchronous phase in [DESIGN §3.3](../DESIGN.md#33-api-contracts) and [Synchronous Checks and Acceptance](../DESIGN.md#synchronous-checks-and-acceptance); the operation states, status guard, interruption step, supervisor completion and retention after `operation_retention` in [DESIGN §3.1](../DESIGN.md#31-domain-model); the time budget, the run-window formula with its margin floor and warning, and the sweep and purge intervals in [DESIGN §4.5](../DESIGN.md#45-configuration); and the shutdown sequence in [DESIGN §3.8](../DESIGN.md#38-deployment-topology).

### Consequences

* The gear owns an operations table with a unique index on open operations, indexes for the sweep and the purge, and the operation sweeper on every replica.
* `plugin_call_timeout`, `run_window_margin`, `operation_retention`, `plugin_max_in_flight`, `max_request_tasks`, `pdp_call_timeout` and `storage_call_timeout` are required settings with no default, and `init` checks the synchronous time budget against `GATEWAY_REQUEST_DEADLINE`.
* Operators size `max_request_tasks` for an environment-wide re-seed, size `plugin_max_in_flight` above it so a plugin meets its cap only when it hangs, and set the database pool acquire timeout below `storage_call_timeout`.
* Request tasks and guarded plugin calls do not take the shutdown token, which is a documented platform deviation with the ToolKit lifecycle owners as review owners.
* Seeding callers read the operation to learn the result, and the seeding documentation tells them to do so.
* Section plugins must tolerate repeated `apply` and `remove`, because a timed-out call may still finish and callers retry an interrupted or failed operation with a new request.
* An application whose operation is abandoned by a stopped replica, or whose acceptance committed after the request timed out, stays blocked until that operation's run deadline. A retry before then gets conflict naming it.
* Operators watch the interrupted-operation count, the calls still running after their timeout, the request tasks at the cap, and the run window margin warning.

### Confirmation

* Paused-clock tests with test plugins that always reach the time limit show that every synchronous answer, refusal or 202, comes within the synchronous time budget of DESIGN §4.5.
* A configuration test shows that `init` fails with `invalid_setting` when that budget exceeds `GATEWAY_REQUEST_DEADLINE`.
* A table-driven test over the synchronous rows of the outcome table in DESIGN §3.3 shows that each case answers with its HTTP status and canonical error category, including 503 from the readiness and capacity gate, and that an accepted request gets 202 with `operation_id`, `status` `accepted` and a `Location` header.
* A run-deadline arithmetic test for a seed and for an uninstall shows that the run deadline matches the run-window formula of DESIGN §4.5.
* Recording test plugins see no call before the operation record and its accepted event commit, and no call for any synchronous refusal.
* A test in which an operation passes its run deadline shows that it ends as interrupted with the section outcomes so far, that no plugin call starts after the deadline, and that after a restart no replica resumes it.
* A test with a plugin that does not answer within `plugin_call_timeout` shows the section outcome `timed_out`, a call that was abandoned and not cancelled, and dispatch that proceeds to the next section plugin.
* A test with a plugin at `plugin_max_in_flight` shows the section outcome `not_sent`.
* A conflict test shows that a second request for an application with an open operation gets 409 naming the open operation, that a request for another application is accepted, and that an expired open operation is interrupted by the next acceptance.
* A test with a request task that fails after acceptance shows the operation completed as internal failure with `not_dispatched` for the plugins not yet called.
* A sweeper test shows that final records older than `operation_retention` are purged and then read as not found, and that open records are never purged.

## Pros and Cons of the Options

### Background operations with a synchronous admission phase and a run window

A bounded synchronous phase checks and accepts the request and answers with the operation id. A supervised request task runs the operation, and the run deadline gives it a known end.

* Good, because the synchronous phase has a checked bound inside the gateway deadline, whatever the number of section plugins.
* Good, because every operation ends completed or interrupted by its run deadline, even when its replica stops.
* Good, because section plugin work is never cancelled, and a caller disconnect cuts no step short.
* Good, because manifest content lives only in the request task's memory, so no record holds it.
* Good, because the operation id gives callers and operators one place to read the status and the report.
* Neutral, because a timed-out call may still finish after the operation has moved on, which section plugins already tolerate through repeated `apply` and `remove`.
* Bad, because callers must read the operation, and an accepted request does not carry its result in the answer.
* Bad, because an interrupted operation can leave an application partly seeded or partly uninstalled until the caller sends a new request.
* Bad, because an abandoned operation blocks its application until its run deadline passes.
* Bad, because the gear needs an operations table, a sweeper, a supervisor, and a status guard on every write.

### Fully synchronous seed and uninstall within one HTTP request

The gear runs every check, schema call and plugin call inside the HTTP request and answers with the final report.

* Good, because the answer carries the result, and callers need no operation read.
* Good, because it needs no operations table, run window, sweeper or supervisor.
* Bad, because the request time grows with the number of section plugins, so (N + 1) × `plugin_call_timeout` must stay under the gateway's 30s deadline, which ties the plugin count and the time limit to that deadline.
* Bad, because a request that passes the deadline is ended by the platform, so the caller gets no result for work that may have run.
* Neutral, because the work could run in a detached task while the request waits, which keeps plugin calls whole when the caller disconnects, but the answer still cannot come within the 30s deadline once the plugin calls of one request take longer.

### Background operations with a durable queue that resumes work after a restart

The gear stores each accepted request durably, and any replica claims open work and continues it after a restart.

* Good, because an operation survives a restart and can finish without a new request.
* Good, because a stopped replica does not leave an application blocked until a run deadline.
* Bad, because resuming needs the manifest and the caller identity in durable storage, which breaks `cpt-cf-application-manager-constraint-no-manifest-content` and the minimal operation record.
* Bad, because a resumed run cannot know the effect of a call that timed out or was in flight, so it repeats calls with unknown outcome.
* Bad, because the plugin set can differ after a restart, so a resumed operation can meet keys and priorities it was not accepted with.
* Bad, because the run time loses its bound, which conflicts with `cpt-cf-application-manager-nfr-bounded-request-time`, and `cpt-cf-application-manager-fr-async-operations` forbids resuming an interrupted operation.
* Bad, because claims across replicas need leases and recovery rules of their own.

### Background operations that cancel plugin calls at timeout

As the chosen option, but the guard drops a plugin call at `plugin_call_timeout` and so cancels it.

* Good, because no abandoned call keeps running, so the in-flight cap and the shutdown drain of plugin calls have less to hold.
* Good, because a timed-out section has a definite end inside the gear.
* Bad, because it breaks `cpt-cf-application-manager-fr-bounded-plugin-calls`, which forbids cancelling plugin work.
* Bad, because cancelling a call can stop a plugin halfway through a change, and neither the gear nor the plugin learns how far it got.
* Bad, because the section plugin contract offers no cancel or undo, so the plugin cannot clean up after a cancelled call.

## More Information

Revisit this decision if resuming or cancelling operations, idempotency keys, or retry rules enter the PRD scope, if out-of-process section plugins become supported, or if the gateway request deadline changes. Related decisions:

* [ADR 0001](./0001-cpt-cf-application-manager-adr-section-plugin-fan-out.md): the plugin set is fixed at startup, so N, and with it the run window, is fixed for the process lifetime.
* [ADR 0002](./0002-cpt-cf-application-manager-adr-plugin-served-section-schemas.md): live schemas are fetched inside the accepted operation, which is the extra `plugin_call_timeout` in a seed's run window.
* [ADR 0004](./0004-cpt-cf-application-manager-adr-audit-ledger.md): the accepted, finished and refused events and their write points.
* [ADR 0005](./0005-cpt-cf-application-manager-adr-platform-level-access.md): the gear-level PDP decision of the synchronous phase.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-application-manager-fr-async-operations` — quick checks refuse at once, an accepted request gets its operation id before any plugin call, and the run deadline brings every operation to completed or interrupted, never resumed.
* `cpt-cf-application-manager-fr-one-operation-per-application` — a unique index on open operations admits one per application, and a conflict refusal names the open operation.
* `cpt-cf-application-manager-fr-operation-read` — callers read the status and, once final, the report, and the sweeper purges final records after `operation_retention`.
* `cpt-cf-application-manager-fr-bounded-plugin-calls` — the call guard stops waiting at `plugin_call_timeout`, abandons the call without cancelling it, and dispatch continues.
* `cpt-cf-application-manager-nfr-bounded-request-time` — the synchronous phase has a time budget that `init` checks against the gateway deadline, and the run deadline bounds every operation.
* `cpt-cf-application-manager-nfr-bounded-call-time` — every plugin call ends for the gear within the configured limit.
* `cpt-cf-application-manager-fr-audit-ordering` — acceptance commits the operation record with its accepted event before any dispatch.
* `cpt-cf-application-manager-constraint-no-manifest-content` — manifest content stays in the request task's memory and is never stored for a later run.
* `cpt-cf-application-manager-entity-operation` — the operation states, the run deadline, the status guard, the interruption step and the supervisor completion.
* `cpt-cf-application-manager-dbtable-operations` — the table that holds operations, with the unique index on open operations.
* `cpt-cf-application-manager-component-request-orchestrator` — runs the synchronous phase and the supervised request task.
* `cpt-cf-application-manager-component-plugin-call-guard` — the time limit and the in-flight cap on every plugin call.
* `cpt-cf-application-manager-component-operation-sweeper` — interrupts expired open operations and purges final records.
* `cpt-cf-application-manager-principle-bounded-waiting` — the gear stops waiting at the limit and never cancels plugin work.
* `cpt-cf-application-manager-seq-pre-dispatch-gate` — the synchronous checks and the acceptance.
* `cpt-cf-application-manager-seq-operation-sweep` — the sweep and purge sequence.
