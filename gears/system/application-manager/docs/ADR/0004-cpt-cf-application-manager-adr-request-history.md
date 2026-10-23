---
status: accepted
date: 2026-10-20
description: The Application Manager gear records every seed and uninstall request as events in an append-only request history table in its own database, written in the same transactions as the changes they describe.
---
<!-- Created: 2026-10-05 by Constructor Tech -->
# Append-Only Request History in the Gear's Database

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Append-only history table written in the same transactions](#append-only-history-table-written-in-the-same-transactions)
  - [Append-only history table written in a separate transaction after the change](#append-only-history-table-written-in-a-separate-transaction-after-the-change)
  - [History events only as structured logs and telemetry](#history-events-only-as-structured-logs-and-telemetry)
  - [History fields on the operation record, purged with `operation_retention`](#history-fields-on-the-operation-record-purged-with-operation_retention)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-request-history`

## Context and Problem Statement

Every seed and uninstall request, including a refused one, must leave a history event in an append-only table that is the only history store (`cpt-cf-application-manager-fr-history-event`). No event may exist for a change that did not commit, and nothing may be dispatched before the accepted event is recorded (`cpt-cf-application-manager-fr-history-ordering`). How should the gear write its history events so they stay consistent with the changes they describe?

## Decision Drivers

* `cpt-cf-application-manager-nfr-history-completeness`
* `cpt-cf-application-manager-fr-history-ordering`
* `cpt-cf-application-manager-constraint-no-manifest-content`

## Considered Options

* Append-only history table written in the same transactions
* Append-only history table written in a separate transaction after the change
* History events only as structured logs and telemetry
* History fields on the operation record, purged with `operation_retention`

## Decision Outcome

Chosen option: "Append-only history table written in the same transactions", because accepted and finished events commit with their changes so the accepted event gates dispatch, refused requests have a place for their events, and the trail is independent of `operation_retention`.

* A `request_history` table in the gear's database, written only by the operation repository, insert only ([DESIGN §3.7](../DESIGN.md#37-database-schemas--tables)).
* The accepted event commits in the acceptance transaction, the finished event in the final transaction, and a refused event in its own transaction before the refusal answers.
* A refusal whose event cannot be recorded keeps its category and is logged and signalled.
* Rows hold only the fields of `cpt-cf-application-manager-entity-history-event-content`, are unrestricted ([ADR 0005](./0005-cpt-cf-application-manager-adr-platform-level-access.md)), and are kept without a purge.
* Operators read a request's events through the gear's history read after a `read_history` permit; the gear never decides from them.

### Consequences

* Operators size database storage for history growth.
* Tamper evidence rests on append-only use by the gear.
* A refused event can fail while its refusal still answers.

### Confirmation

* Inspection: only the operation repository writes the table, and only inserts.
* A table-driven test over every outcome category, the interruption step, and the supervisor completion compares history rows with operations and refusals, including the digest of an out-of-limit id or version.
* A failure-injection test: when a refused event cannot be recorded, the refusal keeps its category and the missing event is logged and signalled; every other refusal leaves a refused event.
* The accepted event and the operation record commit or fail together, and nothing is dispatched before they commit.

## Pros and Cons of the Options

### Append-only history table written in the same transactions

* Good, because events commit with their changes and need only the gear's database.
* Bad, because the table grows without a purge.

### Append-only history table written in a separate transaction after the change

* Good, because the change transactions stay smaller.
* Bad, because the accepted event cannot gate dispatch, and a crash between change and event loses the event.

### History events only as structured logs and telemetry

* Good, because it adds no table.
* Bad, because a log line cannot commit with the change, and pipelines can drop lines; `cpt-cf-application-manager-fr-history-event` requires a table.

### History fields on the operation record, purged with `operation_retention`

* Good, because events are atomic with the change and storage stays bounded.
* Bad, because refusals have no record, and history data is purged with the operation.
