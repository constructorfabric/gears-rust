# Orders High-register disposition — 2026-09-23

Source: [Orders review register](https://artifacts.os.jele.io/orders-design#register), all
33 entries marked HIGH, checked against the current design and supplied platform code.
Scope: documentation remediation. No Orders runtime, migration or platform implementation is
delivered by these changes. Earlier review snapshots are historical, not the current contract.

## Disposition

“Corrected” means the local design defect has a concrete replacement contract. “Prerequisite”
means the local contract is specified but end-to-end behavior still depends on another owner;
it must not be reported as implemented or production-ready.

| Finding | Disposition and authoritative location |
|---------|----------------------------------------|
| OL-2 | Corrected: release exact newly inserted reservation IDs on partial claim failure; preserve pre-existing claims, commit refusal; Foundation §3.6/3.7, ADR-0007 |
| OL-3 | Corrected: acquire, compare and release full payer/key tuples; Foundation §3.6/3.7 |
| OL-4 | Corrected: separate draft revision, coherent prepared snapshot and locked recheck on success/failure paths; Foundation §3.6, Capture §4.1 |
| OL-8 | Corrected: common authoritative idempotency gate before unavailable-input settlement; Foundation §3.6/4.2 |
| OL-9 | Corrected: conflict-safe claim and atomic expired-lease reclaim under row lock; Foundation §3.6/4.2 |
| OL-10 | Corrected: expiry key binds observed generation, dwell and effective/platform policy revisions; Hold/Expiry §3.6 |
| OL-13 | Corrected: requested identity separate from nullable resolved aggregate fields/FK; Foundation §3.6/3.7, D-98/D-104 |
| OL-17 | Removed by platform reuse: no Orders event sequence/counter/table; managed ProducerOutbox owns sequencing; Foundation §3.7 |
| OL-19 | Corrected: managed Chained producer metadata provides broker idempotency; event ID is consumer deduplication only; Foundation §4.4 |
| OL-20 | Corrected: all eleven events remain in one uninterrupted Markdown table; Foundation §4.4 |
| OL-21 | Corrected: actual event base, data field, supported traits, subject partitioning and required envelope members; Foundation §4.4/4.7. Shared guideline correction remains upstream |
| OL-22 | Corrected: canonical Problem type plus domain/code; GTS reasons are registry-derived types, not custom wire type URIs; Foundation §4.7 |
| OL-26 | Corrected: acceptance PK/FK includes accepted_version; amendment never inherits old assent; both channels can renew; Foundation §3.7, Preconditions §4.2 |
| OL-27 | Corrected: buyer resource/payer authority differs from ownership of seller tenant; Gate §3.6/4.6, Read/Authz §4.3 |
| OL-32 | Corrected: removed claims about absent design-check scripts/CI; README and ADR confirmation sections distinguish design review from planned tests |
| OL-37 | Prerequisite: Workflow-owned durable bounded pending-authorization continuation specified; Payments read-by-request and Workflow execution platform still required; Preconditions §4.3, UPSTREAM_REQS §§2.5–2.6 |
| OL-38 | Corrected design choice: explicit platform-root broker tenant, three named PDP business axes with no designated tenant_col; Foundation §4.4/4.7, Read/Authz §4.3. Root identity/grants and deployed policy verification remain prerequisites |
| OL-39 | Removed by platform reuse: toolkit queue/dead-letter vacuum owns producer retention; no custom drain cleanup; Foundation §3.7/3.8 |
| OL-40 | Corrected: removed false Subscriptions reason-name collision; domain/code namespace rationale follows platform errors; D-85 |
| OL-45 | Corrected: state/version checks precede registered slice guards; handlers do not reorder commercial refusal precedence; Capture/Gate/Versioning §3.6 |
| OL-46 | Corrected: amendment carries the full proposed overlap set, including unchanged retained lines and proposed payer; Versioning §3.6 |
| OL-47 | Corrected: failed gate results become engine contributions for audit/settlement, not an early handler return; Gate/Versioning §3.6 |
| OL-51 | Prerequisite: actual Pricing tri-state outcomes and precise normalization documented; public batched fixed-version SDK operations remain missing; Gate §2.2/4.1, UPSTREAM_REQS §2.2 |
| OL-52 | Corrected contract: frontier/composition registered with deadlines, outcomes and budgets. Typed composition and shared breaker readiness remain prerequisites; Gate §§1.3, 2.2, 3.3 |
| OL-57 | Prerequisite: order cancellation closes at pre-dispatch spawn-signal commit; distinguish task cancellation and explicitly route Workflow PRD reconciliation; Workflow Seam §4.3, UPSTREAM_REQS §2.6 |
| OL-58 | Corrected: compare exact acknowledged set/multiplicity with stored current-version line roster, not the submitted projection; Workflow Seam §3.6/4.4 |
| OL-61 | Corrected ownership: Lifecycle alone evaluates tolerate-failure; Workflow submits conclusive failed outcomes. Upstream Workflow behavior must be reconciled; Preconditions §4.3, Workflow Seam §4.2 |
| OL-62 | Corrected: Orders-owned typed date-policy snapshot before transaction; persist switches/revision and validated dates, detect UTC rollover; Capture §4.2, Foundation §3.6 |
| OL-64 | Corrected: seller-only commercial edit/Preview removed; Orders re-drive already withdrawn; Read/Authz §4.3 |
| OL-65 | Corrected contract: explicit finite Workflow order-ID PDP scopes replace nonexistent correlation relationship; provider grant provisioning/revocation remains prerequisite; Read/Authz §4.3, UPSTREAM_REQS §2.9 |
| OL-68 | Corrected: held-fulfillment expiry is an explicit guard refusal, unlike absent in_fulfillment transition; Foundation §4.3, Hold/Expiry §4.3 |
| OL-69 | Corrected: full worker engine input, configured identity, observed version and guarded contribution; Hold/Expiry §3.6 |
| OL-70 | Corrected: exemption filter before LIMIT, keyset progress through refused candidates, bounded pages and explicit restart/backlog limits; Hold/Expiry §3.6 |

## Platform reuse and outstanding evidence

- Existing toolkit scoped transactions, locking select composition, insert-returning and scoped
  updates implement the concurrency primitives. No new raw database executor, savepoint API,
  distributed lease or scheduler checkpoint store is assumed.
- Pricing supplies the integration pattern: PolicyEnforcer/scoped persistence, separate mutable
  row versions, transactional local audit and actual tri-state predicate diagnostics. Its private
  modules are evidence, not imported Orders APIs or proof of deployed Orders policy support.
- Platform producer runtime, initial cursor retry fix, safe operator republication and delivery
  measurements remain the existing P-1 prerequisites in UPSTREAM_REQS §2.7.
- Production PDP policies and root-tenant broker grants need integration evidence. Pricing SDK,
  Payments and Workflow dependencies listed above need owner delivery. Product-owned TTLs and
  latency/cross-gear PRD decisions remain open; no arbitrary defaults close them.
- Race, rollback, restart, authorization and wire-mapping checks in the design are implementation
  acceptance requirements, not executed runtime tests or a claim of CI coverage.

## Validation

Validation completed: TOCs for the master, decisions, upstream register, all slices and ADRs;
local link targets across 20 current/handoff documents; eleven contiguous event-table rows;
33 distinct High finding IDs; targeted searches for superseded contracts; and `git diff --check`.
These are documentation checks, not runtime acceptance tests. No commit or external review
register update is implied by this local documentation handoff.
