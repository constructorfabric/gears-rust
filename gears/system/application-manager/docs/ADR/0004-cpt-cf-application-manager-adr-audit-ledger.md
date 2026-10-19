---
status: accepted
date: 2026-10-20
description: The Application Manager gear records every seed and uninstall request as events in an append-only audit ledger table in its own database, written in the same transactions as the changes they describe.
---
<!-- Created: 2026-10-05 by Constructor Tech -->
# Append-Only Audit Ledger in the Gear's Database

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Append-only ledger table written in the same transactions](#append-only-ledger-table-written-in-the-same-transactions)
  - [Append-only ledger table written in a separate transaction after the change](#append-only-ledger-table-written-in-a-separate-transaction-after-the-change)
  - [Audit events only as structured logs and telemetry](#audit-events-only-as-structured-logs-and-telemetry)
  - [Audit fields on the operation record, purged with `operation_retention`](#audit-fields-on-the-operation-record-purged-with-operation_retention)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-audit-ledger`

## Context and Problem Statement

Every seed and uninstall request that reaches the gear, including a refused one, must leave an audit event (`cpt-cf-application-manager-fr-audit-event`). An event must never exist for a change that did not commit, and nothing may be dispatched before the accepted event is recorded (`cpt-cf-application-manager-fr-audit-ordering`). Audit serves human debugging: it records what happened. The gear never reads it back or decides anything from it, which keeps `cpt-cf-application-manager-fr-pass-through` intact. The PRD fixes the store: the audit ledger is an append-only table and the only place where the gear keeps audit events (`cpt-cf-application-manager-fr-audit-event`). How should the gear write its audit events so that they stay consistent with the changes they describe?

## Decision Drivers

* Every request has its audit events, and no event exists for a change that did not commit (`cpt-cf-application-manager-nfr-audit-completeness`).
* The accepted event is recorded before any dispatch.
* A refusal records its event before it answers, and a failure to record it never changes the refusal's category.
* The gear keeps no manifest or section content (`cpt-cf-application-manager-constraint-no-manifest-content`).
* The audit trail should need no dependency beyond the gear's own database.
* Audit retention and reading audit data are outside the PRD scope.

## Considered Options

* Append-only ledger table written in the same transactions
* Append-only ledger table written in a separate transaction after the change
* Audit events only as structured logs and telemetry
* Audit fields on the operation record, purged with `operation_retention`

## Decision Outcome

Chosen option: "Append-only ledger table written in the same transactions", because it gives refused requests a place for their events, keeps the audit trail independent of `operation_retention`, and lets the gear use the table append-only. Its accepted and finished events commit with the changes they describe, so the accepted event gates dispatch, and it needs nothing beyond the gear's own database.

The decision has these parts:

* **Ledger table.** An append-only `audit_ledger` table lives in the gear's own database and is reached through SecureConn (`cpt-cf-application-manager-dbtable-audit-ledger`).
* **Single writer.** The operation repository is the only writer, and it only inserts (`cpt-cf-application-manager-component-operation-repository`).
* **Write points.**
  * The accepted event is inserted in the acceptance transaction, together with the operation record.
  * The finished event is inserted in the final transaction, together with the final report. The final transaction includes the interruption step and the supervisor completion.
  * A refused event is inserted in its own transaction before the refusal answers.
* **Failed refused event.** A refusal whose event cannot be recorded still answers with its category, and the failure is logged and signalled to operators.
* **Content.** A ledger row holds only these fields, by event kind, as `cpt-cf-application-manager-entity-audit-event-content` sets out:
  * every event: the event id, the kind, the action, the correlation id and the write time;
  * the envelope fields: the application id, and for a seed its version, within the limits; a refused event shows an id or version outside the limits only as its byte length and SHA-256 digest;
  * accepted and refused seed events: the keys of the section key set present in the manifest, with a count of other keys;
  * accepted and refused events: the caller identity and the receive time;
  * the finished event: the category or the interrupted marker, the problem count and rule tokens, the section outcome per key of the key set with schema versions, and unavailable items;
  * the refused event: the category, and the problem count and rule tokens of the envelope problems, the busy operation id, or unavailable items.
* **Reading.** The ledger is read only by operators through direct database access.
* **Retention.** Rows are kept without a purge, because audit retention is outside PRD scope.
* **Scoping.** Rows are unrestricted and have no tenant column ([ADR 0005](./0005-cpt-cf-application-manager-adr-platform-level-access.md)).

### Consequences

* The repository declares the ledger entity and owns every insert. Acceptance and final transactions each gain one insert, and the refusal path gains one transaction.
* A storage failure in the acceptance transaction fails the operation record and its accepted event together, so nothing is dispatched.
* Operators size database storage for ledger growth, because the gear never purges rows.
* Operators inspect the ledger by direct database access, its only read path.
* An acceptance that timed out after it committed leaves one request with an accepted event, a refused event and later a finished event, all with one correlation id ([DESIGN §4.3](../DESIGN.md#43-fault-tolerance)).
* Tamper evidence rests on append-only use by the gear, not on a database-level guarantee.
* If audit reading or retention enters scope, the table, its access and its lifecycle must be revisited.

### Confirmation

* Inspection that the operation repository is the only code that touches the audit ledger table and only inserts into it, and that no route and no sweeper step reads, updates or deletes a ledger row.
* Tests that the accepted event and the operation record commit or fail together.
* Tests that nothing is dispatched before the accepted event commits.
* A test that a refusal whose event fails to record still answers its category and is logged.
* A test that no ledger row holds section content or problem locations.
* A table-driven test over every outcome category, the interruption step and the supervisor completion compares the ledger rows with the operations and refusals: each accepted operation has its accepted event and, once final, its finished event committed with its report; each refusal has its refused event recorded before it answers; and an envelope id or version outside the limits appears only as its byte length and SHA-256 digest.

## Pros and Cons of the Options

### Append-only ledger table written in the same transactions

An `audit_ledger` table in the gear's database, with inserts in the acceptance transaction, the final transaction, and a separate refusal transaction.

* Good, because accepted and finished events commit with the change they describe.
* Good, because refused requests have a place for their events, and the audit trail does not depend on `operation_retention`.
* Good, because the accepted event is durable before any dispatch.
* Good, because it needs no dependency beyond the gear's database and fits its migrations and SecureConn rules.
* Good, because inspection and tests can query rows directly.
* Bad, because the table grows without a purge.
* Bad, because inspection needs direct database access.
* Bad, because append-only use is a code convention, not a storage guarantee.
* Bad, because a refusal's event is in its own transaction, so it can fail while the refusal still answers.

### Append-only ledger table written in a separate transaction after the change

The same `audit_ledger` table, with each accepted or finished event inserted in its own transaction after the change it describes has committed.

* Good, because the acceptance and final transactions stay smaller, one insert fewer each.
* Good, because a failed event insert never fails the change it describes.
* Neutral, because refused events are written as in the chosen option.
* Bad, because the accepted event is not atomic with the operation record, so it cannot gate dispatch: an operation can commit while its accepted event fails, and the gear must either dispatch with no accepted event or leave a committed operation that never runs.
* Bad, because a crash between the change and its event loses the event, so the ledger can miss events for committed changes.

### Audit events only as structured logs and telemetry

Emit each event as a structured log line or telemetry record, and keep no audit table.

* Good, because it adds no table, no growth in the gear's database, and no write to the request path.
* Good, because logs and telemetry already flow to platform tooling.
* Bad, because a log line cannot commit with the operation record, so an event can exist for a change that did not commit, or be missing for one that did.
* Bad, because the accepted event cannot gate dispatch, since a log write has no commit to wait for.
* Bad, because log pipelines can sample, drop or expire lines, so completeness cannot be tested row by row.

### Audit fields on the operation record, purged with `operation_retention`

Keep the audit content as columns of the operation record, and purge it with the record.

* Good, because the acceptance and final transactions already write the record, so the events are atomic with the change at no extra table cost.
* Good, because storage stays bounded by `operation_retention`.
* Bad, because a refusal creates no operation record, so refused requests have no place to keep their event.
* Bad, because audit data is lost when the sweeper purges the record, which ties the audit trail to operation lifetime.
* Bad, because it mixes the working record, which the gear reads and updates, with an audit trail the gear never reads, and the updates make append-only use impossible.

## More Information

Revisit this decision if reading or exposing audit data, or audit retention, enters PRD scope. Either would add a read path or a purge to a table that has neither.

Related decision:

* [ADR 0003](./0003-cpt-cf-application-manager-adr-async-operations.md): the acceptance transaction, the run window and the final transaction that the accepted and finished events share.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-application-manager-fr-audit-event` — every audit event is one insert into the append-only ledger, made only by the operation repository.
* `cpt-cf-application-manager-fr-audit-ordering` — the accepted event commits with the operation record before dispatch, the finished event commits with the report, and a refused event is inserted before the refusal answers.
* `cpt-cf-application-manager-nfr-audit-completeness` — events share the transactions of the changes they describe, so none exists for a change that did not commit.
* `cpt-cf-application-manager-fr-pass-through` — the gear never reads the ledger back or decides from it, and rows hold no section content.
* `cpt-cf-application-manager-constraint-no-manifest-content` — rows hold only the bounded envelope fields, section keys of the key set, counts and rule tokens.
* `cpt-cf-application-manager-dbtable-audit-ledger` — the append-only table this decision creates.
* `cpt-cf-application-manager-component-operation-repository` — the only writer of the ledger, insert only.
* `cpt-cf-application-manager-principle-trace-before-change` — no plugin call happens before the accepted event commits, each final state commits with its finished event, and each refusal records its refused event before it answers.
* `cpt-cf-application-manager-principle-minimal-state` — the ledger is append-only, holds no manifest or section content, and the gear never reads it back.
* `cpt-cf-application-manager-entity-audit-event-content` — the fields of each event kind that a ledger row holds.
* `cpt-cf-application-manager-nfr-operational-signals` — a refused event that fails to record is logged and signalled to operators.
