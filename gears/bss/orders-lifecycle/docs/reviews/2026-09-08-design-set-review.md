<!-- CONFLUENCE_TITLE: [BSS]: Orders Lifecycle — Design Set Review Wave (2026-09-08) -->
<!-- Related: ../DESIGN.md, ../design/, ../PRD.md | Owners: BSS Orders team -->

# Design Set Review — Orders Lifecycle (wave 2026-09-08)

<!-- toc -->

- [Method and scope](#method-and-scope)
- [Verdict](#verdict)
- [A · The engine algorithm (CRITICAL — all five passes)](#a--the-engine-algorithm-critical--all-five-passes)
- [B · The transition table (CRITICAL — all five passes)](#b--the-transition-table-critical--all-five-passes)
- [C · The event contract (CRITICAL — all five passes)](#c--the-event-contract-critical--all-five-passes)
- [D · The canonical schema (CRITICAL / HIGH)](#d--the-canonical-schema-critical--high)
- [E · Authorization (CRITICAL / HIGH)](#e--authorization-critical--high)
- [F · Ownership, inventory and identity (HIGH / MEDIUM)](#f--ownership-inventory-and-identity-high--medium)
- [G · Non-functional depth (HIGH / MEDIUM)](#g--non-functional-depth-high--medium)
- [H · PRD fidelity (HIGH / MEDIUM)](#h--prd-fidelity-high--medium)
- [I · Counts and internal arithmetic (LOW)](#i--counts-and-internal-arithmetic-low)
- [Resolution register](#resolution-register)
- [Consistency wave — 2026-09-09](#consistency-wave--2026-09-09)
- [Design and ADR wave — 2026-09-09](#design-and-adr-wave--2026-09-09)
  - [The blocking cluster](#the-blocking-cluster)
  - [Resolution register](#resolution-register-1)
  - [What changed in the process, not the documents](#what-changed-in-the-process-not-the-documents)
- [Recommended sequence](#recommended-sequence)
- [Traceability](#traceability)

<!-- /toc -->

## Method and scope

Five parallel review passes over all ten artifacts of the Orders Lifecycle design set
([`../DESIGN.md`](../DESIGN.md), [`../design/README.md`](../design/README.md) and slices
`01`–`08`), scored against [`docs/checklists/DESIGN.md`](../../../../../docs/checklists/DESIGN.md)
v2.0 and against [`../PRD.md`](../PRD.md) as the statement of intent. Passes were scoped by
expertise domain — ARCH + SEM, SEC + REL, DATA + INT + PERF + OPS — plus one cross-document
consistency audit and one PRD coverage audit. Roughly 130 raw findings deduplicate to the 74
distinct defects below.

**The deterministic gate passed all ten documents before this review and is blind to every
finding in it.** `cfs validate` reports zero errors, every TOC is valid, no language violation
exists, all 158 relative links resolve and only one duplicate ID definition appears in 193. The
gate verifies form; nothing in it verifies that the specified machine can execute.

Severity follows the checklist dictionary: **CRITICAL** unsafe, misleading or unverifiable,
blocks downstream work · **HIGH** major ambiguity or risk, fix before approval · **MEDIUM**
meaningful improvement · **LOW** minor.

## Verdict

The set is strong where it reasons and weak where it specifies. Boundary discipline, the
recorded rejected alternatives, the fail-closed posture and the honesty about absent upstreams
are above the bar for this artifact class and several reviewers said so independently. But four
clusters make the design **not implementable as written**: the engine's transition algorithm has
three interlocking correctness defects, the transition table cannot admit five operations the
slices specify, the event contract is a placeholder asserting a set it never enumerates, and the
canonical schema contains a primary key that no database will accept plus a dozen columns the
slices normatively require and it does not define.

The cause is procedural rather than analytical: 5 406 lines of interlocking normative content
were authored in one sitting and the semantic review loop the authoring workflow mandates was
run only against the gear-level document, never against slices `01`–`08`.

**This verdict describes the set as it stood on 2026-09-08 and is not withdrawn by the
remediation.** Every finding has since been addressed — see [Resolution register](#resolution-register)
— but a fix pass is not a review, so the set's implementability is re-established by re-running
the five passes, not by this document.

## A · The engine algorithm (CRITICAL — all five passes)

The three defects here interact; fixing any one alone leaves the contract incoherent.

| ID | Sev | Anchors | Defect | Fix |
|----|-----|---------|--------|-----|
| R-01 | CRITICAL | `01 §3.6` steps 9–11, `§4.1` ¶2, `§2.1` vs `01 §3.6` *Idempotent replay*, `§4.2` table row 4 | The normative guard order runs the version check **before** idempotency resolution. Every versioning transition bumps `current_version` on commit, so a retry of a committed submit, amendment or acknowledgement carries a superseded version and receives version-conflict — never reaching "return the stored outcome". The replay sequence diagram documents behaviour the algorithm cannot produce, and `§4.2`'s own table conditions version-conflict on "key **not present**", implying the opposite order. | Resolve idempotency first and return a fingerprint-matching settled outcome before evaluating admissibility or version; keep the version check only for keys with no settled record. Delete the justifying sentence in `§4.1`. |
| R-02 | CRITICAL | `01 §1.2` (`nfr-order-idempotency`) vs `§3.6` steps 3, 13–14, 21–22 | The in-flight marker is inserted **and** settled inside one transaction, so it is never visible to a concurrent caller. `§1.2`'s claim that "the in-flight marker is a committed row, so a concurrent duplicate observes it" is false. The duplicate blocks on the unique index, then receives a violation that has already aborted its transaction — making step 14's plain `RETURN` impossible; and by then the first attempt has settled **successfully**, so the correct answer is the stored success, not still-processing. `still-processing` is unreachable, and a crashed request leaves no in-flight row at all. | Make step 13 an upsert-and-reread (`ON CONFLICT DO NOTHING`, re-resolve, branch to the settled/in-flight cases), or commit the marker in a prior transaction with a lease so a crash is recoverable. Delete the "committed row" claim. |
| R-03 | CRITICAL | `01 §4.1`, `§4.4`, `§3.7` *Additional info* vs `§3.6` steps 6, 8, 9, 12, 14 | `§4.1` requires every refused transition to settle its idempotency record and append an audit entry. Four of five refusal classes do neither, and the authorization denial at step 6 appends without committing. The "100 % of transitions audited, zero silent drops" NFR and `§3.7`'s promise that "a denied authorization or a failed guard is visible to an auditor" are both unmet by the algorithm that is supposed to deliver them. | Add the audit-append, settle and commit sequence to every refusal path, or narrow `§4.1` to the classes it actually covers and say which refusals are unauditable. |
| R-04 | HIGH | `03 §3.6` steps 1, 8.2, 10; `04 §3.6` steps 1–4, 8; `05 §3.6` steps 1, 3, 4; `06 §3.6` steps 1–2; `07 §3.6` step 1 vs `01 §4.1` | Every slice returns refusals **before** calling the engine, so no audit row and no idempotency record exists for them either — the audit trail `08 §3.6` promises is systematically incomplete and a refused submit is not replayable. `03 §1.2` asserts the opposite of its own algorithm ("the transition cannot commit with a predicate unevaluated"). | Make these checks registered guards evaluated by the engine, as `01 §2.1` already requires. |
| R-05 | HIGH | `06 §3.6` steps 4, 2.1.3, 2.2.3, 3; `05 §3.6` step 5; `07 §3.6` step 2; `03 §3.6` step 8.1 vs `01 §2.1`, `§2.2`, `§4.1` | Five slices write a durable row (verdict, linkage, projection, acceptance instant, pre-hold state, gate outcome) and *then* request the transition. A refusal leaves an orphaned write, defeating `01 §1.1`'s "no partial commit to reconcile". `01 §3.7`'s own constraint that `orders_line_fulfillment` is "written only by the workflow-seam acknowledgement transition" is contradicted by `06 §3.6` step 3. | Pass all of these as document contributions to the engine call, per the contribution parameter `01 §3.3` already defines. |
| R-06 | MEDIUM | `01 §3.6` step 17 and `07 §3.6` step 5.2 | Resume's step ordering is wrong: step 17's guard and 17.1 run on a target that is not knowable until 17.3, and 17.2 stores an outgoing state that 17.1 has already overwritten. `07 §3.6` step 5.2 reads the pre-hold state as the target *before* step 5.3 checks one is stored. | Resolve the effective target first, capture the outgoing state before assignment, then branch. |

## B · The transition table (CRITICAL — all five passes)

| ID | Sev | Anchors | Defect | Fix |
|----|-----|---------|--------|-----|
| R-07 | CRITICAL | `01 §4.3` rows 1–19, `§3.6` steps 7–8 vs `02 §3.6` steps 6, 10, `§4.1`; `04 §3.6` step 4, `§4.1` | Five specified operations have no row and are therefore not-admissible refusals: **order creation** (the PRD declares `[*] → draft`), **draft line authoring and mutation**, the **administrative edit**, the **spawn-signal** report and the **draft archival** sweep. The whole of Phase 1 cannot execute, and `02 §1.2` asserts a verification test that no row permits. | Add rows for creation (`∅ → draft`), draft-scoped mutation (`draft → draft`, state-only), the administrative edit (any non-terminal → same state, state-only), the spawn signal (`in_fulfillment → in_fulfillment`, state-only) and archival; correct the "nineteen-row" count everywhere it appears. |
| R-08 | CRITICAL | `01 §4.3` rows 12 and 13 vs `§3.2` | Both rows match the lookup key `(approved, amendment)` — row 12's FROM set includes `approved` — while `§3.2` defines admissibility as exactly that lookup. The machine is non-deterministic where amendment meets approval. Row 12's parenthetical "(versioning, state-only)" also assigns both values of the single versioning-behaviour field. | Restrict row 12 to `submitted`/`pending_approval`; keep row 13 for `approved`; give each row one versioning behaviour. |
| R-09 | CRITICAL | `01 §4.3` row 13, `04 §4.3` vs `../PRD.md` §6.1 state diagram | The PRD declares two guarded edges — `approved → submitted [approval not required]` and `approved → pending_approval [approval required]`. Row 13 collapses them into an unguarded "TO `submitted` or `pending_approval`", and `04 §4.3` then commits to `submitted` unconditionally. Row 13's `pending_approval` target is never exercised. This is a scope change from the PRD presented as an implementation detail, and `04 §1.1`/`§2.2`'s "two possible targets" language contradicts `§4.3`'s own second paragraph. | Either restore the two guarded edges with the verdict as guard, or record the collapse as an approved scope change with its rationale and route it to the PRD owner. |
| R-10 | CRITICAL | `06 §4.3`, `§3.7` vs `01 §4.3` normative exclusions | `spawn_signal_at` is "cleared **only** by a workflow-mediated cancel… so a subsequent attempt re-closes the window" — but that cancel lands in `cancelled`, which is terminal with no row out. There is no subsequent attempt; the rule is unreachable and its explanation describes an impossible sequence. | Delete the clearing rule and state the signal is permanent, or name the transition that re-opens fulfillment. |
| R-11 | HIGH | `07 §4.4`, `§3.2`, `§3.7`; `02 §4.5`; `DESIGN.md §1.2` vs `01 §4.3` state set | The abandoned-draft sweep transitions to `archived` — a state that is in neither the state set nor the terminal set, with no row. `orders_state_ttl_policy.state` even admits `draft` for a transition that cannot exist. The outcome is named three ways across the set: "auto-void", "archived", "expired". | Name the target (`draft → expired`, actor class `system`, reusing `OrderExpired` is cheapest), add the row, and use one term in all three places. |

## C · The event contract (CRITICAL — all five passes)

| ID | Sev | Anchors | Defect | Fix |
|----|-----|---------|--------|-----|
| R-12 | CRITICAL | `DESIGN.md §1.2`, `§1.3`, `§3.3`; `01 §3.7`, `§4.4` | "The eleven state events" is asserted four times and enumerated nowhere. Only seven names appear anywhere in the set; `OrderApproved`, `OrderRejected`, `OrderCancelled` and `OrderFulfillmentFailed` appear in **zero** documents. | Add an event catalogue to `01 §4.4` listing all eleven with the row(s) that emit each. |
| R-13 | CRITICAL | `01 §3.6` step 20 vs `§3.2` state-table tuple | Step 20 enqueues "the row's **declared event type**", but the tuple is `(from, to, trigger, guard set, actor class, versioning behaviour)`. Event type is not a column, so the algorithm reads an attribute the table does not declare. | Add a nullable `event_type` column to the transition table and say what a null means. |
| R-14 | CRITICAL | `01 §4.4` vs `§4.3` rows 3, 7 and the five missing rows of R-07 | "Exactly one outbox row per committed transition" with `event_type` constrained to the eleven is unsatisfiable: `submitted → pending_approval`, `approved → in_fulfillment`, creation, line authoring and the administrative edit have no member in the eleven. Collapsing the rows by outcome needs at least thirteen distinct types. | Soften `§4.4` to "exactly one event where the row declares one", and either extend the event set or mark those rows event-less explicitly. |
| R-15 | CRITICAL | `05 §3.6`, `§4.2` vs `01 §4.1`, `§4.4`, `§1.1`; `orders_event_outbox` `(order_id, sequence)` UNIQUE | Self-service submit is mandated to publish `OrderAcceptanceRecorded` "alongside `OrderSubmitted`" **within the submit commit**. One commit cannot be both row 1 and row 19, and cannot enqueue two outbox rows without breaking the central invariant and the unique constraint. | Publish one `OrderSubmitted` carrying an acceptance-recorded fact, or chain acceptance as a second transition — and state which, since `§4.2` currently mandates the impossible option. |
| R-16 | HIGH | `04 §3.6` step 5, `§4` vs `01 §4.4` | The administrative edit is a committed transition that explicitly publishes nothing, contradicting one-row-per-transition. | Grant it an explicit event-less exemption once `§4.4` is softened, or give it its own event type. |
| R-17 | HIGH | `DESIGN.md §3.3` vs `01` (whole) and `../PRD.md` §6.5, §9.2 | The envelope and payload schema are declared to be "owned by `design/01-foundation`", which never mentions them; the entire payload specification is one column comment. Two PRD-mandated payload members are absent everywhere: the external reference on every event where present, and the per-line net components on `OrderCompleted`. Non-trivial, because the totals are version-scoped and the external reference is mutable in place — *which* version's totals and *which* value at commit time is unanswered. | Specify envelope attributes and per-event payloads in `01 §4.4`, resolving the version/mutability question. |
| R-18 | HIGH | `01 §4.4` | No payload versioning or evolution policy: no schema version on the envelope, no compatibility rule — while `06 §2.2` itself observes that reason values riding payloads make an addition breaking once Billing consumes them. | Add an envelope schema version and the additive-only-within-a-major policy. |
| R-19 | MEDIUM | `01 §3.6` *Drain Outbox* step 3.2.2, `§4.4`; `DESIGN.md §4` | A parked dead-letter row is alerted and "inspectable" with **no re-drive or replay operation**, and the sibling gear's reconciliation sweep is read-only past the idempotency window — so a permanently parked `OrderCompleted` leaves the two gears divergent with no recovery path. | Specify the re-drive operation and its idempotency, or name the reconciliation path that closes the divergence. |

## D · The canonical schema (CRITICAL / HIGH)

| ID | Sev | Anchors | Defect | Fix |
|----|-----|---------|--------|-----|
| R-20 | CRITICAL | `01 §3.7` `orders_resolved_total` | PK `(order_id, version, line_id, charge_kind)` with `line_id` declared **nullable** for the order-level roll-up. No engine accepts a nullable PK column, so the roll-up row `03 §4.4` requires is unstorable. | Add a `scope enum('line','order')` discriminator to the key, or split the roll-up into its own table. |
| R-21 | CRITICAL | `02 §4.1` vs `01 §3.7` `orders_order_line`, `orders_order_version` | Draft authoring is mandated to add, amend and remove lines "freely", against tables declared append-only with no UPDATE or DELETE. The Phase-1 path is defined to violate its own constraints. | Either exempt `draft`-scoped rows explicitly, or hold draft content in a mutable working table that submit materialises into version 1. |
| R-22 | CRITICAL | `04 §3.6` step 3, `§4.5`, `§2.1` vs `01 §2.1`, `§3.7` | The administrative edit applies its value "in place on the current version" — an append-only row that the same slice forbids rewriting "not by a repair path". The design's central commercial/administrative split has no legal place to write. | Give administrative content its own mutable columns or table, keyed at order and line level. |
| R-23 | HIGH | `01 §3.7` (all tables) | **No foreign key is declared anywhere.** Eight child tables are orphan-capable while `DESIGN.md §1.2` claims a recovered database "cannot hold a state change without its trail". | Declare the FK graph, including the deferred treatment the `orders_order.current_version` ↔ `orders_order_version` cycle needs. |
| R-24 | HIGH | `02 §3.7` vs `01 §3.7`; `orders_line_fulfillment`, `orders_resolved_total` | `line_id` is claimed "stable across versions and unique **within an order**"; PK `(order_id, version, line_id)` enforces uniqueness within a *version*. Two other tables key on the order-scoped uniqueness no constraint provides. | Add an order-scoped line-identity key (parent table or UNIQUE `(order_id, line_id)`) that the projection and totals reference. |
| R-25 | HIGH | `01 §3.7` vs `02 §3.7`, `04 §3.7`, `05 §3.7`, `08 §3.7`, `03 §4.2`, `06 §4.4`, `07 §3.1` | The "canonical" schema is missing at least ten columns slices normatively require: the tolerated-authorization **risk flag**; `state_entered_at`; the per-line **date policy-switch state**; audit `changed_field`/`prior_value`/`new_value`; **order-level** administrative fields (external reference, display labels, internal notes); line-level `currency`; `overlapScopeKey` storage; hold actor/instant/reason; and **compensation evidence**, which has no column or table at all. | Fold them into `01 §3.7`, or demote it from "canonical" and name one authoritative schema location. |
| R-26 | HIGH | `01 §3.7` preamble, `§2.1` vs `§3.7` `orders_event_outbox`, `orders_idempotency`, `orders_line_fulfillment`; `02 §4.3` | Three of the nine "append-only" tables are updated in place by the design's own algorithms — outbox delivery bookkeeping, idempotency `in_flight → settled`, projection `created → activated` — plus `order_line.external_reference`, classified as editable administrative content. | Narrow the invariant per table and state the permitted mutation for each. |
| R-27 | HIGH | `01 §3.7` vs `08 §3.6` steps 2–3, `§3.7`, `§3.3`, `§1.2` | Declared indexes do not match declared query paths. The composite on `payer_tenant_id` serves no path any slice describes; the **partner path** (an `IN` over `resource_tenant_id`) and the contract filter are unindexed; the outbox drain has no supporting index and delivered rows are never removed, so its scan grows with every event ever emitted; `expires_at` is unindexed; the expiry sweep gets only a low-cardinality `state` index. | Replace the payer index with `(resource_tenant_id, state)`, add `(contract_id)`, add a partial index for undelivered outbox rows plus a purge policy, index `expires_at`, and make the sweep index composite on `(state, state_entered_at)`. |
| R-28 | HIGH | `01 §3.7` `orders_transition_audit`, `orders_event_outbox` | `sequence bigint` "monotonic per order" under a UNIQUE constraint with the allocation mechanism specified nowhere. `MAX+1` serialises every write on an order and, with no row lock in `§3.6`, surfaces concurrent transitions as unique-violation infrastructure faults instead of the version-conflict refusal `§4.2` promises. | Put a counter column on `orders_order` incremented under the aggregate row lock the transition already needs, and state that step 4 takes that lock. |
| R-29 | HIGH | `01 §3.7` several tables | Several statements labelled "constraints" are not expressible as constraints: `catalog_price_pin NOT NULL for any version at submitted or beyond` (state lives on another table), `current_version` referencing an existing version (circular), `supersedes_version` "immediately prior" (cross-row), "exactly one outbox row per transition" (cross-table cardinality), and `orders_line_fulfillment` "written only by the acknowledgement transition" (no constraint expresses a writer). | Relabel each as an engine-enforced invariant with a named verification test, and reserve "constraint" for genuine DDL. |
| R-30 | HIGH | `03 §4.2` predicate 7 vs `01 §3.6` step 1 | The one-in-flight-order-per-overlap-key rule is resolved outside the transaction, the key is stored nowhere, and no unique constraint backs it — so two concurrent identical submits both pass and both commit. | Persist the overlap key and enforce with a partial unique index over in-flight states inside the transaction; keep the predicate as the friendly pre-check. |
| R-31 | MEDIUM | `03 §3.1` vs `03 §3.7`, `01 §3.1`, `§3.7`; `04 §4.2`; `03 §3.6` *Re-check* step 2 | The order market is specified per **version** but stored as one column on the **root**, so an amendment overwrites the market the prior version was gated against and the activation re-check has no "market frozen at submit" left to compare. | Move `order_market` into the version row, or change the relationship and drop "frozen at submit". |
| R-32 | MEDIUM | `07 §3.7` `orders_state_ttl_policy` | UNIQUE `(scope, seller_tenant_id, state)` does not prevent duplicate platform-scope policies, because SQL treats NULLs as distinct. | Use `NULLS NOT DISTINCT`, a sentinel, or two partial unique indexes split on `scope`. |
| R-33 | MEDIUM | `06 §3.7` `orders_approval_reflection` | UNIQUE `(order_id, version, verdict)` is glossed as "a verdict is reflected once per version" but permits both `granted` and `denied` for the same version. | Make it UNIQUE `(order_id, version)` if that is the intent, or restate the guarantee. |
| R-34 | MEDIUM | `01 §3.7` `orders_line_fulfillment` | PK `(order_id, line_id)` carries no `version`, so the projection cannot be joined to a specific version's lines. | Add `version`, or reference the order-scoped line-identity key of R-24. |
| R-35 | MEDIUM | `01 §3.7` `order_market`, `catalog_price_pin` | Both are opaque `jsonb` with no declared internal shape and no index, yet a gate predicate must filter on the market and the pin is the object the resolvability invariant and the re-pin test are asserted over. | Promote the market to typed columns and specify the pin's fields. |
| R-36 | MEDIUM | `DESIGN.md §3.7`, `§3.8`; `01 §3.7` | No migration or schema-versioning strategy; the only statement is a privilege boundary. Archival is asserted repeatedly with no archive table, store or move mechanism. | Add migration ordering across the phased slice map, backfill posture for append-only tables, and the archive target. |

## E · Authorization (CRITICAL / HIGH)

| ID | Sev | Anchors | Defect | Fix |
|----|-----|---------|--------|-----|
| R-37 | CRITICAL | `08 §4.3` vs the seven slice `§3.3` tables | The permission matrix declares four actor classes over eleven operations while the slices declare twenty-four. Roughly thirteen operations have **no** declaration — including `POST /acceptance`, `POST /preview`, the three line operations, `PATCH /orders`, and the audit read. On `§4.3`'s own rule ("startup MUST fail if an operation exists with no declaration") the gear cannot start. | Extend the table to every operation, or state it is illustrative and name where the exhaustive declaration lives. |
| R-38 | CRITICAL | `08 §4.3` vs `05 §2.1`, `§4.2` | Nothing prevents the partner who **placed** a partner-placed order from recording the customer's acceptance instant. That is precisely the conflation `05 §2.1` exists to prevent — "a design that conflated them would offer a partner's own authority as proof of their customer's consent". | Declare the acceptance operation's actors and state normatively that the placing party may not record acceptance for it. |
| R-39 | CRITICAL | `08 §4.4`, `§2.2`; `01 §3.6` step 6; `../PRD.md` §6.6 | Delegation proof — called the confidentiality failure the design most needs to prevent — has **no specified form, issuer, trust anchor, validation rule, lifetime or revocation**; the algorithm tests for a "valid" proof with validity undefined; the PRD's normative pointer (manifest §2.1.3) appears in no design artifact; and the audit table has no column to hold it despite `§4.4` requiring it be recorded. | Specify the proof as a named credential with issuer, verification and expiry, cite the manifest section, and add the proof-reference column. |
| R-40 | CRITICAL | `06 §4.1`; `DESIGN.md §4` | The privileged workflow operations are gated only by "restrict them to the Workflow system actor", with **no service-to-service authentication** named anywhere — no mTLS, signed service token, gateway-asserted principal or scope claim. Nothing distinguishes the sibling gear from any caller presenting that actor class. | State the service-identity mechanism and the exact `SecurityContext` claims the pre-guard checks per actor class. |
| R-41 | HIGH | `08 §2.1`, `§4.3` vs `§3.6` and `01 §3.3` | `§4.3` claims permissions are enforced by the engine pre-guard "for every operation in the gear", but the engine's only entry point is *attempt a transition* — so read paths necessarily implement their own relationship resolution and delegation check, producing exactly the two-model drift `§2.1` claims to prevent. | Extract one authorization evaluator invoked by both the pre-guard and the read paths, and make that normative. |
| R-42 | HIGH | `08 §4.4` vs `01 §2.2`, `§4.1` | `§4.4` says the proof is recorded "so a review can establish under whose authority an order was **read** or changed", but reads register no transition and only the engine may write the audit store. No record of a read, or of a refused cross-tenant read, can exist. | Define a separate access-log surface for reads, or narrow `§4.4` to changes and say where read access is logged. |

## F · Ownership, inventory and identity (HIGH / MEDIUM)

| ID | Sev | Anchors | Defect | Fix |
|----|-----|---------|--------|-----|
| R-43 | HIGH | `DESIGN.md §3.3`; `01 §4.3` rows 2, 11, 16; `08 §4.3` vs all eight slice `§3.3` tables | **No slice owns the ordinary cancel operation.** `POST /orders/{orderId}/cancel` appears only in the gear-level inventory: no algorithm, no guard set, no registered reasons, no owner in the slice map — although `fr-order-cancel` is `p1`, three actors are declared able to cancel, and three transition rows depend on it. The seller operator's "cancel with an audited reason" has no design behind it. | Assign cancel to a slice (`07` already owns the `on_hold` cancel guard) with its algorithm, guards and reasons. |
| R-44 | HIGH | `DESIGN.md §3.7` vs `01 §3.7`, `§4.1`; `../design/README.md` | Table ownership is assigned twice, incompatibly: the gear index gives each table a slice owner and says the slice mints its `dbtable-*` ID, while all nine IDs are minted in `01` and `§4.1` names the engine sole writer. The same inventory omits the three tables slices actually introduce, so it lists nine where twelve exist. | Replace the Owner column with "engine (schema) / slice (content)", add the three tables, and fix the pointer to `01 §4` — the schema is `§3.7`. |
| R-45 | HIGH | `DESIGN.md §3.3` vs the slice `§3.3` tables | The gear endpoint inventory omits eight endpoints the slices declare and includes one nobody owns, while claiming to bind the PRD's operations "to one REST surface". | Make it the union of the slice surfaces. |
| R-46 | HIGH | `06 §1.1`, `§4.1`; `DESIGN.md §3.2`, `§3.3`; `01 §4.5`; `08 §4.3`; `README` vs `06 §3.3`, `§3.4` | "Four workflow-only operations" in six places against five endpoints in two. Because `§4.1`'s normative sentence enumerates four, the spawn-signal operation escapes the "MUST be a transition-table row subject to the full guard order" rule — the same hole as R-07 and R-10. | Say five in all six places and bring spawn-signal inside `§4.1`. |
| R-47 | MEDIUM | `01 §3.3`, `§4.2`; `04 §3.3`, `§4.4`; `06 §3.3`; `02 §3.3`; `07 §3.3`, `§3.6`, `§4.3` | Three registered reason names for one engine condition (`version-conflict`, `stale-version`, `version-stale`, `verdict-version-stale`), two for one classifier check (`commercial-field-immutable` vs `…-outside-amendment`), and one redundant (`expiry-not-permitted-for-state`, where the engine's `not-admissible` is required). Callers key on these strings, so this is a contract defect. | Keep the engine's name and delete the duplicates. |
| R-48 | MEDIUM | `DESIGN.md §3.1`, `§3.2`, `§3.6`; `01 §3.1` | Seven core entities share a **single** ID; four of them (`OrderLine`, `ResolvedTotal`, `AcceptanceRecord`, `LineFulfillment`) have none. `03 §3.1` cites `…-entity-order-line`, defined nowhere. The Engine's related-components list omits two of seven slices. `…-seq-amendment-supersession` is defined twice with different headings. `OrderVersion → ResolvedTotal` is called one-to-one where it is one-to-many. | Mint per-entity IDs, complete the component list, de-duplicate the sequence ID, correct the cardinality. |
| R-49 | MEDIUM | `../design/README.md` slice map vs `05 §3.5`, `06 §5`, `07 §5`, `08 §5` | The dependency table — presented as the build-order authority — under-declares four slices. Correct edges: `05 → 01, 02, 03`; `06 → 01, 03, 05`; `07 → 01, 02, 06`; `08 → 01, 02, 03, 04, 06`. No listed dependency is unused. | Correct the table. |
| R-50 | MEDIUM | `04 §3.3` vs `08 §3.3`; `README` | The historical-version endpoint is declared by two slices with the same method and path. | One owner registers it; the other names it as delegated. |
| R-51 | MEDIUM | `03 §4.6`, `§3.6` vs `§3.1`, `§3.7`; `DESIGN.md §3.2` | Preview is specified as writing nothing **and** as writing a gate-outcome row per predicate per line with its own retention policy. An unauthenticated basket-shaped surface writing N rows per call is also an amplification vector. | Decide whether Preview persists; if it does, correct the three "creates no state" claims and bound the growth. |

## G · Non-functional depth (HIGH / MEDIUM)

| ID | Sev | Anchors | Defect | Fix |
|----|-----|---------|--------|-----|
| R-52 | HIGH | `DESIGN.md §4`; `../PRD.md` §8 | **No capacity model anywhere**: no order volume, no peak transitions per second, no event rate, no row-growth projection. The PRD says the thresholds "apply at production load (**sizing in Design**)" and gives the NFR workshop only the latency and retention baselines. No slice addresses capacity or marks it inapplicable. In-repo precedent: pricing ratified concrete throughput, size caps and a 24 h idempotency TTL. | Add peak transitions/s, drain throughput, row growth per order and version, and the archival trigger, plus per-slice N/A statements. |
| R-53 | HIGH | `DESIGN.md §1.2`, `§4`; `01 §1.2` | The **event-delivery budget** is the target against which drain lag is verified and alerted in four places and is never given a value or an owner — so the asynchronous half of a `p1` NFR (the PRD requires "durable write + event publish" under 1 s) has no threshold. | Assign it a number or register it as an open value. |
| R-54 | HIGH | `03 §2.1`, `§3.3`; `08 §3.5` | Only *unavailable* ports are handled; nothing specifies a **slow** one. `timeout`, `deadline`, `circuit breaker`, `bulkhead`, `retry budget` and `rate limit` return no hits anywhere in the set, while five synchronous outbound calls sit on the request path — four gate ports plus per-request scope resolution on the 200 ms read path. `§2.1`'s claim that resolving outside the transaction "keeps the p95 achievable with a slow upstream" is false for caller-visible latency. | State per-port deadlines and a total request budget, a bounded retry policy, breaker thresholds mapped to the existing fail-closed reasons, per-port bulkheads, and rate limits in the operation specs as the API baseline requires. |
| R-55 | HIGH | `01 §3.6` *Drain Outbox* step 1, `§3.8` | The only asynchronous egress is a **single-leaseholder, row-at-a-time, unbatched** publisher, capping gear event throughput at one round trip regardless of replica count — while ordering is required only per `orderId`. | Shard the drain by `order_id` hash across N leases and publish in batches. |
| R-56 | HIGH | `DESIGN.md §1.2`, `§3.8`, `§4`; `01 §3.8` | RPO zero / RTO ≤ 60 min is asserted with one mechanism that constrains nothing about surviving loss of the primary: no backup strategy, no point-in-time recovery, no synchronous standby or failure-domain topology, no runbook — while `§4` forbids cross-boundary replication and refers to "DR replicas" that are never specified. The verification tests a mechanism the design does not define. | Specify the synchronous-commit topology, backup/PITR and retention, and how RTO is met inside the residency constraint. |
| R-57 | HIGH | `DESIGN.md §4` | **No data-protection posture**: encryption at rest, encryption in transit, key management, data classification, masking, anonymisation and secure disposal return zero hits, and the only explicit "not applicable" in the whole set is PCI DSS. Per the checklist's evidence standard these are violations, not exemptions. | Add the posture, or an explicit per-item "platform-owned because…" naming the control. |
| R-58 | HIGH | `DESIGN.md §4` | **No threat model** — one sentence naming one threat. No attack vectors, trust-boundary enumeration, mitigation mapping, security assumptions or supply-chain consideration; the threats the design names elsewhere (cross-tenant readability, a partner supplying consent, the Preview fan-out) are never mapped to mitigations. | Add a threat table covering the three tenancy axes, the partner path, the workflow operations and the outbound ports. |
| R-59 | HIGH | `DESIGN.md §1.2`; `01 §3.7`, `§4.4`; `../PRD.md` §7.1 | The PRD requires a **tamper-evident** record; the audit table carries only `(order_id, sequence)` uniqueness with no hash chain, signature or WORM control, and "append-only" is a stated property with no enforcement while the runtime holds database privilege. "The chain is verifiable" is unverifiable as written. | Add a predecessor-hash column or external notarisation plus explicit revocation of UPDATE/DELETE on the audit role, or withdraw the claim. |
| R-60 | HIGH | `DESIGN.md §4` Observability | Observability is specified **once**, at gear level, entirely around engine-owned signals. **No slice declares any signal**, so the risky work is unmonitored: no per-port gate latency or failure breakdown, no pin-unresolvable rate, no acceptance metrics, no per-seam-operation metrics, no list latency by filter shape. Alerting is explicitly volume-agnostic and contains **no alert on either latency SLO**; no error budget exists; tracing is one sentence; health reporting is "platform-standard". | Add a short observability subsection per slice, latency-SLO alerts with burn-rate policy, the tracing propagation contract across the seam and the ports, and the health-endpoint contract. |
| R-61 | HIGH | whole set; `guidelines/GTS.md` | **No extension-points or stability-zone section.** The engine is deliberately closed — no slice may add an edge, guard registration against a missing row fails startup — so any new capability needing a state, edge or event type is an engine change, and the design never says so. All 24 endpoints are uniformly "unstable" with no promotion criterion, deprecation policy or breaking-change definition, though the PRD delegates the major-version mechanism to Design. GTS is named four times without naming a type or citing the guideline. | Add "Extension points and stability" to `01` naming what a slice may add versus what requires an engine change, and an API-evolution subsection to `DESIGN.md §3.3`. |
| R-62 | MEDIUM | `01 §3.6` step 15.3; `01 §3.7` | Refused attempts write permanent audit rows on a table with no retention path, so a caller can grow the audit store without bound and slow every audit read on that order. | Rate-limit refusal auditing or give refusal rows a bounded retention distinct from committed transitions. |
| R-63 | MEDIUM | `03 §4.6`, `§3.6`, `§3.3`; `DESIGN.md §3.5` | Preview's indicative-tax obligation reaches a "tax owner" that appears in **no dependency table**, while `§3.3` states there are exactly four outbound ports. Preview also takes no security context, appears in no permission declaration and carries no rate limit. | Add the tax owner as a fifth port with its own unavailability reason, and declare Preview's actors and rate limit. |
| R-64 | MEDIUM | `08 §3.8` vs `§2.2`, `§4.1`, `§1.2` | `§3.8` permits replica reads "where the replica's lag is acceptable" while `§2.2` and `§4.1` forbid serving stale answers and `§1.2` claims there is no replication lag. A lagging replica answers successfully with old state and nothing detects it. | Delete the allowance, or bound it (read-your-writes via the ETag version plus a staleness ceiling) and drop the no-lag claim. |
| R-65 | MEDIUM | `DESIGN.md §3.8`, `§4`; `08 §4.1` | No resilience or deploy posture: graceful degradation, canary, blue/green, rollback and feature flags return no hits; health reporting does not distinguish readiness from liveness, so `§4.1`'s "MUST report unhealthy" on store unavailability would, wired to liveness, remove every replica rather than fail requests. Schema and permission changes have no rollback posture. | State the health-check contract, the flag mechanism for the unchosen policy values, and the deploy/rollback posture. |
| R-66 | MEDIUM | `DESIGN.md §3.8`, `§4`; `07 §4.5`; `08 §4.5` | Infrastructure-as-code is neither addressed nor marked inapplicable, which the checklist's applicability rule requires. Eight-plus configuration values are deliberately unset with no configuration-management or promotion path stated. | Add the platform-inherited IaC posture or an explicit N/A, and name where the unchosen values live and how a change is promoted. |
| R-67 | MEDIUM | `DESIGN.md §2.2`; `01 §3.1`, `§3.3` | Residency is discussed only as prose with no constraint ID; vendor/licensing and resource constraints are neither present nor marked inapplicable. No machine-readable schema or contract is linked anywhere — both **Location** fields point at prose sections rather than a crate path, module or OpenAPI document. | Promote residency to a constraint ID, add explicit N/A lines, and give the Location fields repo paths. |

## H · PRD fidelity (HIGH / MEDIUM)

| ID | Sev | Anchors | Defect | Fix |
|----|-----|---------|--------|-----|
| R-68 | HIGH | `01 §2.2` vs `../PRD.md` §6.1; `07 §4.5`; `08 §4.5` | The PRD assigns Design a `MUST`: set a finite idempotency-key window. The design restates the obligation, sets no value, and omits it from both open-value registers — so a PRD `MUST` targeted at this artifact is neither met nor visible as open. Pricing chose 24 h for the same decision. | Set the window or register it with an owner and date. |
| R-69 | HIGH | `../PRD.md` §6.1 line-dates and atomic fulfillment; AC 8g | The PRD requires that a line deferred past its quoted service-activation date produce a subscription whose start is the **actual activation instant**, never backdated. `backdat*` and `activation instant` return zero hits: there is one passing rationale clause and no mechanism, no obligation on the activation intent, no port contract, and no seam ask of the `SUB-O*` kind — although Subscriptions owns the start. | Specify what the activation intent carries, and raise the seam ask. |
| R-70 | MEDIUM | `DESIGN.md §4`; `02 §4.2`; `03 §4.5`; `07 §4.5` vs `../PRD.md` §15 | The design tracks a **different open-question register** than the PRD: it claims twenty unanswered where §15 has fifteen rows and twelve unanswered, and three of the four it names are not §15 rows at all. Three §15 rows are silently ignored — trial-conversion/renewal classification (zero hits, and the category enum forecloses a third value), subscription composition granularity (zero hits, with 1:1 hardened as settled), and the quantity model (near-silent, with the PRD's *disputed* position restated as settled). `02 §4.2` resolves a §15 row without flagging it, unlike `03 §4.5` which does. `07 §4.5` labels a committed PRD default (the 24-hour overdue window) and a non-PRD value as "all PRD open questions owned by Product". | Reconcile row by row against §15, cite the row where a slice resolves one, and split `07 §4.5` into PRD questions and design-owned values. |
| R-71 | MEDIUM | `03 §4.4` vs `DESIGN.md §2.2`, `§1.2` R4 row; `../PRD.md` §6.4 R4, AC 11 | A normative TCV formula with multiplication and cycle annualisation sits against `constraint-no-money-arithmetic` ("no derivation, aggregation or currency conversion") and R4's "MUST NOT compute, derive, or modify any price value", with the result persisted and no statement of who evaluates it. Under the PRD this is a compliance question, not a wording one. | State that TCV arrives computed from the evaluation contract and is stored verbatim, or record a scope change. |
| R-72 | LOW | `DESIGN.md §3.3` | "The PRD specifies fourteen business operations"; §9.1 lists thirteen — the fourteenth row is the table header. | Correct the count. |
| R-73 | LOW | `DESIGN.md §3.6`; `../PRD.md` §3.2 | `…-actor-orders-contracts` is the only one of the PRD's eight actors referenced nowhere in the design set, though Contracts is a declared dependency for two predicates. | Cite it in the gate and preconditions sequences. |

## I · Counts and internal arithmetic (LOW)

| ID | Sev | Anchors | Defect | Fix |
|----|-----|---------|--------|-----|
| R-74 | LOW | multiple | Seven count or naming inconsistencies: "four durable effects" enumerated three different ways, with `01 §1.1`'s "version **or** audit entry" contradicting `§4.1`'s requirement of both · background workers stated as two, three and two where the union is four, with the idempotency-window sweep absent from the gear document · "seven delta predicates" against eight enumerated in two other documents · "an eleventh order state" four times when the machine already has eleven · event **exactly-once** claimed twice in `DESIGN.md` and denied in `01 §2.2` · the `SUB-O*` ask list inconsistent about count and registration across three documents · three cross-reference pointers wrong (`§3.7` → `01 §4`, `03 §3.1` → `§3.1`, `DESIGN.md §5` → `§1.3`). | Correct each; adopt `§4.1`'s effect list verbatim in both `§1.1`s. |

## Resolution register

Every finding was addressed in the artifacts between 2026-09-08 and this entry. The
**recorded-as** column names the [`../DECISIONS.md`](../DECISIONS.md) entry carrying the decision
and its propagation targets; seven findings were determinate corrections with no decision worth
recording, and those name the section that now carries the fix instead.

**A verification pass over the remediation found two decisions asserted but not applied**, which
is the failure mode this register is most exposed to: a decision names its propagation targets and
nothing checks that they were edited. D-38's reason dedupe was declared complete while
`verdict-version-stale` survived in three places in `06` and
`commercial-field-immutable-outside-amendment` in one place in `04` — so R-47's contract defect had
moved from the prose into the algorithms rather than being fixed. D-38 also renamed a reason the
PRD names in a `MUST` without acknowledging the divergence, now resolved as D-59. Both are closed,
and D-58 closes a third gap the same pass found: an authorized operation present in no endpoint
inventory. Treat the recorded-as column as a claim to be checked, not as evidence.

Three consequences of this wave are not findings and are tracked separately. Ten items with
product or cross-gear consequence are **routed, not decided** — `Q-01`…`Q-10` in the register,
each with a named owner. One new upstream ask was raised, `SUB-O10` (an explicit subscription
start instant), because R-69 had no order-side mitigation. And two ADRs were written for the
decisions that owed one.

| ID | Sev | Recorded as |
|----|-----|-------------|
| R-01 | CRITICAL | `D-06` |
| R-02 | CRITICAL | `D-07` |
| R-03 | CRITICAL | `D-08` |
| R-04 | HIGH | `D-09` |
| R-05 | HIGH | `D-10` |
| R-06 | MEDIUM | `01 §3.6` step 17, `07 §3.6` step 5 — ordering corrected in place |
| R-07 | CRITICAL | `D-11` |
| R-08 | CRITICAL | `D-12` |
| R-09 | CRITICAL | `D-12` |
| R-10 | CRITICAL | `D-13` |
| R-11 | HIGH | `D-14` |
| R-12 | CRITICAL | `D-15` |
| R-13 | CRITICAL | `D-15` |
| R-14 | CRITICAL | `D-15` |
| R-15 | CRITICAL | `D-16` |
| R-16 | HIGH | `D-15` |
| R-17 | HIGH | `01 §4.4` — envelope members and per-event payloads specified |
| R-18 | HIGH | `01 §4.4` — envelope schema version, additive-only within a major |
| R-19 | MEDIUM | `D-17` |
| R-20 | CRITICAL | `D-18` |
| R-21 | CRITICAL | `D-19` |
| R-22 | CRITICAL | `D-19` |
| R-23 | HIGH | `D-20` |
| R-24 | HIGH | `D-21` |
| R-25 | HIGH | `D-22` |
| R-26 | HIGH | `D-19` |
| R-27 | HIGH | `D-23` |
| R-28 | HIGH | `D-24` |
| R-29 | HIGH | `D-25` |
| R-30 | HIGH | `D-26` |
| R-31 | MEDIUM | `D-27` |
| R-32 | MEDIUM | `D-28` |
| R-33 | MEDIUM | `D-28` |
| R-34 | MEDIUM | `D-21` |
| R-35 | MEDIUM | `D-29` |
| R-36 | MEDIUM | `D-30` |
| R-37 | CRITICAL | `D-31` |
| R-38 | CRITICAL | `D-31` |
| R-39 | CRITICAL | `D-32` |
| R-40 | CRITICAL | `D-32`, `D-33` |
| R-41 | HIGH | `D-32`, `D-34` |
| R-42 | HIGH | `D-32`, `D-35` |
| R-43 | HIGH | `D-36` |
| R-44 | HIGH | `D-37` |
| R-45 | HIGH | `D-37` |
| R-46 | HIGH | `D-38` |
| R-47 | MEDIUM | `01 §4.2` and citing slices — reasons deduplicated to the engine names |
| R-48 | MEDIUM | `DESIGN.md §3.1` — eight per-entity IDs, cardinality corrected, one sequence owner |
| R-49 | MEDIUM | `design/README.md` — dependency table corrected, four edges added |
| R-50 | MEDIUM | `DESIGN.md §3.3` — endpoint union with an Owner column |
| R-51 | MEDIUM | `D-38`, `D-52` |
| R-52 | HIGH | `D-41` |
| R-53 | HIGH | `D-41` |
| R-54 | HIGH | `D-42` |
| R-55 | HIGH | `D-42` |
| R-56 | HIGH | `D-43` |
| R-57 | HIGH | `D-44` |
| R-58 | HIGH | `D-45` |
| R-59 | HIGH | `D-46` |
| R-60 | HIGH | `D-47` |
| R-61 | HIGH | `D-48` |
| R-62 | MEDIUM | `D-49` |
| R-63 | MEDIUM | `D-50` |
| R-64 | MEDIUM | `D-51` |
| R-65 | MEDIUM | `D-47` |
| R-66 | MEDIUM | `D-53` |
| R-67 | MEDIUM | `D-54` |
| R-68 | HIGH | `D-39` |
| R-69 | HIGH | `D-56` |
| R-70 | MEDIUM | `D-57` |
| R-71 | MEDIUM | `D-40` |
| R-72 | LOW | `D-55` |
| R-73 | LOW | `D-55` |
| R-74 | LOW | `D-38` |

**Re-review.** A remediation of this size invalidates the wave that prompted it: the fixes
touched every artifact in the set, added five transition rows, four tables and roughly thirty
schema columns, and inverted the engine's guard order. The five passes **MUST** be re-run against
the remediated set before approval, and this register is the input to that run rather than
evidence that it happened.

## Consistency wave — 2026-09-09

A second review wave ran the core `cf-semantic-reviewer-consistency` methodology over the
fourteen design artifacts with [`../PRD.md`](../PRD.md) as the baseline, at single-dispatch
granularity. Verdict **FAIL**: 25 findings — 2 CRITICAL, 18 MAJOR, 5 MINOR. **All 25 are now
resolved in the artifacts.**

The wave exists because the 2026-09-08 remediation could not be trusted on its own account, and
it confirmed that: **six `DECISIONS.md` propagation claims named target sections that were never
edited** (D-14, D-39, D-47, D-49, D-55, D-56). A decision records its propagation addresses so a
mechanised check can catch drift; nothing runs that check, so the addresses read as satisfied
whether or not the edit happened. That is now the top-priority gap in this gear's process, not in
its design.

| ID | Sev | Defect | Resolution |
|----|-----|--------|-----------|
| Rcons-001 | CRITICAL | `DESIGN.md §4.7` undercounted the register — "fifty-seven entries plus ten" against D-01…D-59 and eleven questions | Corrected to sixty and eleven, D-60 included |
| Rcons-002 | CRITICAL | Fifteen claims across eight files that "No ADRs are authored yet", against two `accepted` ADRs cited by ID in the same documents' §1.2 | Every site now links `ADR/0001` and `ADR/0002` |
| Rcons-003 | MAJOR | `08 §3.7` said it introduces no table, then introduced one | Reworded; reconciles with the canonical 17-table inventory |
| Rcons-004 | MAJOR | The submit algorithm looped over **seven** delta predicates against a normative set of eight | Corrected to eight |
| Rcons-005 | MAJOR | D-14 removed "archived" as a name for the draft outcome; it survived in `DESIGN.md §1.2` and `02 §1.2`, `§4.5` — including a verification column demanding a test for a step the machine has no row for | All sites now say auto-void |
| Rcons-006 | MAJOR | The PRD's no-backdating `MUST` (§6.1, §12 AC 8g) had no normative home in `06`, the slice owning the activation intent | `06 §4.3` now carries it, citing `SUB-O10` |
| Rcons-007 | MAJOR | D-55 claimed the contracts actor was cited in `03 §3.6` and `05 §3.6`; neither had it | Added to both sequences |
| Rcons-008 | MAJOR | D-47 claimed "every slice §3.8" gained observability; `02`, `04`, `05` had none | All three now own their signals; all eight slices carry the subsection |
| Rcons-009 | MAJOR | `design/README.md` carried the `05 → 03` edge; `05` declared no such dependency | Declared in `05 §5` and mirrored in `03 §5` |
| Rcons-010 | MAJOR | `direct-cancel-window-closed` registered by both `06` and `07` | `07` owns it; `06` uses it as a pass-through |
| Rcons-011 | MAJOR | Slice `06`'s upstream asks counted five, four and three in three documents | All three now list five, `SUB-O10` included |
| Rcons-012 | MAJOR | `design/README.md` said "Four of these edges" above an enumeration of six | Corrected to six |
| Rcons-013 | MAJOR | `GET …/versions/{version}` declared by two slices; `GET …/versions` declared only by the slice the inventory says does not own it | `08` owns both surfaces; `04` owns the reader and marks its rows as delegated |
| Rcons-014 | MAJOR | `04 §4.4`'s `MUST` prescribed `stale-version` as a reason name — the identifier D-59's mapping exists to prevent — while `06` expects `version-conflict` | Both sites now name `version-conflict` with the PRD condition cited |
| Rcons-015 | MAJOR | `02 §1.2` cited PRD §15 **row 9**; the question is row 8 | Corrected |
| Rcons-016 | MAJOR | The missing-required-date call was attributed to D-11, which is about transition rows | Minted **D-60** as its real home |
| Rcons-017 | MAJOR | ADR-0002 said "ten artifacts", `design/README.md` said fourteen, neither stating scope | ADR-0002 now says ten design documents inside a fourteen-artifact set |
| Rcons-018 | MAJOR | Circular canonicity for the phased map and dependency order, with `DESIGN.md`'s `§1.3` pointer resolving to a section holding neither | `design/README.md` is the sole build-order authority per ADR-0002; the dead pointer removed |
| Rcons-019 | MAJOR | `07 §4.5` promised §15 row citations and carried none; `03` named "the PRD's open question" without a row | Rows 5, 6 and 7 now cited where they apply |
| Rcons-020 | MAJOR | D-39 and D-49 named `07 §4.5` / `08 §4.5`; neither reflected them, and `08` still listed a settled retention as unchosen | `08 §4.5` rewritten as *Policy values* separating set-here, set-elsewhere and Product-owned; `07 §4.5` states the idempotency window is not its value |
| Rcons-021 | MINOR | ADR-0001 said "nine capability areas" with no referent | Corrected to seven |
| Rcons-022 | MINOR | `DESIGN.md §3.3` counted eleven §6-only endpoints and enumerated ten | Enumeration completed; the version **list** and the per-line read are the two that were missing |
| Rcons-023 | MINOR | `UPSTREAM_REQS.md` promised two of `SUB-O7`–`SUB-O9` and carried one | States that only `SUB-O9` is an ask of this gear |
| Rcons-024 | MINOR | `08`'s matrix read row named five labels for six endpoints, so exhaustiveness was uncheckable by counting | Split into "version list · version read" |
| Rcons-025 | MINOR | `05` registered `order-terminal` where the engine's `not-admissible` already covers it — row 25 admits acceptance from any non-terminal state | Reason dropped; the engine reason cited, matching `07`'s treatment |

**Two of these findings were introduced by the 2026-09-08 remediation itself** (Rcons-001, and
Rcons-014's survival past a grep that saw the line and misread it as prose), and a third was
introduced while fixing this wave — writing D-60 made the register sixty entries one command
after `DESIGN.md` was corrected to fifty-nine. Every count claim in this set should be asserted
by a script in CI rather than by a reviewer's diligence; that is the standing recommendation.

## Design and ADR wave — 2026-09-09

Five reviewers ran against the seventeen artifacts with [`../PRD.md`](../PRD.md) as the yardstick:
the artifact checklist's **SEM** category, three **coverage** passes splitting PRD §5–§16 by
section, and an **ADR sufficiency** pass scoring all sixty decisions against the programme's six
significance tests. Verdict **FAIL** on all five: **63 findings**, of which 1 CRITICAL, 5 HIGH and
20 MAJOR. **324 discrete PRD behaviours** were enumerated — 277 covered, 29 partial, 1 missing, 13
divergent (4 disclosed).

All 63 are resolved. The remediation added **twenty-two decisions** (D-61…D-82), **seven open
questions** (Q-12…Q-18), **three ADRs** (0003–0005), **four upstream asks**, **one table**
(`orders_policy_election`), and a **ninth** gate predicate.

### The blocking cluster

`F-SEM-001`, `RcovC-002` and `RcovC-011` were three symptoms of one hole: **transition rows 19 and
20 guarded on the new version's requirement verdict, which nothing in the specified system can
supply.** Verdicts exist only as reflections keyed `(order_id, version)`, so none can exist for a
version the amendment has not created; `06 §4.2` forbids deriving one; no port to the approval
policy owner is declared; and PRD §12 AC-11a forbids this gear to query it. Amendment from
`approved` was therefore impossible, and an amendment in `pending_approval` whose new version no
longer required approval had **no exit at all** — row 8 admits that verdict only from `submitted`.

Resolved as **D-61 / ADR-0004's sibling decision**: re-approval is a **two-step seam interaction**.
An amendment from either state lands in `submitted` and publishes `OrderAmended`; the sibling gear
obtains the new version's verdict and reflects onward through rows 7 and 8, which already exist and
which it already drives. No new port, no PRD amendment to AC-11a, no caller-supplied verdict. The
cost — PRD §6.1's *direct* `approved → pending_approval` edge becomes two steps — is routed as
Q-12, together with the PRD's own inconsistency between §10 UC-002 / §12 AC-5 and §5.1 / §6.2.

### Resolution register

| Findings | Resolution |
|---|---|
| `F-SEM-001`, `RcovC-002`, `RcovC-011` | D-61: two-step seam; rows 18–20 reshaped, `verdict-unavailable` deleted, Q-12 routes the divergence |
| `F-SEM-002`, `RcovB-001`, `RcovC-003` | D-63: Orders Workflow granted `hold`/`resume` under the service principal |
| `RcovB-004` | D-63: `amend` split into its own matrix row, withheld from Direct Customer per §6.6 |
| `F-SEM-003`, `RcovB-008`, `RcovC-005` | D-40 applied to `03 §1.1`, `§1.2` and `§3.2`; no design statement now assigns money arithmetic to this gear |
| `RcovA-003`, `RcovB-002` | D-62: a **commercial-frozen** field class plus the `tenant-axis-immutable` guard and reason |
| `RcovA-004` | The ninth delta predicate and `required-line-date-unresolved`, honouring D-60's propagation |
| `RcovA-001` | D-64: a draft carries version 1; submit appends version 2; the expected-version contract stated for state-only rows |
| `RcovA-002` | D-65: the registry is probed before guard-input resolution, so a replay invokes no port |
| `RcovA-010` | The in-flight set enumerated as five states, and the partial unique index defined over exactly those |
| `RcovA-013` | `amendment-not-admitted-in-state` deleted as unreachable; the engine's `not-admissible` is the AC-6 reason |
| `RcovC-004` | D-66: `orders_policy_election` gives both begin-fulfillment elections a store, a scope and a safe fallback |
| `RcovB-010` | The submit contribution records the submitting principal as the initiating actor |
| `RcovB-006`, `RcovB-C2` | D-67: a common order-summary block carried by every event |
| `RcovA-005`, `F-SEM-005` | Q-14 routes the contract-effective default; the divergence is now disclosed in `02 §4.2` |
| `RcovA-006` | **D-71 — fixed rather than disclosed.** Acceptance is recordable on the partner path regardless of the required flag, with `requirement_source` = `volunteered` |
| `RcovC-006` | D-68: the not-found refusal reconciled with AC-21, whose wording is routed for amendment |
| `RcovB-007` | D-52 relabelled a PRD §9.1 deviation needing Product's acknowledgement |
| `RcovA-011` | D-14 annotated: `draft → expired` is an addition to a PRD-normative diagram |
| `F-SEM-007` | D-70: the audit read reclassified as design-introduced with no FR basis |
| `F-SEM-004` | `05 §2.2` and `§4.4` now cite Q-08 and state that the PRD carries no §15 row |
| `Radr-012` | Q-13 routes D-59's reinterpretation of a PRD `MUST` |
| `RcovC-010` | D-69: adding a state or event type is additive, with the consumer obligation stated |
| `RcovA-007`, `RcovB-003`, `RcovC-007`, `RcovA-008`, `RcovC-008` | Four upstream asks added — pre-subscription evaluation, annualised TCV, external-reference propagation, indicative tax |
| `Radr-001` | **ADR-0003** fail-closed on an unevaluable gate input, with the tolerated-risk election as the named rejected alternative; D-72 and Q-15 |
| `Radr-002` | **ADR-0004** closed state and event enumerations, consolidating D-14/15/16/60 |
| `Radr-003` | **ADR-0005** a refusal is a committed outcome, plus ADR-0001's Consequences corrected for refusals and the erasure exception |
| `Radr-004`, `RcovC-001`, `F-SEM-015` | Q-16 routes the publish-inclusive `p95 < 1 s`; `DESIGN.md §1.2` now discloses both latency divergences |
| `Radr-005` | ADR-0002 re-scoped to the runtime decomposition; document reasoning moved to D-03 and the index |
| `Radr-007` | ADR-0001's Confirmation cites the tests `01 §1.2` actually contains, plus a new edge-coverage test |
| `Radr-008`, `Radr-009` | ADR-0002's edge count corrected to six and its sibling count to four, naming `ledger` |
| `Radr-006` | `DESIGN.md`'s "exactly one state event" and "event exactly-once" claims corrected to at-least-once with event-declaring rows |
| `Radr-011` | D-73 and D-79 minted, plus **six more** the slices had declared and never recorded (D-75…D-81) |
| `Radr-010` | **Not applied.** The reviewer argued the programme uses a `status: proposed` convention for autonomous ADRs; only `subscriptions` does — `pricing`, `rating` and `ledger` all use `accepted`. The precedent claim is wrong, so the status stays |
| `F-SEM-006` | The Seller Operator's re-drive grant recorded as a role widening in the §3.3 disclosure |
| `F-SEM-008`, `RcovB-009` | The `subscriptions` row corrected to name the read-only port; `contracts` given a deadline row and the submit budget corrected to **1.75 s** |
| `F-SEM-009` | The stale spawn-signal clearing rule removed from `06 §3.1` |
| `F-SEM-010` | `DESIGN.md §4.7` corrected: four of the remaining seven §15 rows carry design positions |
| `F-SEM-011` | Sweep cadence removed from the Product-owned list in `07 §2.2` |
| `F-SEM-013`, `RcovC-009` | `03 §4.3` names the per-state TTL as the bound; Q-17 routes PRD §16's staleness window |
| `F-SEM-014` | `contract-order-events` cited from `DESIGN.md §3.3` |
| `RcovA-009` | D-82: the version reason vocabulary is `{submit, amendment}`; the other six are audit reasons |
| `RcovA-012` | A **read-through, never authored** field class; renewal terms surfaced by `08 §4.2` from the contracts port |
| `RcovC-012` | The §11 UI surfaces delegated to the frontend set; Q-18 routes the partner-admin hold conflict |
| `RcovC-013` | The RPO-zero and RTO-60 claims scoped to intra-cell failure domains, with whole-cell loss as stated residual risk |
| `RcovB-005` | A self-service election added as an input to the permission declaration |
| `RcovC-012` (UI), `F-SEM-012` | The TCV-versus-§5.2 tension reconciled in `03 §4.4` |

### What changed in the process, not the documents

Three of this wave's findings were **created by** the two previous remediation waves, and six were
`DECISIONS.md` propagation claims whose named sections were never edited. That is now the dominant
defect class. Review must explicitly check decision propagation addresses, self-referential
counts, reason-registry uniqueness and reachability, endpoint ownership, and retired vocabulary
declared by a decision's `**Retires**:` line. No automated CI enforcement of these checks is
claimed here.

## Recommended sequence

1. **Clusters A, B and C together, as one change.** The guard order, the in-flight marker, the missing rows, the row 12/13 ambiguity, the event catalogue and the one-row-per-transition invariant are a single coherent problem: the engine's contract. Fixing them separately will produce a fourth inconsistent state.
2. **Cluster D** next, since the schema is what the corrected algorithm writes to, and R-20 through R-22 make the current one unimplementable.
3. **Cluster E**, which is small in volume and large in consequence, and where R-38 is a substantive hole rather than a documentation one.
4. **Clusters F and I** are mechanical and can be done in one editing pass once A–D have settled the facts they are counting.
5. **Cluster G** is genuine new design work — capacity, resilience, data protection, threat model, observability, extension points — and is the largest remaining body of work after the correctness fixes.
6. **Cluster H** needs a row-by-row pass against PRD §15 and two decisions routed to their PRD owners.

Every decision taken while resolving these belongs in a `DECISIONS.md` that does not yet exist;
opening it is a prerequisite rather than a follow-up.

## Traceability

- **Design set reviewed**: [`../DESIGN.md`](../DESIGN.md), [`../design/README.md`](../design/README.md), [`../design/01-foundation.md`](../design/01-foundation.md) … [`../design/08-read-and-authz.md`](../design/08-read-and-authz.md)
- **Requirements**: [`../PRD.md`](../PRD.md)
- **Review instrument**: [`docs/checklists/DESIGN.md`](../../../../../docs/checklists/DESIGN.md) v2.0
- **In-repo precedent for this artifact**: `gears/bss/rating/docs/reviews/2026-07-31-billing-domain-review.md`
