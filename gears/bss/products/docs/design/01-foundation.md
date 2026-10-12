<!-- CONFLUENCE_TITLE: [BSS]: Products — Foundation (Design, Slice 1) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../DECISIONS.md | Owners: BSS Product Catalog team -->

# DESIGN — Foundation (Slice 1)

- [ ] `p1` - **ID**: `cpt-cf-bss-products-design-slice-01`

<!-- toc -->

- [1. Context](#1-context)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [A caller executes a scoped mutation](#a-caller-executes-a-scoped-mutation)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [two-backends](#two-backends)
  - [if-match](#if-match)
  - [idempotency-key](#idempotency-key)
  - [audit-row](#audit-row)
  - [outbox-same-tx](#outbox-same-tx)
- [4. States (CDSL)](#4-states-cdsl)
- [5. API Surface](#5-api-surface)
- [6. Data Model](#6-data-model)
- [7. Events & Alarms](#7-events--alarms)
- [8. Definitions of Done](#8-definitions-of-done)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Non-Functional Considerations](#10-non-functional-considerations)

<!-- /toc -->

## 1. Context

Foundation supplies the scoped repositories, new migration chain, conditional approval Store, audit,
replay and transactional outbox used by slices 02–04. It implements the storage boundary of
[DESIGN §3.2 and §3.7](../DESIGN.md#32-component-model), not new business doors. The slice allocation
is in DESIGN §4; layering is in §1.3. `products-sdk` remains the contract boundary; `bss-approval`
supplies shared types and rules, while Products owns its tables and subjects.

Content authority: `docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout
(hereafter “spec”), §2.2, §4 and §6. [DECISIONS](../DECISIONS.md) P-D-190–194 govern the amended
approval, replay and reference contracts. This slice depends on phase 1a's approval library and
reuses toolkit infrastructure. Phase 0 specifies implementation; the checkboxes do not claim it exists.

## 2. Actor Flows (CDSL)

### A caller executes a scoped mutation

1. [ ] - `p1` - Authenticate the caller and obtain the PolicyEnforcer-derived tenant AccessScope before opening any repository or replay entry - `inst-fnd-authorize`
2. [ ] - `p1` - For a keyed POST, resolve replay before performing fence or approval work; for PATCH, pass the required If-Match version to the conditional repository write - `inst-fnd-preconditions`
3. [ ] - `p1` - Open one scoped transaction for the operation; invoke the owning slice's domain rule and persist state, required audit and outbox records through that transaction - `inst-fnd-mutate`
4. [ ] - `p1` - Store the replay answer when applicable, commit, then return the result and the row's new ETag; only committed outbox rows can dispatch - `inst-fnd-commit`
5. [ ] - `p1` - On a refused or failed transaction, roll back its state, claim, audit and outbox writes; a fence committed earlier remains available for recovery - `inst-fnd-rollback`

## 3. Processes / Business Logic (CDSL)

### two-backends

1. [ ] - `p1` - Run the fresh chain on SQLite and Postgres with the same keys, checks and append-only invariants; do not migrate deployed data - `inst-fnd-migrate`
2. [ ] - `p1` - Execute repositories through SecureConn and scoped transactions; approval child rows are reachable only through a tenant-scoped parent - `inst-fnd-scope`
3. [ ] - `p1` - Use conditional writes instead of FOR UPDATE; reserve and fence transactions use serializable isolation on Postgres and SQLite writer serialization - `inst-fnd-isolation`
4. [ ] - `p1` - Retry a Postgres serialization failure once and SQLite lock-upgrade failure through the same bounded transaction retry loop; re-read all guards on retry - `inst-fnd-retry`

### if-match

1. [ ] - `p1` - Obtain the concurrency version from If-Match; use the toolkit precondition response if absent, use SKU revision or category version, never published_version - `inst-fnd-if-match-read`
2. [ ] - `p1` - Apply a tenant/id and concurrency-token conditional SKU or category update, checking pending ownership where relevant and incrementing version atomically - `inst-fnd-if-match-cas`
3. [ ] - `p1` - A stale version returns 409 STALE_REVISION and makes no write; a successful read or write exposes the concurrency version as ETag - `inst-fnd-if-match-result`

### idempotency-key

1. [ ] - `p1` - Every POST accepts an optional Idempotency-Key; a keyless POST bypasses the client-key store and a keyed POST addresses tenant_id, concrete endpoint and client_key, checking a retained answer before any fence or unit work - `inst-fnd-replay-lookup`
2. [ ] - `p1` - Compare payload_hash before replay; different content cannot execute or replay another request's answer under the same retained key - `inst-fnd-replay-hash`
3. [ ] - `p1` - Claim the key conditionally within the guarded transaction, with expires_at set for 24-hour retention; concurrent claims cannot both perform the mutation - `inst-fnd-replay-claim`
4. [ ] - `p1` - Answer with response_status and response_body in that same transaction; rollback removes the uncommitted claim, and replay returns the saved answer without new domain writes - `inst-fnd-replay-answer`
5. [ ] - `p1` - Commit fence, submission, claim and answer in one transaction; when an existing orphan fence is resumed, retain its fence_op_id instead of starting an independent operation - `inst-fnd-replay-fence`

A committed `UNIT_STALE` refresh is a domain outcome, not a database rollback. The approval service
commits the refresh and its keyed receipt together. Repeating that key replays UNIT_STALE; a decision
on the refreshed generation uses a new key because a changed body under the retained key conflicts.
There is no approval-unit idempotency column and no second replay store (spec §2.2).

### audit-row

1. [ ] - `p1` - Construct a tenant-scoped row with actor_ref, action, subject_kind, a subject identifier, written_at and available correlation_id; use attempted_key for a refusal without a subject id - `inst-fnd-audit-build`
2. [ ] - `p1` - Insert approval submission audit with the submission; insert every terminal audit and operator force-release audit with the corresponding state transaction, even when it also emits an event - `inst-fnd-audit-write`
3. [ ] - `p1` - Insert as unsealed with null chain metadata; reject deletion and record-field updates, allowing only the reserved one-way sealing transition that preserves the record - `inst-fnd-audit-guard`

### outbox-same-tx

1. [ ] - `p1` - Accept the caller's scoped transaction in the event writer; never open a second connection to record the event - `inst-fnd-outbox-tx`
2. [ ] - `p1` - Append required domain and approval events to the existing toolkit outbox with the state and audit writes - `inst-fnd-outbox-append`
3. [ ] - `p1` - Commit all or roll back all; dispatch only after commit, without publishing success for APPLY_REFUSED or stale refresh - `inst-fnd-outbox-dispatch`

## 4. States (CDSL)

The following are storage states; lifecycle and unit business states belong to slice 03.

1. [ ] - `p1` - Replay: absent → claimed → answered inside a successful mutation transaction; rollback returns to the pre-transaction state. An answered row is replayable until expiry; conditional expiry handling must admit only one replacement claimant.
2. [ ] - `p1` - Audit: insert unsealed → optional sealed, preserving every record field; deletion and all other transitions are refused.
3. [ ] - `p1` - Unit Store: an existing version v → v + 1 only if the conditional write matches v; zero affected rows yields UNIT_CONTENDED and rolls back the attempted unit mutation.
4. [ ] - `p1` - Pending ownership: null → unit id only with the observed SKU version; zero affected rows yields ROW_LOCKED_PENDING and rolls back submission. Terminal clear additionally checks the owning unit and, for fenced operations, fence_op_id.

## 5. API Surface

None. This slice wires storage, settings, DomainError-to-Problem mapping and transaction helpers for
later authenticated OperationBuilder doors. Repositories accept scoped executors; the approval Store
implements conditional unit updates and pending ownership without raw connections or database row locks.
Typed SDK clients remain registered through ClientHub, following DESIGN §1.3 and §3.4.

The fresh migration allocation is:

| Migration | Contents and ordering |
| --- | --- |
| `000001` | Category with tenant/code uniqueness. |
| `000002` | SKU heads and immutable versions, identity/as-of indexes and category foreign key by id. |
| `000003` | Policy, unit, item and decision tables. |
| `000004` | Audit table and append-only guards. |
| `000005` | Replay table and response-state guards. |
| `000006` | Reference registry and live-reference indexes. |
| `000007` | A SKU's category becomes optional (P-D-196): Postgres drops the NOT NULL; SQLite rebuilds `products_sku` with its two child tables inside the runner's transaction, without PRAGMA. `down()` is irreversible. |
| `000008` | The audit row carries the SKU lifecycle move its act made (P-D-213): `ADD COLUMN` `from_lifecycle`, `to_lifecycle` (nullable, CHECKed to the five lifecycles), and the append-only guard redefined so the seal keeps both unchanged too. `down()` is irreversible. |
| `000009` | The unit carries its submitter's note (P-D-219): `ADD COLUMN submit_note text` (nullable) on `products_approval_unit`, by the approval library's separate step `ddl::apply_add_submit_note`; the library's `ddl::up()`, the body of `000003`, stays as it shipped. It replays; `down()` drops the column. |
| `000010` | No stored category is a retired default (P-D-220): `UPDATE products_category SET is_default = false, version = version + 1, updated_at = <now>` where the row is the default and retired, as a category write clears a default, with no audit row. Data only: the schema does not change. It replays; `down()` changes nothing. |

Before `000001` runs the guard `m0000_products_refuse_a_legacy_or_stale_schema` ([P-D-195](../DECISIONS.md)).
Its name sorts it before every other migration of the gear, and it creates nothing. It refuses a database
that holds a legacy `products_*` table, which is the legacy chain's tables minus today's, a legacy
`products_category` or `products_sku` without `code` (names both chains create), or a
`products_sku_reference` whose `ref_kind` CHECK does not admit `price_book_entry`. The deployment is not migrated:
it starts from empty products tables.

Tenant isolation uses SecureORM scopes and scoped parent-category reads in the write transaction.
Approval children are accessed through a scoped unit; composite tenant foreign keys are not required.

## 6. Data Model

[DESIGN §3.7](../DESIGN.md#37-database-schemas--tables) is the DDL definition site. This slice maps it
to migrations and repository behavior rather than copying the SQL. Postgres uses `bss.products_*`;
SQLite drops the schema prefix, maps UUID/date/timestamp/JSONB to text and BYTEA to blob, and retains
all checks and tenant keys. SKU details are in slice 02 and approval mutations in slice 03.

| Storage family | Repository obligation |
| --- | --- |
| `products_category`, `products_sku` | Tenant-qualified uniqueness and category links; use revision as SKU concurrency version and published_version as its snapshot counter. |
| `products_sku_version` | Key `(sku_id, published_version)` with tenant-scoped access; immutable inserts; effective dates need not be unique. |
| Four `products_approval_*` tables | Unit version CAS; item author provenance; decision key `(unit_id, actor, generation)`; policy `'*'` default, absent means quorum 1; no unit replay key. |
| `products_audit_log` | Append-only record with tenant/time, subject and actor indexes; only reserved sealing metadata may change as defined in DESIGN. |
| `products_idempotency` | Primary key `(tenant_id, endpoint, client_key)` and tenant/expiry index; response-group check ties nullable response columns to claimed/answered state. |

Audit columns are carried from backup migration `m20260829_000004_create_products_audit_log.rs`,
with the table's name `products_audit_log` kept, as in DESIGN. Required columns are `audit_id uuid`,
`tenant_id uuid`, `actor_ref uuid`, `action text`, `subject_kind text`, `written_at timestamptz` and
`seal_state text`. Nullable columns are `subject_id uuid`, `subject_revision bigint`, `error_code text`,
`attempted_key text`, `reason text`, `correlation_id text`, `session_id uuid`, `ceremony_ref uuid`,
`chain_id uuid`, `seq bigint`, `prev_hash bytea` and `row_hash bytea`. Keep the subject-reference,
seal-group and nonnegative-sequence checks. Reserved columns do not reinstate removed workflows.
Migration `000008` adds the nullable `from_lifecycle text` and `to_lifecycle text`, each CHECKed to the five
lifecycles: the SKU lifecycle an act found and left (P-D-213). The seal keeps them unchanged, as it keeps every
record column.

Replay columns are carried from backup migration `m20260829_000006_create_products_idempotency.rs`:
required `tenant_id uuid`, `endpoint text`, `client_key text`, `state text`, `payload_hash bytea`,
`expires_at timestamptz`; nullable `response_status integer`, `response_body jsonb`, `entity_ref uuid`.
An answered row has both response fields; a claimed row has neither. No `in_flight_until` is added.
Audit and version history are append-only; the replay store is expiring operational state.

## 7. Events & Alarms

This slice supplies persistence, not an additional event vocabulary. Slice 03 chooses terminal
approval/domain events and slice 04 defines their payloads. Submission audit is mandatory; submission
has no domain event unless quorum zero also applies. No success event survives rollback. Storage
failures and exhausted transaction retries use existing toolkit diagnostics; no new alarm contract is
introduced by the spec. Preserve request correlation without logging full snapshots or replay bodies.

## 8. Definitions of Done

These are references to Task 9's definition sites; this slice defines no DoD IDs.

- see [features/foundation.md](../features/foundation.md) — `cpt-cf-bss-products-dod-tables-two-backends`
- see [features/foundation.md](../features/foundation.md) — `cpt-cf-bss-products-dod-sku-version-table`
- see [features/foundation.md](../features/foundation.md) — `cpt-cf-bss-products-dod-audit-append-only`
- see [features/foundation.md](../features/foundation.md) — `cpt-cf-bss-products-dod-idempotency-key-store`
- see [features/foundation.md](../features/foundation.md) — `cpt-cf-bss-products-dod-if-match-version`
- see [features/foundation.md](../features/foundation.md) — `cpt-cf-bss-products-dod-outbox-same-tx`
- see [features/foundation.md](../features/foundation.md) — `cpt-cf-bss-products-dod-unit-store`

## 9. Acceptance Criteria

[PRD §9](../PRD.md#9-acceptance-criteria) supplies the numbered ACs. Verify these against both backends
in phase 1; they are implementation obligations, not checks executed by this documentation task.

| Trace | Given / When / Then |
| --- | --- |
| `cpt-cf-bss-products-fr-concurrency-idempotency`; AC #27 | Given a stale SKU/category ETag, when PATCH uses it, then STALE_REVISION leaves state unchanged. Given a retained keyed POST, when it repeats, then its saved outcome returns before any fence/unit work and no duplicate object appears. |
| Same FR; AC #18–19 | Given concurrent unit writes or pending-lock acquisitions, when one wins, then the other gets UNIT_CONTENDED or ROW_LOCKED_PENDING and cannot leave partial decisions/items behind. |
| `cpt-cf-bss-products-fr-sku-versions`; AC #8–9 | Given equal-date snapshots, when stored and read, then both remain and the greater published_version wins; UPDATE/DELETE of history is refused. |
| `cpt-cf-bss-products-fr-events`, `cpt-cf-bss-products-nfr-audit`; AC #20–21 | Given each terminal path or an injected outbox failure, when the transaction completes, then state/audit/event commit together or all roll back; direct audit deletion or record mutation fails. |
| `cpt-cf-bss-products-nfr-tenant-isolation`; AC #22, #27 | Given two tenants sharing codes and client keys, when repositories and replay are used, then neither conflicts with or reveals the other tenant's records. |
| `cpt-cf-bss-products-nfr-two-backends`; AC #29 | Given fresh SQLite and Postgres databases, when the chain and concurrency scenarios run, then both enforce the same keys, append-only guards and transaction outcomes. |

## 10. Non-Functional Considerations

Deny-by-default scope creation precedes storage and replay (`cpt-cf-bss-products-nfr-authz`, AC #28).
Composite tenant links and scoped parent access protect approval children as well as top-level rows.
Audit durability and backend parity follow `cpt-cf-bss-products-nfr-audit` and
`cpt-cf-bss-products-nfr-two-backends`; transaction retries must be bounded and repeat all predicates.
Foundation provides no raw connection escape hatch and no cross-gear call inside a fence transaction.
