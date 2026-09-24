# Lens review — slices 02–08 (2026-09-23)

**Scope.** `design/02-capture.md` … `design/08-read-and-authz.md`, read on the working tree of
`bss/orders-lifecycle-design-doc` (HEAD `912b4ca5f` **plus uncommitted edits**).

**Method.** One subagent per slice; each ran three lenses as separate passes:

- **A — Self-consistency:** does the document contradict itself?
- **B — Implementability:** could an engineer build this from the text alone?
- **C — Cross-document claims:** is what it says about other docs and crates true?

Every finding carries `path:line` and verbatim quotes. All HIGH findings and a sample of MEDIUM
quotes were re-opened and checked against the files; all checked quotes held. MEDIUM/LOW not
individually re-checked are marked as reported by the lens.

**Totals.** 95 raw findings → 92 items after merging (2 cross-slice duplicates, plus M-1 grouping
two "missing algorithms" findings). **8 HIGH · 43 MEDIUM · 41 LOW.**

Merged (two slices found the same defect independently — strongest signal):
- H-1 = 06 + 07 (held `in_fulfillment` order dead-ends)
- M-2 = 02 + 04 (line-level administrative fields unaddressable after draft)

Paths below are relative to `gears/bss/orders-lifecycle/docs/`.

## Remediation status (2026-09-23)

| Finding | Status | Decision |
|---|---|---|
| H-1 held `in_fulfillment` dead-end | fixed — rows 26/27 from `on_hold` | D-109 |
| H-2 overlap key source | fixed — new gate step 4, ninth port | D-108 |
| H-3 gate-outcome UNIQUE | fixed before remediation (on-disk edit) | — |
| H-4 late approval refusal | fixed — version check first for workflow triggers | D-110 |
| H-5 stored sales path | fixed — `orders_order.sales_path` | D-106 |
| H-6 acceptance-required precedence | fixed — contract > seller > platform > fallback | D-107 |
| H-7 delegated path | fixed — PDP evaluates proof | D-111 |
| H-8 required date unreachable refusal | fixed — defaults never satisfy a requirement | D-60 clarified |

All edits uncommitted; `make design-check` passes (1241 invariants, 46 self-test cases).
MEDIUM and LOW findings are not yet addressed.

---

## HIGH

### H-1 · A held `in_fulfillment` order after spawn signal cannot reach a terminal state — *06 B, 07 B (independent)*
- Where: `design/01-foundation.md:2200-2203, 2210`; `design/06-workflow-seam.md:554-556`; `design/07-hold-and-expiry.md:674, 761-762`; `design/08-read-and-authz.md:949`
- Quote: 01 rows 13/14/16 are all "**FROM** `in_fulfillment`"; only row 23 leaves `on_hold` via `cancel` ("the pre-hold state's own cancel guard admits it"). 07: "An order at the cap can still be cancelled"; "row 22 carries the resume-cap guard unconditionally". 08: cancel for Workflow is "via workflow-cancel only".
- What breaks: acknowledge-completed/failed and workflow-mediated cancel are `not-admissible` from `on_hold`; ordinary cancel is refused `direct-cancel-window-closed` after spawn; expiry is exempt. Once `resume_count` is at the cap (five resumes, shared across all holds), the order is stuck forever with live subscriptions. Workflow PRD explicitly holds orders this way before declaring the outcome.
- Fix: exempt a resume from an `in_fulfillment`-origin hold from the cap, or add `on_hold` rows for acknowledge/workflow-cancel guarded on the stored pre-hold state; correct 07's "can still be cancelled".

### H-2 · No step or port produces the overlap scope key — *03 B*
- Where: `design/03-gate-and-pin.md:307, 548`
- Quote: "The default `overlapScopeKey` is `(payerTenantId, catalogSubscriptionProductKey)`"; step 15 submits "the complete resolved `(payer_tenant_id, overlap_scope_key)` claim set".
- What breaks: none of the eight ports / 14 steps resolves `catalogSubscriptionProductKey`, yet it fills `orders_order_line.overlap_scope_key`, the claim index and predicates 7/9. Two teams derive it differently → one-in-flight rule fails.
- Fix: name the operation/port/unavailable-reason that returns each line's key; add a resolve step before step 4.

### H-3 · Gate-outcome UNIQUE key can't hold Pricing's per-scope-key answers — *03 B* — **RESOLVED on disk (15:30 edit): `component_plan_id` and `catalog_scope_key` columns added and included in the UNIQUE at `03:720`**
- Where: `design/03-gate-and-pin.md:694`; `../../pricing/pricing/src/domain/sellability.rs:333`
- Quote: "`(run_id, line_id, predicate)` UNIQUE NULLS NOT DISTINCT" vs `pub const PER_KEY: &[Self] = &[Self::ActiveWindowWithHorizon, Self::GaGateFlags];`
- What breaks: predicates 1 and 5 are answered once per bound scope key → several rows per `(run, line, predicate)` → unique violation rolls back submit, or collapsing drops the diagnostic §4.1 requires.
- Fix: add nullable `scope_key` to the row identity, the UNIQUE and the declared ordering.

### H-4 · Late approval after amendment gets `not-admissible`, not the promised `version-conflict` — *04 B*
- Where: `design/04-versioning.md:402-403, 591-592`; `design/01-foundation.md:2015, 2206-2207`
- Quote: 04: "an approval reflection or fulfillment acknowledgement carrying version N **MUST** be refused with the engine's `version-conflict` reason"; 01: "state-table admissibility, then the version check, then slice guards".
- What breaks: rows 19/20 move the order to `submitted`, so a late `reflect-approval-*` / `begin-fulfillment` finds no row and is refused `not-admissible` before the version is checked. Breaks §4.4 "MUST name the current version", PRD AC 5a, the sequence diagram and the §3.8 race signal.
- Fix: either make `not-admissible` carry the current version and document it, or check version before admissibility for workflow triggers.

### H-5 · Acceptance steps copy a "stored sales path" no table stores — *05 B*
- Where: `design/05-preconditions.md:309, 341`; `design/01-foundation.md:1238`
- Quote: "recording_path from the stored self_service/partner_placed sales path"; "Separate recording copies the stored sales path, never infers it from the current actor."
- What breaks: `orders_order` has no sales-path column (only `initiating_actor`) and no submitting-actor record, so step 5 can't fill `recording_path` and the recording-party bar has nothing to key on.
- Fix: add `sales_path` (and submitting actor if the bar needs it) to `orders_order` in 01 §3.7, written at create/submit.

### H-6 · Acceptance-required precedence contradicts itself; provenance enum can't record a seller election — *05 A*
- Where: `design/05-preconditions.md:424, 500, 564`; `design/01-foundation.md:1824`
- Quote: "**MUST** be sourced from the referenced contract where one…" vs "`scope` in (`platform`, `seller`) and seller scope overriding"; 01: "`contract`, `platform_default` or `volunteered`".
- What breaks: contract-vs-seller order is never stated; a seller-elected requirement would be stored as `platform_default` — false provenance on the dispute-evidence row.
- Fix: state contract > seller > platform > fallback in §4.1; add `seller` to `requirement_source`.

### H-7 · Orders can't tell which PDP-authorized path "requires delegation" — *08 B*
- Where: `design/08-read-and-authz.md:594, 636, 685`
- Quote: "**IF** the PDP-authorized path requires delegation **AND** no valid delegation proof is present:"
- What breaks: `PolicyEnforcer`/`AccessScope` return compiled constraints with no delegated-path marker. Teams either re-derive from actor class (forbidden by §2.1/§4.3) or trust PDP and skip the proof — the PRD's single cross-tenant control.
- Fix: PDP evaluates the proof and returns allow/deny, or the PDP contract carries a per-alternative delegation marker; record under `upreq-pdp-policy-integration`.

### H-8 · A required date can never be "unresolvable", so the D-60 refusal is dead — *02 A*
- Where: `design/02-capture.md:416-417, 464-466`; `design/01-foundation.md:1368-1369`; `DECISIONS.md:565-567`
- Quote: "Optional unless the tenant policy switch requires it | The contract-effective date"; "Where the policy switch requires a calendar field and none can be resolved, submit **MUST** be refused".
- What breaks: every dependent date defaults to contract-effective, which defaults to UTC(t) — "none can be resolved" never happens. Whether a required date may default (D-60: caller supplies it) and whether optional dates store NULL or the default is open.
- Fix: state that defaults don't apply when the switch requires the field, and when an optional unauthored date is stored as NULL.

---

## MEDIUM

### Cross-slice
- **M-1 · Missing algorithms for declared endpoints/operations.** 02: PATCH order, PATCH line, DELETE line have no sequence; `line-not-in-draft` unreachable; no unknown-`lineId` reason (`02:273-279, 343`). 06: begin-fulfillment, spawn-signal, `/workflow-cancel` have no algorithm; `spawn-signal-already-recorded` raised by no step (`06:324-326, 389-405`).
- **M-2 · Line-level administrative fields unaddressable after draft** — *02 B + 04 B independent.* Line PATCH is draft-only; order-level PATCH / Administrative Edit input has no `line_id` (`02:275, 499`; `04:421, 426, 284-285`).

### 02-capture
- M-3 · Per-tenant date policy has no store; toolkit typed config is static per gear, no `resourceTenantId` key or revision (`02:430-432, 367-368`; `libs/toolkit/src/config.rs`).
- M-4 · Line `currency` authored but unclassified → startup MUST fail (`02:340, 480-482`).
- M-5 · External reference recorded in the line-authoring step mixes row 2 (`draft-mutate`) and row 3 (`administrative-edit`) in one transition (`02:347, 499-502`).
- M-6 · `sellerTenantId` editable in draft vs immutable order number unique per seller — no refusal/reallocation defined (`02:284, 398`; `01:996, 1255`).

### 03-gate-and-pin
- M-7 · `pin_frontier` reads the *caller's* tenant frontier (partner path ≠ seller); `PermissionDenied` unmapped (`03:456-457, 527`; `pricing-sdk/src/api.rs:19, 36-42`). *verified*
- M-8 · Post-`active` collision "not a line rejection" vs D-89 "MUST surface as an `overlap-collision` line rejection" (`03:276`; `DECISIONS.md:1147-1148`). *verified*
- M-9 · `preview-term-or-cycle-missing` registered as 400 vs Preview still answering without TCV (`03:588-589`; `01:2741`).
- M-10 · Predicate 7 "A boolean presence read is insufficient" vs re-check and SUB-O5 being presence reads (`03:758, 612, 231`; `UPSTREAM_REQS.md:68-69`).
- M-11 · Re-check output "proceed, or a per-line rejection" has no value for hold/terminal/superseded or port-unavailable (`03:603, 607`).
- M-12 · Pin composition skipped on gate failure but required in the complete vector and all-failures report (`03:540-543, 677-678, 769-770`).
- M-13 · Party eligibility assigned to both identity port and contract port (`03:471-473, 530, 748`).
- M-14 · Predicate 4 checks each line's region; no line stores a region (`03:750`; `01:1358-1370`).
- M-15 · Dependency table lists `rating` / `account-management` SDK clients that don't exist or lack the operation (`03:492, 498`; `UPSTREAM_REQS.md:189-190`).

### 04-versioning
- M-16 · Principle "moves state only from `approved`" contradicts row 19 (`pending_approval → submitted`) (`04:135-137`; `01:2206`). *verified*
- M-17 · "Crosses seller scope" has no predicate, port or unevaluable reason (`04:358, 168-170, 337-340`).
- M-18 · Amendment guard order omits capture's shared structural guards, line cap and `field-unclassified` (`04:357`; `01:770, 776`).
- M-19 · Skipping gate inputs on local-guard failure collides with engine step 3's unresolvable-input branch (`04:358`; `01:683`).
- M-20 · `amendment_reason` validation is no declared guard and reuses `amendment-empty` (`04:443`).
- M-21 · Missing expected version has no refusal/status/check order (`04:109-110, 503`).

### 05-preconditions
- M-22 · Acceptance instant is "the commit time" (§4.2) vs "not a prediction of physical commit time" (sequence) (`05:470` vs `05:313`). *verified*
- M-23 · Recording-party guard declared, never resolved, no reason registered (`05:305, 257-260, 414`).
- M-24 · Observability alerts on an authorization port Lifecycle doesn't have (`05:417` vs `05:90, 536`).
- M-25 · Workflow "MUST submit a conclusive `authorized` or `failed`" vs guard handling `pending` (`05:520, 515-516` vs `05:361, 70`).
- M-26 · Contract port (03) returns no acceptance-required election; no Contracts SDK exists (`05:280`; `03:471-472`; `gears/bss/contracts/`).
- M-27 · "A seller may change" an election, but no write path, and the cited channel is deploy-time only (`05:398, 403-404`; `DESIGN.md:1027-1028`).

### 06-workflow-seam
- M-28 · Pre-spawn `/workflow-cancel` admitted without evidence vs row 16 requiring it (`06:471, 474-475, 316`; `01:2203`).
- M-29 · Who runs the activation re-check: Lifecycle "through 03" vs Workflow via SDKs, "not a Lifecycle endpoint" (`06:347-348, 276, 331-333` vs `06:612-615`; `03:601-602`).
- M-30 · Denied verdict has no denial-reason input, but `OrderRejected` must carry one (`06:364, 503-512`; `01:2244`).
- M-31 · Compensation-evidence guards read an undefined jsonb shape (`06:424, 474, 522-524`).
- M-32 · Acknowledge Fulfillment input lacks `correlation_id` and `failure_reason` both required elsewhere (`06:421, 308, 85, 629-630`).

### 07-hold-and-expiry
- M-33 · Resume cap violates PRD "A held order **MUST** be resumable"; divergence unregistered (`07:855`; `PRD.md:426`).
- M-34 · Seller override scope listed as open Product question but specified normatively (`07:838` vs `07:600, 460, 199-200`).
- M-35 · Hold actor/instant/reason "column set" has no columns in 01 §3.7 and no write step; D-22 says they were added (`07:227-229, 544-545`; `01:1244`; `DECISIONS.md:413`).
- M-36 · Hold both has the slice supply the pre-hold value and the engine capture it (`07:400`; `01:708, 722`).

### 08-read-and-authz
- M-37 · Read One Order "no row" branch has nothing to send to PDP → timing oracle vs promised indistinguishability (`08:633, 650-651`).
- M-38 · "Target-independent denial" (403 vs 404) has no producing mechanism (`08:590-593, 633-635`).
- M-39 · List Orders PDP denial has no reason and no refused access-log row (`08:688, 606, 1254`).
- M-40 · Cursor failures map to an unregistered "malformed-request response"; non-audit cursors have no contract (`08:243-245, 585`; `01:2782-2788`).
- M-41 · Cites a confidentiality prohibition in 03 §4.6 that isn't there (`08:182`; `03:893`). *verified*
- M-42 · Event-consumer read grants routed to UPSTREAM_REQS §2.7 (broker grants), not §2.9 (`08:962-963`; `UPSTREAM_REQS.md:366-380, 500`).
- M-43 · Required `actor_class` access-log column has no defined source (`08:811, 967, 631`).

---

## LOW

### 02-capture
- Author Line input lacks `expected_version`, `idempotency_key`, `security_context` (`02:340`).
- `date-cascade-invalid` owner listed as the gate only; engine also raises it for stale date basis (`02:280-282, 444-445`).
- `field-unclassified` registered as a 400 though unclassified fields fail startup (`02:251-252, 279`).
- "The only guards on authoring" omits the line cap (`02:111-112, 343`).
- `change` does not have "its own" reason; 01 uses shared `category-not-admitted` (`02:154-155`; `01:772`).
- "Two additions" to 01's schema are already in 01 §3.7 (`02:359-363`).

### 03-gate-and-pin
- Cites 08 §4.2 for the tax-exclusion rendering rule; 08 declares only the overlay exclusion (`03:855-858`; `08:919-920`).
- Step 10 duplicates predicate 8 → duplicate outcome row (`03:539, 761`).
- "predicate 9 above" points forward ~465 lines (`03:297`).
- Preview write path called "unauthenticated" though PDP-authorized (`03:877` vs `03:447`).

### 04-versioning
- "Linear supersedes constraint" not in 01 §3.7 schema (`04:445`).
- Version appender both does and does not move the pointer (`04:224-225` vs `04:232`).
- Row 20 called "unguarded" though it carries the cap guard (`04:572` vs `04:469-471`).
- Administrative-edit entity said to append to the version chain; `entity-order-version-chain` ID undefined (`04:203, 205`; `DESIGN.md:332`).
- "All nine gate predicates" understates the gate (`04:489`; `03:373, 745`).
- DESIGN.md versioning component still lists the deleted amendment-forbidden guard (`04:215-216, 318-322`; `DESIGN.md:524-525`).

### 05-preconditions
- "The guard records whether it read an explicit row or the fallback" — no field holds it (`05:401-402, 567`).
- `requirement_source` description omits `volunteered` (`05:379, 171` vs `05:431`).
- Quoted 01 step 3.1 wording isn't in 01 (it's in DECISIONS.md:243) (`05:262`; `01:684`).
- Q-08 said to route both limitations; covers only declined instrument (`05:602-603`; `DECISIONS.md:1741`).

### 06-workflow-seam
- §3.3 gear-wide scope claim vs §4.1/08 per-operation PDP grant (`06:318-319` vs `06:560-562`; `08:1064`).
- `begin-fulfillment-preconditions-unmet` duplicates 05's specific reasons; raised by no step (`06:325`; `05:257-259`).
- Cancel Order cited at 07 §3.6; it is in 07 §4.6 (`06:455, 485`).
- Traceability says 07 reads the spawn signal for expiry exemption; 07 uses `pre_hold_state` (`06:727`; `07:769-771`).
- `fr-owf-line-progress` attributed to Workflow PRD §9.1; it's §6.3 (`06:285`).
- `orders_approval_reflection` said to be specified in 01 §3.7; it's only in 06, and lacks the version FK (`06:493-495`; `DECISIONS.md:833`).
- §4.6 omits `upreq-overlap-activation-atomicity` that §4.3 relies on (`06:698-719`).

### 07-hold-and-expiry
- Hold admissibility declared as a slice guard returning engine-owned `not-admissible` (`07:399, 359-361`; `04:357`).
- Expiry/auto-void workers don't handle `still-processing` / `idempotency-mismatch` (`07:457`).
- Claims 01 §3.7 dropped the `(state, created_at)` index; 01:1268 still has it (`07:623-624`). *verified*
- `Db::try_lock` cited without its required `LockConfig`; default blocks 30 s (`07:804`; `libs/toolkit-db/src/lib.rs:577`). *verified*
- Resume step cites "§3.6 step 15" meaning 01's (`07:405`).
- Audit reason names "which of the two bounds" elapsed; only one bound expires (`07:95, 696`).
- `in_fulfillment` policy authoring said to return `not-admissible`; it's a schema CHECK (`07:769-770` vs `07:568-570`).
- "One dwell input" claim vs draft sweep using `created_at` (`07:612-613` vs `07:798`).

### 08-read-and-authz
- Acceptance read relies on a wrapper 05 never mentions; unbounded multi-record response not paged (`08:581-582, 189-190`; `05:252`).
- List algorithm validates filters after PDP; wrapper validates before (`08:689` vs `08:585, 588`).
- Amend split justified by "create, submit and cancel only" while matrix grants Direct Customer PATCH/acceptance (`08:1089-1090, 947, 951`).
- D-41 cited for page size; D-41 is throughput baselines (`08:193`; `DECISIONS.md:479`). *verified*
- §4.5 cites §4.3 for page size; values are in §2.2 (`08:1300`). *verified*
- Not-found-instead-of-forbidden deviation routed for AC-21 only; AC-13/AC-16 need it too (`08:869-875`; `PRD.md:983`).

---

## Coverage notes
- Each slice agent ran all three lenses itself, so within-slice agreement between independent
  lenses is not available; the two merged items are cross-slice agreements.
- Lens C was most productive in 06 and 08 (5 each); lens B dominated overall.
- Not covered: 01-foundation (reviewed separately), DESIGN.md, DECISIONS.md, UPSTREAM_REQS.md,
  ADRs as primary targets.
