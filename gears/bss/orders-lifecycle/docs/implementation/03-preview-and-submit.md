# Stage 3 — Preview, shared assessment and commercial submit

Status: implementation plan, not delivered capability. Reviewed 2026-10-05 against the complete
[gate feature](../features/03-gate-and-pin.md), [PRD](../PRD.md), [upstream register](../UPSTREAM_REQS.md)
and decisions D-187–D-200. Stage 3 realizes
`cpt-cf-bss-orders-lifecycle-feature-gate-and-pin`; it does not implement a second transition engine,
subscription activation, order approval policy or monetary evaluator inside Orders.

S1-04 now supplies [local commercial codec and contract evidence](COMMERCIAL.md). Reuse `orders-lifecycle-sdk::commercial` for schema-2 projection and exact term conversion; proposed producer profiles/APIs and real-provider conformance remain open. In particular, full native binding equality is required because current Pricing digests omit meter metadata.

## Entry conditions and file ownership

S1-02/04/05 supply reviewed operation, commercial and owner contracts. S2-01–08 supply the
runtime scaffold, authorization, engine, execution fencing, migrations, audit, claims and outbox;
S2-09/10 supply authored drafts and calendar-preserving date preparation. S2-11 supplies recovery
and retention worker infrastructure. S4-03/04 reuse assessment for amendments; S4-07/08 supply
consent. S5-05–08 supply receiver/Workflow activation; S6-01/04 supply authorized coherent reads
and disclosure logging. No numbered stage is an all-or-nothing predecessor: early owner tracks
S4-02 and S5-06a start from S1 contracts and feed S3 before their later end-to-end flows.

Provider ownership is singular: S4-02 implements payer/profile/delegation/Contracts providers and
shared identity/Contracts adapters; S5-06a implements canonical key/occupancy reads. S3-06 adapts
those outputs into gate predicates and shares their existing adapters. S6-08 owns future reference
release coordination/providers; S3-13 integrates its gate/attempt contribution and handoff tests.
Neither a contract nor a local double closes real-provider readiness.

All new code paths below are **proposed**, relative to repository root:

- `gears/bss/orders-lifecycle/orders-lifecycle/src/domain/gate/`: ports, request/results, predicate registry and orchestration.
- `gears/bss/orders-lifecycle/orders-lifecycle/src/infra/gate/`: owning-SDK adapters and bounded calls.
- `gears/bss/orders-lifecycle/orders-lifecycle/src/api/`: Preview/submit SDK and REST adapters; use the S1 layout if its final naming differs.
- `gears/bss/orders-lifecycle/orders-lifecycle/src/infra/storage/`: S2-owned migration/repository extensions for diagnostics, selected receipts and totals.
- `gears/bss/orders-lifecycle/orders-lifecycle/tests/`: focused gate/Preview/submit and real-provider seam tests.

Coordinate edits to S2-owned engine/migrations rather than creating competing writers. Upstream
packages below own their provider code; Orders imports public SDKs only. Follow [ToolKit SDK/ClientHub](../../../../../docs/toolkit_unified_system/03_clienthub_and_plugins.md),
[DB patterns](../../../../../docs/toolkit_unified_system/11_database_patterns.md),
[testing](../../../../../docs/toolkit_unified_system/12_unit_testing.md) and
[E2E boundaries](../../../../../docs/toolkit_unified_system/13_e2e_testing.md). Read dependency and
security guidelines before adding crates or changing security boundaries. SecureConn scopes come
from PDP, domain types use the required domain macro, and REST uses authenticated OperationBuilder
and canonical Problem responses.

## Review findings that must be resolved before their dependent package

| Finding | Evidence / consequence | Required resolution and owner |
|---|---|---|
| S3-F01: timing table predates new ports | Feature §3.3 / DESIGN port budgets name nine lanes, but assessment, BillingTerms, policy discovery and durable acceptance introduce additional calls. The 2.25/2.5-second sums do not prove end-to-end latency. | S3-01 inventories the complete dependency graph; S3-09 records measured budgets and asks Architecture/Product to ratify missing allocations (Q-11/Q-26). Do not reset deadlines per chunk or invent a new SLA. |
| S3-F02: obsolete read descriptions | DESIGN budget prose says REST-only and “until resolve echoes” SKU flags. D-187 has typed reads; D-189, not command availability, is the interim-SKU-read retirement trigger. | S3-01 narrows these statements to D-187/D-189. Existing `current_revision` uses the current server clock and can catch up publications; it is not an as-of-time pure database read. |
| S3-F03: active-only overlap summaries are stale | Feature predicates/recheck show activeCount plus proposed; D-198 adds admitted-pending capacity and reservation identity deduplication. | S3-06 adopts D-198's owner answer and scope unchanged, updates fixture expectations and feature summaries. Never count an already reserved line twice or fabricate resource-scoped counts from a legacy payer-scoped answer. |
| S3-F04: roster completeness is not proven by current SDK | `ResolvedBindings.cells` lacks an independent expected-roster attestation; absence can be mistaken for omitted work. | S3-03 defines producer profile/expected coverage and tests truncation; adapter cannot infer complete coverage from returned length alone. Required absent evidence remains unavailable. |
| S3-F05: terms/assessment dependency must avoid a cycle | Terms needs selected contexts; term-dependent assessment needs resolved terms. | S3-03/S3-04 use authorized read bindings to obtain context, then resolve terms and run complete assessment. Compare final binding identity/digest; changed binding requires a fresh execution, not mixing two snapshots. Independent checks can run earlier. |
| S3-F06: incomplete Preview versus monetary horizon | Preview promises other figures while withholding TCV; D-197 requires declared monetary basis and complete required figures. An incomplete term cannot support a guessed finite horizon. | Rating S3-07 must declare and test supported per-cycle display outputs for incomplete Preview. If full remaining required figures cannot be supplied, record the incompatibility as a contract blocker; do not claim success or silently widen the withholding exception. |
| S3-F07: tax owner is absent | PRD/Preview requires per-line and total indicative tax, but repository supplies no owning provider/specification. | S3-08 produces an explicit owner contract and implementation package. Full Preview remains unavailable until delivered; reduced Preview would require a separate product decision. |
| S3-F08: pricing quantity and older authored item quantity differ | D-192 requires exact `NewSaleQuery.quantity`; historical line/item descriptions must not imply an extra multiplication. Q-03 still contains broader quantity policy questions. | S3-01 records the supported purchase mapping; S3-07 refuses unsupported shapes. Do not resolve commercial caps or new quantity policy locally. |
| S3-F09: ordinary guard ordering does not authorize a command | Feature high-level submit order is abbreviated; D-188 overrides engine preparation and claim ordering. | S3-12 loads an existing frozen attempt before new resolution, reserves before first command and uses its fence in both success/refusal paths. No remote command in generic read preparation or SQL transaction. |
| S3-F10: “no worker” is feature-local | Gate has no private scheduler, but Preview retention and unresolved attempts require existing lifecycle workers. | S3-10 integrates S2-11 retention; S3-12 integrates S2-05/11 recovery. Add no gate-owned relay, timer or unbounded purge. |

The selected decisions govern these narrow reconciliations. Unspecified tax ownership, policy
precedence/values, budgets and unsupported purchase mappings remain named contract tasks. This
plan does not silently settle those policies.

## Ordered work packages

### S3-01 — Freeze the implementable contract/profile inventory

**Depends on:** S1-02/04/05 public contracts; coordinate S2-04 contribution types. Can begin before runtime work.
**Scope:** existing Orders feature/DESIGN/UPSTREAM_REQS clarifications for S3-F01–F09; proposed
`domain/gate/ports.rs`, `models.rs`, and a contract-fixture manifest under `tests/gate_contracts/`.
**Normative:** [interfaces](../DESIGN.md#contract-03-3-3), [read mapping](../DESIGN.md#contract-03-pricing-read-mapping),
[diagnostic profile](../DESIGN.md#contract-03-diagnostic-mapping), [owner readiness](../DESIGN.md#contract-05-commercial-owner-readiness).

1. Enumerate every operation, owning SDK, actual/missing implementation, authorization axis,
   request identity, outputs, dependencies and error categories. Distinguish absence, refusal,
   denial, transient failure and malformed output; canonical codes come from S1-02/S2-04's registry.
2. Define versioned Orders ports for proposed assessment, policy, terms, Rating, identity/profile,
   Contracts, key/occupancy and tax. Missing providers return typed unavailability, never pass.
3. Record expected predicate/selection coverage independent of response contents, and exact
   supported selection/quantity/term mapping. Keep unresolved facts marked blocked.
4. Reconcile the findings above in a small reviewable documentation/DTO change before dependent
   implementation. Profile/version changes must be visible, not accidental JSON compatibility.

**Tests/done:** compile public boundaries against existing SDKs; mapping fixtures cover unknown
schema, wrong line/revision, absent required fields and selected NULL versus literal `default`.
A reviewer can identify every missing provider and its implementing package. No fake provider is
registered in a production composition root.

### S3-02 — Integrate existing Pricing/Products reads and service authorization

**Depends on:** S3-01, S1-03 capability evidence and S2-03 buyer/PDP and service credential wiring.
**Scope:** proposed `infra/gate/pricing_read.rs`, `products.rs`, `authority.rs`, provider-integration tests;
existing provider grant configuration. Do not import Pricing/Products internal repositories.
**Normative:** [D-187 mapping](../DESIGN.md#contract-03-pricing-read-mapping),
[D-194 authority](../DESIGN.md#contract-08-commercial-service-authorization), catalog [predicate obligations](../DESIGN.md#contract-03-4-1).

1. Call existing `PricingReadV1` with seller CatalogRef and authenticated seller-scoped service
   context; separately verify caller resource/payer authority before foreign-party resolution.
2. Fix assessment UTC date and plan/revision/selection per line. Reject current-revision mismatch
   without selecting a successor. Keep scheduled/catch-up clock behavior explicit.
3. Map resolve cells, uncovered selections, defaults and required invoice/meter evidence exactly;
   successful price-by-ID (including Cancelled history) never proves sellability.
4. Read Products SKU lifecycle/sellable under its own grant until S3-03 supplies agreed owner
   evidence. No Orders reference reservation; no privileged Pricing identity impersonation.

**Tests/done:** real Pricing→Products access, foreign CatalogRef and revoked grants; published,
superseded/scheduled, missing/denied revision; selected/unselected uncovered cells; null default;
SKU deprecated versus retired; Cancelled price read; UTC rollover/catch-up. Verify authorized
catch-up is allowed but no acceptance/hold appears. Every unsupported mandatory fact fails closed.

**Existing precedents:** [provider](../../../pricing/pricing/src/api/pricing_read.rs),
[read tests](../../../pricing/pricing/tests/pricing_read_sdk.rs),
[Products retirement boundary](../../../products/products/src/domain/approvals/retire.rs).

### S3-03 — Implement Pricing's missing assessment and complete diagnostics producer

**Depends on:** S3-01; reuse S3-02 fixtures. May progress alongside S3-04–08.
**Scope:** proposed `gears/bss/pricing/pricing-sdk/src/assessment.rs`, Pricing API/provider registration,
shared fact/evaluator code alongside existing `domain/commercial_terms.rs`, and provider tests.
**Normative:** [D-189 nonbinding assessment](../DESIGN.md#contract-03-nonbinding-assessment),
[D-195 profile/results](../DESIGN.md#contract-03-diagnostic-mapping).

1. Publish the versioned SafeRead basket query/result and independent coverage profile. Accept
   basket-local keys, optional Preview terms, exact revision/selection context; require no order,
   receipt, version or command key. Add scoped read permission and provider registration.
2. Extract/share Pricing-owned rule evaluation with acceptance without changing command semantics.
   Run every independent applicable check; preserve missing-input dependencies and tri-state
   verdicts instead of expanding a first error into fabricated passes.
3. Return exact proposed binding identity/digest and typed reason/evidence/profile provenance.
   Provide the authoritative roster/coverage evidence current reads lack.
4. Keep authoritative acceptance revalidation; assessment is neither reservation nor validity promise.

**Tests/done:** unchanged complete input has parity on overlapping rules with acceptance; multiple
failures plus outage retain all independent results; read-only principal succeeds without acceptance
grant; no receipts/holds/command audits created, including incomplete Preview. A changed catalog
between assessment and acceptance can refuse. Truncation, wrong profile and forged scope refuse.
Existing first-error [commercial evaluator](../../../pricing/pricing/src/domain/commercial_terms.rs)
is a refactoring starting point, not a complete assessment implementation.

### S3-04 — Implement Subscriptions' terms resolver and policy provenance

**Depends on:** S1-04, S3-01 and S3-02 selected read contexts; coordinate a minimal shared Subscriptions SDK/provider scaffold with S5-01/S5-06a. Full S3-03 assessment and S5-05 receiver implementation are not prerequisites.
**Scope:** proposed `gears/bss/subscriptions/subscriptions-sdk/src/billing_terms.rs`, provider/domain
resolver and immutable policy adapter in its new runtime crate; proposed Orders `infra/gate/terms.rs`.
**Normative:** [D-193](../DESIGN.md#contract-03-billing-terms-resolution), [accepted snapshot](../DESIGN.md#contract-03-frozen-commercial-snapshot).

1. Specify/implement the authoritative seller-policy source, version read, precedence and grants;
   record unresolved precedence as a blocker. Do not assume Settings Service already stores it.
2. Resolve complete public Pricing BillingTerms plus Term, earliest start and truthful provenance
   without creating subscription/order identities. Verify public canonical digest byte equality.
3. Preserve authored calendar intent: rolling must be explicit; exact positive month/year period
   conversion only, no 30-day months. Enforce supported UTC anchors, recurring cycles and hourly
   usage alignment; never move an anchor on retry.
4. Map optional incomplete Preview distinctly from required acceptance inputs. Freeze the complete
   result in S3-12; owner-supported delayed-activation geometry is tested with Rating/S5-07.

**Tests/done:** P1Y6M→18 months, annual divisibility, day/time remainders, missing versus rolling,
zero/u32 overflow, leap/month-end/anchor fixtures, incompatible cycles, foreign policy/invalid
provenance, changed policy on retry and no subscription creation. Missing provider remains a
blocking named dependency; policy-derived values cannot be labeled ExplicitOrder.

### S3-05 — Implement Pricing policy discovery and accepted deadline mapping

**Depends on:** S3-01, S3-02 authorization; independent of full Orders commit.
**Scope:** proposed Pricing SDK/provider policy read, owner policy storage/config/version adapter,
and Orders `infra/gate/acceptance_policy.rs`; existing acceptance command keeps issuing deadlines.
**Normative:** [D-191](../DESIGN.md#contract-03-commercial-deadline), [D-188](../DESIGN.md#contract-01-commercial-attempt).

1. Expose authorized policy discovery with immutable positive version and supported seller scope.
   Current deployment-wide policy/default is not seller-specific policy capability.
2. Define policy lookup/version applicability and deployment migration before claiming per-seller
   durations. Missing required policy differs from unavailable/denied/malformed evidence.
3. Freeze hold-policy version in each acceptance query; validate receipt identity and copy issued
   hold_until verbatim. Optional Preview forecast absence may only withhold that forecast.

**Tests/done:** boundary equality/expiry, changed policy, unsupported version, unknown seller,
replay with unchanged deadline and multi-line earliest constraint; no local Orders deadline math.
Clock advances or another command key cannot extend accepted validity.

### S3-06 — Integrate authoritative payer/Contracts and key/occupancy answers

**Depends on:** S3-01, S4-02 owner providers/shared adapters and S5-06a canonical key/occupancy
provider; these early tracks do not depend on successful S3 submit. Missing-provider responses
permit fail-closed local development, not production success.
**Scope:** proposed Orders `infra/gate/{identity,contracts,overlap}.rs` for gate-facing mapping;
reuse S4-02 `infra/adapters/{identity,contracts}.rs` rather than a second SDK adapter/provider.
**Normative:** [D-199 readiness](../DESIGN.md#contract-05-commercial-owner-readiness),
[nine Orders predicates](../features/03-gate-and-pin.md#contract-03-4-2),
[D-198 receiver/count semantics](../DESIGN.md#contract-06-activation-admission), UPSTREAM_REQS §2.1/2.4/2.11.

1. Consume S4-02's authoritative payer currency/region/seller relationship and provenance;
   never derive market from buyer input or generic untyped metadata. Keep tenant validity separate
   from Contracts party eligibility and PDP-verified delegation.
2. Batch referenced-contract status/eligibility/effective/version/declaration through the shared
   adapter. Missing optional reference is not-applicable; provider outage is not.
3. Consume S5-06a's canonical key, scope/provenance, active/admitted-pending counts, limit and
   reservation identities. Never use plan ID as key or double-count reserved proposed lines.
   Until target resource scope is adopted, retain the producer's legacy scope and D-180 interim
   cross-order addend unchanged. Validate full key coverage; missing limits do not default to one.
4. Preserve the exact answered keys for diagnostics/claims/committed lines and later recheck.
   Subscription admission/locking is S5-06b; an advisory read-pass cannot close that release gate.

**Tests/done:** actual S4-02/S5-06a SDKs drive foreign-axis refusal, profile outage versus divergence,
inactive/ineligible/missing contract, no key versus outage, missing count/limit and coverage,
legacy partner scope without disclosure, reservation deduplication and same-order exclusion.
S4-08/10 owns live acceptance-declaration guards; S5-08 owns activation recheck. S3-06 is complete
only with adapter evidence against those providers; it does not implement or claim their delivery.

### S3-07 — Implement Rating exact-binding purchase evaluation

**Depends on:** S3-01, S3-04 supported terms; S3-03 fixed bindings/profile; S3-06 market/context.
**Scope:** new proposed `gears/bss/rating/rating-sdk/src/purchase_evaluation.rs`, Rating runtime
provider/pure evaluator and fixture corpus; Orders `infra/gate/rating.rs`, S2-02 totals encoding.
**Normative:** [D-197](../DESIGN.md#contract-03-rating-purchase-evaluation),
[total](../DESIGN.md#contract-03-4-4), [exclusions](../DESIGN.md#contract-03-4-5), Rating PRD pre-purchase requirement.

1. Implement declared supported money models and exact-binding input/profile; no successor lookup,
   fabricated subscription/cohort, FX or current SKU refresh. Unsupported models fail explicitly.
2. Rating computes every monetary output, minimum fee, discount, rounding, minor-unit conversion
   and aggregation. Return item/line/order gross/net/discount, promotion status, three kinds,
   cycle breakdown and complete currency/scale/policy evidence.
3. Return one TCV and finite/rolling-annualized/mixed basis. Usage amounts are uncommitted/null,
   not zero; only the designated order/recurring carrier holds TCV. Aggregate recurring money
   carries producer amount_basis/period_evidence; Orders neither converts nor sums it.
4. Resolve S3-F06 explicitly and preserve only the accepted Preview withholding exception. Tax
   stays outside Rating; no posting or evaluation state is created by purchase evaluation.

**Tests/done:** finite/rolling month/year and mixed terms, one-time/usage-only, fractional quantity,
minimum fees, scale/rounding/overflow, supported no-discount versus unavailable, unsupported
scope/model, malformed aggregate/identity/digest, and incomplete Preview. Shared accepted-input
fixtures compare later billing with explicitly different activation/proration geometry; quotes
are never billing inputs. Rating presently has docs only: this package includes implementation,
not merely an Orders test double.

### S3-08 — Specify and implement indicative tax owner/provider

**Depends on:** S3-01, S3-07 complete pre-tax figures and authoritative party scope.
**Scope:** first produce the owning billing/tax contract and provider-location decision; then new
owner SDK/runtime/provider and proposed Orders `infra/gate/indicative_tax.rs`. No existing owner
path can honestly be named as delivered; the package must record its chosen path before coding.
**Normative:** [Preview](../features/03-gate-and-pin.md#contract-03-4-6),
[exclusions](../DESIGN.md#contract-03-4-5), `cpt-cf-bss-orders-lifecycle-upreq-indicative-tax-read`.

1. Assign producer ownership and define authenticated batch input, jurisdiction/evidence source,
   supported scope, per-line/order amount output, provenance, currency/scale, unavailable semantics
   and budget. Orders must not infer tax from Pricing tax-category metadata or calculate it.
2. Implement nonbinding read provider and grants; ensure no invoice or tax liability is created.
3. Integrate only into Preview after evaluation; never store tax in orders, totals, diagnostics or
   attempt evidence. Missing tax uses `indicative-tax-unavailable`, not zero or omitted success.

**Tests/done:** supported basket, foreign-party denial, no-tax explicitly answered versus missing,
outage/malformed amount/identity, and no persistence. An approved owner contract plus real provider
is required for complete Preview; a schema proposal alone does not finish this package.

### S3-09 — Build bounded shared assessment orchestration and predicate mapping

**Depends on:** S3-02–08 contracts; missing-provider adapters allow fail-closed development meanwhile.
**Scope:** proposed `domain/gate/assessment.rs`, `predicates.rs`, `diagnostics.rs`, `infra/gate/budget.rs`;
shared platform breaker integration/delivery if no suitable existing facility exists.
**Normative:** [constraints](../DESIGN.md#contract-03-2-2), [D-195](../DESIGN.md#contract-03-diagnostic-mapping),
[feature assessment/budget algorithms](../features/03-gate-and-pin.md#31-assess-all-predicates-and-pin-outcomes).

1. Execute one dependency graph for Preview/submit/amendment: authority/date basis → identity and
   revision/selection context → terms/owner assessment → exact-binding Rating → Preview tax;
   authoritative keys → occupancy. Parallelize independent checks only.
2. Implement all eight adopted catalog obligations, nine Orders predicates and each line's pin
   outcome using their owner evidence. Preserve all evaluable failures/passes; absent prerequisite
   creates unevaluable dependent results, never suppresses independent work or duplicates failures.
3. Map versioned producer profiles losslessly. Preserve item and raw nullable selection identity,
   coverage expectation, mapping version, applicability and allowlisted subresults. Sort by the
   declared binary/NULL ordering. Blocking unavailable wins primary error over blocking failed;
   optional TCV/forecast exceptions remain narrow and nonblocking only where explicitly allowed.
4. Enforce deadlines across queueing/chunks/retries, maximum two transient attempts inside the
   original deadline, global bulkhead 32, at most 200 subrequests and maximum 8 concurrent
   per assessment under the same port deadline. Retain
   baseline breaker 0.5/30 seconds/open 10 seconds and caller limits 10/min submit, 60/min Preview.
   Discover/reuse shared platform facilities; deliver a shared missing breaker capability with
   its own review rather than inventing a private gate breaker. Ratify new port allocations from F01.

**Tests/done:** randomized upstream order yields identical vector, multiple independent failures,
missing prerequisite, wrong coverage/profile, selected NULL/text default, no duplicated pin/date
error, binding changed before Rating, 200-line batching, transient retry/deadline/backpressure and
cancellation. “One logical call” may split existing single-item SDK operations inside the common
budget; do not assert one physical request to a non-batch API.

### S3-10 — Persist complete authorized diagnostics and retention

**Depends on:** S2-02/03/04/05 secure storage/settlement, S3-09 vector and S2-11 cleanup lifecycle.
**Scope:** S2-02 migration/repository extensions for `orders_gate_outcome`, proposed gate diagnostic
writer and current-grant operational inspection integration; no new public diagnostic endpoint.
**Normative:** [diagnostic schema](../DESIGN.md#contract-03-table-orders_gate_outcome),
[D-195 mapping](../DESIGN.md#contract-03-diagnostic-mapping).

1. Persist complete run metadata/vector atomically, including passed and pin outcomes; enforce
   NULLS NOT DISTINCT uniqueness for order/line/item/selection identities and reason constraints.
2. Submit/amendment attach authorized vector/failures/mapping version to immutable settled
   response inside engine transaction; engine-only/stale-date/version refusals discard provisional
   results. Authoritative claim collision replaces advisory predicate 9 before settlement.
3. Preview commits standalone diagnostic rows before returning assessmentId. Each call has new
   identity, even identical baskets; no idempotency marker or order/attempt. Unauthorized requests
   cannot persist caller-supplied commercial metadata.
4. Integrate bounded seven-day Preview purge using indexed evaluated_at/NULL-order filter and
   existing lifecycle cleanup. Preserve order-linked diagnostics and immutable replay snapshots.
   Operational inspection requires current grants and subject scope, not possession of run ID.

**Tests/done:** two components/slots, null identities, concurrent identical baskets, atomic write
failure (no claimed durable ID), cross-tenant denial, retention boundary and settled replay after
profile upgrade. PostgreSQL direct tests verify null-distinctness/index/check semantics; SQLite
alone cannot prove them. Inspect stored JSON to prove no total, TCV, tax, bearer token or raw
unredacted upstream body entered diagnostic evidence.

### S3-11 — Expose Preview and verify read-only commercial effects

**Depends on:** S3-09/S3-10; full success requires real S3-03–08 providers.
**Scope:** proposed SDK Preview DTO and authenticated `POST /bss-orders-lifecycle/v1/orders/preview`
handler, OpenAPI/GTS response types, small HTTP/PDP seam test.
**Normative:** [Preview sequence](../features/03-gate-and-pin.md#contract-03-preview-a-basket),
[Preview invariant](../features/03-gate-and-pin.md#contract-03-4-6), [D-189](../DESIGN.md#contract-03-nonbinding-assessment).

1. Authorize represented payer/resource axes, allowed selling relationship and rate limit before
   lookups. Seller-only role is insufficient; independent buyer authority must be complete.
2. Allocate stable run-local line IDs and call shared composition. Return ordered diagnostics,
   received money/exclusions/tax, expected fulfillment time and per-line deferral as specified.
3. Missing term/cycle omits TCV with the exact successful `tcvWithheld` reason/line IDs, without
   fabricating other required figures. Return no approval verdict, quote validity or issued deadline.
4. Reject diagnostic persistence failure; return only scoped evidence and canonical Problems.

**Tests/done:** real-provider table-delta/spy proof of no command/check/hold, attempt, order, pin
or stored money; catalog catch-up explicitly allowed. Repeat runs differ, no foreign-party leaks,
no seller-only access, absent term/cycle and unavailable tax/provider behave distinctly. Preview
is not a quote; Q-29 remains open and this endpoint cannot close it with stored totals.

### S3-12 — Integrate durable acceptance and atomic submit

**Depends on:** S2-02–07/11 D-188 engine/reservation/recovery, S2-09/10 draft/dates, S3-09/S3-10 and required real providers; S4-07/08a submit-consent contribution and S2-08 producer. Build the S4-08a pure consent contribution alongside submit; its standalone acceptance endpoint/history tests do not block this package.
**Scope:** proposed `domain/gate/submit.rs`, `infra/gate/acceptance.rs`, S2-04/05 engine integration,
receipt/total encoders and commit tests. Engine remains sole writer of attempts and commercial rows.
**Normative:** [submit sequence](../features/03-gate-and-pin.md#contract-03-submit-through-the-gate),
[D-188](../DESIGN.md#contract-01-commercial-attempt), [D-192](../DESIGN.md#contract-03-frozen-commercial-snapshot),
[D-191](../DESIGN.md#contract-03-commercial-deadline), [D-200](../DESIGN.md#contract-01-event-platform-integration).

1. Authorize and resolve settled replay/existing execution before fresh assessment. Existing
   attempts load frozen input/evaluation, preserving caller/service identity; no policy re-resolution.
2. New successful assessment contributes immutable exact queries/evaluation; engine atomically
   claims execution and reserves never-reused candidate version before first Pricing command.
   No placeholder commercial version or overlap claim is created during reservation. Enforce
   200 acquisition lines and 200 consumed items per order, 1,000 selected receipt bindings per
   line before deduplication, and 1 MiB for complete serialized schema-2 immutable-version content.
   Required diagnostic evidence cannot be silently dropped to fit. Coordinate the S2-08 64 KiB
   event limit and worst-case terminal-event capacity reservation for the admitted roster; never
   discover an unpublishable completion only after activation.
3. Outside SQL locks invoke existing SellabilityV1::check under stable per-attempt/line keys.
   Persist receipt results only under current generation/fence. Recover lost replies with exact
   query replay. Do not implement pretend cross-service rollback or call hold from Orders.
4. Verify complete receipt/query/binding/digest equality with evaluated intent, original terms,
   service subject and policy version; copy finite issued deadline unchanged. Freeze schema-2 pin.
5. Final engine transaction rechecks current access, state/base/draft/date basis, ownership/fence,
   deadlines, capacity and authoritative claim collision in normative precedence. Success appends
   candidate C with actual previous committed version, complete pins/totals/market/dates/claims,
   diagnostics/audit/settled response and OrderSubmitted enqueue; Wake only after commit.
6. Self-service automatic consent uses submitting principal tenant==resource and no proof reference,
   never sales_path alone. Failed commercial admission leaves no version/pin/total/event, while
   burned allocation/operational evidence and external receipts may survive. Refusal and replay
   respect their own execution marker instead of falsely rejecting the live owner.

**Tests/done:** accept line A then fail B; lost reply; crash before/after receipt; stale recovery
worker; revoked authority; same key altered payload; fresh key after failure; 24-hour key reuse;
checked version exhaustion and sparse history; UTC midnight/draft change; expired receipt; wrong
receipt/evaluation digest; concurrent overlap claim; rollback has no event; success emits once.
Add boundary fixtures for count/byte limits using production serializers and maximum terminal
event mappings, not estimates or truncated pins. Actual Pricing [acceptance transaction tests](../../../pricing/pricing/tests/acceptance_transaction/mod.rs)
and [receipt tests](../../../pricing/pricing/tests/acceptance_receipts.rs) provide replay/uniqueness
precedents. Use PostgreSQL for locking, unique/FK and transaction races; fake-only tests do not
close remote/local commit separation.

### S3-13 — Integrate reference lifetime and activation-recheck handoff

**Depends on:** S3-12, S5-04–08 Workflow/Subscriptions interfaces and S6-01/04 authorized reads; reference-release contribution depends on S6-08 and stays dormant while release is disabled.
**Scope:** gate/attempt admission-closure integration and contract fixtures; reuse S6-08
Orders usage provider and Pricing/Subscriptions release coordination rather than duplicate them;
Workflow recheck implementation is owned by S5-08; S3 supplies its gate contract fixtures.
**Normative:** [D-196](../DESIGN.md#contract-03-revision-reference-protection),
[D-190](../DESIGN.md#contract-03-accepted-price-activation), [D-198](../DESIGN.md#contract-06-activation-admission),
[recheck sequence](../features/03-gate-and-pin.md#contract-03-re-check-before-first-activation).

1. Integrate S6-08 authoritative scoped usage including potentially committable preparations, selected
   nonterminal versions and uncertain handoffs. Final commit honors durable closure/generation.
   Pricing remains sole Products reference owner; current deferred release remains disabled until
   close/drain/proof and gap-free Subscriptions handoff are implemented and tested.
2. Expose frozen market/key/receipt/version facts through S6-01/04; Workflow executes recheck after
   begin-fulfillment, using stored keys and current profile/occupancy. No new Lifecycle endpoint.
3. Test proceed/reject/not-dispatchable/defer and three attempts/≤60-second retry baseline;
   hold/supersession interrupts retries, rejection/exhaustion follows S5-09/10 compensation semantics.
   No dispatch during defer; spawn signal must commit first. Receiver slot admission and fencing,
   not these reads, authorize effects; post-active collision is fulfillment failure, not rejection.

**Tests/done:** close admission racing new/resumed attempts, unknown usage retains references,
no protection gap on handoff; market drift/outage; pending-count dedup; hold during backoff;
exact expiry; pre-/post-active collision. Production submit/activation remains gated on delivered
receiver enforcement; a count endpoint alone cannot satisfy it.

### S3-14 — Observability and real-provider completion evidence

**Depends on:** all enabled-path packages; S2-08/S6-05 producer/consumer integration and S5-16 receiver evidence; join S6-06 release metrics.
**Scope:** proposed gear metrics/config/runbook and focused tests in `testing/e2e/cross_gear/`;
provider revision/grant/profile readiness manifest.
**Normative:** [deployment signals](../DESIGN.md#contract-03-3-8), feature DoDs/ACs,
[owner readiness](../DESIGN.md#contract-05-commercial-owner-readiness).

1. Instrument port latency/deadline/retry/breaker/bulkhead, refusal origin/predicate, unevaluable
   and pin-unresolvable rates, missing occupancy fields, seller PEP denial and recheck outcomes/
   exhaustion. Keep high-cardinality tenant diagnostics scoped; never expose seller facts publicly.
2. Wire breaker-open, configured unevaluable-threshold and nonzero-pin-unresolvable alerts;
   threshold policy must be explicit. Measure resolution, commercial preparation, commit,
   broker acceptance and receiver effects separately; do not assert subsecond total from DB p95.
3. Record real deployed providers/grants/profile versions and evidence per prerequisite. Missing
   tax blocks complete Preview; missing receiver enforcement blocks production submit/activation;
   missing required authority blocks its affected path. Development doubles stay isolated.

**Tests/done:** fault injection exercises each alert and public redaction; real SDK/provider
integration proves binding/evidence mapping; a minimal live HTTP/PostgreSQL test proves actual
route/authz/serialization and canonical Problem behavior. Do not repeat pure predicate cases
through HTTP. Run relevant workspace format/lint/unit and targeted integration checks according
to the scaffold's actual crate names; record commands/results, with unavailable test environments
reported rather than called passing.

## First milestone and stage exit

**First reviewable milestone:** S3-01/S3-02 plus the S3-09 fail-closed skeleton and S3-10 diagnostics.
Demonstrate a real seller-authorized Pricing read, complete scoped diagnostics, and explicit
unavailable outcomes for missing producers. It may be merged as internal development capability;
it is not full Preview or production submit and must not call acceptance to make the demo pass.

The first complete user-visible vertical slice is S3-11 Preview only after all of its required
providers, including tax, exist. Submit follows S3-12 with durable attempt/receipt recovery and
S5-06b/07/16 receiver release gates. Amendments reuse these operations through S4 rather than forking them.

Stage completion requires each enabled path's provider packages and tests above, exact replay,
full diagnostic/pin/total atomicity, correct no-commercial-effect Preview behavior, scoped
permissions, measured budgets and documented unresolved policy decisions. A checkbox is not
closed by a mock, a DTO, or a design amendment alone.

## Coverage register

| Source contracts / requirement IDs | Packages |
|---|---|
| Feature `flow-gate-and-pin-submit`, `seq-gate-submit`; PRD `fr-order-submit`, `nfr-order-snapshot-integrity` | S3-01/02/09/10/12 |
| Feature `flow-gate-and-pin-preview`, `seq-gate-preview`; Preview contract 03 §4.6 | S3-03/04/07/08/10/11 |
| Feature `flow-gate-and-pin-activation-recheck`, `seq-gate-fulfillment-recheck`; `fr-order-atomic-fulfillment` | S3-06/13; S5-05–08 own receiver/Workflow implementation |
| `algo-gate-and-pin-assessment`, `fixed-catalog`, `port-budgets`, `diagnostics` | S3-01–10 |
| `state-gate-and-pin-admission`, all three `dod-gate-and-pin-*`, complete feature §6 ACs | S3-09–14 |
| DESIGN 03 §3.1 models, §3.2 components, §3.3 ports, §3.7 diagnostics, §3.8 signals | S3-01/09/10/14 |
| DESIGN 03 §4.1 adopted predicates; feature 03 §4.2 nine deltas; §4.3 pin; §4.4 total; §4.5 exclusions | S3-02–09/12 |
| D-187/189/195 reads/assessment/diagnostics; D-193 terms; D-191 policy/deadline | S3-02–05/09/10 |
| D-188 attempts; D-192 schema 2; D-194 identity; D-197 Rating | S3-02/04/07/12 |
| D-190/196/198 receipt activation/reference lifetime/capacity; D-199 authorities; D-200 producer | S3-06/12/13; S2-08/S4/S5/S6-08 counterparts |
| UPREQ Pricing read/catalog grant/purchase assessment/acceptance policy; Rating evaluation/pre-subscription/TCV; terms; payer/Contracts; key/occupancy; indicative tax | S3-02–08 |
| `fr-order-tenant-axes`, `fr-orders-boundary-r4-no-price`, `nfr-order-transition-latency`, `interface-order-ops` | S3-02/06/07/09/11/12/14 |

Full requirement IDs in the feature use prefix `cpt-cf-bss-orders-lifecycle-`; abbreviated IDs here
retain that prefix by reference. All feature sections, detailed algorithms and appended R01–R12
reconciliations were reviewed. Material boundaries intentionally remain elsewhere: foundation
engine/outbox/recovery/capture/calendar intent (S2), amendments/consent/payment/approval guards
(S4), actual slot/OSS/hold/compensation and Workflow (S5), retention worker framework (S2-11),
and read/diagnostic authorization surfaces (S6-01–04). Tax owner selection, new-port budget
ratification, policy precedence and unsupported purchase shape decisions remain explicit unresolved
inputs to the implementing packages, not omissions hidden behind provider placeholders.

### Exact upstream requirement-to-package map

Each suffix below expands under `cpt-cf-bss-orders-lifecycle-upreq-` in [UPSTREAM_REQS](../UPSTREAM_REQS.md).

| Requirement suffix | Package / delivery boundary |
|---|---|
| `pricing-read-sdk`, `pricing-catalog-tenant-reads`, `products-sku-read-grant`, `commercial-service-provisioning` | S3-02; S1-03 contract proof and S2-03 credential/grant implementation |
| `pricing-purchase-assessment`, `pricing-bundle-sellability` | S3-03/09 |
| `pre-subscription-billing-terms` | S3-04; shared early Subscriptions scaffold coordinated with S5-06a |
| `pricing-acceptance-policy` | S3-05/12 |
| `payer-commercial-profile`, `delegation-proof-credential`, `pdp-policy-integration` | S4-02 providers; S3-06/S3-02 adapters; S2-03 PDP and S4-08/10 live guards |
| `contract-party-eligibility`, `contract-acceptance-declaration` | S4-02 providers; S3-06 gate mapping and S4-08/10 guard-time reads |
| `catalog-subscription-product-key`, `overlap-presence-read` | S5-06a producer; S3-06/13 integration |
| `rating-evaluation`, `pre-subscription-evaluation`, `tcv-with-annualisation` | S3-07 |
| `indicative-tax-read` | S3-08/11 |
| `sku-protection`, `initial-binding-acceptance`, `overlap-activation-atomicity` | S3-13 integration; S6-08 reference release and S5-06b/07 hold/admission remain owner packages |
| `event-broker-runtime`, `event-consumer-conformance`, `event-delivery-observability` | S3-12/14 integration evidence; S2-08/S6-05 own complete D-200 delivery |
