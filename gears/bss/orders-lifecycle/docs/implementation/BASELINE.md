# S1-01 — Reconciled implementation baseline

Status: **completed as documentation reconciliation, 2026-10-05**. Runtime, SDKs, fixtures,
migrations and provider/deployment evidence are not delivered by this package. The subsequent [S1-02 catalog](contracts/CONTRACTS.md) is now available.
Accepted D-187–D-200 govern their explicitly superseded seams; earlier decisions remain history,
not competing implementation choices. Genuine product questions retain their registered owners
and conservative interim behavior in [REVIEW](REVIEW.md#product-questions-and-release-boundaries).

## Inventory checked against the normative sources

| Inventory | Verified baseline | Authoritative source / next owner |
|---|---|---|
| Public operations | Capture/admin/Preview, submit/amendment, consent, five Workflow mutation operations, hold/resume/cancel/forced failure, current/historical/list/audit reads | DESIGN §3.3 and feature operation contracts; S1-02 enumerates exact methods/DTOs/errors |
| Workflow local SDK | Nine methods including hold/resume and current/version reads; report_spawn_signal includes typed roster contribution and returns grant identity/generation | DESIGN orders-lifecycle-workflow-sdk; S1-02/05, S5-01 |
| States | 11 total, five terminal | Foundation state contract |
| Transition rows / trigger tokens | 29 numbered rows / 21 distinct tokens; multi-state rows must expand into unique lookup keys | Foundation state contract; S1-02 manifest and S2-04 |
| Event types | 11; no new event for commercial attempt reservation or staged control preparation | DESIGN Foundation event set; S2-08 |
| Orders-owned tables | 24 including D-188 attempts and D-198 grants/controls | DESIGN §3.7; S2-02 is sole DDL owner |
| Maintenance workers | Five: state expiry, draft auto-void, idempotency maintenance, retention purge, audit verification/checkpoints | DESIGN Foundation §3.8; toolkit producer drain is separate |
| Commercial scope | new_sale admitted; change/live-subscription add-on extensions remain deferred | D-176, Q-01/Q-33; S3-01 supported-profile inventory |
| Idempotency retention | Ordinary 24 hours; workflow-class at least 30 days under D-173; persisted absolute deadlines, no retry extension | Foundation §4.2 / schema; S2-05/11 |
| Version identity | Sparse committed commercial versions, permanent burned candidates, explicit predecessor; contiguous audit sequence is separate | D-188; S2-05/S3-12 |
| Commercial evidence | Complete schema-2 receipt/query/BillingTerms/selected bindings, exact issued hold_until | D-191/192; S3-04/05/12, S6-01 |
| Receiver authority | D-198 immutable grant, attempt/generation, pending plus active capacity, staged pause/revoke controls | D-198; executable remaining corrections S5-01 |

### States

`draft`, `submitted`, `pending_approval`, `approved`, `in_fulfillment`, `on_hold`, `completed`, `rejected`, `cancelled`, `fulfillment_failed`, `expired`.

### Trigger tokens

`create`, `draft-mutate`, `administrative-edit`, `submit`, `cancel`, `auto-void`, `reflect-approval-required`, `reflect-approval-not-required`, `reflect-approval-granted`, `reflect-approval-denied`, `begin-fulfillment`, `report-spawn-signal`, `acknowledge-completed`, `acknowledge-failed`, `cancel-workflow-mediated`, `amendment`, `hold`, `resume`, `expire`, `record-acceptance`, `force-fail-unreconciled`.

### Event types

`OrderSubmitted`, `OrderApproved`, `OrderRejected`, `OrderAmended`, `OrderHeld`, `OrderResumed`, `OrderCancelled`, `OrderExpired`, `OrderCompleted`, `OrderFulfillmentFailed`, `OrderAcceptanceRecorded`.

### Orders-owned tables

- `orders_order`
- `orders_order_version`
- `orders_order_line_identity`
- `orders_order_line`
- `orders_draft_content`
- `orders_order_admin`
- `orders_order_line_admin`
- `orders_resolved_total`
- `orders_transition_audit`
- `orders_audit_checkpoint`
- `orders_audit_checkpoint_member`
- `orders_fulfillment_grant`
- `orders_fulfillment_control`
- `orders_commercial_attempt`
- `orders_idempotency`
- `orders_line_fulfillment`
- `orders_inflight_overlap_claim`
- `orders_acceptance`
- `orders_gate_outcome`
- `orders_approval_reflection`
- `orders_state_ttl_policy`
- `orders_date_policy`
- `orders_policy_election`
- `orders_read_access_log`

## Review dispositions

| Finding | S1-01 disposition | Remaining delivery |
|---|---|---|
| Broker TODO / missing runtime claims | Corrected current DESIGN dependency to existing runtime and D-200 integration gates | S2-08/S6-05 deployment, producer and recovery |
| Blanket single-transaction / no owner-fence claims | Narrowed to ordinary triggers; D-188/D-198 exceptions explicit in DESIGN and Foundation | S2-05/S3-12/S5-09 implementation |
| Legacy read pin / unresolved R12 language | Schema-2 selected receipt mapping and selected D-198 status applied | S3/S5/S6 real adapters and conformance |
| Default-dimension collision | Same explicit tagged comparator in Foundation and D-195 | S1-04/06 codec fixtures and S3-09 |
| Duplicate component anchors | 68 later duplicate occurrences renamed; original first anchors preserved; no references to ambiguous old fragments were found | [Anchor migration record](anchor-migrations.json); future links use distinct anchors |
| Audit precedent and nonexistent source files | Current unsealed Pricing writer distinguished from Orders integrity requirements; historical reports explicitly marked superseded as current-code evidence | S2-06/11 integrity implementation |
| Missing historical worker/fixture infrastructure | Removed claims that nonexistent Pricing jobs or shared fixture runner are reusable current code | S1-03 capability proof; S1-06 chooses/delivers an actual harness |
| Nullable platform policy PKs | Assigned concrete physical-key correction; no incidental migration policy invented in this documentation package | S2-02, S4-01/07, S5-12 |
| Q-12 reapproval contradiction | PRD diagram, normative text, AC-5, feature rationale and live question row aligned to selected D-174 | S4-01/09 and S5-03 provider/reflection |
| Sparse submit consent lookup | Retained exact-submit-version requirement; dense-version implementation forbidden | S4-08a/b |
| Tax owner and cyclic dependency risks | Provider ownership and acyclic early delivery explicitly retained in subplans | S3-04/08/09; owner assignment S1-07 |
| Port budgets versus full-path SLO | Existing latency objective retained; new call graph and measurement assigned, no 30-second replacement adopted | S1-04/S6-06; Q-11/Q-16 remain open |
| Workflow topology / successor grants / force-fail generation / actual-start clock | Concrete contract corrections remain a named gate before grant DDL and receiver code; not claimed solved by renaming aliases | S5-01, then S5-02/08/15 |
| Early provider and consent/submit dependencies | Single owners and early/later package splits already recorded in handoff | S4-02/09, S5-06a and S4-08a |
| Structural security verification | Blocking trybuild/lint/read-only-transaction requirements explicit | S1-03/S2-03/S6-07 |
| Missing RUST.md guideline | Verified missing; recorded limitation, existing ToolKit guidance remains authoritative | Repair guideline link independently; no invented replacement |
| Workflow retention conflicts with ordinary 24-hour prose | Applied existing D-173 ≥30-day exception to live schema/retention rules and S2 plan | S2-05/11 boundary and cleanup tests |
| SUB numbering fork | Source-qualified semantic alias map published and linked from both peers; no guessed branch-only methods | UPSTREAM_REQS alias map; S1-05/S5-01 signatures and receiver delivery |
| Q-17 deadline and Q-21 Preview wording | Live register distinguishes issued receipt deadline from TTL and diagnostic writes from commercial effects | S3-05/10/11; broader product policy remains owned externally |

## Validation and next assignment

Validation passed: **4,159 local links**, unique explicit anchors, source inventory cardinalities
and coverage routing for **27 documents / 2,557 units / 44 upstream requirements**. Commands:

```sh
python3 gears/bss/orders-lifecycle/docs/implementation/validate_docs.py
python3 gears/bss/orders-lifecycle/docs/implementation/build_coverage.py --check
git diff --check
```

Source-routing regeneration, package references and the focused before/after diff were also reviewed. These are
documentation checks, not runtime or real-provider tests. No Cargo dependency or Rust file changed.

Proceed to [S1-02](01-contracts-and-fixtures.md#s1-02--freeze-the-public-operation-and-storage-contract-catalog):
turn the inventory into the exact operation/storage/state/error/event manifest and boundary
fixtures. S5-01's explicit receiver corrections must precede final grant/control schema and SDK
fixtures; a documentation baseline is not permission to bypass that prerequisite.
