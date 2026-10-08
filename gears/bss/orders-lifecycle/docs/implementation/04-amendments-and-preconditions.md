# Stage 4 — Amendments, administrative corrections, customer consent and money guards

Status: implementation plan, not delivered runtime. Complete packages in order unless their dependencies explicitly permit parallel work. This stage covers **all behavior in features 04 and 05**, including upstream work needed to make those paths usable. For local amendment/consent implementation, Foundation, Capture and Assessment packages supply the engine, authenticated operation wrapper, sparse-version attempts, field classifier, date preparation, claims, diagnostics, commercial snapshots and transaction outbox. **Provider contract work S4-02 and S4-09 starts after Stage 1 interface/security foundations and runs alongside Stage 2/3; Stage 3 consumes those owner deliverables where required.** Stage numbering is not a requirement to wait for Stage 3 completion before starting its upstream dependencies. Do not build parallel versions of shared facilities.

## Agent entry instructions

Read [guidelines entry point](../../../../../guidelines/README.md), [ToolKit entry point](../../../../../docs/toolkit_unified_system/README.md), [gear layout](../../../../../docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md), [REST wiring](../../../../../docs/toolkit_unified_system/04_rest_operation_builder.md), [secure authorization](../../../../../docs/toolkit_unified_system/06_authn_authz_secure_orm.md), [database patterns](../../../../../docs/toolkit_unified_system/11_database_patterns.md) and [testing](../../../../../docs/toolkit_unified_system/12_unit_testing.md). Read [dependency guidance](../../../../../guidelines/DEPENDENCIES.md) before Cargo changes. The README's `guidelines/DNA/languages/RUST.md` link is absent in the inspected checkout; record that broken guidance link and use current ToolKit/project conventions rather than inventing its contents.

Read both [feature 04 in full](../features/04-versioning.md) and [feature 05 in full](../features/05-preconditions.md), their linked schema sections, [PRD](../PRD.md), [decision register](../DECISIONS.md) and [upstream register](../UPSTREAM_REQS.md). D-188 through D-200 refine earlier sections. A historical design question does not override a later selected decision.

All Rust scopes below are **proposed paths**, relative to `gears/bss/orders-lifecycle/`; the implementation crates do not exist on the inspected baseline. Reuse the actual Foundation layout if already created. Use `orders-lifecycle-sdk/src/` for public transport-independent types and `orders-lifecycle/src/{domain,infra,api/rest}/` for implementation. Do not expose peer implementation crates through dependencies. Handlers validate/authorize and call the engine; slices return contributions, never write aggregate state independently. Use scoped DB runners and `PolicyEnforcer`, not handcrafted `AccessScope`; no SQL locks span peer calls. Domain models use project macros, REST uses authenticated `OperationBuilder` and canonical Problems. No customer-facing implementation switches or fake provider success.

Each package must finish with changed paths, source contract IDs, focused commands/results, remaining provider dependencies and evidence distinguishing doubles from real providers. Tests listed below are required runtime behavior tests, not tests for this Markdown file. Run relevant unit/contract tests, PostgreSQL migration/concurrency tests, formatting, checks and repository lint for changed crates; obtain crate names from their actual manifests. Do not mark a production gate complete from fixture-only tests.

## Decisions and execution boundaries

- D-188: failed Pricing acceptance preparations burn candidate numbers. `M > N`, not `M = N + 1`; actual predecessor is N. Failed candidates do not increment the commercial amendment count. Replays reuse frozen attempts and service identity.
- D-192/D-197: a new amendment re-assesses all proposed lines and freezes newly accepted exact inputs and Rating results. No old receipt, total or TCV is carried as current evidence. Do not interpret feature 04's older “accepted deadline then Rating” prose as changing the selected assessment → frozen attempt → acceptance → final commit order.
- D-174: two-step reapproval is selected. Amendment commits `submitted`, emits `OrderAmended`, and Workflow obtains the new-version verdict afterward. Lifecycle must not query the approval owner.
- D-199: Pricing acceptance, customer consent and policy approval are three independent facts. A Ledger settlement is not payment authorization. Tolerance applies only to a conclusive failed authorization, never pending, outage or missing provider.
- D-198 receiver fencing belongs to the fulfillment stage; passing the guards here does not authorize arbitrary receiver effects or waive admission fencing.

## Source and coverage map

| Full feature area | Normative source / IDs | Packages |
|---|---|---|
| Amendment flow, guard ordering and claim replacement | [04 §3.6](../features/04-versioning.md#contract-04-3-6); `…-flow-versioning-amend`, `…-algo-versioning-append`, `…-seq-amend-order` | S4-01, 02, 03, 04 |
| Admissibility, cap, delta and tenant axes | [04 §4.1](../features/04-versioning.md#contract-04-4-1); `…-state-versioning-amendment`, `…-constraint-no-amendment-in-fulfillment`, `…-constraint-paired-payer-seller-rebinding` | S4-01–04 |
| Carry-forward/re-resolution and approval supersession | [04 §4.2–4.4](../features/04-versioning.md#contract-04-4-2); `…-constraint-reapproval-target-external`, `…-seq-amendment-supersession` | S4-03, 04, 09 |
| Administrative edits and history | [04 §4.5–4.6](../features/04-versioning.md#contract-04-4-5); `…-flow-versioning-admin-history`, `…-algo-versioning-admin-edit`, `…-algo-versioning-history` | S4-05, 06 |
| Requirement policy and both consent paths | [05 §3.6](../features/05-preconditions.md#contract-05-3-6), [05 §4.1](../features/05-preconditions.md#contract-05-4-1), [05 §4.2](../features/05-preconditions.md#contract-05-4-2); `…-flow-preconditions-submit-acceptance`, `…-flow-preconditions-record-acceptance`, `…-algo-preconditions-requirement`, `…-algo-preconditions-record` | S4-01, 07, 08 |
| Money guards and durable pending continuation | [05 §4.3](../features/05-preconditions.md#contract-05-4-3); `…-flow-preconditions-begin`, `…-algo-preconditions-evaluate`, `…-algo-preconditions-pending-continuation`, `…-seq-begin-fulfillment-guards` | S4-09, 10, 11 |
| Persistence, delivery, observability and product limits | [04 persistence](../DESIGN.md#contract-04-3-7), [05 persistence](../DESIGN.md#contract-05-3-7), [05 limitations](../DESIGN.md#contract-05-4-4); four feature DoDs | S4-01, 06, 07, 11, 12 |

`…-` in this file expands to `cpt-cf-bss-orders-lifecycle-`. Concrete contracts, rather than approximate prose or source line numbers, are the acceptance authority.

## Existing gears: reuse evidence, not missing business capabilities

| Inspected source | What to reuse | What it does not supply |
|---|---|---|
| [AccountManagementClient](../../../../system/account-management/account-management-sdk/src/client.rs) | SecurityContext on every operation, ClientHub SDK boundary, scoped tenant reads and distinction between missing metadata and failed resolution | A typed payer currency/region/named-seller commercial relationship schema, or Orders delegation policy |
| [ApprovalSourceV1](../../../approvals/approvals-sdk/src/lib.rs) | Inbox forwarding to source-owned authorized doors; votes preserve source responses | An Orders approval requirement/routing owner. Closed kinds currently cover Pricing/Products; do not add an Order kind without its owner protocol |
| [Pricing approval tests](../../../pricing/pricing/tests/approval_doors.rs) | Content drift invalidates earlier generation votes; replay and permission cases | Orders commercial version numbers are not Pricing approval generations; copy the invariant, not the schema |
| [Pricing receipt migration](../../../pricing/pricing/src/infra/storage/migrations/m20260930_000019_commercial_receipts.rs) | Physical identity separate from business-key uniqueness; explicit constraints and backend-specific migration definitions | Customer consent. Pricing receipt identity cannot populate `orders_acceptance` |
| [Pricing acceptance transaction tests](../../../pricing/pricing/tests/acceptance_transaction/mod.rs) and [event integration](../../../pricing/pricing/src/infra/events.rs) | Exact business-key replay, immutable input mismatch and post-commit outbox wake pattern | Orders distributed rollback; already-issued Pricing receipts survive an Orders refusal |
| [LedgerClientV1](../../../ledger/ledger-sdk/src/api.rs) | Scoped canonical API and idempotent settlement/allocation | `settle_payment` records money already received. It is not authorize/read-by-request and must not satisfy begin-fulfillment |

## S4-01 — Reconcile implementer-facing contracts and register guards

**Dependencies:** Foundation transition registry and Capture field classifications. **Scope:** feature04/05, relevant DESIGN/PRD/DECISIONS/UPSTREAM_REQS sections; proposed `domain/versioning.rs`, `domain/preconditions.rs`, SDK request/outcome definitions and operation registry. This package may correct documentation but must not change selected business scope.

1. Reconcile feature04's stale Q-12-open wording and question-register status with D-174: two-step reapproval is the implementation target; the reciprocal Workflow PRD amendment remains a delivery task. Keep Q-28 seller-transfer and Q-30 customer-access limitations explicit. Clarify Q-41's trigger: create is event-less, submit emits `OrderSubmitted`, actual amendment alone emits `OrderAmended`.
2. Make sparse submitted-version lookup explicit. Creator is version 1, but the submitted role version is the committed version whose reason is `submit`, not hardcoded 2. An amendment role version is its actual expected committed version. This is required by recording-party checks.
3. Reconcile the nullable policy business key with a valid physical schema in S4-07. Retain current logical election semantics. Record this as a representation erratum, not a new policy or user approval gate.
4. Register the full ordered amendment guards from feature04, the acceptance guards with **already-recorded before recording-party**, and money guards. Mark precluded inputs separately from unavailable inputs. Bind all reasons to canonical error categories and response schemas; `authorization-failed-tolerated` is an admission risk, not a refusal reason.
5. Inventory routes/DTO fields: amendments POST, existing order/line PATCH, version list/get and acceptance POST/GET. Reject unknown authored keys at boundary; missing/unparseable expected version is boundary `expected-version-required`; missing/bad amendment reason is the engine guard `amendment-reason-invalid`. Actor/consent instant are never caller-authored.

**Exit:** registry completeness tests fail on missing field classes/guards/reasons; boundary versus engine-refusal tests demonstrate correct audit/idempotency behavior; source errata recorded with no unresolved instruction to use consecutive versions.

## S4-02 — Deliver commercial owner contracts needed by amendments and consent

**Dependencies:** Stage 1 SDK/authorization foundations and agreed D-194 caller model; start before Stage 3 completion and co-design its commercial port contracts. Stage 3 submit/assessment consumes the payer/profile/delegation/Contracts deliverables; S4 consumes the same ports. **Scope:** Orders `infra/adapters/{identity,contracts}.rs` and conformance fixtures; provider work in the named owner SDK/implementation, separately assigned to its owner. [D-199](../DESIGN.md#contract-05-commercial-owner-readiness) is the readiness contract.

**Owner package AM:** deliver `…-upreq-payer-commercial-profile`. Agree a versioned typed response or authoritative documented metadata schema for payer currency, region and relationship to the requested seller, with identity/provenance/freshness and missing/denied/unavailable taxonomy. Implement PEP-scoped reads and grants; obtain current AM through its SDK, never inspect owner storage or infer a commercial relationship from tenant ancestry. The displayed `payer_profile(...)` in UPSTREAM_REQS is proposed, not an existing callable API. One amendment assessment resolves it once and shares it between payer guard and market evaluation.

**Owner package Contracts:** deliver `…-upreq-contract-party-eligibility` and `…-upreq-contract-acceptance-declaration`: referenced contract status, eligible parties/reason, effective instant/version and current acceptance-required declaration. Agree batched semantics, completeness, grants and authoritative missing/outage outcomes. The provider and SDK are missing; add no made-up adapter call that silently succeeds. Record requirement source at consent, but reread declaration at each later guard.

**Owner package identity/PDP:** deliver `…-upreq-delegation-proof-credential` and `…-upreq-pdp-policy-integration`: issuer/audience/delegate/scope/expiry/revocation and missing/invalid reasons, original actor/proposed resource-payer authorization. PDP verifies delegation. Seller-scoped service credentials do not prove buyer authority.

**Tests/exit:** real providers distinguish legitimate optional-contract absence from unavailable referenced contract; foreign seller/resource/payer cannot leak facts; changed same-seller payer is accepted only with confirmed relationship; outage returns `identity-party-unavailable`, not cross-seller denial. Contract-required changes and invalid/revoked proof fail appropriately. Doubles unblock local packages but these paths remain production-gated until provider evidence is recorded.

## S4-03 — Prepare an amendment through the shared assessment and attempt pipeline

**Dependencies:** S4-01; Stage 3 working D-188/D-192/D-193/D-195/D-197 pipeline; S4-02 doubles for development, real providers for release. **Scope:** `domain/versioning.rs`, existing assessment orchestration, amendment DTO/handler, SDK amendment request; no standalone version writer.

1. Authorize current parent and proposed relationship before foreign-party reads. Authorized settled replay returns without any external resolution. Resumable D-188 execution loads frozen attempt inputs; only a genuinely new execution prepares a fresh assessment.
2. Snapshot current committed N; apply nonempty delta to carried-forward authored commercial content, preserving unnamed fields and stable retained line IDs. Validate all named fields through Capture's single classifier. Drop no retained line merely because absent from delta.
3. Apply guards in order: finite amendment cap (20 baseline), delta, reason length 1–4096, administrative-field rejection (including mixed deltas), immutable seller/resource axes, payer seller-scope relationship, shared structural guards (category, currency, 200-line cap), prepared date cascade, complete gate. State admissibility precedes these. A failed early local guard precludes dependent external work; do not turn skipped work into an outage.
4. Prepare policy/date basis before date-dependent calls; fix revision identity per line. Re-assess **every** proposed line, including untouched ones. Recompute market, exact selected-binding/terms snapshots, totals and annualized TCV. Preserve all reached diagnostics. An unavailable pin contributes unevaluable once, not duplicate failures.
5. Use D-188 allocation and frozen query/Rating evidence before remote acceptance. Verify issued receipt matches the frozen inputs and store Pricing's issued deadline verbatim. No legacy inherited pin or previous receipt becomes acceptance for M; no assumption that Rating runs against a future not-yet-issued receipt.
6. Contribute the full distinct proposed `(payer, resource, overlap_scope_key)` set; a changed payer changes tuples even when textual keys remain equal. The engine will perform authoritative claim replacement. Keep buyer consent and approval verdict absent for M.

**Tests/exit:** delta inheritance fixture with add/remove/retained lines; mixed admin/frozen-axis precedence; identity dependency called once per new run; early cap skips upstream calls; UTC basis rollover refuses stale prepared facts; failed attempt leaves old version byte-identical and candidate burned; exact retry makes no fresh terms/market/Rating resolution. Stage 3 immutable evidence conformance remains a dependency rather than duplicated test implementation.

## S4-04 — Commit amendment and supersession atomically

**Dependencies:** S4-03 and Foundation transaction/claims/idempotency/outbox. **Scope:** engine contribution handling, version/line/claim repositories, amendment event DTO and focused PostgreSQL integration tests.

1. Under engine lock/fence order, recheck current N, actor/proposed authority, state, attempt ownership/generation, dates and immutable receipt/evaluation evidence. Reject stale work; never rebase silently. Admit only `submitted`, `pending_approval`, `approved`.
2. Acquire the complete replacement claim set with Foundation's deterministic ordering. If partial collision occurs, release only exact newly returned claim IDs, retain all old claims, persist released reservation history with the refused audit/idempotency outcome. If cleanup count or storage fails, rollback instead of committing a misleading refusal.
3. Append M with `supersedes_version=N`, complete new commercial content and receipts/totals, reason `amendment`, explanation in version `amendment_reason` and audit `caller_reason`. Increment amendment_count once only on successful append, move current pointer and claims, settle the response and enqueue exactly one `OrderAmended` in the same transaction. Fire outbox Wake after commit.
4. Target `submitted` in all three cases. Only actual state change updates `state_entered_at`; submitted→submitted does not. No approval-owner read and no copied consent/verdict. Workflow gets the event after commit.
5. Stale Workflow result naming N must fail `version-conflict` with current M before Workflow-class state admissibility, even though submitted has no matching approval/ack row. Never transform stale approval into a denial.

**Tests/exit:** PostgreSQL races amendment/amendment, amendment/approval and amendment/consent with two connections; one winner and consistent pointer/predecessor/claims. Candidate holes 1→4→7, 20 successful amendments vs many failed attempts, claim collision after one new reservation, injected cleanup failure, lost commit response/exact replay, rollback before outbox commit. Snapshot old rows and assert unchanged contents. No receipt-only attempt can appear in public history.

## S4-05 — Administrative order and line edits

**Dependencies:** S4-01, Foundation audit and Capture PATCH trigger selection. Can run beside S4-03/04 in disjoint files. **Scope:** existing Capture PATCH adapter, `domain/administrative_edit.rs`, admin repositories and audit contribution; reuse classifier and engine.

1. Empty/unknown-field request fails boundary validation. Any commercial field routes to draft-mutate; outside draft it refuses by Capture rules. Admin-only reaches this contribution and requires nonterminal/current expected version.
2. For line edits check membership first: draft working lines in draft, current committed lines afterward. Then field-class defense, then changed-value check under the engine's locked read. All-equal request refuses `administrative-edit-unchanged` as audited/settled guard failure.
3. Write only admin tables. For each changed field append one audit entry with actual locked prior and new values; line field name is `lines/<line_id>/<field>`. Entries are consecutive in contiguous audit sequence; settle with last entry. Unchanged fields emit no entry.
4. Leave commercial version, candidate high-water, draft commercial revision, consent, approval, pins and event count unchanged. Same-field concurrent edits are deliberately last-write-wins; no `admin_revision` token.

**Tests/exit:** two changed and one unchanged fields yield exactly two audit entries; simultaneous different-key same-field edits reconstruct the actual value chain; missing/removed line and terminal state refusal; same-key replay has no second audit; stale expected commercial version rejects despite admin LWW. Keep administrative audit visibility restricted separately from ordinary customer history.

## S4-06 — Version and acceptance-history read projections

**Dependencies:** S4-04, Stage read/authz wrapper; acceptance response extension after S4-08. **Scope:** `infra/storage/repo/versions.rs`, read projection, history DTO/route and common authorization/access-log integration.

Use current-parent authorization **before** disclosing a requested version exists. Read full stored commercial versions directly by order/version, no event or predecessor-chain reconstruction. Cursor-page actual committed rows; gaps do not create empty placeholder pages or fabricated versions. Return actor, engine timestamp, `create|submit|amendment`, explanation and explicit predecessor. A reserved/failed candidate yields authorized `version-not-found`. Enforce retention and append-only grants; repair/archive cannot rewrite historical rows.

Acceptance GET uses `order × read`, returns current version and its acceptance/absence plus bounded prior records; it does not expose internal command recovery evidence. Do not apply a historical payer authorization scope instead of current-parent access. Customer version history permission does not confer audit-trail permission.

**Tests/exit:** current parent moves payer scope and former unauthorized reader loses all versions without existence leakage; sparse pages and missing version; receipt evidence remains exact across replay/read; access logs and p95 <200 ms at realistic chain depth. Boundary page-size/cursor tests reuse read stage. No unbounded per-version upstream reads.

## S4-07 — Policy election and consent persistence

**Dependencies:** Foundation schema/roles and S4-01. **Scope:** Orders migration/entity/repository for `orders_policy_election`, acceptance constraints and risk timestamp; deployment policy channel; no public policy-write endpoint.

Implement [05 schema](../DESIGN.md#contract-05-3-7) and [acceptance table](../DESIGN.md#contract-01-table-orders_acceptance). `orders_acceptance` key/FK is `(order_id,accepted_version)` referencing committed versions; accepted_at is nonnull with **no default/backfill**. Grant only engine insert/select, no update/delete or slice writer. Source vocabulary is contract/seller/platform_default/volunteered.

**Concrete schema erratum:** a PostgreSQL primary key cannot contain nullable platform scope_id. Use an internal nonnull UUID `policy_id` primary key, with `UNIQUE NULLS NOT DISTINCT(election,scope,scope_id)` for the logical key and a CHECK requiring NULL for platform / nonnull for seller. Preserve the logical key in domain lookups. For SQLite test support use separate unique partial indexes for platform `(election)` and seller `(election,scope_id)` plus the same scope CHECK; do not emulate with an all-zero tenant sentinel. Document the physical representation in DESIGN with the migration. Pricing's receipt migration above demonstrates identity separated from a business unique index; this policy null handling still requires its own PostgreSQL tests.

Promote policy rows through the existing deployment channel with `elected_by/elected_at`. No seller REST mutation or new PDP write action. Runtime policy reads must distinguish a successful query returning no row from failed I/O. Consent requirement precedence is live referenced contract, seller, platform, truly-unset required=true; tolerate failure is seller, platform, truly-unset=false. Preserve recorded source after changes; it is evidence of the recording decision, not a cached future policy.

**Tests/exit:** migrations up/down on supported engines; duplicate platform row fails despite NULL, seller/platform coexist, bad scope-null shape fails; default cannot manufacture consent; denied update/delete fails under actual runtime role. Promotion metadata and safe-unset vs outage cases. Required missing provider is unavailable, never fallback. Policy reads/freshness follow engine input checking; do not promise cross-service atomicity.

## S4-08 — Customer consent on submit and separate recording

**Dependencies:** S4-08a (pure submit-consent contribution) needs S4-07, S1-02 engine interfaces and live Contracts adapter S4-02; it feeds S3-12 without depending on completed submit. S4-08b (standalone recording and integrated history) additionally uses S4-06 and completed S3-12. **Scope:** `domain/preconditions.rs`, acceptance repository, acceptance DTO/handler, submit contribution and two event projections.

1. Self-service **submit request** records consent only if allowed request carries no delegation-proof reference and trusted subject tenant equals current resource tenant. Do not infer from creation-time sales_path. Contribute consent on the actually committed sparse submit version in the same engine transaction; publish only `OrderSubmitted`, carrying accepted_version/instant. No second transition/event.
2. For POST acceptance authorize resource-tenant membership and acceptance-record permission. Engine checks state/version. Resolve live requirement and source; false means record `volunteered`, not refusal. Optional policy does not prevent consent recording.
3. Load creator, actual committed submit version, and expected amendment role records. For each independently, actor is barred if identities match and either stored sales_path is partner_placed or that role's actor tenant differs from resource tenant. A resource-tenant creator is not barred solely because a different delegated submitter acted. Seller-role authority alone is insufficient.
4. Prepare duplicate and recording-party evidence; engine evaluates already-recorded before party bar. Use engine timestamp t and trusted actor; never client date/time, line acceptance due date, Pricing receipt or historical approval as assent. Separate recording_path copies immutable stored sales_path; automatic submit path is self_service by construction.
5. Insert consent/audit/idempotency and one `OrderAcceptanceRecorded` atomically; neither state nor commercial version changes. Different-key duplicates refuse; same-key replay returns exact original result. Amendment starts M with no consent on both sales paths. Admin/state-only transitions preserve consent to unchanged version.

**Tests/exit:** complete actor/path matrix, delegation introduced after creation, proof on own-tenant request suppresses auto-consent, eligible original customer can reaccept amendment, older consent cannot satisfy M, already-recorded+barred precedence, concurrent amendment/consent and duplicate recording, rollback/event uniqueness, caller timestamp rejected. Own-tenant membership without explicit endpoint grant fails. Q-30 onboarding remains real release dependency for partner scope; no operator-attested workaround.

## S4-09 — Workflow approval and Payments owner packages

**Dependencies:** Stage 1 public contracts/security and S4-02 owner security contracts; provider design/implementation starts now, in parallel with Stage 3. S4-04 amendment event and Stage fulfillment/Workflow implementation are dependencies only for integrated event/concurrency tests, not for beginning provider delivery. **Scope:** sibling owner docs/SDK/providers after owner assignment; Orders only test contracts and adapter-owned seam types. Interfaces in D-199/UPSTREAM_REQS are proposed work, not callable providers.

**Workflow approval package:** implement `…-upreq-workflow-amendment-verdict` with order/current version/policy authority identity, new-version requirement lookup, routing and immutable reflected verdict. Consume D-186 events with dedup and current authorized reads; cancel/supersede prior-version work, obtain M's verdict and reflect submitted→pending_approval or approved. Do not derive from N, query approval owner inside Lifecycle amendment, or treat BSS Approvals inbox availability as policy implementation. If a UI inbox adapter is needed, first deliver the order owner, then extend closed SDK kinds under its own compatibility work. Two-step park keeps submitted TTL running; escalate before expiry.

**Payments package:** implement `…-upreq-authorization-outcome` including idempotent authorize and read-by-request. Agree request_ref binding to Workflow intent/order/version/payer/currency and authoritative amount or explicit postpaid policy; TCV is never the amount. Define supported states, provider provenance/freshness, unknown outcomes, conflict/replacement rules, grants and audit. Ledger `settle_payment`/balance or platform admission policy cannot replace this authority. Lifecycle consumes Workflow's D-175 adapter-owned authorized/failed input, not a newly invented provider receipt or direct Payments call.

**Tests/exit:** real approval owner unavailable vs required/not-required; late N approval rejected after M; duplicate OrderAmended cannot create duplicate gate. Payment lost authorize response rereads same request, changed same-key payload refuses, denial/outage/pending differ from conclusive failed; no new authorization per poll. Confirm provider amount provenance independently of totals display. Deliver canonical error taxonomy and cross-tenant negatives before enabling affected production paths.

## S4-10 — Begin-fulfillment guard contribution

**Dependencies:** S4-07/08a and S1-05 Workflow seam signatures, not completed S5-04; S4-09 real providers for production. **Scope:** `domain/preconditions.rs`, begin-fulfillment contribution and risk audit; operation/state mutation stays owned by feature06.

Resolve live referenced-contract requirement and local election inputs before transaction. Unavailable requirement produces `acceptance-requirement-unevaluable` through engine refusal settlement; do not read safe fallback on outage. Revalidate current version and acceptance under lock. Required + missing current consent refuses `acceptance-required-not-recorded` before payment branches. Authorized admits; defensive pending refuses `authorization-pending`; conclusive failed uses seller/platform tolerance, unset false, otherwise `authorization-failed`. Only tolerated failure contributes permanent `authorization_failure_tolerated_at=t` and risk audit on admitted begin-fulfillment. Keep machine transition reason begin-fulfillment; do not replace with the risk label. No payment token, instrument or authoritative authorization-outcome column on Orders. No twelfth payment_pending state.

**Tests/exit:** required/optional consent × authorized/pending/failed × seller/platform/unset/outage matrix; unknown provider failure never routed through failed tolerance; previously volunteered assent remains a real recorded consent if live requirement later becomes true; policy change does not rewrite recorded source; risk timestamp written atomically, never cleared. Failed precondition leaves approved and no receiver dispatch. Stage D-198 fencing remains required after guard admission.

## S4-11 — Durable pending payment continuation in Workflow

**Dependencies:** S4-09 Payments read-by-request and selected Workflow execution infrastructure (Q-10); S4-10 outcomes. **Scope:** Workflow-owned process/checkpoint/timer/manual-task implementation and conformance fixtures; **no Lifecycle scheduler or payment port**.

Checkpoint request identity, order/version, process correlation, first-pending instant, next-check deadline and remaining budgets before relying on a timer. Poll the same request. Positive finite pending interval/elapsed budget is required before enabling this path. Outage consumes dependency retry budget without becoming payment failure. Deduplicate timer deliveries and recover overdue checkpoints after restart. Before each wake and before forwarding, reread Orders: terminal/superseded stops the continuation, hold suspends while preserving budgets, only matching current approved may forward conclusive authorized/failed. Workflow never reads/caches seller tolerance and must forward conclusive failure once other prerequisites hold. Pending does not call begin-fulfillment; defensive Lifecycle pending refusal is only protection against bad callers.

On exhaustion create one correlated manual task/alert; do not authorize, decline or fabricate cancellation. Escalation must work without buyer-acceptance events and independently of optional TTL settings; where TTL exists, escalate before expiry. Preserve current deadline/budget on hold/resume. Recover unknown authorization after restart before issuing a replacement request. Use the selected durable engine, not Pricing's cancellation-aware process worker as a substitute for durable timers.

**Tests/exit:** pending→authorized without any acceptance event; repeated pending→one escalation; network uncertainty before/after provider commit; duplicate timer; restart; hold/resume; amendment while asleep; terminalization; conclusive failed forwarded and handled only by Lifecycle. Real providers and durable engine required for signoff; Q-10 platform choice remains explicit dependency.

## S4-12 — Integrated evidence, operations and production scope

**Dependencies:** all previous packages and Stage read/event/Workflow integration. **Scope:** Orders integration/conformance tests, shared `gears/bss/fixtures` corpus where appropriate, owner readiness register and deployment runbook.

Run the complete scenario: submit actual candidate 4 with Pricing receipt, customer consent independently recorded, Workflow approval; amend to candidate 7, lose a Pricing response and recover frozen inputs, invalidate old assent/verdict, reject late old approval, get fresh current consent/verdict, process pending payment across restart, then admit current begin-fulfillment. Verify each version/event/audit/claim and that receipt-only attempts never become consent or activation authority. Add the refused path for unavailable referenced contract, denied proposed payer, failed-not-tolerated payment and exact replay.

Instrument amendment depth and cap refusals, assessment failures, stale Workflow results, amended submitted dwell against reflection lead time, administrative edit audits, separate authorized/pending/failed observations, tolerated-admission risk count, consent latency against due date, party-barred and requirement-unevaluable rates. Workflow owns payment dependency outage/pending age alerts; Lifecycle has no provider port. Test configured alerts with failure injection, not counters alone. Keep consent operations p95 <1 s and historical reads p95 <200 ms under realistic chain size; use the project's declared measurement boundary.

Record each provider SDK revision, implementation/configuration, commercial scope, principal/grants, evidence identity/freshness, integration run and operation enabled under D-199. Do not convert all gates to passed because local doubles work. Production scope is **provision-then-collect only**: checkout capture, SCA, alternate-instrument reauthorization UI, refund/chargeback and credit scoring are not implemented here. Declined authorization remains approved with manual remediation and bounded expiry; there is no new payment state. Partner scope requires a customer acceptance surface or an explicitly promoted commercial acceptance-not-required election; no automatic scope downgrade.

**Final acceptance:** close `…-dod-versioning-amendment`, `…-dod-versioning-history`, `…-dod-preconditions-acceptance`, `…-dod-preconditions-money-gate` only with their source acceptance criteria and real-owner evidence. Report Q-28 cross-seller transfer, Q-30 onboarding, payment-owner and durable-execution delivery limits accurately; never claim that approved documentation or existing Ledger/Approvals crates close them.


## Coverage and handoff register

| Contract group | Concrete requirement IDs | Executable package / handoff |
|---|---|---|
| Feature04 complete flows/processes/states/DoDs/ACs and §3.6/§4.1–4.6 | `…-fr-order-amendment`, `…-fr-order-history`, `…-fr-order-tenant-axes`, `…-fr-order-idempotency`; audit/retention/snapshot-integrity/read-latency NFRs | S4-01–06, S4-12; Stage read wrapper owns common pagination/access-log implementation |
| Feature05 complete flows/processes/states/DoDs/ACs and §3.6/§4.1–4.3 | acceptance record, policy-election table; `…-constraint-no-payment-pending-state`, `…-constraint-declined-instrument-exit`, `…-constraint-no-payment-collection` | S4-07–12; Stage hold/expiry owns expiry sweeps, not payment timers |
| Feature06 requirement/verdict reflection and begin-fulfillment composition | [06 approval contract](../DESIGN.md#contract-06-4-2), [Workflow upstream register](../UPSTREAM_REQS.md#26-orders-workflow); `…-upreq-workflow-amendment-verdict` | S4-04/09 stale-verdict and two-step contract; S4-10 guards; fulfillment stage owns full seam/receiver state transitions |
| Market and delegation facts needed already by Stage 3 | `…-upreq-payer-commercial-profile`, `…-upreq-delegation-proof-credential`, `…-upreq-pdp-policy-integration`, `…-upreq-commercial-service-provisioning` | S4-02 starts after S1; consumed by S3 and S4. Stage security/provisioning work remains shared |
| Contracts live policy and party status | `…-upreq-contract-party-eligibility`, `…-upreq-contract-acceptance-declaration` | S4-02 owner SDK/provider; S3 gate, S4-07/08/10 guard-time consumers |
| Payment authority and durable recovery | `…-upreq-authorization-outcome`; Workflow pending-continuation requirement in feature05 §4.3 | S4-09 provider starts after S1; S4-11 Workflow continuation, S4-10 Lifecycle guard, no Lifecycle timer |
| Event delivery / cross-owner effects | `…-upreq-event-consumer-conformance`, [D-200](../DESIGN.md#contract-01-event-platform-integration) | S4-04/08 produce through shared outbox; S4-09/12 consumer fixtures; event/platform stage owns global schema/readiness/recovery wiring |

The stage's four DoDs remain unchecked until both local runtime and applicable real-provider handoffs pass. A subagent assigned one package must name all blocked handoffs explicitly rather than silently replacing them with fixtures or requesting a broader scope change.
