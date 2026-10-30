<!-- CONFLUENCE_TITLE: [BSS]: Orders — Design Decisions Register -->
<!-- Related: ./DESIGN.md, ./features/, ./ADR/ | Owners: BSS Orders team -->

# Design Decisions — Orders

<!-- toc -->

- [How to use this document](#how-to-use-this-document)
- [Status board](#status-board)
- [A. Foundational shape](#a-foundational-shape)
  - [D-01 (H) The engine owns every state change *(autonomous)*](#d-01-h-the-engine-owns-every-state-change-autonomous)
  - [D-02 (M) Standard architecture, decomposition and feature layout](#d-02-m-standard-architecture-decomposition-and-feature-layout)
  - [D-03 (H) Foundation plus seven capability features](#d-03-h-foundation-plus-seven-capability-features)
  - [D-04 (H) The state machine is data, not control flow *(autonomous)*](#d-04-h-the-state-machine-is-data-not-control-flow-autonomous)
  - [D-05 (M) Refused attempts are audited *(autonomous, carries ADR-0005)*](#d-05-m-refused-attempts-are-audited-autonomous-carries-adr-0005)
- [B. Engine algorithm — resolves R-01…R-11](#b-engine-algorithm--resolves-r-01r-11)
  - [D-06 (H) Idempotency resolution precedes admissibility and the version check *(autonomous, fixes R-01)*](#d-06-h-idempotency-resolution-precedes-admissibility-and-the-version-check-autonomous-fixes-r-01)
  - [D-07 (H) The in-flight marker is upsert-and-reread *(autonomous, fixes R-02)*](#d-07-h-the-in-flight-marker-is-upsert-and-reread-autonomous-fixes-r-02)
  - [D-08 (H) Every refusal path audits, settles and commits *(autonomous, fixes R-03; carries ADR-0005)*](#d-08-h-every-refusal-path-audits-settles-and-commits-autonomous-fixes-r-03-carries-adr-0005)
  - [D-09 (H) Slice pre-checks become registered guards *(autonomous, fixes R-04)*](#d-09-h-slice-pre-checks-become-registered-guards-autonomous-fixes-r-04)
  - [D-10 (H) Slice writes become document contributions *(autonomous, fixes R-05)*](#d-10-h-slice-writes-become-document-contributions-autonomous-fixes-r-05)
  - [D-11 (H) Five transition rows are added *(autonomous, fixes R-07)*](#d-11-h-five-transition-rows-are-added-autonomous-fixes-r-07)
  - [D-12 (H) Rows disambiguated and the PRD's two `approved` edges restored *(autonomous, fixes R-08 and R-09)*](#d-12-h-rows-disambiguated-and-the-prds-two-approved-edges-restored-autonomous-fixes-r-08-and-r-09)
  - [D-13 (M) The spawn signal is permanent *(autonomous, fixes R-10)*](#d-13-m-the-spawn-signal-is-permanent-autonomous-fixes-r-10)
  - [D-14 (M) Draft auto-void targets `expired` and is called auto-void *(autonomous, fixes R-11; carries ADR-0004)*](#d-14-m-draft-auto-void-targets-expired-and-is-called-auto-void-autonomous-fixes-r-11-carries-adr-0004)
- [C. Event contract — resolves R-12…R-19](#c-event-contract--resolves-r-12r-19)
  - [D-15 (H) The event set stays at eleven; six row classes are event-less *(autonomous, fixes R-12, R-13, R-14, R-16; carries ADR-0004)*](#d-15-h-the-event-set-stays-at-eleven-six-row-classes-are-event-less-autonomous-fixes-r-12-r-13-r-14-r-16-carries-adr-0004)
  - [D-16 (H) Self-service acceptance is a fact in the submit commit, not a second event *(autonomous, fixes R-15; carries ADR-0004)*](#d-16-h-self-service-acceptance-is-a-fact-in-the-submit-commit-not-a-second-event-autonomous-fixes-r-15-carries-adr-0004)
  - [D-17 (M) Events use the platform producer outbox; Orders has no re-drive API *(autonomous, supersedes the R-19 resolution)*](#d-17-m-events-use-the-platform-producer-outbox-orders-has-no-re-drive-api-autonomous-supersedes-the-r-19-resolution)
- [D. Schema — resolves R-20…R-36](#d-schema--resolves-r-20r-36)
- [E. Authorization — resolves R-37…R-42](#e-authorization--resolves-r-37r-42)
  - [D-31 (H) The permission matrix is exhaustive, and the placing party may not record acceptance *(autonomous, fixes R-37 and R-38)*](#d-31-h-the-permission-matrix-is-exhaustive-and-the-placing-party-may-not-record-acceptance-autonomous-fixes-r-37-and-r-38)
  - [D-32 (H) Delegation proof is a named, verifiable credential *(autonomous, fixes R-39)*](#d-32-h-delegation-proof-is-a-named-verifiable-credential-autonomous-fixes-r-39)
- [F. Ownership and inventory — resolves R-43…R-51 and R-74](#f-ownership-and-inventory--resolves-r-43r-51-and-r-74)
- [G. Non-functional posture — resolves R-52…R-67](#g-non-functional-posture--resolves-r-52r-67)
- [H. PRD fidelity](#h-prd-fidelity)
  - [D-56 (H) The deferred-activation instant is specified and a new seam ask is raised *(autonomous, fixes R-69)*](#d-56-h-the-deferred-activation-instant-is-specified-and-a-new-seam-ask-is-raised-autonomous-fixes-r-69)
  - [D-57 (M) The open-question register is reconciled row by row against PRD §15 *(autonomous, fixes R-70)*](#d-57-m-the-open-question-register-is-reconciled-row-by-row-against-prd-15-autonomous-fixes-r-70)
  - [D-58 (M) The design-introduced outbox re-drive endpoint is withdrawn *(autonomous)*](#d-58-m-the-design-introduced-outbox-re-drive-endpoint-is-withdrawn-autonomous)
  - [D-59 (M) PRD reason phrases are descriptors; the design owns the identifiers *(autonomous)*](#d-59-m-prd-reason-phrases-are-descriptors-the-design-owns-the-identifiers-autonomous)
  - [D-60 (M) A missing required line date is refused at the gate *(autonomous, closes Rcons-016, now carries ADR-0004)*](#d-60-m-a-missing-required-line-date-is-refused-at-the-gate-autonomous-closes-rcons-016-now-carries-adr-0004)
  - [D-61 (H) Re-approval after an amendment is a two-step seam interaction *(autonomous, supersedes D-12's mechanism)*](#d-61-h-re-approval-after-an-amendment-is-a-two-step-seam-interaction-autonomous-supersedes-d-12s-mechanism)
  - [D-62 (H) A third field class: commercial-frozen, for the two non-amendable axes *(autonomous)*](#d-62-h-a-third-field-class-commercial-frozen-for-the-two-non-amendable-axes-autonomous)
  - [D-63 (H) Two permission-matrix corrections against PRD §6.6 *(autonomous)*](#d-63-h-two-permission-matrix-corrections-against-prd-66-autonomous)
  - [D-64 (H) A draft carries version 1; submit appends version 2 *(autonomous)*](#d-64-h-a-draft-carries-version-1-submit-appends-version-2-autonomous)
  - [D-65 (H) Authorization precedes the idempotency probe; the probe precedes guard inputs](#d-65-h-authorization-precedes-the-idempotency-probe-the-probe-precedes-guard-inputs)
  - [D-66 (H) Both begin-fulfillment elections are policy rows with safe fallbacks *(autonomous)*](#d-66-h-both-begin-fulfillment-elections-are-policy-rows-with-safe-fallbacks-autonomous)
  - [D-67 (M) Every event carries a common order-summary block *(autonomous)*](#d-67-m-every-event-carries-a-common-order-summary-block-autonomous)
  - [D-68 (M) A cross-tenant read is refused as not-found, not forbidden *(autonomous)*](#d-68-m-a-cross-tenant-read-is-refused-as-not-found-not-forbidden-autonomous)
  - [D-69 (M) Adding a state or event type is additive; consumers must tolerate unknown values *(autonomous)*](#d-69-m-adding-a-state-or-event-type-is-additive-consumers-must-tolerate-unknown-values-autonomous)
  - [D-70 (M) The audit read is a design-introduced surface with no FR basis *(autonomous)*](#d-70-m-the-audit-read-is-a-design-introduced-surface-with-no-fr-basis-autonomous)
  - [D-71 (H) Acceptance is recordable on the partner path regardless of the required flag *(autonomous)*](#d-71-h-acceptance-is-recordable-on-the-partner-path-regardless-of-the-required-flag-autonomous)
  - [D-72 (H) Fail closed on an unevaluable gate input *(autonomous, now carries ADR-0003)*](#d-72-h-fail-closed-on-an-unevaluable-gate-input-autonomous-now-carries-adr-0003)
  - [D-73 (H) Every stored verdict records its deciding authority *(autonomous)*](#d-73-h-every-stored-verdict-records-its-deciding-authority-autonomous)
  - [D-74 (M) The per-line result is a projection, not a state machine *(autonomous)*](#d-74-m-the-per-line-result-is-a-projection-not-a-state-machine-autonomous)
- [I. Slice-local calls — each declared by its slice as warranting an entry](#i-slice-local-calls--each-declared-by-its-slice-as-warranting-an-entry)
  - [D-82 (M) The version reason vocabulary is `{create, submit, amendment}`](#d-82-m-the-version-reason-vocabulary-is-create-submit-amendment)
  - [D-83 (H) The in-flight order cap stays at one; route (b) does not resolve Q-05 *(carries `ADR/0007`)*](#d-83-h-the-in-flight-order-cap-stays-at-one-route-b-does-not-resolve-q-05-carries-adr0007)
  - [D-84 (H) One order line produces one subscription — Q-02 answered no](#d-84-h-one-order-line-produces-one-subscription--q-02-answered-no)
  - [D-85 (H) The cross-gear contract surface is GTS-typed *(closes the review's GTS findings)*](#d-85-h-the-cross-gear-contract-surface-is-gts-typed-closes-the-reviews-gts-findings)
  - [D-86 (H) The overlap collision is taken first, and detected as a row shortfall *(closes a CodeRabbit finding on PR #4775)*](#d-86-h-the-overlap-collision-is-taken-first-and-detected-as-a-row-shortfall-closes-a-coderabbit-finding-on-pr-4775)
  - [D-87 (H) Ordering follows platform partition semantics; permanent reject may create a gap](#d-87-h-ordering-follows-platform-partition-semantics-permanent-reject-may-create-a-gap)
  - [D-88 (H) The idempotency key is scoped by authorized principal *(closes an IDOR finding)*](#d-88-h-the-idempotency-key-is-scoped-by-authorized-principal-closes-an-idor-finding)
  - [D-89 (M) The subscription axis of the overlap rule is disclosed as open, not bounded by a timed window](#d-89-m-the-subscription-axis-of-the-overlap-rule-is-disclosed-as-open-not-bounded-by-a-timed-window)
  - [D-90 (H) Bounded lifetime is a per-state TTL plus two re-entry caps, and the residual gap is disclosed](#d-90-h-bounded-lifetime-is-a-per-state-ttl-plus-two-re-entry-caps-and-the-residual-gap-is-disclosed)
  - [D-91 (H) No Orders-owned table is partitioned *(closes a defect found in the 2026-09-11 buildability review)*](#d-91-h-no-orders-owned-table-is-partitioned-closes-a-defect-found-in-the-2026-09-11-buildability-review)
  - [D-92 (H) The audit-chain verifier is a declared worker, not an assumed job *(closes a defect found in the 2026-09-11 buildability review)*](#d-92-h-the-audit-chain-verifier-is-a-declared-worker-not-an-assumed-job-closes-a-defect-found-in-the-2026-09-11-buildability-review)
  - [D-93 (H) One catalog version governs a whole submit *(closes a defect found in the 2026-09-11 buildability review)*](#d-93-h-one-catalog-version-governs-a-whole-submit-closes-a-defect-found-in-the-2026-09-11-buildability-review)
  - [D-94 (M) Ports that scale with the basket are called once per run *(closes a defect found in the 2026-09-11 buildability review)*](#d-94-m-ports-that-scale-with-the-basket-are-called-once-per-run-closes-a-defect-found-in-the-2026-09-11-buildability-review)
  - [D-95 (H) Internal lifecycle events use explicit platform-root tenancy](#d-95-h-internal-lifecycle-events-use-explicit-platform-root-tenancy)
  - [D-96 (H) Audit actor references are immutable; identity lifecycle is separate](#d-96-h-audit-actor-references-are-immutable-identity-lifecycle-is-separate)
  - [D-97 (H) Retain gear-owned transactional audit following Pricing](#d-97-h-retain-gear-owned-transactional-audit-following-pricing)
  - [D-98 (H) Refusal audit distinguishes requested and resolved order identity](#d-98-h-refusal-audit-distinguishes-requested-and-resolved-order-identity)
  - [D-99 (H) Freeze the Orders audit hash byte contract](#d-99-h-freeze-the-orders-audit-hash-byte-contract)
  - [D-100 (H) Tenant audit roll-ups with optional independent anchoring](#d-100-h-tenant-audit-roll-ups-with-optional-independent-anchoring)
  - [D-101 (M) Audit presentation uses one live keyset order](#d-101-m-audit-presentation-uses-one-live-keyset-order)
  - [D-102 (H) Bind audit identity to verified platform surfaces; keep lifetime guarantees open](#d-102-h-bind-audit-identity-to-verified-platform-surfaces-keep-lifetime-guarantees-open)
  - [D-103 (H) Reconcile Orders audit with Pricing's baseline and explicit domain differences](#d-103-h-reconcile-orders-audit-with-pricings-baseline-and-explicit-domain-differences)
  - [D-104 (H) Resolve audit tenancy, refusal scope, creation shape and writer boundaries](#d-104-h-resolve-audit-tenancy-refusal-scope-creation-shape-and-writer-boundaries)
  - [D-105 (H) Complete the create branch and qualify audit-read visibility](#d-105-h-complete-the-create-branch-and-qualify-audit-read-visibility)
  - [D-106 (H) Store the sales path on the aggregate at create](#d-106-h-store-the-sales-path-on-the-aggregate-at-create)
  - [D-107 (H) One precedence for the acceptance-required election, recorded by source](#d-107-h-one-precedence-for-the-acceptance-required-election-recorded-by-source)
  - [D-108 (H) The gate resolves the overlap key from the catalog registry at the fixed version](#d-108-h-the-gate-resolves-the-overlap-key-from-the-catalog-registry-at-the-fixed-version)
  - [D-109 (H) A held in_fulfillment order reaches a terminal state through Workflow without resuming](#d-109-h-a-held-in_fulfillment-order-reaches-a-terminal-state-through-workflow-without-resuming)
  - [D-110 (H) A stale Workflow result is refused version-conflict: the version check precedes admissibility for workflow triggers](#d-110-h-a-stale-workflow-result-is-refused-version-conflict-the-version-check-precedes-admissibility-for-workflow-triggers)
  - [D-111 (H) The platform PDP evaluates delegation proof; Orders forwards it and maps the denial](#d-111-h-the-platform-pdp-evaluates-delegation-proof-orders-forwards-it-and-maps-the-denial)
  - [D-112 (M) A missing expected version is rejected at the boundary before authorization, unaudited](#d-112-m-a-missing-expected-version-is-rejected-at-the-boundary-before-authorization-unaudited)
  - [D-113 (M) An unavailable input never masks an earlier-registered failing guard; precluded inputs are not unresolvable](#d-113-m-an-unavailable-input-never-masks-an-earlier-registered-failing-guard-precluded-inputs-are-not-unresolvable)
  - [D-114 (M) A targeted denial answers 403 only when the caller may read the target, otherwise 404](#d-114-m-a-targeted-denial-answers-403-only-when-the-caller-may-read-the-target-otherwise-404)
  - [D-115 (M) One closed actor class, derived from the authenticated context alone](#d-115-m-one-closed-actor-class-derived-from-the-authenticated-context-alone)
  - [D-116 (M) Draft order and line edits have algorithms, and an unknown line answers line-not-found](#d-116-m-draft-order-and-line-edits-have-algorithms-and-an-unknown-line-answers-line-not-found)
  - [D-117 (M) Line administrative fields stay editable after draft through line PATCH](#d-117-m-line-administrative-fields-stay-editable-after-draft-through-line-patch)
  - [D-118 (M) One request maps to one trigger; a draft request mixing field classes is refused](#d-118-m-one-request-maps-to-one-trigger-a-draft-request-mixing-field-classes-is-refused)
  - [D-119 (M) The seller is fixed at creation](#d-119-m-the-seller-is-fixed-at-creation)
  - [D-120 (M) Administrative fields are last-write-wins per field](#d-120-m-administrative-fields-are-last-write-wins-per-field)
  - [D-121 (M) The per-tenant date policy is a revisioned table on the policy channel](#d-121-m-the-per-tenant-date-policy-is-a-revisioned-table-on-the-policy-channel)
  - [D-122 (M) The gate reads the seller's catalog, named explicitly](#d-122-m-the-gate-reads-the-sellers-catalog-named-explicitly)
  - [D-123 (M) Pin composition runs in every gate run](#d-123-m-pin-composition-runs-in-every-gate-run)
  - [D-124 (M) A line's region is its resolved price row's market scope](#d-124-m-a-lines-region-is-its-resolved-price-rows-market-scope)
  - [D-125 (M) Preview withholds TCV in a successful response](#d-125-m-preview-withholds-tcv-in-a-successful-response)
  - [D-126 (M) The overlap port is an occupancy read](#d-126-m-the-overlap-port-is-an-occupancy-read)
  - [D-127 (M) The activation re-check has four outcomes](#d-127-m-the-activation-re-check-has-four-outcomes)
  - [D-128 (M) A payer change crosses seller scope unless the identity answer confirms the relationship](#d-128-m-a-payer-change-crosses-seller-scope-unless-the-identity-answer-confirms-the-relationship)
  - [D-129 (M) A bad amendment explanation has its own reason](#d-129-m-a-bad-amendment-explanation-has-its-own-reason)
  - [D-130 (M) A partner-placed order's creator, submitter or amender cannot record its acceptance, refused by a Lifecycle guard](#d-130-m-a-partner-placed-orders-creator-submitter-or-amender-cannot-record-its-acceptance-refused-by-a-lifecycle-guard)
  - [D-131 (M) A pending authorization outcome is refused defensively, not expected](#d-131-m-a-pending-authorization-outcome-is-refused-defensively-not-expected)
  - [D-132 (M) The contract-resolution port also returns the acceptance declaration, read live](#d-132-m-the-contract-resolution-port-also-returns-the-acceptance-declaration-read-live)
  - [D-133 (M) Acceptance and tolerate-failure elections change only by deployment promotion](#d-133-m-acceptance-and-tolerate-failure-elections-change-only-by-deployment-promotion)
  - [D-134 (M) A workflow-mediated cancel always carries complete compensation evidence](#d-134-m-a-workflow-mediated-cancel-always-carries-complete-compensation-evidence)
  - [D-135 (M) A denied verdict carries a denial reason, stored and never evaluated](#d-135-m-a-denied-verdict-carries-a-denial-reason-stored-and-never-evaluated)
  - [D-136 (M) A fulfillment acknowledgement carries the correlation identifier and a closed failure reason](#d-136-m-a-fulfillment-acknowledgement-carries-the-correlation-identifier-and-a-closed-failure-reason)
  - [D-137 (M) Seller-scope TTL overrides sit behind a default-off gear flag](#d-137-m-seller-scope-ttl-overrides-sit-behind-a-default-off-gear-flag)
  - [D-138 (M) The hold record is the stored pre-hold state plus the hold transition's audit entry](#d-138-m-the-hold-record-is-the-stored-pre-hold-state-plus-the-hold-transitions-audit-entry)
  - [D-139 (M) An invalid cursor is `cursor-invalid`, under one cursor contract for all five paged collections](#d-139-m-an-invalid-cursor-is-cursor-invalid-under-one-cursor-contract-for-all-five-paged-collections)
  - [D-140 (H) `sales_path` is `partner_placed` iff the allowed create carried a delegation proof reference](#d-140-h-sales_path-is-partner_placed-iff-the-allowed-create-carried-a-delegation-proof-reference)
  - [D-141 (M) A targeted request answers `order-not-found` on a delegation-proof denial; only untargeted requests disclose the reason](#d-141-m-a-targeted-request-answers-order-not-found-on-a-delegation-proof-denial-only-untargeted-requests-disclose-the-reason)
  - [D-142 (M) A boundary validation failure with no more specific reason is `request-invalid`; a no-op administrative edit is refused](#d-142-m-a-boundary-validation-failure-with-no-more-specific-reason-is-request-invalid-a-no-op-administrative-edit-is-refused)
  - [D-143 (M) Caller-supplied explanations go in a nullable audit `caller_reason`, covered by audit hash v2](#d-143-m-caller-supplied-explanations-go-in-a-nullable-audit-caller_reason-covered-by-audit-hash-v2)
  - [D-144 (M) The composed read carries the activation re-check's fulfillment inputs](#d-144-m-the-composed-read-carries-the-activation-re-checks-fulfillment-inputs)
  - [D-145 (M) A `PATCH` selects its trigger from field classes alone; a commercial edit outside draft is `not-admissible`](#d-145-m-a-patch-selects-its-trigger-from-field-classes-alone-a-commercial-edit-outside-draft-is-not-admissible)
  - [D-146 (H) The submit-time automatic acceptance keys on the submit request, and the recording-party bar also keys on stored actor tenants](#d-146-h-the-submit-time-automatic-acceptance-keys-on-the-submit-request-and-the-recording-party-bar-also-keys-on-stored-actor-tenants)
  - [D-147 (M) `expected_draft_revision` is optional at the boundary for `draft-mutate` and compared only after admissibility](#d-147-m-expected_draft_revision-is-optional-at-the-boundary-for-draft-mutate-and-compared-only-after-admissibility)
  - [D-148 (M) A committed audit entry's `reason` is one closed token per trigger](#d-148-m-a-committed-audit-entrys-reason-is-one-closed-token-per-trigger)
  - [D-149 (M) An administrative edit that changes nothing refuses `administrative-edit-unchanged`, keeping `request-invalid` boundary-only](#d-149-m-an-administrative-edit-that-changes-nothing-refuses-administrative-edit-unchanged-keeping-request-invalid-boundary-only)
- [PriceBook seam remediation (2026-09-29)](#pricebook-seam-remediation-2026-09-29)
- [High-register reconciliation (2026-09-23)](#high-register-reconciliation-2026-09-23)
- [Medium-register reconciliation (2026-09-23)](#medium-register-reconciliation-2026-09-23)
- [Open questions](#open-questions)
- [Traceability](#traceability)
- [Documentation review history](#documentation-review-history)

- [D-241 — Residual follow-through, proposed mirrors and runtime evidence](#d-241--residual-follow-through-proposed-mirrors-and-runtime-evidence)
- [D-242 — Adopt PR5516 protected semantics and publish proposed integration contract](#d-242--adopt-pr5516-protected-semantics-and-publish-proposed-integration-contract)
- [D-243 — A maintenance block must schedule the retention purge](#d-243--a-maintenance-block-must-schedule-the-retention-purge)
- [D-244 — PR #5536 toolkit-pr-review findings: fixes, accepts, deferrals](#d-244--pr-5536-toolkit-pr-review-findings-fixes-accepts-deferrals)
- [D-245 — Orders reads Rating's exact money with main's fraction API](#d-245--orders-reads-ratings-exact-money-with-mains-fraction-api)
- [D-246 — PR #5536 maintainer review (macrocheese1, 2026-10-29): fixes](#d-246--pr-5536-maintainer-review-macrocheese1-2026-10-29-fixes)
- [D-247 — Main's D-227…D-230 are restored; this branch's decisions move to D-231…D-246](#d-247--mains-d-227d-230-are-restored-this-branchs-decisions-move-to-d-231d-246)
- [D-248 — The in-crate tests use main's gear-local `FakePdp`; the test-host plugin stays the e2e PDP](#d-248--the-in-crate-tests-use-mains-gear-local-fakepdp-the-test-host-plugin-stays-the-e2e-pdp)

<!-- /toc -->

## How to use this document

Every call taken while authoring or repairing the design set lands here. An entry records the
**decision**, its **rationale**, and its **propagation** target — the sections that must agree
with it, addressed as `<doc> §<n>`. A wrong propagation address defeats every mechanised check
that could have caught a substantive drift, so the address is checked when the entry is written.

Items marked *(autonomous)* were decided by the authoring agent under a standing mandate —
decide where the call is technical. Items carrying product or cross-gear consequence are **not**
decided here: they are recorded in [Open questions](#open-questions) with a named owner.
Reopening a decision means flipping its status and recording why.

**Severity**: **[H]** breaks money or correctness, or is unimplementable as written · **[M]**
teams can build incompatible behaviour · **[L]** contained. Headings carry the severity in
parentheses rather than brackets, because a bracketed heading produces an unparseable
table-of-contents link.

**Two shapes.** An entry whose reasoning a reader needs in order to implement it correctly gets
its own section — the engine algorithm, the event contract, authorization and PRD fidelity are
written this way. An entry that is a determinate correction with a one-line reason gets a table
row in its area — schema, ownership and the non-functional baselines are written this way. Every
entry carries the same three fields either way; only the room given to the rationale differs.

**Status**: opened 2026-09-08. D-01…D-05 record calls taken during original authoring; D-06…D-57
resolve the 74 findings of the **2026-09-08 review wave** (`R-01`…`R-74`),
whose finding ids (`R-nn`) are cited per entry; D-58…D-60 come from the verification passes over
that remediation, which found two decisions asserted but not fully applied; D-61…D-90 come from
the 2026-09-09 to 2026-09-11 waves and from the review of PR #4775. **Thirty-three items are
routed as open questions** (`Q-01`…`Q-33`), of which twenty-eight are routed and unanswered — Q-10
is out of this gear's scope, and Q-02, Q-13, Q-14 and Q-19 are closed by design decisions.

## Status board

| Area | Entries | Severity | State |
|------|---------|----------|-------|
| A. Foundational shape | D-01…D-05 | [H] ×3, [M] ×2 | decided; D-01 and D-03 carry ADRs, D-05 and D-08 carry ADR-0005 |
| B. Engine algorithm | D-06…D-14 | [H] ×7, [M] ×2 | decided (autonomous) — resolves R-01…R-11 |
| C. Event contract | D-15…D-17 | [H] ×2, [M] ×1 | decided (autonomous) — resolves R-12…R-19 |
| D. Schema | D-18…D-30 | [H] ×6, [M] ×7 | decided (autonomous) — resolves R-20…R-36 |
| E. Authorization | D-31…D-35 | [H] ×4, [M] ×1 | decided (autonomous) — resolves R-37…R-42 |
| F. Ownership and inventory | D-36…D-38 | [H] ×1, [M] ×2 | decided (autonomous) — resolves R-43…R-51, R-74 |
| G. Non-functional posture | D-39…D-55 | [H] ×10, [M] ×5, [L] ×2 | decided (autonomous, working baselines) — resolves R-52…R-67 |
| H. PRD fidelity | D-56…D-74 | [H] ×10, [M] ×9 | decided; §15 rows and PRD-wording asks routed to owners |
| I. Slice-local calls | D-75…D-149 | [H] ×29, [M] ×46 | decided; D-79 is a historical see-D-74 stub; **D-82…D-149 sit outside the area table**, each carrying its own full entry below. D-91…D-94 record decisions taken in the 2026-09-11 review round; D-95 selects platform-root event tenancy; D-96 adopts immutable audit identity; D-97 retains local audit following Pricing; D-98 resolves unknown-target refusal storage; D-99 freezes audit hash encoding; D-100 defines tenant roll-ups; D-101 defines live audit pagination; D-102 records identity evidence; D-103 reconciles Pricing; D-104 resolves audit consistency findings; D-105 specifies create execution and read visibility; D-106 stores the sales path at create; D-107 fixes the acceptance-required precedence; D-108 resolves the overlap key from the catalog registry; D-109 gives a held `in_fulfillment` order a Workflow-driven terminal exit; D-110 checks a workflow trigger's version before admissibility; D-111 moves delegation-proof evaluation into PDP policy; D-112 rejects a missing expected version at the boundary; D-113 keeps registration precedence on the unavailable-input branch; D-114 maps a targeted denial by the caller's read access; D-115 closes the actor class and derives it from the authenticated context; D-116 specifies the draft edit and line removal algorithms and replaces `line-not-in-draft` with `line-not-found`; D-117 admits line administrative edits after draft; D-118 maps one request to one trigger; D-119 fixes the seller at creation; D-120 makes administrative fields last-write-wins; D-121 stores the per-tenant date policy in `orders_date_policy`; D-122 scopes every gate catalog read to the seller's catalog; D-123 runs pin composition in every gate run; D-124 takes a line's region from its resolved price row; D-125 makes a withheld Preview TCV a successful-response annotation; D-126 redefines the overlap port as an occupancy read; D-127 gives the activation re-check four outcomes; D-128 defines crossing seller scope by the identity answer; D-129 gives a bad amendment explanation its own reason; D-130 gives the acceptance recording-party bar its own Lifecycle guard and reason; D-131 keeps a `pending` authorization outcome as a defensive refusal; D-132 widens the contract-resolution port to the acceptance declaration; D-133 makes policy elections deployment-promoted only; D-134 always requires compensation evidence on `/workflow-cancel`; D-135 carries a denial reason on a denied verdict; D-136 completes the acknowledgement inputs with a closed failure reason; D-137 puts seller TTL overrides behind a default-off gear flag; D-138 makes the hold transition's audit entry the hold record; D-139 registers `cursor-invalid` and gives all five paged collections one cursor contract; D-140 sets the sales path from an observable proxy, a proof reference on the allowed create; D-141 discloses a delegation-proof reason only on untargeted requests; D-142 registers `request-invalid` for boundary validation failures and refuses a no-op administrative edit; D-143 moves caller-supplied explanations to a nullable audit `caller_reason` under audit hash v2; D-144 puts the re-check's fulfillment inputs on the composed read; D-145 selects a `PATCH` trigger from field classes alone; D-146 keys the submit-time automatic acceptance on the submit request and extends the recording-party bar to stored actor tenants; D-147 makes `expected_draft_revision` optional at the boundary for `draft-mutate`; D-148 closes the committed audit reason vocabulary at one token per trigger; D-149 registers `administrative-edit-unchanged` and keeps `request-invalid` boundary-only |
| J. Atlas overlay alignment | D-169…D-178 | [H] ×5, [M] ×5 | decided 2026-10-02; aligns the design with the written-down overlay of Seam Atlas v2 rev 8, the Pricing/Products fork tip `7d3544156` and the Workflow branch `3ccf7793c`; rows in the PriceBook seam remediation table |
| K. S2 user decisions | D-202…D-205 | [M] ×4 | decided 2026-10-06 (user, "do the recommended way"); full entries at the end of this file |
| L. S2 implementation bindings | D-206…D-240, D-244…D-248 | [M] ×40 | D-206 selected 2026-10-06 by the S2-09 implementation at the coordinator's request; D-207 selected 2026-10-07 by S2-10 (date-policy channel and frozen date basis); D-208 selected 2026-10-07 by the S2-10 gap review at the coordinator's request (admitted-line date nullability, stale-basis refusal context); D-209 selected 2026-10-07 by S2-11 (worker class connections, conditional sweep lock, streamed checkpoints); D-210 selected 2026-10-07 by the early S6-01/S6-04 draft-read package (read wire bindings, cursor token, snapshot and access-log transactions); D-211 selected 2026-10-07 by S2-12 and amended by its gap review (pre-engine throttling bindings, delivered-route census, real readiness, live E2E host); D-212 selected 2026-10-12 by the user on the PR #5259 review (REST guideline conformance: 412 for a stale `If-Match`, replay marker, the refusal-replay exception, the shared list format); D-213 selected 2026-10-12 by the user on the same review (decimal text from `bss_money`); D-214 selected 2026-10-15 by the user on the same review (the gear is named `orders`); D-215 selected 2026-10-16 by the user on the same review (the payer relationship is a business rule, not a permission); D-216 selected 2026-10-18 by the user on the follow-up review (resolved totals as exact decimals plus display estimates, explicit discount availability); D-217 selected 2026-10-18 by the user on the same review (service start is the receiver's activation commit, amending D-201); D-218 selected 2026-10-18 by the user on the code review (worker pass deadline and statement/lock timeouts); D-219 selected 2026-10-21 by the user on the same code review (transport-neutral SDK error, REST error gear owns the Problem, status and routes); D-220 selected 2026-10-21 by the user on the same code review (SDK contract round: codec causes, named selection evidence, one term unit, closed sales path, non-exhaustive `OrdersError`); D-221 selected 2026-10-21 by the user on the same code review (input bounds: well-known token prefixes in references, amending D-202; the limiter key-store ceiling, amending D-211; a 1024-byte overlap scope key); D-222 selected 2026-10-21 by the user on the same code review (request-path lock-wait and statement timeouts are retryable, extending D-218 item 3); D-223 selected 2026-10-21 by the user on the same code review (worker and limiter metrics: no tenant attribute, roster-wide audit age gauges, edge-limiter rejection and key-store signals); D-224 selected 2026-10-21 by the user on the same code review (Orders test tiers: Docker-backed tests are ignored in the unit tier and run by `make test-integration LANE=orders-pg`); D-225 selected 2026-10-21 by the user on the same code review (review findings accepted without a code change, each with its reason and precedent; amended 2026-10-22 with items 11 to 15); D-226 selected 2026-10-22 by the user on the same code review (one actor class for every authorization decision: the configured identities decide, the subject type only tightens); D-227 selected 2026-10-23 by the user (in-crate tests use a gear-local fake PDP; the live E2E keeps the real rules provider; items 4 and 5 superseded by D-228); D-228 selected 2026-10-23 by the user (the live E2E suite is removed from PR #5259 so it no longer depends on #5375 and #5376); D-229 selected 2026-10-23 by the user (the #5381-dependent in-crate tests are compiled out behind the custom `orders_event_broker_test_support` cfg until #5381 merges, so the lib test target compiles and runs and `--all-features` does not re-enable them); D-230 selected 2026-10-23 when merging main (the GTS 0.16 regular-expression profile refuses `\u` escapes in event schema patterns); D-231 2026-10-22 (Stage 3 port inventory, typed unavailability, S3-F01/F02/F08; amended the same day: per-input reasons, per-port run caps, item 9 catalog-read vocabulary and precedence); D-232 selected 2026-10-22 by the user (Q1: `payer-relationship-unconfirmed` is a gate predicate 1 result, FailedPrecondition/400, one new registered reason; item 6 payer withholding); D-233 selected 2026-10-22 by the user (Q2: the all-failures gate report is one optional `RefusalDiagnostics.assessment` member; item 3 amended: open reason tokens); D-234 selected 2026-10-23 by the user (definitive local refusals outrank unavailability in the gate's primary: a buyer with an unconfirmed payer relationship gets 400 `payer-relationship-unconfirmed`, not 503 `catalog-predicates-unavailable`); D-235 selected 2026-10-23 by the Stage 3 implementation, milestone 2 (development delivery switch, engine diagnostics hook, submit protocol bindings, `OrdersV1::preview`, Workflow mirror list); D-236 selected 2026-10-26 by the user (option C: the commercial attempt recovery worker body is deferred to the next Stage 3 milestone; `RecoveryUnavailable` stays; the abandonment reason stays open inside D-236); D-237 selected 2026-10-26 by the Stage 3 implementation, milestone 3 (gate signals, gear-local breaker, alert baselines, fault-injection matrix, activation re-check vectors, the `MaxConcurrentActive` encoding ask); D-239 selected 2026-10-26 by the user (build our own e2e-only PDP: the declarative test-host `bss-orders-test-authz-plugin` for the Orders e2e suite; amends the lib-test known-error invariant 7 → 4); D-238 selected 2026-10-26 by the user (build the milestone-3b scenarios: the dev-delivery submit REST route, the e2e development build, the two-boot seed, the 17 Preview and submit scenarios and the defects they found); D-240 selected 2026-10-27 by the user-approved milestone-3c plan (contract and terms gates, predicate-9 shared ownership, keyless lines, wire additions, parallel waves, envelope integrity and explicit deferrals); D-244 selected 2026-10-28 by the user on the PR #5536 review (toolkit-pr-review of 175 findings: the six user decisions and accepted defaults, the fixes with their commits, accepts, deferrals, false positives and the known gaps on record; amends D-231 item 5, D-233 item 1, D-225 item 7 and the D-237 items 6 and 8 evidence lines); D-245 selected 2026-10-29 by the user after main merged the bss-money exact-fraction API (Orders reads Rating's exact money with main's fraction API; amends D-244 items 37 and 41 and the D-216 money note); D-246 selected 2026-10-29 by the owner on macrocheese1's PR #5536 review (the 22 fixes with their commits, the OQ3 note for D-236, corrections to D-244 item 66, D-245 item 3 and the PLAN-M3C migration rule; amends D-206 item 4, the D-208 note, D-233, D-235 items 7 and 12, D-240 items 2 and 4, D-144, D-179, D-180 and D-194); D-247 selected 2026-10-30 by the owner (main's D-227…D-230 restored; this branch's D-227…D-242 renumbered D-231…D-246); D-248 selected 2026-10-30 by the owner (D-227 carried: the in-crate tests use main's `FakePdp`, the test-host plugin stays the e2e PDP); all pending design review; full entries at the end of this file |
| Open questions | Q-01…Q-33, Q-40, Q-41 | — | Q-40 design target selected by D-198 with peer delivery pending; remaining questions retain their recorded statuses (Q-40 split from Q-05 by D-179); Q-10 out of scope for this gear; Q-02, Q-13, Q-14 and Q-19 closed without a Product decision (D-84; the PRD's own reason-phrase usage; a design correction; withdrawal of the Orders re-drive surface); Q-25 closed by D-186, its §6.5 trigger half split to Q-41 |

## A. Foundational shape

### D-01 (H) The engine owns every state change *(autonomous)*

**Decision**: one Order Transition Engine owns the aggregate, the append-only version chain, the
state-machine table, guard evaluation, the idempotency registry, the version check, the audit
store, the typed event contract and the reason registry. Slices declare guards and supply document
contributions and never write order state.

**Rationale**: four `p1` NFRs are properties of how a state change commits, not of any
capability. Full alternatives analysis in [`ADR/0001`](./ADR/0001-cpt-cf-bss-orders-adr-transition-through-engine.md).

**Propagated**: `DESIGN.md §1.1`, `§2.1`; [01 §4.1](DESIGN.md#contract-01-4-1).

### D-02 (M) Standard architecture, decomposition and feature layout

**Decision**: architecture and shared contracts live in `DESIGN.md`; build order and
coverage live in `DECOMPOSITION.md`; detailed implementation behavior lives in eight
`features/NN-*.md` specifications. The former separate slice directory is removed.

**Rationale**: match the platform Gears layout while retaining every normative contract
and stable CPT ID. The original 2026-09-08 choice favored the BSS index-plus-slices layout;
the 2026-09-24 document migration supersedes that location choice, not the runtime architecture.
A contract location map supports mechanical coverage and the existing invariant checks.
No Studio execution, code coverage or runtime readiness is implied by this migration.

**Propagated**: `DESIGN.md §6`; `DECOMPOSITION.md` preamble; `ADR/0002` More Information.

### D-03 (H) Foundation plus seven capability features

**Decision**: retain the foundation and seven capability boundaries. The DESIGN contains
the canonical domain, schema, interface, security and deployment contracts; each FEATURE
contains its full behavior, state rules and acceptance criteria.

**Rationale**: the correctness core retains an independent implementation and review boundary.
The alternatives remain in [ADR-0002](ADR/0002-cpt-cf-bss-orders-adr-slice-decomposition.md).
The decomposition is inside one deployable Gear; document relocation changes no service boundary.

**Propagated**: `DESIGN.md §1.3`, `§3.2`; `DECOMPOSITION.md`.

### D-04 (H) The state machine is data, not control flow *(autonomous)*

**Decision**: the machine is a transition table of rows; an edge that is not a row cannot be
taken; normative exclusions are expressed as absent rows.

**Rationale**: edge coverage becomes enumerable and testable, and the `in_fulfillment` expiry
exclusion becomes structural — a sweep defect produces a refusal rather than an orphaned order.

**Propagated**: [01 §3.2](DESIGN.md#contract-01-3-2), `§4.3`; [07 §3.6](features/07-hold-and-expiry.md#contract-07-3-6) *Sweep Expired Orders* step 2.4.4, `§4.3`.

### D-05 (M) Refused attempts are audited *(autonomous, carries ADR-0005)*

**Decision**: the audit store records refused transitions alongside committed ones, with an
`outcome` discriminator.

**Rationale**: a denied authorization or a failed guard that leaves no trace is invisible to a
reviewer, which defeats the point of a financial-grade audit trail.

**Propagated**: [01 §3.7](DESIGN.md#contract-01-3-7) `orders_transition_audit`, `§4.4`; [08 §3.6](features/08-read-and-authz.md#contract-08-3-6) *Audit retrieval*.
**Consequence recorded in D-49**: unbounded refusal auditing is an amplification vector.

## B. Engine algorithm — resolves R-01…R-11

### D-06 (H) Idempotency resolution precedes admissibility and the version check *(autonomous, fixes R-01)*

**Decision**: the guard order becomes authorization → **idempotency resolution** → state-table
admissibility → version check → slice guards. *Amended by D-110*: for the workflow-trigger class of [01 §4.1](DESIGN.md#contract-01-4-1) the version
check precedes state-table admissibility; every other trigger keeps this order. A settled record whose request fingerprint matches
returns its stored outcome immediately. The version check applies only where no settled record
exists for the key.

**Rationale**: the original order was wrong and self-contradicting. Every versioning transition
bumps `current_version` on commit, so a retry of a committed submit or amendment carried a
superseded version and received version-conflict — never reaching the stored outcome. The replay
sequence and the `§4.2` outcome table both described the corrected order; only `§4.1`, `§2.1` and
the algorithm described the broken one. The justifying sentence in `§4.1` ("a replayed key against
a superseded version is a version conflict rather than a stored-outcome replay") was the error and
is deleted.

**Propagated**: [01 §2.1](DESIGN.md#contract-01-2-1) (`principle-guard-declared-not-embedded`), `§3.6` steps 5–15, `§4.1`
¶2, `§4.2` table.

### D-07 (H) The in-flight marker is upsert-and-reread *(autonomous, fixes R-02)*

**Decision**: the in-flight insert becomes insert-if-absent followed by a re-read that branches to
the settled or in-flight case. The claim that the marker is "a committed row, so a concurrent
duplicate observes it" is withdrawn.

**Rationale**: inserted and settled inside one transaction, the marker is never visible to a
concurrent caller. A duplicate blocked on the unique index and then received a violation that had
already aborted its own transaction, making a plain return impossible — and by then the first
attempt had settled *successfully*, so the correct answer was the stored success, not
still-processing. `still-processing` was unreachable and a crashed request left no marker at all.

**Propagated**: [01 §1.2](DESIGN.md#contract-01-1-2) (`nfr-order-idempotency`), `§3.6` steps 13–14, `§4.2` outcome table.

### D-08 (H) Every refusal path audits, settles and commits *(autonomous, fixes R-03; carries ADR-0005)*

**Decision**: each refusal class appends its audit entry, settles the idempotency record where one
exists, and commits before returning.

**Rationale**: `§4.1` required exactly this and the algorithm delivered it for one refusal class
in five, so the 100 % audit NFR was unmet by the algorithm meant to guarantee it.

**Propagated**: [01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt Transition* steps 8, 11, 12, 13, 15, `§4.1`.

**The one exception is now closed.** *Attempt Transition* step 3 refuses an unresolvable guard
input before step 4 opens the ordinary transaction, which once left the most frequent refusal in
the system — [`ADR/0003`](./ADR/0003-cpt-cf-bss-orders-adr-fail-closed-gate.md)'s
fail-closed posture makes it so — unaudited and unsettled. Step 3.1 now **opens a refusal
transaction of its own**, taking the row lock, resolving or creating the idempotency record,
settling it with the guard's registered unevaluable reason, appending the refused-attempt audit
entry and committing. The open finding of review wave 4 (`F2-SEM-007` / `Rc2-025`) is resolved,
and "without exception" holds as written; the only remaining scoping caveat is authorization
denial, which audits and commits but deliberately does not settle a caller-supplied key
([01 §4.1](DESIGN.md#contract-01-4-1), [`ADR/0005`](./ADR/0005-cpt-cf-bss-orders-adr-refusals-commit.md)).

### D-09 (H) Slice pre-checks become registered guards *(autonomous, fixes R-04)*

**Decision**: checks a slice performed before calling the engine become guard predicates
registered against the transition row and evaluated by the engine.

**Rationale**: a refusal raised before the engine is called produces no audit row and no
idempotency record, so a refused submit was neither auditable nor replayable — and [01 §2.1](DESIGN.md#contract-01-2-1)
already required guards to be declared and engine-evaluated.

**Propagated**: [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6), [04 §3.6](features/04-versioning.md#contract-04-3-6), [05 §3.6](features/05-preconditions.md#contract-05-3-6), [06 §3.6](features/06-workflow-seam.md#contract-06-3-6), [07 §3.6](features/07-hold-and-expiry.md#contract-07-3-6).

### D-10 (H) Slice writes become document contributions *(autonomous, fixes R-05)*

**Decision**: durable rows a slice wrote before requesting the transition — verdict, linkage,
projection, acceptance instant, pre-hold state, gate outcome — are passed as contributions to the
engine call and written inside its transaction.

**Rationale**: writing then transitioning leaves an orphaned row on refusal, defeating "no partial
commit to reconcile" and contradicting the single-writer constraint.

**Propagated**: [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) *Run Gate and Submit* step 13.1; [05 §3.6](features/05-preconditions.md#contract-05-3-6) *Record Acceptance* step 6;
[06 §3.6](features/06-workflow-seam.md#contract-06-3-6) *Acknowledge Fulfillment* steps 2.1.2, 2.2.2, 3, 4;
[07 §3.6](features/07-hold-and-expiry.md#contract-07-3-6) *Hold Then Resume* step 2.

### D-11 (H) Five transition rows are added *(autonomous, fixes R-07)*

**Decision**: rows are added for order creation (`∅ → draft`), draft content mutation
(`draft → draft`, state-only), the administrative edit (any non-terminal → same state,
state-only), the spawn-signal report (`in_fulfillment → in_fulfillment`, state-only) and draft
auto-void (`draft → expired`, actor class `system`). The table is twenty-five rows (twenty-seven
since D-109).

**Rationale**: five operations the slices specify had no row, and the engine refuses any
`(state, trigger)` without one — so the whole of Phase 1 was inadmissible under the design's own
machine.

**Propagated**: [01 §4.3](features/01-foundation.md#contract-01-4-3); [02 §1.2](DESIGN.md#contract-02-1-2), `§3.6`; [04 §3.6](features/04-versioning.md#contract-04-3-6), `§4.1`; [06 §3.3](DESIGN.md#contract-06-3-3), `§4.1`, `§4.3`;
[07 §3.2](DESIGN.md#contract-07-3-2), `§4.4`; `DECOMPOSITION.md` authoring status.

### D-12 (H) Rows disambiguated and the PRD's two `approved` edges restored *(autonomous, fixes R-08 and R-09)*

**Decision**: the in-place amendment row is restricted to `submitted` and `pending_approval`. The
amendment-from-`approved` row is **split into the PRD's two guarded edges** —
`approved → submitted [approval not required for the new version]` and
`approved → pending_approval [approval required]` — with the requirement verdict as the guard.
Each row carries exactly one versioning behaviour.

**Rationale**: two rows matched `(approved, amendment)`, making the lookup non-deterministic. The
original collapse also narrowed a PRD edge and [04 §4.3](features/04-versioning.md#contract-04-4-3) then committed to `submitted`
unconditionally, leaving the `pending_approval` target unreachable — a scope change presented as
an implementation detail. Restoring the PRD's edges removes the need for a scope-change approval
entirely, which is why this is decidable here rather than routed.

**Propagated**: [01 §4.3](features/01-foundation.md#contract-01-4-3) rows; [04 §1.1](DESIGN.md#contract-04-1-1), `§2.2`, `§3.6` step 11, `§4.3`.

**Superseded in part by D-61**: the two-row split stands as a description of
the PRD's declared edges, but the requirement verdict is no longer their guard — nothing in this
gear can obtain a verdict for an uncreated version. Rows 19 and 20 are now unguarded amendment
rows targeting `submitted`.

### D-13 (M) The spawn signal is permanent *(autonomous, fixes R-10)*

**Decision**: `spawn_signal_at` is written once and never cleared. The clearing rule is deleted.

**Rationale**: the rule cleared it "only by a workflow-mediated cancel", but that cancel lands in
`cancelled`, which is terminal with no row out — so no subsequent attempt exists and the rule was
unreachable.

**Propagated**: [06 §3.7](DESIGN.md#contract-06-3-7), `§4.3`; [01 §1.2](DESIGN.md#contract-01-1-2) (`fr-order-cancel` row), `§3.7`.

### D-14 (M) Draft auto-void targets `expired` and is called auto-void *(autonomous, fixes R-11; carries ADR-0004)*

**Decision**: an abandoned draft transitions `draft → expired` with actor class `system`, reusing
`OrderExpired`. The outcome is called **auto-void** everywhere; "archived" and "expired" as
descriptions of it are removed. Auto-void writes no destructive path — the order and its trail
remain readable in the terminal state.

**Rationale**: the sweep targeted `archived`, a state in neither the state set nor the terminal
set, with no row; and the outcome was named three ways across the set. Reusing `expired` avoids a
twelfth state and a twelfth event.

**Retires**: archived, archival, archive — except: WAL archiving, archival tier, retention tier, is not a state, only term used, no `archived` state

**Propagated**: [01 §4.3](features/01-foundation.md#contract-01-4-3); [02 §1.2](DESIGN.md#contract-02-1-2), `§4.5`; [07 §3.2](DESIGN.md#contract-07-3-2), `§4.4`, `§3.7`; `DESIGN.md §1.2`.

## C. Event contract — resolves R-12…R-19

**Not in the PRD's diagram.** PRD §6.1's normative state machine contains no `draft → expired` edge; §7.1 only says abandoned drafts *should* be auto-voided rather than deleted. Reusing `expired` avoided a twelfth state and a twelfth event, but it means `OrderExpired` now carries two commercially different facts — a committed order whose TTL lapsed, and a basket never submitted. The payload distinguishes them, so the gap is disclosure rather than correctness; flagged here as an addition to a PRD-normative diagram needing Product's acknowledgement, the treatment D-58 already applies.

### D-15 (H) The event set stays at eleven; six row classes are event-less *(autonomous, fixes R-12, R-13, R-14, R-16; carries ADR-0004)*

**Decision**: the transition table gains a nullable `event_type` column. `§4.4` becomes "exactly
one platform producer-outbox message per committed transition **where the row declares an event
type**". The eleven PRD
events map to rows as follows, and six row classes are deliberately event-less:

| Event | Emitting rows |
|-------|---------------|
| `OrderSubmitted` | `draft → submitted` |
| `OrderApproved` | `submitted → approved`, `pending_approval → approved` |
| `OrderRejected` | `pending_approval → rejected` |
| `OrderAmended` | both amendment rows and the in-place amendment |
| `OrderHeld` | `* → on_hold` |
| `OrderResumed` | `on_hold → pre-hold state` |
| `OrderCancelled` | every cancel row |
| `OrderExpired` | both TTL expiry rows and draft auto-void |
| `OrderCompleted` | `in_fulfillment → completed` |
| `OrderFulfillmentFailed` | `in_fulfillment → fulfillment_failed` |
| `OrderAcceptanceRecorded` | the acceptance row, partner path only (see D-16) |

**Event-less rows**: `submitted → pending_approval`, `approved → in_fulfillment`, creation, draft
content mutation, the administrative edit, and the spawn-signal report.

**Rationale**: "the eleven state events" was asserted four times and enumerated nowhere, four
were never named, and `event_type` was read by the algorithm from a tuple that did not declare
it. The event-less rows are safe rather than a gap: in each case the **caller caused the
transition and already knows** — the sibling gear reflects the verdict and calls
begin-fulfillment, and its trigger set (PRD §6.1 of the Workflow PRD) contains neither event.
Adding event types would have been a PRD scope change; this resolution needs none.

**Propagated**: `DESIGN.md §1.2`, `§1.3`, `§3.3`; [01 §3.2](DESIGN.md#contract-01-3-2), `§3.6` step 24, `§3.7`
*Platform-managed producer persistence*, `§4.3`, `§4.4`; [04 §3.6](features/04-versioning.md#contract-04-3-6), `§4`.

### D-16 (H) Self-service acceptance is a fact in the submit commit, not a second event *(autonomous, fixes R-15; carries ADR-0004)*

**Decision**: on the self-service path the acceptance instant is written by the submit
contribution inside the submit commit, and **only `OrderSubmitted` is published**, carrying the
acceptance instant and accepted_version in its payload. `OrderAcceptanceRecorded` is published
for a separate recording on either path, including renewed self-service assent after amendment;
initial submit still publishes exactly one event.

**Amended by D-146**: "the self-service path" for the submit contribution means a submit whose
allowed request carried no delegation proof reference and whose submitter's subject tenant equals
`resourceTenantId`, not `sales_path = self_service`; any other submit writes no acceptance.

**Rationale**: the original text mandated publishing both events "within the submit commit",
which one commit cannot do — it cannot be two transition rows, and it cannot enqueue two producer
messages without breaking the one-message invariant. The
chosen resolution keeps the PRD's rule that self-service submit *constitutes* acceptance with no
separate field, and keeps the event set intact.

**Propagated**: [05 §1.2](DESIGN.md#contract-05-1-2), `§3.6` *Self-service*, `§4.2`; [01 §4.4](DESIGN.md#contract-01-4-4).

### D-17 (M) Events use the platform producer outbox; Orders has no re-drive API *(autonomous, supersedes the R-19 resolution)*

**Decision**: Orders uses `event-broker-sdk::DbProducer` with feature `outbox`, backed by
`toolkit_db::outbox`, in managed `ProducerMode::Chained`. Orders owns typed event construction and
the transactional enqueue only. Platform code owns sequencing, leases, retry classification,
dead letters and vacuum. The former Orders REST re-drive is removed; operations use platform
outbox facilities.

**Rationale**: the earlier R-19 resolution designed a recovery API around an Orders-owned outbox.
The repository now supplies that infrastructure. Keeping the custom table and endpoint would fork
platform behavior; moreover, after a chained partition advances, replay of an older rejected
sequence is not guaranteed to republish it. Consumers must reconcile notifications with
Orders-authoritative state rather than rely on an Orders-specific replay ledger.

**Propagated**: `DESIGN.md §1.1`, `§3.3`, `§3.4`, `§3.7`, `§3.8`, `§4.5`; [01 §3.2](DESIGN.md#contract-01-3-2), `§3.4`,
`§3.6` *Platform producer-outbox publication*, `§3.7` *Platform-managed producer persistence*,
`§3.8`, `§4.4`; [08 §4.3](DESIGN.md#contract-08-4-3); `ADR/0006`.

## D. Schema — resolves R-20…R-36

| ID | Sev | Decision | Rationale | Propagated |
|----|-----|----------|-----------|-----------|
| D-18 | [H] | `orders_resolved_total` gains `scope enum('line','order')` in the primary key; `line_id` is non-null for line rows and a zero UUID for the roll-up | A nullable column cannot participate in a primary key, so the order-level roll-up row the design requires was unstorable (R-20) | [01 §3.7](DESIGN.md#contract-01-3-7); [03 §4.4](DESIGN.md#contract-03-4-4) |
| D-19 | [H] | Draft content lives in mutable working tables that submit materialises into version 2; administrative content lives in mutable `orders_order_admin` and `orders_order_line_admin` tables | Draft "free modification" and the in-place administrative edit both wrote tables declared append-only, so two specified paths violated their own constraints (R-21, R-22, R-26) | [01 §3.7](DESIGN.md#contract-01-3-7), `§2.1`, `§4.3`; [02 §3.7](DESIGN.md#contract-02-3-7), `§4.1`, `§4.3`; [04 §3.6](features/04-versioning.md#contract-04-3-6), `§3.7` |
| D-20 | [H] | The foreign-key graph is declared: every child references `order_id`, version-scoped children reference `(order_id, version)`, and the `orders_order.current_version` cycle is a deferred constraint | No foreign key existed across twelve tables while the design claimed a recovered database could not hold a state change without its trail (R-23) | [01 §3.7](DESIGN.md#contract-01-3-7) |
| D-21 | [H] | `orders_order_line_identity(order_id, line_id)` is added as the parent of order-scoped line identity; `orders_line_fulfillment` and `orders_resolved_total` reference it, and the projection gains `version` | `line_id` was claimed unique within an *order* while the PK enforced uniqueness within a *version*, and two tables keyed on the uniqueness nothing provided (R-24, R-34) | [01 §3.7](DESIGN.md#contract-01-3-7); [02 §3.7](DESIGN.md#contract-02-3-7), `§2.1` |
| D-22 | [H] | Ten columns are added to the canonical schema: the tolerated-authorization risk flag, `state_entered_at`, the per-line date policy-switch state, audit `changed_field`/`prior_value`/`new_value`, line `currency`, `overlap_scope_key`, the hold actor/instant/reason, and compensation evidence. *Corrected 2026-09-23 (D-138): the hold actor/instant/reason columns are withdrawn — [01 §3.7](DESIGN.md#contract-01-3-7) never carried them, and the hold transition's audit entry already records all three — so nine columns stand: the risk flag, `state_entered_at`, the policy-switch state, the three audit field columns, line `currency`, `overlap_scope_key` and compensation evidence* | Slices required each of them normatively and the schema calling itself canonical defined none (R-25) | [01 §3.7](DESIGN.md#contract-01-3-7); [02 §3.7](DESIGN.md#contract-02-3-7); [04 §3.7](DESIGN.md#contract-04-3-7); [05 §3.7](DESIGN.md#contract-05-3-7); [06 §3.7](DESIGN.md#contract-06-3-7); [07 §3.1](DESIGN.md#contract-07-3-1), `§3.7`; [08 §3.7](DESIGN.md#contract-08-3-7) |
| D-23 | [M] | Orders-owned indexes are matched to declared query paths: `(resource_tenant_id, state, state_entered_at)` and `(seller_tenant_id, state, state_entered_at)` replace the payer composite and serve scoped lists and sweeps, while `(contract_id)` and idempotency `expires_at` are added. **Amended for payer-reader access:** add current-payer keyset indexes in [01 §3.7](DESIGN.md#contract-01-3-7); the earlier no-payer-query rationale no longer applies. Producer indexes and vacuum policy are inherited from `toolkit_db::outbox` | The original Orders indexes left the partner path and contract filter unserved; producer queue indexing is a platform concern rather than Orders DDL (R-27) | [01 §3.7](DESIGN.md#contract-01-3-7); [07 §3.6](features/07-hold-and-expiry.md#contract-07-3-6); [08 §3.7](DESIGN.md#contract-08-3-7) |
| D-24 | [M] | Audit `sequence` is allocated from a counter on `orders_order` under the aggregate row lock. Producer sequence is independently assigned by `toolkit_db::outbox` and carried by `DbProducer`; Orders does not persist or allocate it | `MAX+1` cannot safely allocate the audit chain under concurrency, while duplicating the platform producer sequence would create two authorities (R-28) | [01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt Transition* step 5, `§3.7`; `ADR/0006` |
| D-25 | [M] | Statements not expressible as DDL are relabelled **engine-enforced invariants** with a named verification test; "constraint" is reserved for genuine DDL | Five items called constraints require cross-table or cross-row conditions, or express a writer, which no constraint can (R-29) | [01 §3.7](DESIGN.md#contract-01-3-7) |
| D-26 | [H] | *(carries [`ADR/0007`](./ADR/0007-cpt-cf-bss-orders-adr-in-transaction-concurrency.md))* `overlap_scope_key` is persisted on the line and `orders_inflight_overlap_claim` enforces one open payer/key claim with a partial unique index inside the transition transaction; the gate predicate remains as the friendly pre-check | The rule was resolved outside the transaction with no constraint behind it, so two concurrent identical submits both passed (R-30); root state cannot be indexed from an append-only line, so the claim table owns the mutable lifecycle | [01 §3.7](DESIGN.md#contract-01-3-7); [03 §4.2](features/03-gate-and-pin.md#contract-03-4-2) predicate 9 |
| D-27 | [M] | `order_market` moves to `orders_order_version` | Stored on the root, an amendment overwrote the market the prior version was gated against, and the activation re-check had no "market frozen at submit" left to compare (R-31) | [01 §3.1](DESIGN.md#contract-01-3-1), `§3.7`; [03 §3.1](DESIGN.md#contract-03-3-1), `§3.6`, `§3.7`; [04 §4.2](features/04-versioning.md#contract-04-4-2) |
| D-28 | [M] | `orders_state_ttl_policy` uses `NULLS NOT DISTINCT`; `orders_approval_reflection` is UNIQUE on `(order_id, version, verdict_kind)` | SQL treats NULLs as distinct, so duplicate platform policies were possible; and one approval-required version needs both a requirement verdict and a later gate outcome, while each kind must still exclude contradictory values (R-32, R-33) | [07 §3.7](DESIGN.md#contract-07-3-7); [06 §3.7](DESIGN.md#contract-06-3-7) |
| D-29 | [M] | `order_market` becomes `market_currency` + `market_region` columns; `catalog_price_pin` declares its fields | Both were opaque `jsonb` while a gate predicate must filter on the market and the pin is the object the resolvability invariant is asserted over (R-35) | [01 §3.7](DESIGN.md#contract-01-3-7); [03 §4.3](DESIGN.md#contract-03-4-3) |
| D-30 | [M] | A migration and schema-versioning subsection is added, plus the auto-void terminal as the archive posture | No migration strategy existed and archival was asserted with no target (R-36) | `DESIGN.md §3.7`; [01 §3.7](DESIGN.md#contract-01-3-7) |

## E. Authorization — resolves R-37…R-42

### D-31 (H) The permission matrix is exhaustive, and the placing party may not record acceptance *(autonomous, fixes R-37 and R-38)*

**Decision**: the matrix declares every operation across all slices, and gains a normative rule:
**on a partner-placed order the actor who created or submitted it may not record the customer's
acceptance instant.** Recording requires a principal of the resource-tenant party; Seller
Operator has no recording authority because no verifiable customer-instruction artifact exists.

**Amended by D-146**: "partner-placed" is not read from `sales_path` alone. The creator,
submitter or amender is also barred wherever its recorded `orders_order_version.actor_tenant_id`
differs from `resourceTenantId`, and the submit writes the automatic acceptance only when the
allowed submit carried no delegation proof reference and the submitter's subject tenant equals
`resourceTenantId`.

**Rationale**: thirteen operations had no declaration, so on the design's own startup rule the
gear could not start — and among them was the acceptance operation, leaving nothing to prevent
exactly the conflation [05 §2.1](DESIGN.md#contract-05-2-1) was written to prevent: a partner's own authority offered as
proof of their customer's consent. This is the one review finding that was a substantive hole
rather than a documentation gap.

**Propagated**: [08 §3.2](DESIGN.md#contract-08-3-2), `§4.3`; [05 §2.1](DESIGN.md#contract-05-2-1), `§3.6`, `§4.1`, `§4.2`.

### D-32 (H) Delegation proof is a named, verifiable credential *(autonomous, fixes R-39)*

**Decision**: delegation proof is a signed assertion issued by Account Management naming the
delegating tenant, the delegated scope, the delegate, an issue instant and a finite expiry,
verified against Account Management's issuer key at the pre-guard, and revocable by the
delegating tenant with revocation checked at verification. *Amended by D-111*: the verification
is PDP policy, not Orders code — the pre-guard and read paths forward the supplied reference as
PolicyEnforcer request context and map PDP's deny reasons. Its reference is recorded on the audit
entry in a new column, and the design cites BSS manifest §2.1.3 as the PRD requires.

**Rationale**: the single control preventing cross-tenant leakage had no form, issuer, trust
anchor, validation rule, lifetime or revocation, the algorithm tested a "valid" proof with
validity undefined, the PRD's normative pointer appeared nowhere, and the audit table had no
column to hold what `§4.4` said must be recorded.

**Propagated**: [08 §2.2](DESIGN.md#contract-08-2-2), `§3.6`, `§4.4`; [01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt Transition* step 1, `§3.7` `orders_transition_audit`.

| ID | Sev | Decision | Rationale | Propagated |
|----|-----|----------|-----------|-----------|
| D-33 | [H] | The Workflow-only operations require a gateway-asserted service principal plus a scope claim naming this gear; the pre-guard checks both. Actor class alone is insufficient | Nothing distinguished the sibling gear from any caller presenting that actor class (R-40) | [06 §4.1](DESIGN.md#contract-06-4-1); `DESIGN.md §4`; [01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt Transition* step 1 |
| D-34 | [H] | **Amended for C-1 (2026-09-22):** one shared PolicyEnforcer adapter invokes platform PDP for engine pre-guard and read authorization; Orders does not own a permission evaluator. Preserve one permission model, with resource/action registration and wiring in [08 §3.5](DESIGN.md#contract-08-3-5) | The original shared-evaluator mechanism prevented read/write drift but did not satisfy the platform PDP requirement. The adapter preserves that intent while delegating decisions and scope compilation to the platform; the bounded trusted-worker and private-persistence exceptions are defined in [08 §3.5](DESIGN.md#contract-08-3-5); runtime enforcement remains pending | [08 §2.1](DESIGN.md#contract-08-2-1), `§3.5`, `§4.3`; [01 §3.3](DESIGN.md#contract-01-3-3) |
| D-35 | [M] | A read access log is defined as a separate append-only surface recording reads and refused reads with the delegation-proof reference; `§4.4`'s claim is narrowed to point at it | Reads register no transition and only the engine may write the audit store, so no record of a read could exist (R-42) | [08 §3.7](DESIGN.md#contract-08-3-7), `§4.4` |

## F. Ownership and inventory — resolves R-43…R-51 and R-74

| ID | Sev | Decision | Rationale | Propagated |
|----|-----|----------|-----------|-----------|
| D-36 | [H] | The ordinary cancel operation is assigned to `07-hold-and-expiry`, which already owns the cancel-from-`on_hold` guard, with its algorithm, guard set and registered reasons | No slice owned it: three transition rows and three actor permissions depended on an operation with no algorithm, no guards and no reasons (R-43) | [07 §3.3](DESIGN.md#contract-07-3-3), `§3.6`, `§4`; `DESIGN.md §3.3`; `DECOMPOSITION.md` |
| D-37 | [M] | `DESIGN.md §3.7` becomes the complete gear-table inventory with one ownership rule — **engine owns schema and writes, slice owns content** — and `§3.3` becomes the union of the slice endpoint surfaces | Ownership was assigned twice incompatibly, the inventory omitted the tables slices introduce, and the endpoint inventory omitted eight endpoints while declaring one nobody owned (R-44, R-45) | `DESIGN.md §3.3`, `§3.7`; [01 §3.7](DESIGN.md#contract-01-3-7) |
| D-38 | [M] | One reason name per condition: the engine's `version-conflict` replaces `stale-version`, `version-stale` and `verdict-version-stale`; `commercial-field-immutable` replaces the two variants; `expiry-not-permitted-for-state` is deleted in favour of the engine's `not-admissible`; `administrative-field-in-amendment` is registered by [04 §3.3](DESIGN.md#contract-04-3-3) for a delta naming an administrative field, distinct from capture's `commercial-field-immutable` and resolved ahead of `tenant-axis-immutable` by [01 §4.1](DESIGN.md#contract-01-4-1)'s registration order. Per-entity IDs are minted, the duplicate sequence ID is removed, the dependency table gains four edges, and all seven count inconsistencies are corrected | Callers key on reason strings, so three names for one condition is a contract defect; and derived facts had drifted across ten documents (R-46…R-51, R-74) | [01 §3.3](DESIGN.md#contract-01-3-3); [02 §3.3](DESIGN.md#contract-02-3-3); [04 §3.3](DESIGN.md#contract-04-3-3); [06 §3.3](DESIGN.md#contract-06-3-3); [07 §3.3](DESIGN.md#contract-07-3-3); `DESIGN.md §3.1`, `§3.6`; `DECOMPOSITION.md` |

## G. Non-functional posture — resolves R-52…R-67

All values below are **working baselines** pending the program-wide NFR workshop, consistent with
the PRD's own posture on its latency and retention thresholds. They are recorded as decisions
rather than left blank because a threshold nobody set is a threshold nobody can verify against.

| ID | Sev | Decision | Rationale | Propagated |
|----|-----|----------|-----------|-----------|
| D-39 | [H] | The idempotency-key window is **24 hours**, matching the sibling catalog gear's ratified value, and must exceed the sibling Workflow gear's reconciliation-sweep horizon | The PRD assigns Design a `MUST` to set a finite window; the design restated the obligation, set nothing, and omitted it from both open-value registers (R-68) | [01 §2.2](DESIGN.md#contract-01-2-2), `§3.7`; [07 §4.5](DESIGN.md#contract-07-4-5) |
| D-40 | [H] | TCV **arrives computed** from the price-evaluation contract and is stored verbatim; the formula in [03 §4.4](DESIGN.md#contract-03-4-4) is marked as reproduced from the PRD glossary for the reader, not as an instruction to this gear | A normative multiplication and annealisation formula sat against `constraint-no-money-arithmetic` and R4's prohibition, with the result persisted and no statement of who evaluated it — a compliance question, not a wording one (R-71) | [03 §4.4](DESIGN.md#contract-03-4-4); `DESIGN.md §2.2`, `§1.2`; [03 §1.1](DESIGN.md#contract-03-1-1), `§1.2`, `§3.2` |
| D-41 | [H] | *(carries [`ADR/0006`](./ADR/0006-cpt-cf-bss-orders-adr-outbox-publication.md))* Capacity baselines: 50 order transitions/second peak, 200 platform-produced events/second, 16 toolkit producer-queue partitions with the high-throughput profile, ~11 Orders rows per order at version 1 and ~5 per amendment, archival tier triggered at 24 months past terminal. **Delivery target reopened:** 30 seconds p95 is an unapproved proposal; the Lifecycle PRD's p95 < 1 s durable-write-plus-publish baseline governs until Product/Architecture approves a change under Q-16 | R-52/R-53 exposed missing capacity and delivery thresholds. The 30-second value was borrowed from Orders Workflow PRD §7.1's process-event class, not derived from Lifecycle measurements or approved as a relaxation. Asynchronous publication does not itself rule out sub-second delivery. | `DESIGN.md §4`; [01 §1.2](DESIGN.md#contract-01-1-2), `§3.8` |
| D-42 | [H] | Per-port deadlines (250 ms each for catalog predicates, frontier and pin composition, 250 ms identity, 500 ms evaluation, 250 ms overlap, 250 ms contracts, 250 ms Preview-only tax) inside a **2 s submit** budget over the seven submit-path operations and a **2.25 s Preview** budget over all eight operations; bounded retry with two attempts on transient failure only; a breaker per port opening on a rolling failure ratio and mapping to that port's existing fail-closed reason; a concurrency bulkhead per port; and rate limits declared in the operation specs. The platform producer queue uses **16 toolkit partitions and the high-throughput profile** | Only unavailable ports were handled and nothing specified a slow one, while five synchronous calls sat on the request path; and producer throughput needed an explicit scalable platform profile rather than an Orders-owned single worker (R-54, R-55) | [03 §2.1](DESIGN.md#contract-03-2-1), `§3.3`; [08 §3.5](DESIGN.md#contract-08-3-5); [01 §3.6](features/01-foundation.md#contract-01-3-6), `§3.8` |
| D-43 | [H] | Synchronous commit to a quorum with one standby in a second failure domain **inside** the residency boundary; nightly base backup with continuous WAL archiving for point-in-time recovery; RTO ≤ 60 min met by standby promotion, with a named DR drill each release | RPO zero and RTO ≤ 60 min were asserted with one mechanism that constrained nothing about surviving loss of the primary, and `§4` referred to "DR replicas" never specified (R-56) | `DESIGN.md §3.8`, `§4`; [01 §3.8](DESIGN.md#contract-01-3-8) |
| D-44 | [H] | **Erasure clause superseded by D-96; other provisions retained.** Data protection: encryption at rest by the platform's storage layer, TLS in transit on every hop, keys held in the platform KMS with the gear holding none, order content classified **commercial-confidential** and actor identifiers **personal-minimal**, no masking requirement since no surface returns another tenant's data, and erasure satisfied by pseudonymising actor identifiers in place — the one permitted mutation of the audit store, itself audited | The whole of data protection returned zero hits and the only explicit non-applicability in the set was PCI DSS, which the checklist's evidence standard treats as a violation rather than an exemption (R-57) | `DESIGN.md §4` |
| D-45 | [H] | A threat table is added covering the three tenancy axes, the partner-placed path, the Workflow-only operations, the outbound ports and the Preview surface, each with vector, boundary crossed, mitigation and residual risk | There was no threat model — one sentence naming one threat — while the threats the design named elsewhere were never mapped to mitigations (R-58) | `DESIGN.md §4` |
| D-46 | [H] | The audit store gains a predecessor-hash column forming a per-order chain, plus explicit revocation of UPDATE and DELETE on the audit role; the chain is verified by a periodic job | The PRD requires a tamper-**evident** record; `(order_id, sequence)` uniqueness detects nothing, and "append-only" was a property with no enforcement while the runtime holds database privilege (R-59) | [01 §3.7](DESIGN.md#contract-01-3-7), `§4.4`; `DESIGN.md §1.2` |
| D-47 | [H] | Each slice gains an observability subsection naming its own metrics, log fields and alerts; latency-SLO alerts are added for both budgets with a burn-rate policy; the tracing propagation contract across the seam and the ports is stated; readiness and liveness are distinguished | Observability was specified once, at gear level, entirely around engine-owned signals, so the risky work was unmonitored — and the alert list contained no alert on either latency SLO (R-60, R-65) | `DESIGN.md §4`; every slice `§3.8` |
| D-48 | [H] | An "Extension points and stability" section is added to `01` naming what a slice may add without an engine change — guards, reasons, contributions, policy rows — versus what requires one: a state, an edge, an event type or a schema column. `DESIGN.md §3.3` gains an API-evolution subsection with the stability ladder, the breaking-change definition and a deprecation window | The engine is deliberately closed and the design never said so, while all endpoints were uniformly "unstable" with no promotion criterion and the PRD delegates the major-version mechanism to Design (R-61) | [01 §4.6](DESIGN.md#contract-01-4-6); `DESIGN.md §3.3` |
| D-49 | [M] | *(Amended by D-185: the retention is executed by the phase 0/1 `retention-purge` worker (`cpt-cf-bss-orders-component-retention-purge`); the limit is a pre-engine request limiter, not a repeated-refusal limiter — per caller through the platform api-gateway identity-keyed zone, per (caller, order) still open under Q-26 — and the audit store is sized by request and refusal volume.)* Refusal audit rows carry a 90-day retention distinct from committed transitions, and repeated refusals against one order are rate-limited | Unbounded refusal auditing let any caller grow the audit store and slow every audit read on that order (R-62) | [01 §3.7](DESIGN.md#contract-01-3-7); [08 §4.5](DESIGN.md#contract-08-4-5) |
| D-50 | [M] | The billing-chain tax owner is declared as a fifth outbound port with its own unavailability reason; Preview declares its actor classes and a rate limit | The tax owner appeared in no dependency table while the slice stated there were exactly four ports, and Preview took no security context and carried no rate limit (R-63) | [03 §3.3](DESIGN.md#contract-03-3-3), `§3.5`, `§4.6`; `DESIGN.md §3.5`; [08 §4.3](DESIGN.md#contract-08-4-3) |
| D-51 | [M] | Replica reads are **forbidden**; the no-replication-lag claim stands because the read is the aggregate row | `§3.8` permitted replica reads while `§2.2` and `§4.1` forbade stale answers, and a lagging replica answers successfully with old state and nothing detects it (R-64) | [08 §1.2](DESIGN.md#contract-08-1-2), `§3.8` |
| D-52 | [M] | Preview **persists** its gate outcomes, with a 7-day retention and a rate limit; the three "creates no state" claims are corrected to "creates no order" | Preview was specified as writing nothing and as writing a row per predicate per line with its own retention; persistence is worth keeping for support, so the claims are what changes (R-51) | [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6), `§3.7`, `§4.6`; `DESIGN.md §3.2` — **and is a PRD §9.1 deviation**: §9.1 specifies Preview as creating and mutating **no state**, while it persists a gate-outcome row per predicate per line under a 7-day retention. Needs Product's acknowledgement alongside D-58 (D-70 routing) |
| D-53 | [M] | The platform-inherited IaC posture is stated, and the unchosen policy values are delivered as `orders_state_ttl_policy` rows and gear configuration promoted through environments with the deployment | IaC was neither addressed nor marked inapplicable, and "no code default" had no stated delivery path (R-66) | `DESIGN.md §3.8` |
| D-54 | [L] | Residency is promoted to `constraint-data-residency`; vendor/licensing and resource constraints are marked explicitly inapplicable; the two **Location** fields gain repository paths | Residency was prose with no constraint ID, two checklist categories were neither present nor excluded, and no machine-readable contract was linked anywhere (R-67) | `DESIGN.md §2.2`, `§3.1`, `§3.3` |
| D-55 | [L] | `actor-orders-contracts` is cited in the gate and preconditions sequences, and the operation count is corrected to thirteen | One of the PRD's eight actors was referenced nowhere, and the count was wrong (R-72, R-73) | `DESIGN.md §3.3`, `§3.6`; [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6); [05 §3.6](features/05-preconditions.md#contract-05-3-6) |

## H. PRD fidelity

### D-56 (H) The deferred-activation instant is specified and a new seam ask is raised *(autonomous, fixes R-69)*

**Decision**: the activation intent carries the **actual activation instant** as the subscription
start, and the quoted service-activation date separately as the requested date. A new upstream
ask, **`SUB-O10`**, is raised on the Subscriptions gear: `create` and the activation intent must
accept an explicit start instant and must not derive it from any date on the order. The order
read and Preview continue to show the per-line deferral.

**Rationale**: the PRD requires that a line deferred past its quoted date produce a subscription
whose start is the actual activation instant, never backdated. The design carried one rationale
clause and no mechanism — no obligation on the intent, no port contract, no seam ask — although
Subscriptions owns the start, so the requirement was unenforceable from this side.

**Propagated**: [02 §4.2](features/02-capture.md#contract-02-4-2); [03 §4.6](features/03-gate-and-pin.md#contract-03-4-6); [06 §3.3](DESIGN.md#contract-06-3-3), `§4.3`, `§4.6`; [07 §4.3](features/07-hold-and-expiry.md#contract-07-4-3); [08 §4.2](DESIGN.md#contract-08-4-2);
`UPSTREAM_REQS.md`.

### D-57 (M) The open-question register is reconciled row by row against PRD §15 *(autonomous, fixes R-70)*

**Decision**: every place the design speaks about open questions now cites the PRD §15 row it
means. Three rows the design ignored are recorded as explicit deferrals with their PRD owners
(see Q-01, Q-02, Q-03). [07 §4.5](DESIGN.md#contract-07-4-5) is split into PRD-owned questions and design-owned values, and
the 24-hour overdue window is recorded as a **committed PRD default**, not an open question.

**Rationale**: the design tracked a different register than the PRD — claiming twenty unanswered
where §15 has fifteen rows and twelve unanswered, with three of the four it named not being §15
rows at all. Two ignored rows were additionally foreclosed by schema choices made without
reference to them.

**Propagated**: `DESIGN.md §4`; [02 §4.2](features/02-capture.md#contract-02-4-2); [03 §4.5](DESIGN.md#contract-03-4-5); [07 §4.5](DESIGN.md#contract-07-4-5); [08 §4.5](DESIGN.md#contract-08-4-5).

### D-58 (M) The design-introduced outbox re-drive endpoint is withdrawn *(autonomous)*

**Decision**: the former twenty-fifth endpoint,
`POST /bss-orders/v1/outbox/dead-letters/{eventId}/re-drive`, is removed from the API
inventory, authorization matrix and foundation interface. Producer dead-letter operations use the
shared platform operator interface and SDK recovery mechanism required by
`cpt-cf-bss-orders-upreq-event-broker-dead-letter-recovery`
([`UPSTREAM_REQS.md §2.7`](./UPSTREAM_REQS.md#27-event-broker)) and are not Orders business APIs.
Both capabilities remain open production release prerequisites; toolkit dead-letter claiming
alone does not implement republication.

**Rationale**: D-17 now adopts the platform producer outbox. Keeping an Orders wrapper would
preserve a custom operational contract with no PRD basis and imply that replaying an old chained
sequence is always valid, which the platform behavior does not promise. Withdrawal closes Q-19
without a Product scope decision.

**Propagated**: `DESIGN.md §3.3`; [01 §3.3](DESIGN.md#contract-01-3-3), `§4.4`; [08 §4.3](DESIGN.md#contract-08-4-3); `DECOMPOSITION.md`; Q-19.

### D-59 (M) PRD reason phrases are descriptors; the design owns the identifiers *(autonomous)*

**Decision**: the PRD's reason phrases are read as **descriptors of a condition**, not as literal
reason names, and [01 §4.2](features/01-foundation.md#contract-01-4-2) records the descriptor-to-identifier mapping. Where a descriptor is
already a good identifier it is adopted verbatim; `stale-version` is not, and resolves to the
engine's `version-conflict`.

**Rationale**: PRD §6.2 and §12 require "a machine-readable **stale-version** reason", which D-38
renamed without acknowledging the divergence — leaving a `MUST` apparently unmet. The PRD uses the
same running-prose construction for reasons it plainly does not name ("a machine-readable
business-level reason code"), so the phrases describe conditions rather than mint identifiers. The
mapping is recorded so a later reader does not restore a descriptor as a name and reintroduce the
duplication D-38 removed.

**Retires**: verdict-version-stale, version-stale, commercial-field-immutable-outside-amendment

**Propagated**: [01 §3.2](DESIGN.md#contract-01-3-2) reason registry, `§4.2`; [04 §3.3](DESIGN.md#contract-04-3-3); [06 §3.3](DESIGN.md#contract-06-3-3).

### D-60 (M) A missing required line date is refused at the gate *(autonomous, closes Rcons-016, now carries ADR-0004)*

**Decision**: where a line's required service-activation or customer-acceptance date is absent,
submit is **refused at the gate** with a machine-readable reason. The order stays in `draft` and
the caller supplies the date. The rejected alternative was a twelfth order state — a waiting state
for orders whose dates are incomplete.

**Clarified 2026-09-23** (slice-lens review H-8): "absent" means **not authored**. The cascade
default (the contract-effective date) applies only to a field the policy snapshot does not
require, so a required field is never satisfied by a default and the refusal is reachable. An
optional unauthored field stores its resolved default; an admitted line never stores NULL for any
of its three dates.

**Rationale**: a twelfth state would carry its own TTL, its own permitted edges and its own
event, for a condition that is a missing field rather than a commercial position. Refusing at the
gate keeps the state machine at eleven and makes the omission visible where every other
sellability failure is visible. [02 §1.2](DESIGN.md#contract-02-1-2) cited this decision as `D-11`, which is about
transition rows and does not cover it — this entry is its real home, and resolves **PRD §15 row
8** (owner: Product with Design).

Full alternatives analysis in
[`ADR/0004`](./ADR/0004-cpt-cf-bss-orders-adr-closed-enumerations.md), which
consolidates this decision with D-14, D-15 and D-16 as one closed-enumerations decision.

**Propagated**: [02 §1.2](DESIGN.md#contract-02-1-2), `§4.2`; [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) *Run Gate and Submit* step 12, `§4.2` predicate 8.

### D-61 (H) Re-approval after an amendment is a two-step seam interaction *(autonomous, supersedes D-12's mechanism)*

**Decision**: an amendment from `pending_approval` or `approved` transitions the order to
`submitted` and publishes `OrderAmended`; the sibling gear then obtains the requirement verdict
for the new version and reflects the order onward through the existing rows 7 and 8. Rows 18, 19
and 20 carry **no verdict guard**. The reason `verdict-unavailable` is deleted.

**Rationale**: D-12 split the amendment-from-`approved` edge into two rows guarded by "the new
version's requirement verdict" — a guard input nothing in the specified system can supply.
Verdicts exist only as reflections keyed `(order_id, version)`, so none can exist for a version
the amendment has not yet created; [06 §4.2](DESIGN.md#contract-06-4-2) forbids deriving one from the superseded version;
no port to the approval policy owner is declared; and PRD §12 AC-11a forbids this gear to query
that owner. The guard therefore failed always, rows 19 and 20 were unreachable, and amendment
from `approved` was impossible — while an amendment in `pending_approval` whose new version no
longer required approval had no exit at all, since row 8 admits that verdict only from
`submitted`. The two-step shape needs no new port, no PRD amendment to AC-11a, and no caller-
supplied verdict (which would have breached R2). Its cost is one extra transition and a
divergence from the PRD's *direct* `approved → pending_approval` edge, routed as Q-12.

**Retires**: verdict-unavailable

**Propagated**: [01 §4.3](features/01-foundation.md#contract-01-4-3) rows 18-20 and exclusions; [04 §3.3](DESIGN.md#contract-04-3-3), `§3.6` step 11, `§3.8`, `§4.3`;
[06 §4.2](DESIGN.md#contract-06-4-2).

### D-62 (H) A third field class: commercial-frozen, for the two non-amendable axes *(autonomous)*

**Amended by D-119**: `sellerTenantId` is frozen from creation, not only from `submitted`; a draft
edit naming it is refused with the same `tenant-axis-immutable`.

**Clarified by D-128**: "within one seller's scope" is now defined — a payer change crosses
seller scope when the identity operation does not confirm that the proposed payer has a commercial
relationship with the order's `sellerTenantId` ([04 §2.2](DESIGN.md#contract-04-2-2)).

**Decision**: the field classifier gains a **commercial-frozen** class holding
`resourceTenantId` and `sellerTenantId`. An amendment delta naming either is refused with the new
`tenant-axis-immutable` reason. `payerTenantId` stays commercial and amendable **within one
seller's scope**; a payer change that would cross seller scope is **refused** with
`payer-rebinding-requires-seller`, not paired with a seller rebinding.

**Corrected 2026-09-10.** This entry originally said `payerTenantId` was "paired with a seller
rebinding where the change crosses seller scope" — which the same decision makes impossible, since
freezing `sellerTenantId` means no amendment can carry the paired half. The register entry was
itself the source of the contradiction [04 §2.2](DESIGN.md#contract-04-2-2) inherited, so the pairing language is removed
rather than reworded: there is no post-submit path that rebinds a seller, so the cross-seller payer
change has no admissible form and is refused.

**This diverges from a PRD MUST, and the divergence is now routed rather than decided here.** The
pairing language did not originate in this register — PRD §6.1 requires that a cross-seller payer
change "**MUST** follow the paired payer/seller rebinding semantics", and §12's acceptance
criterion restates it. Removing the pairing therefore does not resolve the contradiction, it
relocates it: this design refuses an operation the PRD requires be honoured. The refusal stands as
specified, because the alternative reachable from here is an unguarded amendment that can rebind
the selling party. The reconciliation — amend §6.1, specify an ownership-transfer transition that
moves both axes together, or accept the refusal — is **Q-28**.

**Rationale**: PRD §6.1 fixes all three axes at `submitted` and permits exactly one post-submit
mutation. [04 §2.2](DESIGN.md#contract-04-2-2) asserted that payer was the only axis with an amendment path but registered no
guard, and the classifier's binary commercial/administrative split had no way to express
"commercial but not amendable" — so both other axes were classified commercial, the amendment path
accepted them, and step 4's payer-pairing check did not fire for a delta that changed
`sellerTenantId` alone. A submitted order's resource recipient or selling party could be silently
rebound, which is the mis-billing and seller-attribution failure the PRD locks the axes to prevent.

**Propagated**: [02 §4.3](DESIGN.md#contract-02-4-3); [04 §3.3](DESIGN.md#contract-04-3-3), `§3.6` *Append Amendment* step 1's commercial-frozen guard
(declared alongside the payer-pairing guard the new class now lets fire correctly), `§4.1`.

### D-63 (H) Two permission-matrix corrections against PRD §6.6 *(autonomous)*

**Decision**: `amend` is separated from the authoring row and granted to Partner Admin only;
Orders Workflow is granted `hold` and `resume` under the same service principal as the other seam
operations.

**Rationale**: PRD §6.6 grants Direct Customer create, submit and cancel — not amend — and the
collapsed authoring row marked them permitted across all four operations, widening a privilege the
PRD withheld and handing self-service callers a path that re-runs the gate and re-pins. Separately
PRD §6.6, §12 AC-15, §5.1 and §6.3 all name Orders Workflow as a hold actor, the sibling gear's
PRD commits to calling hold/resume, and [07 §3.6](features/07-hold-and-expiry.md#contract-07-3-6) already listed Workflow as an actor of the
sequence — while the matrix, which [08 §4.3](DESIGN.md#contract-08-4-3) makes exhaustive and startup-enforced, denied it.
A `MUST`-level acceptance criterion was therefore unbuildable, and the sibling gear's remediation
path for a permanently failed line had no callable operation.

**Propagated**: [08 §4.3](DESIGN.md#contract-08-4-3).

### D-64 (H) A draft carries version 1; submit appends version 2 *(autonomous)*

**Amended by D-188:** version 1 remains; submit uses a permanently reserved candidate and may commit above 2. The following rationale is historical.

**Decision**: creation appends version 1 — an empty commercial document — so
`orders_order.current_version` is **NOT NULL** from creation. Draft content lives in the mutable
working tables and submit materialises it into **version 2**. Draft mutation and the
administrative edit are state-only rows that present the current version as their expected
version like every other transition.

**Rationale**: [01 §3.7](DESIGN.md#contract-01-3-7) declared `current_version` "nullable until first version" while the same
section said the aggregate row and its first version are inserted in one transaction, and PRD §12
AC-1 requires the version to be 1 on draft creation — three statements, no two compatible. Worse,
the optimistic version check is mandatory on every transition, so a null `current_version` through
the whole draft phase meant draft operations either bypassed the check (a hole stated nowhere) or
refused against null.

**Propagated**: [01 §3.7](DESIGN.md#contract-01-3-7), `§4.1`; [02 §4.1](features/02-capture.md#contract-02-4-1).

### D-65 (H) Authorization precedes the idempotency probe; the probe precedes guard inputs

**Decision**: the transition algorithm evaluates authorization before it probes the registry for
`(operation, key)`. An unauthorized caller is audited and refused without probing or returning an
idempotency outcome. For an authorized caller, the advisory probe occurs before any guard input is
resolved and returns a fingerprint-matching settled outcome immediately; authoritative resolution
remains inside the transaction.

**Rationale**: An idempotency outcome is order state. Returning one before authorization let any
caller holding a matching scoped key — `(operation, principal_scope, idempotency_key)` since D-88 — and request fingerprint learn a committed
outcome, contradicting the engine's confidentiality rule. After authorization, PRD §12 AC-4 still
requires a replay to "return the same result without creating a second order **or re-running the
sellability gate**". Without the advisory probe, every authorized retry of a committed submit
re-invokes **seven** submit-path upstream operations under the full 2 s submit budget — and could then meet a deadline or an
adopted-predicate refusal that the engine discards in favour of the stored success, making the
upstream load pure waste and the observability series misleading.

**Propagated**: [01 §2.1](DESIGN.md#contract-01-2-1), `§3.6` step 1, `§4.1`, `§4.2`.

### D-66 (H) Both begin-fulfillment elections are policy rows with safe fallbacks *(autonomous)*

**Decision**: `orders_policy_election` (introduced by [05 §3.7](DESIGN.md#contract-05-3-7)) holds
`tolerate_authorization_failure` and `acceptance_required` keyed `(election, scope, scope_id)`,
seller scope overriding platform. An unset election reads as its safe value — tolerate-failure not
elected, acceptance required — and the guard records whether it read a row or the fallback.

**Rationale**: both guards were specified as reads with no source: no table, no configuration key,
no scope, no default and no delivery path, and neither appeared in the policy-value registers of
[07 §4.5](DESIGN.md#contract-07-4-5) or [08 §4.5](DESIGN.md#contract-08-4-5). Unlike the TTLs, where "no code default" is a deliberate and visible
failure mode backed by a policy table, these had nowhere to be set at all — so an implementer
would have invented a default for the two guards that decide whether a non-paying tenant gets
resources and whether fulfilment may start without recorded consent. [05 §2.1](DESIGN.md#contract-05-2-1) forbids exactly
that for acceptance.

**Propagated**: [05 §3.7](DESIGN.md#contract-05-3-7), `§4.3`; `DESIGN.md §3.7`.

### D-67 (M) Every event carries a common order-summary block *(autonomous)*

**D-158 supersedes the no-commercial-content-read assumption for the PriceBook target. Common provenance remains; expanded bindings and totals use authorized immutable-version reads. Historical rationale follows.**

**Decision**: *(Amended by D-186: the Current qualification below is resolved — a read is always required before a business effect, the event is a trigger and version reference, and only effect-free consumers act on payload alone ([event consumer contract](DESIGN.md#contract-01-event-consumer-contract)); Q-25's §9.2 half is closed and its §6.5 half is Q-41.)*
[01 §4.4](DESIGN.md#contract-01-4-4) declares one common summary block — `orderId`, `orderVersion`,
`category`, resulting `state`, the three tenant axes, the contract reference and the external
reference where present — carried by **every** event; the per-event table lists only what each
event adds beyond the envelope and that block.

**Rationale**: PRD §9.2 requires "sufficient order summary fields for consumers to act without a
callback read". The design stated that as a blanket `MUST` but enumerated order content for only
`OrderSubmitted` and `OrderCompleted`; the other nine listed their delta alone, so what satisfied
the requirement for them was unspecified and would have been decided per-implementation. Because
the event contract is a declared stability zone, adding the members later is only non-breaking if
no consumer had already compensated.

**Current qualification:** event content remains complete, but the platform-outbox contract
requires an authoritative current-state read before business effects. This is not conformance
with the literal no-callback wording: Q-25 now also routes that PRD reconciliation. All three
consumer services require target-scoped read grants and durable retry on read unavailability;
root stream access is insufficient. `UPSTREAM_REQS.md §2.7` records that integration work.

**Propagated**: [01 §4.4](DESIGN.md#contract-01-4-4).

### D-68 (M) A cross-tenant read is refused as not-found, not forbidden *(autonomous)*

**Decision**: where the caller has no relationship to the order, every read path returns
`order-not-found`. PRD §12 AC-13's, AC-16's and AC-21's confidentiality requirement is met in
full; their stated refusal *kind* — "a business-level authorization failure" / "an authorization
error" — is not, and the criteria's wording is the thing that needs amending. Recorded rather than
left for a tester to discover.

**Clarified 2026-09-23** (LOW L-08.6): the deviation was first recorded against AC-21 alone; it
equally touches AC-13 (Direct Customer on another tenant's order) and AC-16 (cross-tenant attempt
without delegation proof, which returns `order-not-found` where disclosing the proof reason would
reveal a hidden target, [08 §3.6](features/08-read-and-authz.md#contract-08-3-6) common read wrapper item 3).

**Rationale**: a forbidden response confirms the order exists, turning the read surface into an
enumeration oracle across tenancy boundaries. The anti-enumeration posture is right and the AC's
wording is the weaker constraint, but a blocking show-stopper cannot be signed off against a
substituted outcome that no document acknowledges.

**Propagated**: [08 §4.4](DESIGN.md#contract-08-4-4).

### D-69 (M) Adding a state or event type is additive; consumers must tolerate unknown values *(autonomous)*

**Decision**: [01 §4.6](DESIGN.md#contract-01-4-6) and `DESIGN.md §3.3` classify **adding** a state or an event type as
additive and non-breaking, conditional on a stated consumer obligation: a consumer **MUST**
tolerate an unknown `state` or event-type value and **MUST NOT** exhaustively match either
enumeration. Removal or renaming remains breaking.

**Rationale**: PRD §8's Versatility show-stopper requires the state machine to be extensible
without breaking consumers when new states are added — the only §8 criterion about forward
compatibility rather than a threshold. The breaking-change definition classified removal and
renaming and said nothing about addition, so the criterion had no answer and a future state
addition would have been argued either way with no prior decision.

**Propagated**: [01 §4.6](DESIGN.md#contract-01-4-6); `DESIGN.md §3.3`.

### D-70 (M) The audit read is a design-introduced surface with no FR basis *(autonomous)*

**Decision**: `DESIGN.md §3.3` reclassifies the audit read. Ten of the eleven §9.1-absent
endpoints have an FR basis; the audit read does not and is the remaining design-introduced surface
needing Product's acknowledgement.

**Rationale**: PRD §6.1 requires every transition to *be recorded* and the audit NFR requires
complete logging — both obligations on writing, not on exposing — and §9.1 contains no
audit-retrieval operation. The slice grounds the surface in a rationale ("a complete audit nobody
can read is not an audit"), which is a reason, not a requirement basis. The distinction between
"described in §6, omitted from §9.1" and "no PRD basis at all" is how this design keeps its scope
extensions honest, and one endpoint was on the wrong side of it — one that exposes actor
identities, delegation-proof references and correlation identifiers.

**Propagated**: `DESIGN.md §3.3`.

### D-71 (H) Acceptance is recordable on the partner path regardless of the required flag *(autonomous)*

**Decision (clarified for OL-26)**: acceptance can be recorded for the current immutable
nonterminal, non-draft version on either sales path, whether required or volunteered. Initial
self-service assent rides submit. Amendment preserves old assent but does not copy it forward;
the original resource-tenant buyer may accept again. Uniqueness is per `(order_id, accepted_version)`,
not for the lifetime of the order. The partner placing/selling party cannot manufacture customer
consent; that restriction does not bar a self-service buyer simply because they submitted it.

**Rationale**: PRD §6.1's "a customer-acceptance instant **MUST** be recordable as a first-class
fact" is unconditional — the required flag appears in the next sentence and governs only whether
fulfilment waits. The design read the flag as governing recordability too, so on an uncontracted
or platform-default partner-placed order a genuine customer agreement could not be recorded at
all. That is the common case, not an edge, and it is exactly the dispute scenario the requirement
exists for. Provenance on the row keeps the evidentiary hygiene the refusal was protecting.

**Propagated**: [05 §3.6](features/05-preconditions.md#contract-05-3-6) *Record Acceptance* step 3 (the `recording-path-admissible` guard
input), `§4.1`.

### D-72 (H) Fail closed on an unevaluable gate input *(autonomous, now carries ADR-0003)*

**Decision**: an input the submit gate cannot evaluate is a **refusal** with its own reason,
distinct from a predicate that was evaluated and failed. The closed port-reason set is
catalog-predicates-unavailable, catalog-frontier-unavailable, catalog-pin-composition-unavailable,
identity-party-unavailable, contract-resolution-unavailable, overlap-presence-unevaluable,
evaluation-unavailable and indicative-tax-unavailable. A successful frontier read with no value
is `catalog-frontier-absent`; returned Pricing predicates normalize to `catalog-predicate-failed`
or `catalog-predicate-unevaluable`, retaining upstream diagnostics rather than inventing upstream
reason codes. The rejected alternatives were admitting
and relying on the activation-time re-check, and admitting under a seller tolerated-risk election
of the kind `05-preconditions` uses for payment authorization.

**Rationale**: the posture was stated as two constraints in [03 §2.1](DESIGN.md#contract-03-2-1) and `§2.2` and recorded as a
decision **nowhere** — the 2026-09-09 review found no register entry and no ADR, despite the
posture determining that no submit can pass the gate until the Subscriptions read and missing Pricing inputs/interfaces land. The asymmetry with the payment-authorization election needed recording too: a
tolerating seller there accepts a *credit risk* they own and can price, whereas an unevaluable
sellability predicate would have them accept an *unknown* nobody established. Full alternatives
analysis in [`ADR/0003`](./ADR/0003-cpt-cf-bss-orders-adr-fail-closed-gate.md).

**Propagated**: [03 §2.1](DESIGN.md#contract-03-2-1), `§2.2`, `§3.3`; `DECOMPOSITION.md` authoring status.

### D-73 (H) Every stored verdict records its deciding authority *(autonomous)*

**Decision**: a stored approval verdict **MUST** carry a named deciding authority and the order
version it was decided against; a reflection lacking an authority is refused. While the approval
policy owner is unspecified, the stand-in that returns "approval not required" **MUST** be
recorded by name.

**Rationale**: [06 §1.2](DESIGN.md#contract-06-1-2) declared this decision worth a register entry and none existed, so the
rule lived in slice prose with no propagation address and nothing mechanised could catch it being
dropped. It is the only defence the design has while the policy owner does not exist: given the
stand-in currently exempts every order, distinguishing "policy exempted this" from "nobody ever
asked" is the entire audit value of the field.

**Propagated**: [06 §3.6](features/06-workflow-seam.md#contract-06-3-6), `§4.2`; [06 §3.7](DESIGN.md#contract-06-3-7) `orders_approval_reflection`.

### D-74 (M) The per-line result is a projection, not a state machine *(autonomous)*

**Decision**: `orders_line_fulfillment` is a **read-only projection** advanced only by the
acknowledgement transition. No per-line state machine, no per-line guards, no per-line events.

**Rationale**: [06 §1.2](DESIGN.md#contract-06-1-2) declared this worth a register entry and none existed. PRD §6.1 mandates
the absence of a per-line state machine, so this is a recorded constraint rather than a free
choice — but it is load-bearing for R5, which forbids mirroring downstream per-request status, and
it needed a propagation address so a later author does not grow the projection into a machine.

**Propagated**: [06 §4.5](DESIGN.md#contract-06-4-5); [01 §3.7](DESIGN.md#contract-01-3-7) `orders_line_fulfillment`; [08 §3.6](features/08-read-and-authz.md#contract-08-3-6).

## I. Slice-local calls — each declared by its slice as warranting an entry

Six slices named a decision in their `§1.2` as warranting a register entry and none existed, so
each rule lived in slice prose with no propagation address and nothing mechanised could catch it
being dropped. They are determinate calls with a stated alternative, so a table row is the right
shape.

| ID | Sev | Decision | Rationale | Propagated |
|----|-----|----------|-----------|-----------|
| D-75 | [M] | The submit gate reports **every** predicate failure rather than short-circuiting on the first | A caller fixing one refusal at a time needs as many round trips as it has problems, each costing the full port budget; the rejected alternative was short-circuit evaluation, cheaper per call and worse per basket | [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) *Run Gate and Submit* steps 7–13, `§4.2` |
| D-76 | [M] | The order-time total **excludes** overlays needing subscription-level evaluation context, and the exclusion is stated on the read and Preview responses rather than left implicit | A total that silently omits an overlay is worse than one that says what it omits; the rejected alternative was computing them from an order-level approximation, which would have this gear deriving price. Interim until `…-upreq-pre-subscription-evaluation` lands; closes PRD §15 row 6 | [03 §4.5](DESIGN.md#contract-03-4-5); [08 §4.2](DESIGN.md#contract-08-4-2); `UPSTREAM_REQS.md §2.2` |
| D-77 | [M] | An amendment **carries forward** the prior version's commercial content and re-resolves only what the gate produces | The rejected alternative was requiring the caller to resubmit the whole document, which makes every amendment a chance to drop a line by omission and gives the diff no meaning | [04 §3.6](features/04-versioning.md#contract-04-3-6) *Append Amendment* steps 3-8, `§4.2` |
| D-78 | [H] | The payment-authorization outcome is consumed as a **guard input** and never stored as an order fact | Storing it would make the order a second record of a payment state it does not own and cannot keep current; the rejected alternative was a twelfth `payment_pending` state, which would need its own TTL, guards, event and table row to represent a condition that is external and transient. Only the *tolerated-failure* decision is stored, because that is a decision this gear's actor took | [05 §4.3](features/05-preconditions.md#contract-05-4-3), `§4.4`; [01 §3.7](DESIGN.md#contract-01-3-7) |
| D-79 | [M] | **See D-74.** No separate decision remains. | Duplicate of D-74; retained only as a stable historical reference. | D-74 |
| D-80 | [M] | The **pre-hold state is stored** on the aggregate rather than derived from the audit trail | The rejected alternative was reconstructing it from the last transition before the hold — a read that derives state from the audit store, which [01 §4.4](DESIGN.md#contract-01-4-4) forbids outright, and which would break silently if a hold ever followed a non-state-changing transition | [07 §4.1](features/07-hold-and-expiry.md#contract-07-4-1); [01 §3.7](DESIGN.md#contract-01-3-7) `orders_order` |
| D-81 | [M] | The read projection **is the aggregate row**, not a separately maintained materialised view | The rejected alternative was an asynchronously updated projection, which would reintroduce the replication lag [08 §4.2](DESIGN.md#contract-08-4-2) forbids and make the read's freshness a second thing to reason about; the cost is that read shape and write shape are coupled | [08 §4.1](DESIGN.md#contract-08-4-1), `§4.2` |

### D-82 (M) The version reason vocabulary is `{create, submit, amendment}`

**Decision**: `orders_order_version.reason` carries exactly `create`, `submit` or `amendment`.
Creation produces empty version 1 as required by PRD §12 AC-1; submit materialises the draft into
version 2. The six reasons PRD §6.2 lists — approval reflection, hold, resume, cancel, fulfillment
outcome, expiry — name state-only transitions and live on `orders_transition_audit.reason`.

**Rationale**: five transition rows append a version: creation, submit and the three amendment
rows. The PRD's eight reasons all describe audit activity, but only `submit` and `amendment`
identify its specified commercial-version causes; `create` is the required additional reason for
the version-1 creation row. The split is correct; the version vocabulary must state the complete
closed set rather than imply the state-only reasons belong on the version chain.

**Propagated**: [01 §3.7](DESIGN.md#contract-01-3-7), `§4.3`; [02 §4.1](features/02-capture.md#contract-02-4-1); [04 §3.3](DESIGN.md#contract-04-3-3), `§4.5`.

### D-83 (H) The in-flight order cap stays at one; route (b) does not resolve Q-05 *(carries [`ADR/0007`](./ADR/0007-cpt-cf-bss-orders-adr-in-transaction-concurrency.md))*

**Decision**: gate predicate 9 and `orders_inflight_overlap_claim` enforce **exactly one**
in-flight order per `(payer_tenant_id, overlap_scope_key)`, by a partial UNIQUE index on that pair.
*(Amended by D-179: the tuple is now `(payer_tenant_id, resource_tenant_id, overlap_scope_key)`;
the cap of one and everything else below stand.)*
The cap is **not** shared with predicate 7's concurrent-subscription cardinality and is **not**
configurable. Predicate 9 excludes the requesting order from its own count.

**Rationale**: this entry previously decided the opposite — a `slot` column letting the index
express *at most N*, so that raising `maxConcurrentActive` would admit N concurrent in-flight
orders and unblock the partner case of Q-05 by "route (b)". That was wrong on the requirement and
wrong in the algorithm, and both errors are recorded here rather than quietly reverted.

**Wrong on the requirement.** PRD §6.1 states two separate rules. Clause (f) bounds
concurrent-active **subscriptions** per key and says explicitly "default cardinality 1,
**configurable via Catalog/Contract `maxConcurrentActive`**". Clause (g) bounds in-flight
**orders** — "at most one in-flight order per overlap key — a second submit against the same key
while one is in flight **MUST** be rejected" — and carries no configurability clause at all. The
separation is deliberate: how many subscriptions a payer may hold is a commercial policy; how many
orders may be racing toward the same key is a concurrency-correctness rule. Giving them a shared
cardinality overrode a MUST with no amendment and no disclosure.

**Wrong in the algorithm.** The reworded predicate read "the number of in-flight orders holding
the key is below the cardinality resolved for it", dropping the word **other**. An amendment is
issued by an order that is already in-flight and already holds its key, so at cardinality one the
predicate counted the amending order itself and refused every amendment. The self-exclusion is
restored and its load-bearing role is now stated in [03 §4.2](features/03-gate-and-pin.md#contract-03-4-2) so it is not dropped again.

**What this means for Q-05.** Route (b) — leave the key, raise `maxConcurrentActive` — gives a
partner more concurrent *subscriptions* but still admits only one in-flight *order* per key, so
the reseller buying one product for many customer tenants must place those orders **serially**.
Whether that is acceptable is a Product question, and it is the one Q-05 now asks: take route (a)
and bind a resource dimension into the key, accept serialised ordering under route (b), or amend
§6.1(g). Route (b) alone does not resolve the case it was chosen for.

**Rejected**: keeping the `slot` column against a future §6.1(g) amendment. Under the requirement
as written the column can only ever hold `0`, so it is schema surface with no expressible state,
and the review that produced this reversal criticised exactly that shape.

**Propagated**: [01 §3.7](DESIGN.md#contract-01-3-7) `orders_inflight_overlap_claim`; [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) *Run Gate and Submit* step 15,
`§4.2` predicates 7 and 9.

### D-84 (H) One order line produces one subscription — Q-02 answered no

**Decision**: several order lines **MAY NOT** compose into a single subscription. The 1:1
line-to-subscription mapping holds without exception for this phase, and it is now **enforced**:
`orders_line_fulfillment` carries a partial UNIQUE on `(order_id, subscription_id)` and a completed
acknowledgement is refused with the new `acknowledgement-subscription-duplicated` reason where two
lines report the same identifier.

**Rationale**: Q-02 asked whether one tenant's several products could be a single subscription with
a multi-product entitlement set. The premise had already moved: `subscriptions/docs/PRD.md` defines
a subscription as carrying **effective-dated composition** (`PlanLink`, `AddOn`), so a subscription
is already multi-product and the real question was whether the **order** may map many lines onto one
subscription's component set.

Answering no costs nothing and keeps the boundary the design already drew. The alternative — letting
lines compose — breaks the mapping in five places, one of them the `OrderCompleted` payload, which is
a published contract read by three gears. Nothing in this phase needs composition: [02 §2.2](DESIGN.md#contract-02-2-2) already
records that **add-on selection is not expressible** here, because add-on rules are authored in the
pricing gear, and PRD §1 defers a line targeting an existing subscription to the later
`category = change` phase. So the two shapes that would want composition are both already out of
scope, and a no confirms a boundary rather than imposing one.

**What the answer changed, and why it is not merely a confirmation.** The 1:1 claim was asserted in
the `subscription_id` **column comment** — "the spawned subscription; 1:1 with the line" — and
enforced nowhere. The primary key gave one row per line, so a line could not map to two
subscriptions; the reverse was unconstrained. A sibling gear composing two lines and reporting one
identifier twice would have been admitted silently, and `OrderCompleted` would have carried a
non-injective mapping. Declining composition therefore required making the invariant real, not just
recording a preference — an invariant nothing enforces is the defect class this register keeps
finding.

**Obligation this places upstream**: Subscriptions **MUST** create one subscription per activation
intent, since Orders now refuses an acknowledgement that reports otherwise. This is not a new ask —
it is the shape [06 §4.3](features/06-workflow-seam.md#contract-06-4-3) already assumes — but it is now a refusal rather than an expectation, so
it is stated here as the seam's contract.

**Propagated**: [01 §3.7](DESIGN.md#contract-01-3-7) `orders_line_fulfillment`; [06 §3.3](DESIGN.md#contract-06-3-3), `§3.6` *Acknowledge Fulfillment*
step 1, `§4.4`.

### D-85 (H) The cross-gear contract surface is GTS-typed *(closes the review's GTS findings)*

**Decision**: three surfaces that cross a gear boundary are GTS types under the namespace this gear
now claims, `gts.cf.bss.orders.*` — the **eleven events** (one abstract order-event base derived
from the platform event base, eleven final derived types, `data` as the extension field, and
the GTS type identifier carried by each `TypedEvent`), the **refusal reason registry** (derived
error types mapped to canonical Problem categories and stable domain/code pairs, not custom
RFC 9457 `type` URIs), and the **order category** (well-known instances rather
than a database enum). The states, the transition table, the guard set and the permission matrix
stay Rust types and configuration. Specified in [01 §4.7](DESIGN.md#contract-01-4-7), with the boundary stated in `§4.8`.

**SDK alignment (2026-09-22).** The event contract follows `event-broker-sdk/src/gts.rs`, not
conflicting guideline examples: base `gts.cf.core.events.event.v1~`, business content under
`data`, and only `topic`, `allowed_subject_types` and `partition_key` as event traits. Orders
registers subject type `gts.cf.bss.orders.order.v1~` and explicitly sets `partition_key` to
`/subject`, where the subject is the order UUID. Retention is a topic property; no audit-bearing
trait is declared. Foundation §4.4 maps the SDK envelope; §4.7 records required registration and
publication tests, not completed runtime verification. D-95's root tenancy is unchanged. The
shared guideline/SDK discrepancy is recorded separately in `UPSTREAM_REQS.md §2.7`.

**Canonical error alignment (2026-09-22).** Foundation §4.7 maps every Orders-owned refusal to
an explicit `ContractError` code under `orders.v1` and one platform canonical category.
That category determines wire `type`, HTTP status and title; the GTS reason key remains registry
metadata. Canonical preconditions use 400, concurrency conflicts 409 and dependency outages 503.
`authorization-context-changed` retains 409 with canonical title "Aborted", its specific code
and sanitized detail. The complete gate failure report remains available; its primary error is
the first unavailable result, otherwise first failed result, in declared predicate order (refined by D-234: a definitive local refusal ranks first).
Adopted catalog reasons retain upstream identity and require explicit adapter mappings.
These wire changes do not change business guards, audit settlement, access masking or retry
authority. Required mapping, round-trip and non-disclosure tests remain pending implementation.

**Rationale**: four places named GTS as the domain layer's technology and none of it was
specified — no base type, no extension field, no registry, no identifier ownership, no validation
flow. `guidelines/GTS.md` §14 is a checklist for reviewing a DESIGN, and six of its seven mandatory
items were absent, so the claim was a label.

The cost of the gap was not abstract. The design had **hand-built five artefacts the guideline
supplies**: an `envelope_version` column, a dual-major publication plan for rolling out a breaking
event change across three gears, a bespoke reason registry with flat kebab-case names, a
forward-compatibility obligation pushed onto three consumer teams as prose ("a consumer **MUST**
tolerate an unknown event-type value"), and a `category` enum whose own column comment recorded it
as "open to a third value" while a DB `enum` makes a third value an `ALTER TYPE` plus an OpenAPI
widening plus a coordinated client release. `guidelines/GTS.md` §6.6's exemption for plain strings
requires the set be closed and never grow; `category` failed that on its own text.

Namespace ownership, not a demonstrated collision with another gear, motivates qualified reason
identifiers. The earlier example asserting that Subscriptions also raises `version-conflict`
was incorrect: that gear uses `stale_version`. At the wire edge, `error_domain` plus `error_code`
disambiguate Orders reasons; the canonical category remains the Problem `type`. GTS names own
the registry namespace, not the wire category. The open reason set still requires registration
and mapping tests; GTS does not itself implement consumer compatibility or remove dual-major
rollout obligations.

**Timing is why this is cheap.** The gear has no implementation yet, so changing a published
contract costs nothing beyond the documents. The same change after three consumers had built
against `event_type text` would have been the dual-major rollout the design was already planning
for.

**Rejected**: deleting the four GTS claims and recording the deviation. That was an hour of work
against a day, and defensible only if the guideline were aspirational here. It is not —
`gears/bss/ledger` specifies `x-gts-traits.owns_billing_books` concretely and builds an ADR on it,
and `gears/bss/pricing` ships `gts_id!` in Rust, so a BSS gear declining the platform type system
would be the outlier rather than the norm.

**Propagated**: [01 §1.3](DESIGN.md#contract-01-1-3), `§3.3`, `§3.4`, `§3.7` (`orders_order.category` and platform-managed producer persistence), `§4.7`, `§4.8`, `§4.9`; `DESIGN.md §1.3`, `§3.4`.

### D-86 (H) The overlap collision is taken first, and detected as a row shortfall *(closes a CodeRabbit finding on PR #4775)*

*(Amended by D-179: claim tuples are `(payer_tenant_id, resource_tenant_id, overlap_scope_key)`, offered
by payer, then resource-tenant, then overlap-key bytes; the mechanism below is unchanged.)*

**Decision**: claim maintenance is [01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt Transition* **step 17**, placed **before** the
version append at step 18 and before every other document contribution, and it runs on **every**
row. Sub-step 17.1 releases every claim on a terminal target; 17.2 skips the rows that neither
acquire nor release; 17.3–17.6 are the acquiring path and are **check-then-mutate** — partition the
proposed `(payer_tenant_id, overlap_scope_key)` tuples into those the order already holds and
those it does not, acquire only the second group with `ON CONFLICT … DO NOTHING`, and on a row
shortfall release only this attempt's returned claim IDs **without releasing pre-existing claims** before refusing. Then
release superseded claims only after acquisition succeeds. Four properties are normative with it:
that ordering; tuples offered **distinct** (duplicate lines must not cause self-collision);
tuples offered in a common **total order** by payer UUID bytes then overlap-key bytes; and
**READ COMMITTED** (under snapshot isolation
the insert raises a serialisation failure instead of reporting a shortfall). The refusal is a
**failed slice guard** under `§4.1`, so the seven-class taxonomy and its four-of-seven settlement
split are unchanged and no eighth class appears.

**Rationale**: `§3.7` asserted the collision was "settled and audited in the same transaction". A
raw unique violation **aborts** the PostgreSQL transaction, and the audit append is step 20 with
the settle at 24 — so the abort would precede both and the promised refusal could not be produced.
`ON CONFLICT … DO NOTHING` is what keeps the transaction alive to write it, and an unmapped
constraint violation surfacing as an infrastructure error is what `§4.2` forbids.

**Correction (2026-09-22).** Ordering before version insertion prevents phantom versions, but
the earlier assertion that `DO NOTHING` leaves nothing to unwind was incorrect: a multi-tuple
insert can partially succeed. **OL-2 is resolved at design level using existing platform APIs**:
Foundation §3.7 releases only the inserted claim IDs returned by this attempt, checks the update
count, and commits the refusal audit/idempotency outcome in that same transaction. Release or
audit failure rolls back everything. Released history explicitly includes unsuccessful
reservation attempts whose proposed version may never exist; no savepoint or DELETE grant is
introduced. Runtime acceptance tests remain pending. Tuple identity is fixed:
the incoming version's payer participates in both set comparison and release, so changing payer
with the same overlap key acquires the new pair and releases the old pair atomically on success.
Regression acceptance covers that replacement, destination collisions and partial acquisition.
Acquisition remains before version insertion; consequently
`orders_inflight_overlap_claim` carries **no foreign key** to `orders_order_version` — an FK would
force the version to pre-exist the claim, which is exactly the ordering that produced the phantom.
Enforcement stays where D-26 put it: inside the transaction and inside the index.

**Two further defects came out of the first statement of this decision, and both are corrected
here** *(2026-09-11, CodeRabbit Major on PR #4775 plus one found alongside it)*. The first: that
version released the order's existing claims **before** inserting the replacements. A refusal
commits under `ADR/0005`, so the release committed with it — a refused amendment left its order
non-terminal and **no longer holding its own overlap key**, free for another order to take. The
savepoint this decision removed had been covering exactly that, and the removal turned a hidden
dependency into a live defect; "nothing durable to unwind" was true of the version row and false of
the release. Partitioning fixes it and also removes the self-collision **structurally**, since a key
the order already holds is never re-offered.

The second: claim release on a terminal transition was written as part of the acquisition branch,
whose condition is that the contribution carries resolved overlap keys. **No terminal row carries
any**, so the release never executed and every `completed` order would have held its overlap key
permanently — a leak on the happy path, externally indistinguishable from the deliberate
`in_fulfillment` exemption. It is now sub-step **17.1**, ahead of that branch.

**Propagated**: [01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt Transition* step 17 (sub-steps 17.1–17.6) and steps 18–27,
`§3.7` `orders_inflight_overlap_claim`; `ADR/0007`.

### D-87 (H) Ordering follows platform partition semantics; permanent reject may create a gap

**Decision**: *(Amended by D-186: the consumer duties in the last sentence are now obligations C1–C5 of the [event consumer contract](DESIGN.md#contract-01-event-consumer-contract), verified by the `orders-events` golden corpus; the ordering posture is unchanged.)*
`orderId` is the GTS event partition key, so one order's events route to one Event
Broker partition. FIFO holds during ordinary processing and transient retry. The SDK returns
`Retry` for transport/rate-limit faults, retaining the toolkit queue-partition cursor without an
Orders attempt cap. For permanent faults it returns `Reject`; toolkit-db writes the dead letter
and advances the cursor, so later messages — including later events for that order — may proceed.
Consumers must de-duplicate by event ID and validate `orderVersion` and resulting state against
authoritative Orders state.

**Rationale**: the previous strict per-order suspension depended on an Orders-owned table,
selector, shard leases and re-drive protocol. The adopted platform outbox orders by toolkit queue
partition, not by an Orders `(order_id, sequence)` barrier, and `Reject` deliberately advances the
partition. Rebuilding strict suspension beside it would duplicate the platform and allow one
invalid notification to block unrelated orders indefinitely. Orders is the source of truth and
Workflow already requires stale/out-of-order triggers to be resolved through an authoritative
state/version read, so availability-oriented ordering is the supported fit.

**Propagated**: [01 §2.2](DESIGN.md#contract-01-2-2), `§3.2`, `§3.6` *Platform producer-outbox publication*, `§3.7`
*Platform-managed producer persistence*, `§4.4`; `DESIGN.md §4.5`, `§4.7`; `ADR/0006`.

### D-88 (H) The idempotency key is scoped by authorized principal *(closes an IDOR finding)*

**Decision**: `orders_idempotency` gains `principal_scope`, taken from the security context by the
pre-guard and **never** from the request body, and its primary key becomes
`(operation, principal_scope, idempotency_key)`. `order_id` is non-null for every operation except
create, and a settled record whose `order_id` differs from the resolved target refuses as
`idempotency-mismatch` rather than being overwritten. The request fingerprint is defined: a hash
over operation, trigger, resolved target, the three tenant axes, `expected_version` and the
canonicalised contribution — explicitly **not** `correlation_id`, the request instant, headers or
any server-assigned value.

**Rationale**: the key was `(operation, idempotency_key)` over a caller-chosen string, with
`order_id` nullable and outside it, and the fingerprint was defined nowhere in the set. Two
tenants choosing the same human-readable key — `submit-2026-001`, or a client library's sequence
number — collided: at best one received `idempotency-mismatch` on a valid request and was frozen
for the 24-hour window, at worst it resolved another tenant's stored outcome. D-65 solved the
confidentiality half by refusing before the registry is read; it did not address collision.

**Two consequences of the scoping, both stated in [01 §4.2](features/01-foundation.md#contract-01-4-2) rather than glossed.** First, scoping
**adds a fifth outcome**: the same key text from a different principal is a different key, so the
request **executes again** where a global key would have de-duplicated it. An earlier statement of
this decision claimed the four outcomes stayed exhaustive; that was wrong, and it matters because
"zero duplicate orders" is therefore a guarantee **per principal** — for every operation but
create the fingerprint's `order_id` and `expected_version` still catch the duplicate, and create
is the one place cross-principal duplication is possible.

Second, the scope **MUST** be a **stable subject identifier**, and [01 §4.2](features/01-foundation.md#contract-01-4-2) prohibits deriving it
from session, token, `jti`, delegation-proof, replica or transport identity. Any of those can
differ between a request and its own retry, and a scope that moves makes the retry a different key
— so the retry re-executes and the registry becomes a no-op in precisely the crash-and-retry case
it exists for. A deployment that cannot supply a stable identifier **MUST** fail startup.

**Propagated**: [01 §1.2](DESIGN.md#contract-01-1-2), `§3.1`, `§3.2`, `§3.6` *Attempt Transition* steps 1 and 6, `§3.7`
`orders_idempotency`, `§4.2`.

### D-89 (M) The subscription axis of the overlap rule is disclosed as open, not bounded by a timed window

**Decision**: *(Amended by D-180: subscription-side cardinality is advisory at order time and the
submit/activation path is not production-ready until `…-upreq-overlap-activation-atomicity` is
agreed and delivered; Orders' contribution to the open axis is narrowed to one in-flight order per
claim tuple plus predicate 7's interim cross-order count; the recommended upstream mechanism is an
ADR-0007-style slot claim.)* the order axis of the overlap rule is closed in-transaction by
[01 §3.7](DESIGN.md#contract-01-3-7)'s index; the **subscription** axis **MUST** be closed by Subscriptions re-evaluating
`overlapScopeKey` and committing `active` under one reservation boundary, and this gear **MUST
NOT** present its re-check as that boundary. Until that upstream enforcement exists **the gap is
open and this design does not bound it.** Two obligations remain, and both are expressible through
declared interfaces: the re-check is specified as an **early abort** carrying no admission
guarantee, and a collision appearing at or after activation **MUST** surface as an
`overlap-collision` line rejection on the failure-acknowledgement path with compensation evidence.

**Clarified 2026-09-23** (medium-remediation M-8): the outcome carrying `overlap-collision`
depends on whether a subscription has committed `active`. **Before `active`** the collision is a
**per-line rejection** and a **pre-activation abort**. **At or after `active`** it is **not** a
line rejection: it is a **fulfillment failure** carrying `overlap-collision`, and its compensation
evidence **MUST** show that no active subscription remains. "Line rejection" in the Decision
above applies to the pre-activation case only ([03 §2.2](DESIGN.md#contract-03-2-2)).

**Rationale**: the re-check was a bare presence read, and the in-flight claim bounds *orders*, not
active subscriptions — so two activation waves could both pass it and exceed `maxConcurrentActive`.
Atomicity is unreachable from this gear because the committing transaction belongs to another.

An earlier version of this decision bounded the gap on three terms, the first being a **30-second
verdict validity window**. It is withdrawn, because the window was not implementable from anything
this design declares. No port operation, event payload or endpoint response carries a validity
origin or a deadline, and the transition the caller then drives — `spawn-signal`, [01 §4.3](features/01-foundation.md#contract-01-4-3) row 12
— is event-less, so the expiry could not be communicated. "Re-invoke the re-check" placed a
**MUST** on a party this gear cannot signal and whose violation it cannot observe. The design's own
two-phase barrier puts a whole fulfillment wave between the read and the last line's activation, so
one window could not cover N activations. And the origin would be read on one gear's clock and
evaluated on another's, with no declared clock source and no skew bound, so both skew directions
fail silently — while the gate's 10-second breaker hold would consume a third of the window by
itself. A bound nobody can enforce or detect the breach of is worse than a disclosed gap, because it
reads as protection.

The closable form is recorded rather than adopted: a **server-side relative TTL enforced at
`spawn-signal`**, where the engine persists the re-check instant and refuses the spawn signal if it
is older than a configured age — one clock, persisted state, the party that owns the transition. It
needs a column, a guard, a refusal reason and a value, none of which this design set has.

**Propagated**: [03 §2.2](DESIGN.md#contract-03-2-2), `§3.6` *Re-check Activation Preconditions* steps 6 and 9 (renumbered by D-127), [06 §4.3](features/06-workflow-seam.md#contract-06-4-3),
`UPSTREAM_REQS.md` §2.1 (`SUB-O5` enforcement ask; clarified 2026-09-23 to split the pre- and
post-`active` outcomes).

### D-90 (H) Bounded lifetime is a per-state TTL plus two re-entry caps, and the residual gap is disclosed

**Decision**: *(Amended by D-181: Layer 1 now always holds in production. The migration seeds
provisional platform durations as revisioned rows — `draft` 90 days, `submitted` and
`pending_approval` 14 days, `approved` and `on_hold` 30 days, `policy_revision` 1, `provisional` —
and the policy channel refuses to promote a NULL duration to production, so the "unbounded where
unset" residual below and the "disclosed and alerted rather than covered by a fallback" clause no
longer describe a production deployment; at those values the 74 entries sum to at most
74 × 30 = 2,220 days plus scheduler delay. Startup is still never blocked: readiness gating was
rejected for the reason given here. Layer 2, the caps and everything else below stand.)*
**Layer 1** is the per-state TTL, measured from `state_entered_at`, which a resume
restarts — and which holds only where a TTL is configured. An unconfigured TTL **MUST NOT** block
startup, because the values are Product-owned open questions. **Layer 2** is **two re-entry caps**,
one per transition that resets `state_entered_at`: `orders_order.resume_count`, incremented by
[01 §4.3](features/01-foundation.md#contract-01-4-3) row 22, guard `resume-cap-exhausted`, baseline **5**; and
`orders_order.amendment_count`, incremented by rows 18, 19 and 20, guard
`amendment-cap-exhausted`, baseline **20**, owned in [04 §4.1](features/04-versioning.md#contract-04-4-1). No transition resets either.
The full approval/hold graph bounds TTL-covered pre-fulfillment dwell entries by
`3 × (A + 1) + 2 × R + 1` = **74** at these caps, not 26. `74 × T_max` sums configured dwell
budgets under [07 §4.2](features/07-hold-and-expiry.md#contract-07-4-2)'s assumptions; scheduler delay must be added and exempt states excluded.
Without bounded policy values and scheduler delay this is not a hard calendar lifetime bound. Where a TTL is unset that state is
**unbounded**, and that is disclosed in [07 §4.2](features/07-hold-and-expiry.md#contract-07-4-2) and alerted in [07 §3.8](DESIGN.md#contract-07-3-8) rather than covered by a
design-owned fallback.

**Rationale**: `state_entered_at` was the sole dwell input and resume rewrites it, so any actor
holding hold permission could cycle hold/resume and keep an order in `submitted` or `approved`
indefinitely — defeating the bounded-lifetime MUST, and with it [05 §4.4](DESIGN.md#contract-05-4-4)'s declined-instrument
exit, since an order whose payment authorization failed leaves only by expiry. Separately, the
design asserted every in-flight state has a bounded lifetime while tolerating an unset TTL, so the
claim was false wherever the value was missing. The cap closes the first problem at the operation
that creates it. The second is a Product dependency (PRD §15 row 7) and is now stated as one.

**An earlier version of this decision made Layer 2 an absolute order lifetime** measured from
`created_at`, baseline 90 days, enforced by a second sweep pass. It is withdrawn on four grounds,
recorded here because the shape of the mistake is reusable:

* Its enforcement pass shared one deterministic idempotency key with the per-state pass, to avoid double-expiring an order both selected. But refusals settle and replay under their key (ADR-0005), and the key was invariant in the order's version — so once a per-state attempt refused as not-admissible, every absolute-pass request for that order and version **replayed the refusal instead of attempting**. The backstop was inert for exactly the orders something had already gone wrong with, and inert invisibly, since a replayed refusal and a fresh one are the same response.
* It did not close the loop it existed for. The hold that can be cycled indefinitely is the one taken from `in_fulfillment`, which [07 §4.3](features/07-hold-and-expiry.md#contract-07-4-3) exempts from both layers.
* It pre-empted legitimate orders: one gear-level duration cannot distinguish an abandoned order from an enterprise order awaiting a slow approval, and it expired both.
* Its value was a commercial policy with no PRD basis (Q-27), taken autonomously.

The count cap was rejected in that earlier version for permitting `n × TTL` and needing a counter,
a reset rule and a new refusal. That reasoning is inverted here. A finite multiple of a configured
TTL is a **bound**, which is what was asked for; each counter is written by one trigger and reset
by none, so there is no reset rule to get wrong; and the new refusal is the point — a bound whose
breach is a refused, audited transition is observable, where a backstop's failure to fire is not.

**The first statement of this decision capped only resumes, and the loop stayed open**
*(corrected 2026-09-11, from a CodeRabbit Major on PR #4775 and what checking it exposed)*. It
claimed `(cap + 1) × TTL` as the order's bound, which was wrong twice. It bounded **one state's**
repeated dwell, while an order traverses several states each with its own TTL — so it was never an
upper bound on the order. And **amendments reset the dwell too**: rows 19 and 20 target `submitted`
from `pending_approval` and `approved`, the effective target differs from the outgoing state, and
[01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt Transition* step 20.1 sets `state_entered_at`. `approved → submitted → approved`
therefore restarted the clock indefinitely through an entirely uncapped operation — the same defect
on a different trigger.

**Two counters rather than one shared budget**, and the reason is commercial rather than
structural. A single re-entry budget would be tidier — one column, and any future clock-resetting
row covered by construction — but a resume is a **seller-side operational** act and an amendment a
**buyer-side commercial** one, so one budget would let a seller's compliance holds consume a
buyer's ability to correct their own order. The amendment baseline of 20 is argued in [04 §4.1](features/04-versioning.md#contract-04-4-1):
negotiated orders revise two to five times, an amendment is a re-quote rather than an edit, and a
buyer-facing cap must be generous because an order a buyer cannot correct is worse than a
long-lived one.

*Note (2026-09-23)*: the resume cap qualifies PRD §6.3's "A held order **MUST** be resumable".
This decision did not say so. A held order at the cap cannot resume; it leaves only by cancel,
expiry or, for a hold taken from `in_fulfillment`, Workflow's rows 26 and 27 (D-109), never by
completion. The qualification is routed to Product as **Q-31** rather than decided here.

**Propagated**: [07 §1.1](DESIGN.md#contract-07-1-1), `§2.1`, `§2.2`, `§3.1`, `§3.2`, `§3.3`, `§3.6` *Sweep Expired Orders*
and *Hold Then Resume*, `§3.7`, `§3.8`, `§4.1`, `§4.2`, `§4.3`, `§4.4`, `§4.5`, `§5`;
[01 §3.7](DESIGN.md#contract-01-3-7) `orders_order` schema and index list, `§3.6` steps 20.4 and 21, `§4.3` rows 18, 19, 20
and 22; [04 §3.3](DESIGN.md#contract-04-3-3), `§3.6`, `§4.1`; [03 §4.3](DESIGN.md#contract-03-4-3); [05 §2.2](DESIGN.md#contract-05-2-2); `DESIGN.md` §3.2 and §4.2.

<a id="hold-and-expiry-alternative-history"></a>

**Hold and expiry alternative history.** Preserved from Hold and Expiry contracts 07 §3.6 and §4.2; their local section addresses below retain that namespace.

**There is deliberately no second pass.** An earlier draft added an absolute-lifetime pass
selecting on `orders_order.created_at`; it shared one deterministic idempotency key with this pass,
and because a refusal settles and replays under its key, a per-state attempt refused as
not-admissible made every later absolute-pass request replay that refusal instead of attempting.
The backstop was inert for exactly the orders something had already gone wrong with.
[`../DECISIONS.md`](DECISIONS.md) **D-90** carries the reasoning; the restart bound lives on
`resume` and `amendment` instead, which needs no second pass and therefore no key to share.

**Why re-entry caps rather than an absolute order lifetime.** An earlier version of this section
made Layer 2 an absolute lifetime measured from `orders_order.created_at`. It is withdrawn on four
grounds — it could not fire where it mattered, it did not close the loop it was created for, it
pre-empted orders nobody was cycling, and its value had no PRD basis — each recorded in
[`../DECISIONS.md`](DECISIONS.md) **D-90**, which is the single home for that argument. The cost
of the caps, stated here because it is caller-visible: an order needing one more resume or
amendment than its cap allows must be cancelled and re-placed, or the cap raised. That is a
visible, audited refusal with a named reason — the property the absolute bound lacked.

**Additional retained history from 07 §4.2 and §4.3.** Two earlier statements of this section got it
wrong in opposite directions, and D-90 records both.

An earlier version of this paragraph
said the exemption covered "both layers alike", which would have left exactly one hold/resume cycle
uncapped — the `in_fulfillment` one, which is the cycle an operator is most able to repeat and the
one D-90 was written to close.

### D-91 (H) No Orders-owned table is partitioned *(closes a defect found in the 2026-09-11 buildability review)*

**Decision**: no Orders-owned table is range-partitioned. The read access log and Preview gate
outcomes are purged row-wise by the **retention purge sweep**, through the partial indexes their
own tables declare ([08 §3.7](DESIGN.md#contract-08-3-7), [03 §3.7](DESIGN.md#contract-03-3-7)). `orders_transition_audit` is likewise unpartitioned.
Platform-managed toolkit outbox tables follow library migrations and are outside this decision.

**Rationale**: an earlier [01 §3.7](DESIGN.md#contract-01-3-7) range-partitioned the two traffic-driven append-only stores by
month so retention would be a partition drop. Three independent faults.

* It **contradicted both owning slices**, each of which declares a row-level purge against an index it names. A table's owner is authoritative over its own retention mechanism, and this paragraph was the only statement claiming otherwise.
* **A monthly partition cannot express a 7-day retention.** Preview outcomes are kept 7 days, and no month contains only rows older than a week — so the scheme was not merely coarser, it was unable to implement its own declared window.
* **Nothing created the partitions.** No declared worker managed them, and a range-partitioned table with no partition covering the current month **rejects every insert**. For the read access log that is not degraded service: the audit read's access-log write is fail-closed ([08 §4.2](DESIGN.md#contract-08-4-2)), so every audit read would have begun failing at midnight on the first of the month.

An additional Orders worker to manage partitions was the alternative and buys nothing the two
index-driven purges already deliver.

**Propagated**: [01 §3.7](DESIGN.md#contract-01-3-7) *Partitioning*; `ADR/0006` Consequences.

### D-92 (H) The audit-chain verifier is a declared worker, not an assumed job *(closes a defect found in the 2026-09-11 buildability review)*

**Decision**: the **audit-chain verifier** is the gear's fifth Orders-owned advisory-lock-coordinated worker. Scope is
**per order**, walked in a rolling pass. Cadence is a full pass within a design-owned window,
baseline **30 days**. A mismatch **alerts and MUST NOT repair** — the verifier holds only the audit
role's SELECT. It **MUST** skip refused rows, whose NULL `sequence` joins no chain. **Amended by
D-96:** identity removal never changes stored actor references or hashes; verification uses those
references without identity resolution and no erasure record exempts a mismatch from alerting.

**Coordination clarified (2026-09-22).** All Orders-owned workers use toolkit-db's
`Db::lock` / `Db::try_lock` session advisory locks under Foundation §3.8's authoritative roster,
keys, deployment constraints and acceptance checks. These are not TTL leases or fenced locks.
Transactional eligibility/idempotency checks and checkpoint uniqueness must remain safe after
lock-session loss; the Rust guard alone does not prevent stale work. The lock connection must
be direct or session-pooled, never transaction-pooled. `cluster-sdk` was considered but not
selected because its current unfenced guard forbids database writes in its critical section.
Outbox workers and their coordination remain owned by toolkit, not this worker roster. The
implementation and multi-replica/session-loss verification remain pending.

**Rationale**: [01 §3.7](DESIGN.md#contract-01-3-7) required the predecessor-hash chain to be verified periodically and
`§3.8` already alerted on "any chain-verification mismatch" — while naming four Orders-owned workers, none of
them the verifier. `DESIGN.md` §4.2's threat model answers audit tampering with *the chain plus the
absent UPDATE grant*, so that mitigation rested on work nobody owned; a chain nothing checks
detects nothing. Per-order scope is forced by the chain being per-order and by the committed trail
reaching the order of billions of rows inside the 24-month tier, which no single-pass verification
survives. The no-repair posture is the only one consistent with the trail being evidence rather
than state.

**Propagated**: [01 §3.4](DESIGN.md#contract-01-3-4), `§3.7`, `§3.8`; `DESIGN.md` §3.7 inventory, §4.2, §4.3.

### D-93 (H) One catalog version governs a whole submit *(closes a defect found in the 2026-09-11 buildability review)*

**Superseded for the PriceBook target by D-150–D-158 / ADR-0008. Historical rationale follows.**

**Decision**: the catalog **pin-eligibility frontier** is read once, at [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) *Run Gate and
Submit* step 3, and the resulting `catalog_version` governs every catalog-facing resolution in that
run — the adopted predicates, the price evaluation producing the resolved total, and the pin. No
step **MAY** re-read the frontier and an advance mid-run **MUST NOT** be picked up. The same rule
binds an amendment's re-pin and re-evaluation.

**Rationale**: the algorithm resolved the total at step 5 and composed the pin at step 13 (both now in step 5, D-123) with
nothing binding them to one version, and the pricing gear publishes the frontier with an advance
instant *precisely because it moves* — well inside the 2 s submit budget. An order could commit a
total evaluated at one version and a pin frozen at another, with nothing on the document saying so.
Not a money defect, since the total is non-authoritative either way; a defect in the **commercial
record**, which is the artifact this gear exists to be.

**Propagated**: [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) *Run Gate and Submit* steps 3 and 5, `§4.3`.

### D-94 (M) Ports that scale with the basket are called once per run *(closes a defect found in the 2026-09-11 buildability review)*

**Decision**: every port deadline in [03 §2.2](DESIGN.md#contract-03-2-2) is **per port per run, not per line**. A port whose
input scales with the basket — catalog predicates, price evaluation, overlap presence, pin
composition — **MUST** be invoked once with the whole line set and **MUST NOT** be invoked per line.

**Rationale**: the 200-line cap is justified as fitting the 250 ms catalog deadline, which holds
only of a batched call: per-line invocation would need 1.25 ms round trips, which no network port
achieves. The cap and the deadline were consistent only by accident, and an implementation that
fanned out per line would miss the budget at a fraction of the cap while satisfying every other
rule in the slice. Stating the cap without stating the call shape left the requirement inferable
rather than declared.

**Propagated**: [03 §2.2](DESIGN.md#contract-03-2-2) port deadline table; [02 §3.7](DESIGN.md#contract-02-3-7) line cap.

### D-95 (H) Internal lifecycle events use explicit platform-root tenancy

**Decision**: selected by the user during P-1 review. Orders lifecycle events are internal service
notifications with explicit platform-root broker tenancy. Foundation §4.7 defines the envelope
mapping and routing; [08 §4.3](DESIGN.md#contract-08-4-3) defines service authorization. The resource, seller and payer axes
remain business payload fields and retain their existing authorization meaning for Orders actions.
Direct customer, partner and seller access to this internal stream is not granted by this decision.

**Rationale**: Event Broker's design declares platform-root tenancy for inter-service traffic.
The SDK already supports an explicit `TypedEvent::tenant_id()` override, so relying on the
producer context's default tenant would leave the chosen contract implicit. Root tenancy is an
event access scope, not an authorization bypass or a replacement for business tenant boundaries.

**Integration status**: open. The canonical root UUID source and deployed producer/consumer grants
are not yet verified. `ROOT_TENANT_ID` denotes the platform identity, not a claim that an exported
SDK constant currently exists. Platform confirmation and release evidence are required by
`cpt-cf-bss-orders-upreq-event-broker-root-tenancy`
([`UPSTREAM_REQS.md §2.7`](./UPSTREAM_REQS.md#27-event-broker)).

**Propagated**: [01 §4.4](DESIGN.md#contract-01-4-4), `§4.7`, `§3.8`; [08 §4.3](DESIGN.md#contract-08-4-3); `DESIGN.md §4.5`; `UPSTREAM_REQS.md §2.7`.

### D-96 (H) Audit actor references are immutable; identity lifecycle is separate

*(Interim before/after minimization amended by D-204: `display_label`/`internal_notes` are a
keyed HMAC-SHA256 under required Orders configuration, no longer an unkeyed SHA-256 digest.)*

**Decision**: adopt Pricing's PII-minimized, immutable audit-identity approach for both Orders
audit stores. Store opaque principal references from the platform context (source and shared
lifetime assumptions clarified by D-103); manage
identifying attributes and mappings in the identity platform. No identity-erasure UPDATE grant,
historical actor replacement, chain recalculation or verifier exemption is permitted. Orders
retains local authoritative audit storage, transaction-bound writes and existing retrieval and
retention behavior. This is not a dependency on a future shared Audit Gear.

**Supersedes**: D-44's in-place pseudonymisation exception and D-92's erasure-aware verification
exception only. D-44's other data-protection provisions remain in force.

**Rationale**: separating identity data preserves the original audit bytes and avoids a privileged
chain-rewriting path. Pseudonymous references do not by themselves make evidence anonymous;
Privacy/Legal must approve retained content, linkage risks and retention. Before/after values and
other metadata also require minimization, not merely the actor field.

**Integration status**: open identity-platform requirement
`cpt-cf-bss-orders-upreq-audit-identity-lifecycle`. **Amended by D-103:** this is a
shared p2 platform follow-up, not an Orders-only p1 gate or deferred actor format. Provider
lifetime guarantees remain unverified; an IdP deletion alone is not evidence of compliance.
Existing deployed data, if any, needs separately approved
remediation rather than an assumed migration.

**Propagated**: `DESIGN.md §4.3` (authoritative identity contract); [01 §2.2](DESIGN.md#contract-01-2-2), `§3.7`, `§3.8`;
[08 §3.7](DESIGN.md#contract-08-3-7); `ADR/0001`; `UPSTREAM_REQS.md §2.8`.

### D-97 (H) Retain gear-owned transactional audit following Pricing

**Decision**: retain Orders-owned authoritative audit storage, atomic transition/audit writes,
per-order committed-entry hash chains, authorized local retrieval, and the declared verification
worker. Follow Pricing's local audit architecture, not the proposed event-only replacement.
Identity handling follows D-96: no identity-erasure UPDATE grant or historical chain rewriting.
P-1's managed outbox remains lifecycle-event transport; enqueue is not a replacement for retained,
queryable audit evidence. A future platform Audit Gear may receive copies under a separately
specified integration; it is not this design's authoritative store.

**Review disposition**: explicit pushback on the event-only resolution proposed for OL-14/OL-15,
not a claim to implement that recommendation or have reviewer agreement. The user's selected
direction preserves local audit on the Pricing precedent. OL-13 and the remaining contract issues
stay open; neither this decision nor Pricing's existence proves Orders implementation complete.

**Historical precedent report (superseded as evidence of current main by S1-01, 2026-10-05).**
The report below records the earlier pinned review; it must not be used to claim these capabilities
exist now. The current [audit writer](../../pricing/pricing/src/infra/storage/repo/audit_repo.rs)
explicitly writes unsealed rows, leaves sequence/hash fields NULL and does not verify a chain.
The current [audit migration](../../pricing/pricing/src/infra/storage/migrations/m20260926_000008_create_pricing_audit.rs)
is the schema reference. Only scoped same-transaction insertion is the verified reusable pattern.
Orders' integrity, roles, retrieval and checkpoints remain independent implementation obligations.

**Earlier report**: Pricing upstream `main` at
`8aca4d6df17c90f8ecb8b0195e4db4ec40921937` (checked 2026-09-21).
`../../pricing/docs/design/05-governance.md#audit-trail-and-retention`
records D-14/D-135. The implemented
[`audit writer`](../../pricing/pricing/src/infra/storage/repo/audit_repo.rs) receives the caller's
transaction; the `../../pricing/pricing/src/infra/storage/migrations/m20260821_000007_create_pricing_audit_log.rs`
creates the segmented chain and UPDATE/DELETE rejection triggers;
`../../pricing/pricing/src/infra/audit_read.rs` provides authorized local retrieval.
These are architectural precedents, not public SDKs to import from Pricing internals.

| Concern | Pricing precedent | Orders alignment or explicit difference |
|---------|-------------------|------------------------------------------|
| Ownership and atomicity | Local table; append inside mutation transaction | Same pattern; failed audit append aborts the transition transaction |
| Concurrency | `(tenant_id, chain_id, seq)` unique; conflicting append rolls back; independent aggregates | Per-order counter under aggregate lock plus `(order_id, sequence)` uniqueness; same isolation objective, different allocation algorithm; hash tenant binding still needs specification |
| Write protection | Append-only role and database triggers | Both required; no committed-row UPDATE/DELETE. Orders-only exception permits retention-role deletion of expired refusals |
| Identity | Pseudonymous IDs, no names/emails | D-96/D-103 adopt the subject UUID; lifetime/privacy guarantees are shared platform follow-ups, not an Orders-only identity gate |
| Refusals and retention | Denials use the audit writer; design retains audit at least seven years | Refusals remain outside Orders' committed chain, retained 90 days. Not equivalent tamper evidence for refusals. Commercial retention stays Q-07; Pricing's duration is not imported |
| Retrieval | Local Auditor-authorized keyset-paged trail | Local retrieval under Orders' role/delegation matrix; D-101 defines consistent mixed-outcome ordering and explicitly live pagination |
| Verification | Design specifies verifier and per-tenant roll-up; `domain/audit.rs` says both are unimplemented | Orders verifier remains work to implement, not a platform facility. D-100 now specifies local roll-ups and optional independent anchoring, with explicit coverage limits; implementation remains required |

**Remaining acceptance work**: implement the hash bytes/version/genesis and tenant binding specified by D-99;
implement D-101's pagination contract (unknown-order refusal persistence is addressed by D-98); implement D-100's
bounded completeness/checkpoint coverage and retention operations. Require tests for atomic rollback on audit failure, same-order
non-forking and different-order independence, database UPDATE/DELETE rejection, bounded refusal
purging, tenant/delegation isolation, tamper alerts and D-96 identity removal. Pricing's tests are
references for test design, not evidence that these Orders tests have run.

**Propagated**: `DESIGN.md §4.3`; [01 §3.7](DESIGN.md#contract-01-3-7), `§4.4`; D-96 remains the identity decision.

### D-98 (H) Refusal audit distinguishes requested and resolved order identity

**Decision**: `orders_transition_audit.requested_order_ref` preserves the validated target UUID
without an FK; nullable `order_id` links an aggregate only when safely resolved. Early denials
do not look up or lock the aggregate merely for audit, so they leave that link, observed states
and version NULL whether the target is absent or inaccessible. NULL means unresolved, not absent.
Committed entries still require a real aggregate and chain fields. Refusals never acquire a
sequence or predecessor hash; create without a supplied identifier may leave both references NULL.

**Rationale**: the former mandatory FK and state/version fields made the documented no-load
denial path impossible for unknown identifiers, and could require invented facts even for an
existing inaccessible order. No placeholder aggregate, later backfill, idempotency settlement
for authorization denials or target-existence disclosure is introduced. Audit failure remains
fail-closed with a non-disclosing infrastructure error, not a successful audited refusal.

**Read boundary**: requested UUIDs confer no tenant scope. Read inclusion requires authorization
for the order and audit rows; no unknown-order audit endpoint or cross-tenant lookup is added.
The read-access log's existing representation is unchanged: it has its own resolution path.

**Acceptance**: unknown and inaccessible target denials persist without an aggregate lookup or
FK error and have indistinguishable external refusals. Test create and resolved-refusal shapes,
committed-row constraints, audit failure, idempotency non-settlement and scoped read isolation.

**Propagated**: [01 §3.6](features/01-foundation.md#contract-01-3-6), `§3.7`; [08 §3.6](features/08-read-and-authz.md#contract-08-3-6). Resolves D-97's unknown-order refusal storage gap;
hash encoding, pagination ordering, integrity completeness and identity integration remain open.

### D-99 (H) Freeze the Orders audit hash byte contract

**Amended by D-143**: encoding v2, v1 with the new `caller_reason` column appended, is what every
writer emits; v1 stays frozen and the verifier selects the encoding per entry by `hash_version`.

**Amended by D-104**: genesis binds immutable `audit_tenant_id`, not editable resource tenancy;
the pre-implementation v1 field list also includes `subject_tenant_id`. `resource_tenant_id`
remains an observed business snapshot. Foundation §4.4 is the corrected authoritative encoding.

**Decision**: [01 §4.4](DESIGN.md#contract-01-4-4) defines audit encoding v1: SHA-256, Orders-specific versioned row/genesis
tags, length-prefixed NULL-safe fields, fixed field order, binary UUIDs, big-endian numbers,
microsecond UTC instants and exact persisted UTF-8 tokens/text. Add `hash_version` and a nullable
`resource_tenant_id` snapshot to the audit row. Every column except the digest itself is covered.
The first committed sequence is 1 (transactional aggregate counter initialized to 0); genesis
binds the resource tenant and order. Later rows link the preceding committed digest, with a
frozen resource-tenant binding and no gaps. Refusals remain standalone hashes, not chain members;
unknown-target refusals do not invent a tenant. Hash binding grants no access permissions.

**Current-code correction (S1-01):** the former `Pricing domain/audit.rs` reference is absent.
Pricing's current audit repository supplies no hash codec. The Orders encoding/tag/sequence
contract above remains selected; it is implemented and verified independently. The historical
claim about Pricing's sequence starting at zero is not current implementation evidence.
There is no tenant-wide chain lock and no broker dependency in Orders verification.

**Failure/evolution**: encoding failure aborts the append transaction. Unsupported hash versions
fail verification explicitly; old rows and decoders are retained. New evidence fields or changed
encoding require a versioned contract and rollout, never silent hash omission or rehashing.

**Acceptance**: frozen preimage/digest vectors, every-field mutation tests, NULL/empty and framing
tests, database timestamp round trips, genesis/link/tenant checks and transactional concurrency
tests. These remain to implement; this decision specifies the contract, not a running verifier.

**Scope**: resolves D-97's byte encoding/genesis/tenant-binding detail. Whole-chain/tail deletion
and complete-history evidence are now bounded by D-100; a plain hash chain is not a signature or an
independent checkpoint. No integrity guarantee is added to the read-access log.

**Propagated**: [01 §3.7](DESIGN.md#contract-01-3-7) (schema), `§4.4` (authoritative hash contract); D-97 remaining-work register.

### D-100 (H) Tenant audit roll-ups with optional independent anchoring

**Amended by D-104**: “tenant” below means the immutable audit namespace captured at create,
not the order's current resource tenant. Inventory, member keys and roll-up digests use
`audit_tenant_id`; a draft recipient edit is not a missing-order integrity finding.

**Decision**: match Pricing D-135's design baseline: periodic per-resource-tenant roll-ups of
committed order-chain heads, chained locally, with external WORM/object-lock anchoring optional.
Add append-only checkpoint header/member tables; the existing audit worker gains a separately
permissioned append phase, not another business-transaction dependency or tenant-wide write lock.
The authoritative capture, encoding, verification and acceptance contract is [01 §4.4](DESIGN.md#contract-01-4-4).

**Cadence**: capture/reconcile each tenant at least once per 24 hours as a design-owned baseline;
retain the 30-day full rehash/verification window. Capacity/storage tests must validate both.
Reconcile live orders with their transactional counters and the previous checkpoint using one
consistent snapshot. Publish header and members atomically; no partial or known-bad checkpoint.

**Guarantees and limits**: detect shortened/missing trails while aggregate counters survive, and
loss/change of previously captured orders/prefixes while checkpoint evidence survives. Legitimate
chain growth is not a mismatch. No completeness proof is claimed for an entire order lost before
its first checkpoint, an uncaptured suffix removed together with its counter, or a privileged
rewrite of all local evidence. Refusals/read logs remain outside committed-chain coverage.
Independent anchors improve protection only for already anchored evidence; they do not provide
recovery or retroactive proof of completeness. Residency, retention, provider, independent
credentials, schedule and restore behavior must be approved before enabling that stronger claim.

**Pricing status**: this matches its recorded architecture, not a claim of reusing a finished
implementation: Pricing's roll-up and verifier remain unimplemented in the inspected source.
Orders checkpoint/verifier code, frozen vectors, capacity tests and tamper/failure tests are still
required. D-97's completeness design question is answered by this bounded contract, not by an
unqualified claim of tamper-proof storage or completed implementation.

**Propagated**: [01 §3.7](DESIGN.md#contract-01-3-7), `§3.8`, `§4.4`; `DESIGN.md §4.3`; D-97/D-99 remaining-work notes.

### D-101 (M) Audit presentation uses one live keyset order

**Decision**: every audit-read row, committed or refused, sorts by `(created_at ASC, audit_id ASC)`.
The cursor uses that exact exclusive tuple with stored microsecond precision and binary UUID
comparison. Committed `sequence` remains the chain-verification order, never a second display
sort. Scope both D-98 row branches before merging/paging; cursors carry request bindings but no
authorization, and each page rechecks current permissions and delegation.

**Concurrency contract**: live per-page database reads, not a frozen snapshot, complete export
or incremental feed. Late commits behind the cursor can be absent from an ongoing walk; callers
must start a fresh scan to include them. Inserts ahead may appear later; expired refusals may
disappear. Continuation uses the encoded position even if its row has expired. No long-lived
snapshot, new export endpoint, global sequence allocator or refusal lock is introduced.

**Rationale**: the prior description mixed committed-sequence order with timestamp pagination,
which could skip/repeat rows even on a fixed dataset. One immutable tuple resolves that defect,
but does not convert an append timestamp into a database commit watermark. Pricing's local
keyset pattern is a reference, not proof of snapshot-complete enumeration.

**Acceptance**: [08 §2.2](DESIGN.md#contract-08-2-2) specifies fixed-set boundaries, mixed outcomes, precision, duplicate-free
scoped branch merging, cursor request binding, revoked access, retention and concurrent-commit
tests. Existing endpoint page limits and malformed-request mapping are unchanged.

**Amended by D-139**: a cursor that fails structure, version, precision or request-binding
validation returns the registered `cursor-invalid` (400), not an unregistered malformed-request
response, and the token and binding rules above now apply to all five paged collections, not
only the audit read. The ordering, live-view and concurrency contract stays audit-only.

**Propagated**: [08 §2.2](DESIGN.md#contract-08-2-2) (authoritative paging/concurrency contract); [01 §3.7](DESIGN.md#contract-01-3-7); D-97 work register.
Resolves the audit ordering/cursor design inconsistency, not the remaining implementation tests.

### D-102 (H) Bind audit identity to verified platform surfaces; keep lifetime guarantees open

**Decision**: follow Pricing's source of actor identity, authenticated
`SecurityContext.subject_id()`, without adding a profile lookup or Orders-owned identity mapping.
AM/IdP owns identifying data and lifecycle. Do not claim that a UUID proves cross-issuer
uniqueness, non-reuse, pseudonymity or effective erasure. SecurityContext exposes no issuer; the
**D-103 supersedes the deferred format:** use the platform subject UUID as Pricing does while
tracking lifetime/namespace guarantees as shared assumptions. Orders must not invent a missing guarantee or decode untrusted
claims to supply it. Internal jobs require a configured service identity, not an anonymous actor.

**Evidence/status**: upstream commit, implemented context/deprovisioning surfaces and outstanding
questions are recorded in `UPSTREAM_REQS.md §2.8`. The ownership split is known at subsystem level;
concrete deployment/provider, accountable approvers, service configuration and identity/privacy
guarantees still need confirmation. No external outreach or approval is implied.

**Status amended by D-103**: `cpt-cf-bss-orders-upreq-audit-identity-lifecycle` remains
open as a shared p2 follow-up, not an Orders-only p1 gate. Source inspection cannot confirm
namespace/non-reuse or provider deletion/restore policies. Orders still owns service-principal
configuration and local acceptance tests; ordinary security/privacy review is unchanged.

**Propagated**: `UPSTREAM_REQS.md §2.8` (verification and owner questions); `DESIGN.md §4.3`.

### D-103 (H) Reconcile Orders audit with Pricing's baseline and explicit domain differences

**Tenancy clarification (D-104)**: the resource tenant at create initializes the immutable
audit namespace; later draft changes remain business snapshots and do not rebind the chain.

**Decision**: one architectural audit pattern, separate gear-owned evidence and transactions.
Pricing Governance G4/D-14/D-135 and its audit writer/domain encoding are the precedent; they
are not a shared SDK or a claim that its missing verifier/roll-up are implemented. Orders does
not introduce a second identity system or require a future Audit Gear. Shared-mechanism changes
should be reconciled with Pricing/platform owners before either gear presents them as a common
contract; this decision changes Orders documentation only.

| Shared baseline | Orders-specific adaptation retained |
|-----------------|-------------------------------------|
| Local durable append in the business transaction | Transition and refusal outcomes, delegation proof, reason, order version and administrative delta |
| Pseudonymous `SecurityContext.subject_id()`; IdP owns identifying data | Existing text actor is canonical lowercase hyphenated UUID; Orders actor_class and configured scheduler/service identities remain |
| Append-only permissions/triggers; aggregate-segmented chains | Resource tenant + order bind the chain; existing aggregate lock/counter starts at 1; refusal rows remain unchained with bounded deletion |
| SHA-256, versioned domain separation, NULL-safe framing and deterministic encoding | D-99's Orders field list/tag/encoding stays separate from Pricing's; do not couple private helper code or assert wire-byte compatibility |
| Periodic tenant roll-ups/verifier; optional asynchronous external anchoring | D-100 supplies concrete Orders checkpoint tables, consistent capture and capacity-test baselines; these are refinements, not a claimed Pricing implementation or new shared framework |
| Authorized paginated local retrieval | Orders role/delegation scopes, unknown-target denials and explicitly live D-101 pagination; no claim of snapshot-complete export |
| Policy-owned retention and PII minimization | Orders' 90-day refusals/read logs and Q-07 commercial-retention question; do not copy Pricing's seven-year duration |
| Correlation carried as evidence | Orders retains its nullable cross-gear process correlation, distinct from Pricing's mandatory per-request correlation; no silent semantic substitution |

**Identity reconciliation**: use the platform subject UUID now, as Pricing does, without waiting
for a new actor-reference format. Cross-issuer uniqueness/non-reuse, migration and IdP
deletion/cache/backup/restore behavior remain **shared platform assumptions and follow-ups**,
not verified guarantees. Reclassify `…-upreq-audit-identity-lifecycle` to p2 shared follow-up;
supersede D-96/D-102's Orders-only p1 release gate and D-102's deferred actor representation.
This does not waive normal security/privacy/retention approval or permit a known unsafe identity
configuration. Keep Orders-side tests for faithful subject capture, minimization, authorization,
system transitions, and unchanged evidence when profiles disappear.

**Do not copy implementation gaps**: Pricing's verifier/roll-up remain unfinished and its
window-activation path does not persist its nil actor to the audit log. Orders retains its
requirement to audit every engine transition, including scheduler transitions under a real
configured service identity. No manufactured human principal or silent audit omission is allowed.

**Status and boundaries**: D-98 through D-101 remain Orders-specific resolved contracts, not
claims of exact parity. Their implementation/capacity tests are still outstanding. D-97's
pushback on event-only audit is unchanged, and reviewer/platform agreement is not presumed.
Extracting a common toolkit implementation, modifying Pricing, or contacting owners is separate
work and is not performed by this decision.

**Propagated**: `DESIGN.md §4.3`; [01 §3.7](DESIGN.md#contract-01-3-7); [08 §3.7](DESIGN.md#contract-08-3-7); `UPSTREAM_REQS.md §2.8`;
amendment notes on D-96/D-97/D-102.

### D-104 (H) Resolve audit tenancy, refusal scope, creation shape and writer boundaries

**Decision**: correct the four P-2 consistency findings:

* Immutable `orders_order.audit_tenant_id` is initialized from the authorized resource tenant
  at create. Genesis and roll-ups use it; audit `resource_tenant_id` snapshots the current axis.
  Draft edits do not move chain ownership, and historical ownership grants no current read access.
* Mandatory trusted `subject_tenant_id` scopes unresolved refusals. **Amended during PDP
  simplification:** the engine appends through private persistence under restricted service
  database authority, not a separate `audit-unresolved × append` PDP grant. Operational readers
  still require explicit PDP-scoped `audit-unresolved × read`. No customer default grant or
  UUID-only join is introduced; [08 §3.5](DESIGN.md#contract-08-3-5) defines the business-operation boundary.
* Committed create has NULL `from_state`, `to_state = draft`, version 1 and sequence 1. Other
  committed entries require a prior state; no synthetic lifecycle enum is added.
* Engine-only ownership covers transition evidence/business writes. Audit workers append
  checkpoints, verification is read-only, and access-log/retention roles retain narrow duties.

**Amends**: D-98 refusal scope, D-99 hash coverage/genesis, D-100 inventory namespace, D-101
unresolved-row access and D-103 tenancy adaptation. This corrects the unimplemented v1 contract;
if an earlier writer has shipped, use a new encoding version, never rehash persisted evidence.

**Acceptance**: draft recipient change preserves one chain and checkpoint namespace without
granting the old tenant access; two subject tenants guessing one target cannot read each other's
denials without an explicit operational grant; service-scoped denial insertion works; creation
accepts NULL prior state but other commits reject it. Verify worker/engine/retention permission
separation and ownership-field hash vectors. These remain runtime implementation tests.

**Propagated**: [01 §2.2](DESIGN.md#contract-01-2-2), `§3.6–3.8`, `§4.1`, `§4.4`; [08 §3.6](features/08-read-and-authz.md#contract-08-3-6); `ADR/0001`; `DESIGN.md §4.3`.

### D-105 (H) Complete the create branch and qualify audit-read visibility

**Decision**: trigger create dispatches to Foundation §3.6's dedicated transaction branch,
not the existing-order load/lock algorithm. Authorize first; fingerprint uses a stable create
sentinel and no generated values. Serialize same-key creates on the scoped idempotency row.
Refusals persist audit/owned settled outcome without an aggregate. Success atomically inserts
the aggregate, version 1, create audit sequence 1 and settled result containing the generated
order ID. Replay returns the original ID/number; all failures roll back. Create stays event-less.
Capture delegates identity/number allocation and returns the engine result rather than minting
a replay identity. Foundation specifies live/expired leases, concurrency and crash tests.

**Read correction**: the audit-read diagram and prose promise only authorized rows. Unresolved
denials require the additional subject-tenant-scoped operational grant; ordinary order access
does not promise an exhaustive denial listing. Permissions remain unchanged.

**Review disposition**: addresses P2-H1/P2-M1 in the 2026-09-21 handoff at design level.
Implementation tests and reviewer acceptance of the Pricing-style alternative remain outstanding.

**Creation completeness clarification (2026-09-22).** Foundation §3.6 explicitly maps every
aggregate and first-version initialization field, including trusted initiating actor, shared
creation/state-entry timestamp, NULL predecessor and counter defaults. Capture derives the
actor from SecurityContext rather than accepting an actor override. The no-aggregate-lookup
test applies to new creation only; successful replay permits the scoped read required to
reauthorize disclosure against the existing order's current relationships.

**Propagated**: [01 §3.6](features/01-foundation.md#contract-01-3-6), `§3.7`; [02 §3.6](features/02-capture.md#contract-02-3-6); [08 §3.6](features/08-read-and-authz.md#contract-08-3-6); P-2 handoff status.

### D-106 (H) Store the sales path on the aggregate at create

**Decision**: `orders_order` carries `sales_path` (enum `self_service` | `partner_placed`,
NOT NULL, immutable). The create branch writes it once from the PDP-authorized access path of
the creating request — the delegated partner path gives `partner_placed`, the resource-tenant /
direct-customer path gives `self_service` — and no transition updates it. Separate acceptance
recording copies it into `orders_acceptance.recording_path`; the recording-party bar keys on it
plus `orders_order_version.actor` of version 1 (creator) and of the submitted version (submitter).
Neither is ever inferred from the current actor.

**Rationale**: 05 copied a "stored sales path" that no table stored; only `initiating_actor`
existed, which names a principal, not the path it was authorized through. Deriving the path later
from the current actor would let a re-scoped or re-delegated principal change which acceptance
rule applies to an order. Closes slice-lens review finding H-5 (2026-09-23).

**Propagated**: [01 §3.6](features/01-foundation.md#contract-01-3-6), `§3.7`; [05 §3.6](features/05-preconditions.md#contract-05-3-6), `§4.2`; [08 §4.3](DESIGN.md#contract-08-4-3).

**Amended by D-140**: Orders cannot observe the PDP-authorized path (D-111), so the create writes
`partner_placed` iff the allowed create request carried a delegation proof reference, and
`self_service` otherwise, until a PDP path marker replaces the proxy.

### D-107 (H) One precedence for the acceptance-required election, recorded by source

**Decision**: whether customer acceptance is required resolves by one precedence, highest
first: the referenced contract's declaration (where a contract is referenced) > the seller-scope
`acceptance_required` election > the platform-scope election > the safe fallback (acceptance
required). `orders_acceptance.requirement_source` gains `seller`, becoming `contract` | `seller`
| `platform_default` | `volunteered`; `platform_default` covers both the platform-scope row and
the unset fallback, which the guard already distinguishes by recording an explicit-row versus
fallback read. The election is keyed `(election, scope, scope_id)` only — it has no sales-path
dimension. An unavailable contract or policy input is an input failure, never the fallback.

**Clarified 2026-09-23** (LOW L-05.1): the guard does **not** record whether it read an explicit
platform-scope row or the unset fallback; both resolve to `requirement_source = platform_default`
and no record distinguishes them ([05 §4.1](features/05-preconditions.md#contract-05-4-1)). The "already distinguishes" clause above is withdrawn.

**Rationale**: [05 §4.1](features/05-preconditions.md#contract-05-4-1) named only contract-then-platform-default while §3.7/§4.3 stored seller
elections that override platform ones, and §4.2 spoke of an election "for the partner path"
that the key cannot express; the enum then had no value to record a seller election, so a
seller-decided requirement would have been written down as a platform default. Closes
slice-lens review finding H-6 (2026-09-23).

**Propagated**: [01 §3.7](DESIGN.md#contract-01-3-7); [05 §3.1](DESIGN.md#contract-05-3-1), `§3.2`, `§3.5`, `§3.6`, `§3.7`, `§4.1`, `§4.2`, `§4.3`.

### D-108 (H) The gate resolves the overlap key from the catalog registry at the fixed version

**Superseded for the PriceBook target by D-150–D-158 / ADR-0008. Historical rationale follows.**

**Decision**: [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) *Run Gate and Submit* gains step 4, between fixing the catalog version and
the parallel port resolution: resolve each line's `catalogSubscriptionProductKey` from the catalog
registry (Product & SKU) **at the run's fixed `catalog_version`**, in one call batched for the
basket under its own 250 ms deadline, and build each line's overlap key
`(payer_tenant_id, catalogSubscriptionProductKey)` from it — the product key fills
`orders_order_line.overlap_scope_key`, the payer is the claim tuple's `payer_tenant_id`. The
overlap-presence read, predicates 7 and 9 and the submit's claim set consume that resolved key;
no step derives its own. A line with no key refuses `overlap-key-unresolvable`; outage or deadline
refuses `catalog-product-key-unavailable`. The activation re-check reads the key stored on the line
and does not re-resolve it. The gate therefore has **nine** outbound operations; the submit
resolution ceiling rises to **2.25 s** (eight operations) and Preview's to **2.5 s** (nine),
amending the D-42 figures. Amendment resolves the key through the same operation. The registry
operation is raised upstream as `cpt-cf-bss-orders-upreq-catalog-subscription-product-key`,
tied to Subscriptions `SUB-G1` / PR #4177.

**Rationale**: the default overlap key was adopted as `(payerTenantId, catalogSubscriptionProductKey)`,
yet no step or port produced the product half, while the key fills the line column, the in-flight
claim index, predicates 7 and 9 and the activation re-check. Two implementers would derive it
differently — from SKU, plan or product fields — and the one-in-flight rule and Subscriptions'
cardinality rule would then key on different things. The key is registry-owned (`SUB-G1`), so it
is read from the registry rather than computed, and at the fixed version so it cannot drift from
the pin within one run; the re-check reads the stored key so it tests exactly what the claim holds.
Closes slice-lens review finding H-2 (2026-09-23).

**Propagated**: [01 §3.7](DESIGN.md#contract-01-3-7), `§4.7`; [03 §2.2](DESIGN.md#contract-03-2-2), `§3.3`, `§3.5`, `§3.6`, `§3.8`, `§4.2`, `§4.3`, `§5`;
[04 §3.6](features/04-versioning.md#contract-04-3-6); `DESIGN.md §3.5`; `UPSTREAM_REQS.md §2.10`, `§3`; ADR-0003; Q-11.

### D-109 (H) A held in_fulfillment order reaches a terminal state through Workflow without resuming

**Decision**: *(Amended by D-182: rows 26 and 27 are no longer the only terminal exits of an
`in_fulfillment`-origin hold; row 29, `on_hold → fulfillment_failed` on the two-person operator
trigger `force-fail-unreconciled`, is admitted from the same pre-hold state under the same
`prehold-not-in-fulfillment` guard once the overdue window has elapsed post-spawn, and the table is
twenty-nine rows. Rows 26 and 27 and everything below stand.)*
[01 §4.3](features/01-foundation.md#contract-01-4-3) gains rows 26 and 27. Row 26 is `on_hold → fulfillment_failed` on
`acknowledge-failed` and row 27 is `on_hold → cancelled` on `cancel-workflow-mediated`, both
admitted only when the stored pre-hold state is `in_fulfillment`; any other pre-hold state is
refused `prehold-not-in-fulfillment`, a new reason registered by `06`. Otherwise they carry the
guards of rows 14 and 16 unchanged: compensation evidence asserting no active subscription remains
for row 26, and the shared cancel guard of [06 §3.6](features/06-workflow-seam.md#contract-06-3-6) for row 27, with its pre-spawn window and its
evidence requirement (amended by D-134: the evidence guard now sits on the
`cancel-workflow-mediated` rows rather than in the shared guard, and applies before the spawn
signal as well as after it). They emit `OrderFulfillmentFailed` and `OrderCancelled`. They add no
endpoint and no PDP action: they are reached through the existing `fulfillment-acknowledgement` and
`workflow-cancel` operations, so only the Workflow service principal can drive them.
`acknowledge-completed` gets **no** `on_hold` row: a held order must resume (row 22) before it
completes. The table is twenty-seven rows. The resume cap of row 22 stays unconditional.

**Rationale**: suppose Workflow holds an order from `in_fulfillment` after the spawn signal
(`on_hold` plus a manual task, per the Workflow PRD) and the resume cap is already exhausted. That
order had no terminal exit. Rows 13, 14 and 16 left from `in_fulfillment` only. Row 23's cancel
applies the pre-hold guard, which refuses every non-Workflow caller with
`direct-cancel-window-closed` once the spawn signal is recorded, and the matrix gives Workflow
cancel via `workflow-cancel` only. Row 24's expiry is exempt for that pre-hold state, and row 22
refuses `resume-cap-exhausted`. The order sat non-terminal forever, and [07 §4.1](features/07-hold-and-expiry.md#contract-07-4-1)'s claim that "an
order at the cap can still be cancelled" was false for it. Exempting the `in_fulfillment` cycle
from the cap would reopen the uncapped loop D-90 closed, so this decision adds the terminal exits
instead and keeps D-90 intact. Row 26 is an edge absent from the PRD diagram and is disclosed as
such in [01 §4.3](features/01-foundation.md#contract-01-4-3). Closes slice-lens review finding H-1 (2026-09-23), which slices 06 and 07 found
independently.

**Propagated**: [01 §3.6](features/01-foundation.md#contract-01-3-6), `§4.3`, `§4.4`, `§4.7`; [06 §3.3](DESIGN.md#contract-06-3-3), `§3.6`, `§4.1`, `§4.4`; [07 §4.1](features/07-hold-and-expiry.md#contract-07-4-1),
`§4.3`; [08 §4.3](DESIGN.md#contract-08-4-3); `DESIGN.md §4.8`; `DECOMPOSITION.md`; ADR-0001; ADR-0002; ADR-0004.

### D-110 (H) A stale Workflow result is refused version-conflict: the version check precedes admissibility for workflow triggers

**Decision**: [01 §4.1](DESIGN.md#contract-01-4-1) defines the **workflow-trigger class** — the triggers of the five
Workflow seam operations, which only the Workflow service principal may issue:
`reflect-approval-required`, `reflect-approval-not-required`, `reflect-approval-granted`,
`reflect-approval-denied`, `begin-fulfillment`, `report-spawn-signal`, `acknowledge-completed`,
`acknowledge-failed` and `cancel-workflow-mediated` ([01 §4.3](features/01-foundation.md#contract-01-4-3) rows 7–16 except 15, and rows 26
and 27). The normative guard order stays one statement, total with one declared exception:
authorization, then idempotency resolution, then — for a workflow-class trigger — the version
check before state-table admissibility, and for every other trigger admissibility then the version
check, then slice guards in registration order. [01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt Transition* step 10 implements
it as branch 10.1, which settles, audits, commits and returns `version-conflict` naming the current
version exactly as step 12.1 does; no step is renumbered, and draft-revision handling is unchanged
because no draft trigger is in the class.

**Rationale**: a workflow result is always computed against a specific version, so when that
version has moved the moved version is the true cause. Under the old order, an amendment from
`pending_approval` or `approved` (rows 19 and 20) moved the order to `submitted`, so a late
`reflect-approval-granted`/`-denied` or `begin-fulfillment` carrying the superseded version found
no row for `(submitted, trigger)` and was refused `not-admissible`, never reaching the version
check — contradicting [04 §4.4](features/04-versioning.md#contract-04-4-4), its sequence diagram, and PRD §12 AC 5a's "machine-readable
stale-version reason", which this decision satisfies. Rejected alternative: keep the order and
extend `not-admissible` to carry the current version — it would give one condition two names
(D-38) and make the sibling gear infer staleness from a state it never saw. Closes slice-lens
review finding H-4 (2026-09-23).

**Propagated**: [01 §2.1](DESIGN.md#contract-01-2-1), `§3.6` step 10, `§4.1`; [04 §3.6](features/04-versioning.md#contract-04-3-6), `§3.8`, `§4.4`; [06 §3.3](DESIGN.md#contract-06-3-3), `§3.6`,
`§3.8`, `§4.1`; ADR-0001; ADR-0005; D-06 (amended).

### D-111 (H) The platform PDP evaluates delegation proof; Orders forwards it and maps the denial

*(REST carrier fixed by D-202: the optional `X-Delegation-Proof-Ref` header.)*

**Decision**: Orders passes the delegation proof reference the caller presents on the request,
unvalidated, to the platform PDP as request context on every PolicyEnforcer call, for reads and
writes alike. PDP policy decides whether an authorized path requires delegation and whether the
supplied proof is valid for it; the answer is allow with constraints or deny with a reason. Orders
never classifies a path as delegated and never verifies a proof's signature, expiry, scope or
revocation. Orders keeps two duties: it records in the audit or access-log entry the proof
reference PDP reports accepting — or, while the PDP response cannot name one, the reference
supplied on the allowed request, recorded as supplied rather than verified; and it maps PDP
denial reasons to the existing registered reasons `delegation-proof-required` (required proof
absent) and `delegation-proof-invalid` (supplied proof rejected), still owned by `08`, with the
hidden-target case still answering `order-not-found`. The path semantics become a PDP policy
obligation unchanged: missing or invalid required proof refuses that path and does not veto an
independently complete non-delegated path. Served-read logging treats a read as delegated when a
proof reference was supplied on the allowed request, since Orders cannot observe the path PDP used.

**Rationale**: `08` asked Orders to decide whether "the PDP-authorized path requires delegation",
but PolicyEnforcer/AccessScope returns compiled property constraints with no marker saying which
alternative is delegated, so the local branch could not be built. Evaluating the proof in policy
aligns with the platform rule "Use PolicyEnforcer from authz-resolver-sdk for all authorization
decisions" and with D-34's no-local-evaluator stance; `EnforcerError::Denied` already surfaces a
`DenyReason.error_code`, so only a request carrier and agreed codes are new, recorded under
`…-upreq-pdp-policy-integration`. Rejected alternative: PDP returns a per-alternative delegation
marker and Orders enforces proof — it needs a new PDP response contract and still keeps a local
proof evaluator beside the PDP. Closes slice-lens review finding H-7 (2026-09-23).

**Propagated**: [08 §2.1](DESIGN.md#contract-08-2-1) `…-constraint-delegation-proof-required`, §3.1, §3.5, §3.6 common read
wrapper step 3, *Read One Order* steps 1, 3, 4 and 9, *List Orders* steps 4, 5.2 and 7, §4.3,
§4.4; [01 §1.2](DESIGN.md#contract-01-1-2), `§3.6` *Attempt Transition* step 1 and *Create Transition* step 1, `§3.7`
`orders_transition_audit.delegation_proof_ref`; [05 §2.1](DESIGN.md#contract-05-2-1); `DESIGN.md §4.2`;
`UPSTREAM_REQS.md` `…-upreq-pdp-policy-integration`, `…-upreq-delegation-proof-credential`, §1.2,
§3; D-32 (amended).

**Amended by D-141**: the mapped reasons `delegation-proof-required` / `delegation-proof-invalid`
are returned only on untargeted requests (list, create, preview); a targeted request answers
`order-not-found` on a proof denial, with no "hidden-target" test left to decide.

### D-112 (M) A missing expected version is rejected at the boundary before authorization, unaudited

**Amended by D-142**: `expected-version-required` is one of the specific boundary reasons ahead of
the `request-invalid` fallback for every other schema-validation failure.

**Decision**: every transition against an existing order — ordinary, workflow-class or internal
worker trigger — carries an expected version. A missing or unparseable one is rejected during
boundary input validation, **before** authorization, with the new engine-owned reason
`expected-version-required` (`EXPECTED_VERSION_REQUIRED`, canonical `FailedPrecondition` with the
SDK's same-class `Http::status_code(428)` transport override). The rejection appends no audit
entry and probes, claims or settles no idempotency record; it is input validation like
`page-size-exceeded`, not one of [01 §4.1](DESIGN.md#contract-01-4-1)'s seven refusal classes. Create is unaffected. A
present, well-formed but stale expected version remains the engine's `version-conflict`.

**Rationale**: `04` required a caller omitting the expected version to be refused, but no reason,
status or position in the guard order existed, and `expected_version` is part of the idempotency
fingerprint, so a request lacking it has no fingerprint to compare or settle against.
Validation precedes authorization under [01 §4.7](DESIGN.md#contract-01-4-7)'s boundary flow, so the rejection needs no
authorization, audit or registry work and discloses nothing about the target. HTTP 428
(Precondition Required) names the missing-precondition case exactly; the registry's "no Orders
overrides" rule gains this one declared same-class exception. Rejected alternative: treat an
absent version as a mismatch and refuse it as the engine's `version-conflict` — it would give a
malformed request an audited, settled refusal naming the current version to an unauthorized
caller, and give two conditions one name. Closes slice-lens review finding M-21 (2026-09-23).

**Propagated**: [01 §3.3](DESIGN.md#contract-01-3-3), `§3.6` *Attempt Transition* input, `§4.1`, `§4.7`; [04 §2.1](DESIGN.md#contract-04-2-1), `§4.1`;
[06 §3.3](DESIGN.md#contract-06-3-3); `DESIGN.md §3.3`.

### D-113 (M) An unavailable input never masks an earlier-registered failing guard; precluded inputs are not unresolvable

**Decision**: [01 §4.1](DESIGN.md#contract-01-4-1) defines a **precluded input** — a declared guard input a slice deliberately
did not resolve because a guard registered earlier on the same row already fails on its resolved
inputs. A precluded input is never unresolvable and never enters [01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt Transition*
step 3; a slice may preclude only on a guard whose failure is fixed by the request and by stored
content the step-12 version check pins. When an input is genuinely unresolvable, step 3.1, after
the steps 10–12 checks, evaluates in registration order every slice guard registered ahead of the
first unresolved input whose own inputs resolved, and settles the first that fails as an
engine/guard-only refusal without an assessment (sub-step 3.1.1); only if none fails does it
settle the selected unevaluable reason with the gate assessment (sub-step 3.1.2). No top-level
step is renumbered.

**Rationale**: `04` *Append Amendment* step 2 skips dependent external work once a local guard
fails, but engine step 3 treated any unresolved input as settling the unevaluable reason without
running earlier guards, so an exhausted amendment cap with a catalog outage answered 503 instead of
`amendment-cap-exhausted` — contradicting `01`'s own rule that earlier-registered non-gate guards
keep precedence. Rejected alternative: never skip, always resolve the full gate — it spends every
external call on a request already known to fail and still leaves an outage able to mask the
earlier reason. Closes slice-lens review finding M-19 (2026-09-23).

**Propagated**: [01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt Transition* step 3 and the diagnostic settlement contract,
`§4.1`; [04 §3.6](features/04-versioning.md#contract-04-3-6) *Append Amendment* step 2.

### D-114 (M) A targeted denial answers 403 only when the caller may read the target, otherwise 404

**Decision**: [08 §3.6](features/08-read-and-authz.md#contract-08-3-6) common read wrapper item 2 defines the denial mapping once, for reads and
writes: a PDP denial of a request with **no target** (List, Create, Preview) maps to
`operation-not-permitted-for-actor` (403). A denial of a **targeted** request maps to 403 only if a
follow-up `order × read` decision on that target — made on the deny path only — allows it;
otherwise it maps to `order-not-found` (404). A denied `order × read` is therefore always
`order-not-found`. Delegation-proof denials keep D-111's mapping and its hidden-target rule.
[01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt Transition* and *Create Transition* step 1 cite the same rule.

**Rationale**: `08` mapped a "target-independent" denial to `operation-not-permitted-for-actor`,
but nothing produced that classification; *Read One Order* mapped every other denial to
`order-not-found`, and `01`'s write paths said only "if denied, return", so a registered reason
was never produced and a seller denied a draft `PATCH` could not be told why. Asking PDP whether
the caller may read the target discloses nothing the caller could not already learn by reading it.
Rejected alternative: always 404 for targeted requests and 403 only for untargeted ones — it hides
a plain permission error from a caller who can see the order. Closes slice-lens review finding
M-38 (2026-09-23).

**Propagated**: [08 §3.6](features/08-read-and-authz.md#contract-08-3-6) common read wrapper item 2, *Read One Order* step 3, *List Orders* step 4,
`§4.3` seller-scope test; [01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt Transition* step 1 and *Create Transition* step 1.

**Amended by D-141**: a delegation-proof denial of a targeted request is always
`order-not-found`, whatever the follow-up answers; the 403 rule above holds for every other
denial.

**Clarified 2026-09-23** (remediation-review R-M1, applying M-37's equal-cost rule of D-68): the
follow-up `order × read` is made on every denial of a targeted request whose action is not
`order × read`, on **both** arms — on the no-row arm with the target ID and an empty property set,
its result discarded — so a hidden and a nonexistent target make the same PDP calls and a PDP
outage on either call is the sanitized 503. A denied `order × read` makes no follow-up on either
arm. Propagated: [08 §3.6](features/08-read-and-authz.md#contract-08-3-6) common read wrapper item 2 and route census test; [01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt
Transition* step 1.

**Note 2026-09-24** (re-review RR-L2): the rationale's "hides a plain permission error from a
caller who can see the order" no longer covers delegation-proof denials. Under D-141 a targeted
proof denial answers `order-not-found` even to a caller who can see the order, deliberately; the
403 argument holds for every other denial only. The classified proof reason is kept on the
refused read access-log row's operational-only `internal_refusal_detail` ([08 §3.7](DESIGN.md#contract-08-3-7)), never
returned to the caller, so AC-16's proof fact is not lost.

### D-115 (M) One closed actor class, derived from the authenticated context alone

**Decision**: *(Refined by D-226: the PEP's user-only and finite-ID rules and the engine's row actor rule read one decision class from these configured identities; the subject type only tightens an unconfigured subject to `service` there.)* [01 §3.7](DESIGN.md#contract-01-3-7) defines one closed actor-class enumeration shared by
`orders_transition_audit.actor_class` and `orders_read_access_log.actor_class`, derived only from
the authenticated context compared with configured identities: `system` is the configured Orders
worker actor (schedulers and sweeps), `service` is any of the configured Workflow, Subscriptions
and Billing service principals, and `user` is every other authenticated subject. It is never
derived from the permission-matrix column or the PDP path that authorized the request, and grants
nothing. Event-consumer reads log as `service`; a payer reader logs as `user`. `06`'s shared
cancel guard tests for the configured Workflow service principal (`service` class).

**Rationale**: `08`'s access log required an `actor_class` with no value set, and logged service
consumers as `system` while `01` reserved `system` for scheduler transitions; `08` also declared
Payer Reader a permission, not a class, and under D-111 Orders cannot see which PDP path was
used, so a path-derived class was unbuildable. The authenticated context is the only source
Orders can trust and observe. Rejected alternative: drop the column — it loses the
worker/service/user distinction the audit-completeness NFR and operational review rely on.
Closes slice-lens review finding M-43 (2026-09-23).

**Propagated**: [01 §3.7](DESIGN.md#contract-01-3-7) `orders_transition_audit.actor_class`; [08 §3.7](DESIGN.md#contract-08-3-7)
`orders_read_access_log.actor_class`, `§4.3`; [06 §3.3](DESIGN.md#contract-06-3-3), `§3.6` *Evaluate Cancel From
In-Fulfillment* step 3; `UPSTREAM_REQS.md §2.8`.

### D-116 (M) Draft order and line edits have algorithms, and an unknown line answers line-not-found

**Decision**: [02 §3.6](features/02-capture.md#contract-02-3-6) specifies the two algorithms its declared endpoints lacked. *Edit Order*
(`PATCH /orders/{orderId}`) classifies each named field through [02 §4.3](DESIGN.md#contract-02-4-3): an administrative-only
request, or any request outside `draft`, takes the `administrative-edit` path of [04 §3.6](features/04-versioning.md#contract-04-3-6) *Apply
Administrative Edit*; a draft request naming a commercial field is `draft-mutate` with
`expected_draft_revision`, the one-trigger, seller and category guards, and [08 §4.3](DESIGN.md#contract-08-4-3)'s
proposed-arrangement authorization when the resource or payer axis changes. *Edit or Remove Line*
(`PATCH` / `DELETE …/lines/{lineId}`) resolves `lineId` against current membership, then the
currency guard, then `draft-mutate`; the line cap does not apply to an edit or a removal. The
registered `line-not-in-draft` is retired — no step raised it, because outside `draft` the engine
already returns `not-admissible` — and `line-not-found` (`LINE_NOT_FOUND`, NotFound, 404) is added,
owned by `02`: a `lineId` that is not a member of the current working set (never existed, or
removed) refuses as the first-registered slice guard. The [08 §4.3](DESIGN.md#contract-08-4-3) action map now names the
commercial draft `PATCH` under `order × write`.

**Rationale**: [02 §3.3](DESIGN.md#contract-02-3-3) declared three endpoints whose behaviour no algorithm stated, one
registered reason was unreachable, and an unknown or removed `lineId` had no reason at all even
though removed line identities stay reserved in [01 §3.7](DESIGN.md#contract-01-3-7). A 404 fits: a removed line is not a
member of what the caller can address. Rejected alternative: keep `line-not-in-draft` alongside
the new reason — it names a condition the engine's `not-admissible` already reports, so two names
would cover one condition, against D-38. Closes slice-lens review finding M-1, capture part
(2026-09-23). Retirement of `field-unclassified` added 2026-09-23 (LOW L-02.3): startup-only
condition, never returned on a request.

**Retires**: line-not-in-draft, field-unclassified

**Propagated**: [02 §3.3](DESIGN.md#contract-02-3-3), `§3.6` *Edit Order*, *Edit or Remove Line*; [01 §4.7](DESIGN.md#contract-01-4-7) reason registry;
[08 §4.3](DESIGN.md#contract-08-4-3) action map; [04 §3.6](features/04-versioning.md#contract-04-3-6), `§4.1` (field-unclassified mentions).

**Amended by D-145**: routing is by field class only. "An administrative-only request, or any
request outside `draft`, takes the `administrative-edit` path" no longer holds: an
administrative-only request takes `administrative-edit` in every non-terminal state, and a request
naming any commercial field takes `draft-mutate` in every state, refusing `not-admissible` outside
`draft`.

### D-117 (M) Line administrative fields stay editable after draft through line PATCH

**Amended by D-145**: the named fields' classes alone select the trigger; a line `PATCH` naming
a commercial field outside `draft` is `draft-mutate` and refuses the engine's `not-admissible`,
not `commercial-field-immutable`, and only an administrative-only post-draft line `PATCH` maps
to `order × edit`.

**Amended by D-142**: an administrative edit in which no named field's value changes is refused
`request-invalid` — at the boundary when it names no field, otherwise by *Apply Administrative
Edit*'s change guard — so a committed edit always appends at least one entry. **Amended by
D-149**: the change guard refuses `administrative-edit-unchanged`; only the no-field edit is
`request-invalid`.

**Decision**: `PATCH /orders/{orderId}/lines/{lineId}` is admitted in every non-terminal state.
In `draft` it edits a line's commercial fields (`draft-mutate`) or its administrative fields;
outside `draft` it accepts administrative line fields only, and a commercial field refuses
`commercial-field-immutable`, mirroring order `PATCH`. [04 §3.6](features/04-versioning.md#contract-04-3-6) *Apply Administrative Edit* takes
an optional `line_id`, checks it is a member of the current working set (`line-not-found`), writes
`orders_order_line_admin`, and appends **one audit entry per changed field**, a line field named
`lines/<line_id>/<field>`, consecutive in sequence, settling the idempotency record with the last.
`01`'s transition contract states that exception to "one audit entry". [08 §4.3](DESIGN.md#contract-08-4-3) maps every
post-draft line `PATCH` to `order × edit`.

**Rationale**: `02` made line `PATCH` draft-only and `04` routed administrative edits through the
order `PATCH`, whose input had no line, yet *Apply Administrative Edit* wrote
`orders_order_line_admin`, which [01 §3.7](DESIGN.md#contract-01-3-7) declares mutable in every non-terminal state — so a
post-submit line purchase-order reference had no reachable edit. The audit row holds one changed
field, so a multi-field edit needs one entry per field. Rejected alternative: carry a `lines[]`
array on order `PATCH` — it overloads one endpoint with two addressing schemes and still needs
per-line membership checks. Closes slice-lens review finding M-2 (2026-09-23).

**Propagated**: [02 §3.2](DESIGN.md#contract-02-3-2), `§3.3`, `§3.6` *Edit or Remove Line*; [04 §3.1](DESIGN.md#contract-04-3-1), `§3.3`, `§3.6` *Apply
Administrative Edit*; [01 §1.1](DESIGN.md#contract-01-1-1), `§3.1`, `§3.7` `orders_transition_audit.changed_field`, `§4.1`;
`DESIGN.md §1.1`, `§3.3`; [08 §4.3](DESIGN.md#contract-08-4-3); `ADR/0001-cpt-cf-bss-orders-adr-transition-through-engine.md`.

### D-118 (M) One request maps to one trigger; a draft request mixing field classes is refused

**Amended by D-145**: the mapping holds in every state and reads no state: any commercial field
selects `draft-mutate`, so a mixed request outside `draft` refuses `not-admissible` ahead of
`mixed-field-classes`.

**Decision**: normative in [02 §4.3](DESIGN.md#contract-02-4-3): a request maps to exactly one trigger. A draft request naming
only commercial fields is `draft-mutate`, one naming only administrative fields is
`administrative-edit`, and a draft request naming both is refused with the new
`mixed-field-classes` (`MIXED_FIELD_CLASSES`, InvalidArgument, 400), owned by `02` and registered
first on the `draft-mutate` path after line membership. `04`'s `administrative-field-in-amendment`
is kept as is for the amendment path: its name describes the amendment surface, not a draft edit.
*Author Line* no longer records the external reference; its former step 5 is removed and the
reference is set through line `PATCH` (D-117).

**Rationale**: *Author Line* recorded an administrative field inside a `draft-mutate` request,
while administrative content goes through [01 §4.3](features/01-foundation.md#contract-01-4-3) row 3 — one request committed the effects of
two triggers, with two audit and revision semantics, under one idempotency key. Rejected
alternative: let `draft-mutate` write the administrative tables too — it makes the audit of an
administrative value depend on which endpoint carried it and blurs the per-field trail. Closes
slice-lens review finding M-5 (2026-09-23).

**Propagated**: [02 §3.2](DESIGN.md#contract-02-3-2), `§3.3`, `§3.6` *Author Line*, *Edit Order*, `§4.3`; [01 §4.7](DESIGN.md#contract-01-4-7) reason
registry; [04 §3.3](DESIGN.md#contract-04-3-3).

### D-119 (M) The seller is fixed at creation

**Decision**: `sellerTenantId` is immutable from creation. A draft edit naming it is refused with
the existing `tenant-axis-immutable`, whose scope now covers draft as well as amendment. The
resource and payer axes stay editable in `draft`. D-62's commercial-frozen class is amended
accordingly; D-104's draft recipient edit concerns the resource axis and still holds.

**Rationale**: the order number is allocated at create, seller-unique and immutable, with UNIQUE
per `seller_tenant_id` in [01 §3.7](DESIGN.md#contract-01-3-7), yet [02 §4.1](features/02-capture.md#contract-02-4-1) listed axis changes among free draft edits and
[02 §4.3](DESIGN.md#contract-02-4-3) froze the seller only after submit — so a draft seller change would either break the
number's uniqueness or silently renumber an addressable order. Rejected alternative: reallocate
the order number on a seller change — it breaks the handle a buyer may already have recorded and
the create branch's one-number-per-order replay guarantee. Closes slice-lens review finding M-6
(2026-09-23).

**Propagated**: [02 §1.2](DESIGN.md#contract-02-1-2), `§2.2`, `§4.1`, `§4.3`; [01 §3.6](features/01-foundation.md#contract-01-3-6) creation initialization, `§3.7`
`orders_order.seller_tenant_id`; [04 §2.2](DESIGN.md#contract-04-2-2), `§3.3`, `§4.1`; [08 §4.3](DESIGN.md#contract-08-4-3) axis-change tests.

### D-120 (M) Administrative fields are last-write-wins per field

**Decision**: administrative fields, order-level and line-level, are last-write-wins per field. An
administrative edit is guarded only by `expected_version`, which it never advances, so concurrent
edits both commit and the later value stands. Every change is audited per field with its prior
and new value, so an overwrite is always reconstructible. There is no administrative revision
token. Stated normatively in [04 §4.6](features/04-versioning.md#contract-04-4-6) and in [02 §4.3](DESIGN.md#contract-02-4-3) *Administrative*.

**Rationale**: two concurrent administrative edits both succeeded and the second silently
overwrote the first, with nothing saying whether that was intended. The owner accepted it: the
fields carry no commercial meaning and the per-field audit trail already preserves every
overwritten value. Rejected alternative: an `admin_revision` counter on the aggregate, bumped by
row 3 and required as `expected_admin_revision`, mirroring `draft_revision` — a second client
concurrency token and a read-then-write round trip for every purchase-order correction. Closes
review item N-1 (2026-09-23).

**Propagated**: [04 §3.3](DESIGN.md#contract-04-3-3), `§3.6` *Apply Administrative Edit*, `§4.6`; [02 §4.3](DESIGN.md#contract-02-4-3).

### D-121 (M) The per-tenant date policy is a revisioned table on the policy channel

**Decision**: the per-tenant date policy lives in the Orders-owned table `orders_date_policy`
([02 §3.7](DESIGN.md#contract-02-3-7)): one row per resource tenant that overrides, plus exactly one platform default row
with a NULL `resource_tenant_id`. Each row carries the two switches `service_activation_required`
and `acceptance_due_required` and a positive monotonic `revision` bumped on every promoted
change. The effective policy is the tenant row if present, else the platform default; a missing
default is a deployment failure, and a missing or invalid effective policy fails the date guard.
The gate snapshots the effective switches, the source row's scope and its `revision` into
`orders_order_line.date_policy_switch_state`. The rows are delivered on the same policy channel
as `orders_state_ttl_policy` — promoted with the deployment, never edited at runtime. The line
cap and the order-number format are static per-gear configuration through the toolkit's typed
gear configuration, deployment-wide rather than tenant-scoped.

**Rationale**: the design called the date policy tenant-scoped configuration keyed by
`resourceTenantId` with a configuration revision, loaded through the platform's typed
configuration — but `get_gear_config` in `libs/toolkit/src/config.rs` is static per gear, with no
tenant key and no revision, so the snapshot's provenance named a store that cannot exist. A
table on the existing policy channel gives both the tenant key and the revision with no new
mechanism. Rejected alternative: Settings Service cascading settings with an ETag revision —
design-only today, it would need an upstream ask before Orders could depend on it. Closes review
finding M-3 (2026-09-23).

**Propagated**: [02 §3.7](DESIGN.md#contract-02-3-7), `§4.2`; [01 §3.7](DESIGN.md#contract-01-3-7) `orders_order_line.date_policy_switch_state`,
`§4.3` submit/amendment contributions; [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) *Run Gate and Submit* step 1, `§4.2` predicate 8; `DESIGN.md
§3.7` inventory, `§3.8` policy channel.

### D-122 (M) The gate reads the seller's catalog, named explicitly

**Superseded for the PriceBook target by D-150–D-158 / ADR-0008. Historical rationale follows.**

**Decision**: the pin-eligibility frontier the gate fixes at [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) *Run Gate and Submit*
step 3 **MUST** be the **seller's** catalog frontier (`seller_tenant_id`), read through a Pricing
operation that takes the catalog-owner tenant explicitly — `pin_frontier_for(ctx,
catalog_tenant_id)`. The same explicit catalog-tenant scoping binds every other catalog-facing
read of the run: the adopted predicates, the product key (step 4), the price evaluation and pin
composition. None of them infers the catalog from the caller's `SecurityContext`. A PEP denial
maps to `catalog-frontier-unavailable` (or the denied port's own unavailable reason) with an
operator diagnostic naming the catalog tenant, and is never surfaced to the buyer as a 403. Until
the explicit operation is exposed, the existing `pin_frontier` may be used only when the caller's
tenant is the seller; otherwise the frontier outcome is `catalog-frontier-unavailable`. The ask is
raised as `cpt-cf-bss-orders-upreq-pricing-catalog-tenant-reads` (p1). Preview and
amendment read the same seller-scoped frontier.

**Rationale**: `pricing-sdk/src/api.rs` documents `pin_frontier` as reading "the caller's tenant
pin-eligibility frontier" and failing with `PermissionDenied` when the PEP denies, while the
algorithm named no tenant and mapped only `None` and outage/deadline. On the partner and direct
paths the caller's tenant is not the seller, so the gate would fix a version of the wrong catalog —
or surface the buyer's lack of access to the seller's catalog as a 403 on their own order. Rejected
alternative: a seller-scoped service `SecurityContext` — it needs platform impersonation, which
does not exist and would widen every Pricing read the gate performs to the seller's authority.
Closes slice-lens review finding M-7 (2026-09-23).

**Propagated**: [03 §2.2](DESIGN.md#contract-03-2-2), `§3.3`, `§3.5`, `§3.6` *Run Gate and Submit* steps 3 and 4, `§3.8`,
`§4.3`, `§4.6`, `§5`; [04 §3.6](features/04-versioning.md#contract-04-3-6) *Append Amendment* step 5; `UPSTREAM_REQS.md §2.2`, `§1.2`, `§3`.

### D-123 (M) Pin composition runs in every gate run

**Superseded for the PriceBook target by D-150–D-158 / ADR-0008. Historical rationale follows.**

**Decision**: catalog pin composition moves into the **parallel** resolve step of [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) *Run
Gate and Submit* (step 5): it depends only on the fixed frontier, so it runs whatever the
predicates and other ports answer, and its per-line outcome is collected with the others (step 10)
before the failure check (step 13). The former composition steps after the failure return are
deleted and the following steps renumbered: the total is assembled at step 14, the transition is
requested at step 15 and the submitted order returned at step 16. De-duplication rule: when a line's
reference is unresolvable, that line's pin outcome is recorded as `unevaluable` carrying the same
`reference-unresolvable` reason, not as a second failure. *Append Amendment* takes its re-pin
from the same parallel resolution and no longer skips it on a refused gate. Composition's
deadline now overlaps the critical path; the resolution ceilings stay the conservative sums of
every operation deadline and are not reduced.

**Rationale**: the "failure list is non-empty … RETURN" step preceded pin composition, so a refused
run never composed the pin — while [03 §3.7](DESIGN.md#contract-03-3-7)'s run contract requires pin composition in the
assessment with a persisted outcome for unavailable or invalid pins, and `§4.2` requires the gate
to evaluate every predicate it can. A caller fixing a predicate failure would discover a pin
failure only on the next round trip, which is exactly what all-failures reporting (D-75) exists to
prevent. Rejected alternative: a new `not_evaluated` verdict for checks skipped after a failure —
it adds a fourth verdict to a closed tri-state enumeration and still hides the pin answer the
caller needs. Closes slice-lens review finding M-12 (2026-09-23). Every citation of the renumbered
steps is updated across the set (D-10, D-60, D-75, D-83, D-93, ADR-0003, [02 §4.2](features/02-capture.md#contract-02-4-2)).

**Propagated**: [03 §2.2](DESIGN.md#contract-03-2-2), `§3.6` *Run Gate and Submit* steps 5, 10, 11, 13, 14, 15 and 16, `§3.7`,
`§4.3`; [04 §3.6](features/04-versioning.md#contract-04-3-6) *Append Amendment* steps 5, 6 and 7; [02 §4.2](features/02-capture.md#contract-02-4-2); ADR-0003.

### D-124 (M) A line's region is its resolved price row's market scope

**Decision**: predicate 4 (order-market consistency, [03 §4.2](features/03-gate-and-pin.md#contract-03-4-2)) compares each line's currency and
region with the version's market. No line stores a region, so a line's region is the **market
scope of its resolved price row at the fixed catalog version**, returned by the
reference-resolution/predicate port in [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) *Run Gate and Submit* step 5; the gate compares
it with the version's `market_region`, and the line's currency with `market_currency`. A port
answer missing the market scope makes the predicate unevaluable with `catalog-predicates-unavailable`.
The field is named in the Pricing ask `cpt-cf-bss-orders-upreq-pricing-catalog-tenant-reads`.

**Rationale**: the predicate checked "each line's currency and region" while only the version-level
`market_region` exists, so the check had no left-hand side for region; PRD §6.1 (c) requires
per-line currency and region. The price row is where the catalog already scopes a price to a
market, so reading it at the fixed version keeps the check inside the one-version rule (D-93)
without an Orders-owned line column. Rejected alternative: narrow predicate 4 to currency and
disclose a PRD gap — it drops a PRD requirement the catalog can already answer. Closes slice-lens
review finding M-14 (2026-09-23). Two mechanical corrections landed in the same batch: party
eligibility has one owner, the contract-resolution port (M-13), with a new Contracts section in
`UPSTREAM_REQS.md` and the reason `contract-party-ineligible`; and the Rating and Account
Management dependencies are marked unexposed with their asks (M-15), the payer commercial profile
raised to p1.

**Propagated**: [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) *Run Gate and Submit* step 5, `§4.2` predicate 4; `UPSTREAM_REQS.md §2.2`.

### D-125 (M) Preview withholds TCV in a successful response

**Decision**: when a basket line omits term duration or billing cycle, Preview answers
**successfully** with every other field, omits `tcv`, and carries
`tcvWithheld: {reason: "preview-term-or-cycle-missing", lineIds: […]}` naming every such line. The
name leaves the refusal table of [01 §4.7](DESIGN.md#contract-01-4-7) and moves to a new **Response annotations
(non-refusal)** list beside it, which holds names carried on successful responses with no canonical
category or HTTP status. [03 §3.3](DESIGN.md#contract-03-3-3) lists it as a response annotation contributed, not as a reason
contributed to the registry, so it keeps one registration.

**Rationale**: `01`'s registry made `preview-term-or-cycle-missing` an InvalidArgument/400 refusal
while [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) and `§4.6` said Preview "withholds TCV entirely" and still answers, and PRD acceptance
criterion 2c asks only that Preview **MUST NOT** return a TCV figure — it does not ask for a refusal. A 400
would also discard the gate verdicts, the resolved total and the indicative tax, the answers a buyer
asks Preview for. Rejected alternative: keep the 400 refusal and drop the "still answers" prose —
it withholds every figure to withhold one. Closes slice-lens review finding M-9 (2026-09-23).

**Propagated**: [01 §4.7](DESIGN.md#contract-01-4-7) (refusal table, response-annotation list); [03 §3.2](DESIGN.md#contract-03-3-2) *Preview*
boundaries, `§3.3` reason list, `§3.6` *Preview a basket*, `§4.6`.

### D-126 (M) The overlap port is an occupancy read

**Decision**: the Subscriptions overlap port is an **occupancy read**. Batched per basket, it
returns for each `(payer_tenant_id, overlap_scope_key)` the triple
`(activeCount, maxConcurrentActive, provenance)`: the subscriptions `active` on the key (drafts
excluded), the effective concurrent-active cardinality, and the Catalog/Contract policy it came
from. Predicate 7 passes for a key iff `activeCount + proposed ≤ maxConcurrentActive`, where
`proposed` is the basket's lines on that key. The activation re-check tests
`activeCount + pending ≤ maxConcurrentActive`, where `pending` is this order's not-yet-activated
lines on the key. A missing count or limit refuses `overlap-presence-unevaluable`. **The name is
kept for stability**: it is already registered, carried on audit entries and cited across the
set, so a rename would buy no precision. `UPSTREAM_REQS.md §2.1` asks for the occupancy shape as
an **amendment to `SUB-O5`**; the Subscriptions gear's own seam map is not edited.
*(Amended by D-179: the read is asked for per `(payer_tenant_id, resource_tenant_id, overlap_scope_key)`;
until Subscriptions enforces that tuple, predicate 7 applies the tuple it answers.)*
*(Amended by D-180: while the answer is per payer, predicate 7's `proposed` also counts the lines of
this payer's other in-flight orders claiming the key under another resource tenant; the re-check's
`pending` is unchanged. Either pass is advisory at order time, not an admission guarantee.)*

**Rationale**: predicate 7 says "a boolean presence read is insufficient", yet [03 §2.2](DESIGN.md#contract-03-2-2), `§3.3`
and the re-check named the port a presence read. `UPSTREAM_REQS` asked "does a non-terminal
subscription already hold that key", and Subscriptions' `SUB-O5` says "this gear answers
presence". A boolean cannot evaluate a cardinality above one, so the port and its predicate
disagreed. Rejected alternative: a boolean plus the limit, refusing as unevaluable whenever
`maxConcurrentActive > 1` — it fails closed on exactly the configurations the configurable
cardinality exists for. Closes slice-lens review finding M-10 (2026-09-23).

**Propagated**: [03 §1.3](DESIGN.md#contract-03-1-3), `§2.1`, `§2.2` (deadline table, constraint), `§3.3` port list, `§3.5`,
`§3.6` *Run Gate and Submit* steps 4 and 5 and *Re-check Activation Preconditions* steps 6 to 8,
`§3.8`, `§4.2` predicate 7, `§5`; [06 §4.6](DESIGN.md#contract-06-4-6); `UPSTREAM_REQS.md §1.2`, `§2.1`.

### D-127 (M) The activation re-check has four outcomes

**Decision**: *Re-check Activation Preconditions* ([03 §3.6](features/03-gate-and-pin.md#contract-03-3-6), executed by Workflow) returns
exactly one of `proceed | reject (per-line reasons) | not-dispatchable (state or version) |
defer (port reason)`. The order read comes first (step 1), so a held, terminal or superseded order
returns `not-dispatchable`. Unavailability of the identity port or the overlap-occupancy
port, a deadline elapsing on either, or an occupancy answer without a count or limit, returns
`defer`. The outcomes drive these transitions:

* `proceed` → `report-spawn-signal` (row 12).
* `reject` → drafts voided and `acknowledge-failed` (row 14) carrying the line reasons.
* `not-dispatchable` → no acknowledgement and no transition. Workflow stops dispatch and re-reads
  the order: it waits for resume and re-checks if the order is held, follows the current version if
  it is superseded, and ends the attempt if it is terminal.
* `defer` → Workflow retries from step 1 with bounded backoff under the new deployment value
  **`activation-recheck-retry-budget`** (baseline 3 attempts over ≤ 60 s; no existing Workflow
  retry budget was defined to reuse). Once the budget is exhausted, Workflow voids the drafts and
  calls `acknowledge-failed` (row 14) carrying the port's unevaluable reason
  (`identity-party-unavailable` or `overlap-presence-unevaluable`).

The steps are renumbered: re-read at 6, proceed at 9.

**Rationale**: the output was "proceed, or a per-line rejection reason". The preamble said held,
terminal or superseded versions stop dispatch, but no output value expressed that, and no step
branched on an unavailable identity or overlap port. Workflow therefore had no specified move for
a held order or a port outage. The only rule was "pause or retry under Workflow policy": it named
no bound, and nothing said what happened when retrying stopped. Rejected alternative: abort the
wave via `acknowledge-failed` on the first port unavailability. That turns a transient upstream
blip after a successful submit and a completed wave 1 into a failed order the buyer must
re-place. Closes slice-lens review finding M-11 (2026-09-23).

**Propagated**: [03 §2.2](DESIGN.md#contract-03-2-2), `§3.3` fulfillment-time reasons, `§3.6` *Re-check Activation
Preconditions* (all steps, outcome table, budget), `§3.8`; [06 §3.3](DESIGN.md#contract-06-3-3), `§3.6` *Begin fulfillment
and record the spawn signal*, `§4.3`, `§4.4`; `UPSTREAM_REQS.md §2.6`; D-89 (step citations).

### D-128 (M) A payer change crosses seller scope unless the identity answer confirms the relationship

**Decision**: a payer change **crosses seller scope** when the identity operation ([03 §3.6](features/03-gate-and-pin.md#contract-03-3-6)
*Run Gate and Submit* step 2) does not confirm that the proposed payer tenant has a commercial
relationship with the order's `sellerTenantId`, which is immutable from creation (D-119). The
answer is part of the payer commercial profile the identity port already returns from Account
Management. *Append Amendment* step 2 resolves it through 03 once per run, and the gate run reuses
it. A change that is not confirmed is refused with `payer-rebinding-requires-seller`. If the
operation is unavailable or misses its deadline, the refusal is the existing
`identity-party-unavailable`. No reason and no port is added. `upreq-payer-commercial-profile`
now also requires that the profile state the payer↔seller relationship.

**Rationale**: [04 §3.6](features/04-versioning.md#contract-04-3-6) *Append Amendment* step 2 said "determine whether a `payer_tenant_id` change crosses seller
scope", and `§2.2` and `§4.1` used the term, but nothing defined it. D-62 said only "within one
seller's scope", and [04 §3.5](DESIGN.md#contract-04-3-5) says the slice holds no port, so the guard had no stated input and
no source to read one from. Rejected alternative: a separate tenant-hierarchy port. It would be a
tenth outbound port with its own deadline and a new unavailable reason, just to answer a question
about the payer that the identity read already fetches. Closes slice-lens review finding M-17
(2026-09-23).

**Propagated**: [04 §2.2](DESIGN.md#contract-04-2-2) (definition), `§3.3`, `§3.6` *Append Amendment* steps 1 and 2;
`UPSTREAM_REQS.md §2.4` `upreq-payer-commercial-profile`; D-62 (clarification note).

### D-129 (M) A bad amendment explanation has its own reason

**Decision**: a new reason `amendment-reason-invalid` (`AMENDMENT_REASON_INVALID`,
InvalidArgument, 400), owned by `04`, refuses an amendment whose `amendment_reason` is absent or
outside 1–4096 characters, the bound [01 §3.7](DESIGN.md#contract-01-3-7) gives `orders_order_version.amendment_reason`. It
is a declared guard of *Append Amendment* step 1, registered immediately after
`amendment-empty`, so the engine audits and settles it like every other amendment refusal.

**Rationale**: [04 §3.7](DESIGN.md#contract-04-3-7) said "invalid/empty explanations refuse `amendment-empty` with field
detail". That check was not a declared guard in step 1, so nothing placed it in the engine's
order, and it gave one name to two conditions (an empty delta, a bad explanation), against D-38's
one name per condition. Rejected alternative: reuse `amendment-empty` with a field discriminator.
Callers key on the reason string, and a discriminator would make one reason mean two different
corrections. Closes slice-lens review finding M-20 (2026-09-23).

**Propagated**: [01 §4.7](DESIGN.md#contract-01-4-7) reason registry; [04 §3.3](DESIGN.md#contract-04-3-3), `§3.6` *Append Amendment* step 1, `§3.7`,
`§4.1`.

### D-130 (M) A partner-placed order's creator, submitter or amender cannot record its acceptance, refused by a Lifecycle guard

**Decision**: [05 §3.6](features/05-preconditions.md#contract-05-3-6) *Record Acceptance* gains a resolve step, step 4, between the path
step and the already-recorded step. On `orders_order.sales_path = partner_placed` it refuses when
the trusted SecurityContext actor equals the `orders_order_version.actor` of version 1 (the
creator), of the submitted version (the submitter), or of `expected_version` (the amender, where
that version was appended by an amendment). A new reason `acceptance-recording-party-barred`
(`ACCEPTANCE_RECORDING_PARTY_BARRED`, PermissionDenied, 403), owned by `05`, names the refusal.
`resourceTenantId` membership and the `acceptance × record` grant stay with the engine's PDP
pre-guard (`operation-not-permitted-for-actor`, or `order-not-found` per D-114). The `§3.8`
recording-party refusal metric and alert count this reason.

**Amended by D-146**: step 4 applies per role version (creator, submitter, amender) and refuses
when the caller equals that version's actor and either `sales_path = partner_placed` or that
version's `orders_order_version.actor_tenant_id` differs from `resource_tenant_id`; a
`self_service` order no longer passes on its sales path alone.

**Rationale**: step 1 declared a "recording-party" guard, but no step resolved it and no reason
named its refusal, while `§3.8` counted "recording-party refusals". The rule compares the caller
with stored `orders_order_version.actor` values, and the PDP evaluates request context only and
cannot see them (D-111), so the comparison has to be Lifecycle's own guard. The amender is barred
too, since a partner who appended the accepted version authored it as surely as one who created
it. Rejected alternative: pass the creator and submitter actors to the PDP as resource properties
and reuse `operation-not-permitted-for-actor`. That adds an upstream dependency on the platform
policy, and it merges a missing grant and a barred party under one name, against D-38. Closes
slice-lens review finding M-23 (2026-09-23).

**Propagated**: [01 §4.7](DESIGN.md#contract-01-4-7) reason registry; [05 §3.3](DESIGN.md#contract-05-3-3), `§3.6` *Record Acceptance* steps 1 and 4,
`§3.8`, `§4.2`; [08 §4.3](DESIGN.md#contract-08-4-3) (acceptance rule and permission matrix); D-10 (step citation).

### D-131 (M) A pending authorization outcome is refused defensively, not expected

**Decision**: the authorization outcome stays a three-valued outcome; only `authorized` and
`failed` are expected on begin-fulfillment. The `pending` branch of *Evaluate Begin-Fulfillment
Preconditions* is kept as a defensive fail-closed path: should a `pending` outcome nonetheless be
submitted, Lifecycle **MUST** refuse it `authorization-pending` without state change. This branch
is defensive, not a protocol step. Workflow still **MUST** submit a conclusive outcome.

**Rationale**: [05 §4.3](features/05-preconditions.md#contract-05-4-3) said Workflow **MUST** submit a conclusive `authorized` or `failed`, yet
the begin-fulfillment guard had a `pending` branch refusing `authorization-pending`, and `§1.2`
and `§3.1` called pending "a distinct input". A reader could not tell whether a pending
submission was part of the protocol. Rejected alternative: make the field two-valued and delete
`authorization-pending`. A Workflow defect that forwarded a pending answer would then have no
registered refusal and would have to be coerced into `failed`, which the tolerate-failure
election could admit to fulfillment. Closes slice-lens review finding M-25 (2026-09-23).

**Propagated**: [05 §1.2](DESIGN.md#contract-05-1-2), `§3.1`, `§3.6` *Evaluate Begin-Fulfillment Preconditions*, `§4.3`;
`UPSTREAM_REQS.md §2.5` `upreq-authorization-outcome`.

### D-132 (M) The contract-resolution port also returns the acceptance declaration, read live

**Decision**: the contract-resolution port of [03 §3.3](DESIGN.md#contract-03-3-3) returns, where a contract is referenced,
the contract's `acceptance_required` declaration beside contract status and party eligibility.
`05`'s *Record Acceptance* and begin-fulfillment guards call the same operation outside the gate
and read the declaration live at each guard; nothing is snapshotted onto the version. The count
of outbound operations stays nine. A new ask,
`cpt-cf-bss-orders-upreq-contract-acceptance-declaration` (p1), sits in
`UPSTREAM_REQS.md §2.11` and cites Contracts PRD §6.6 *Booking instant and acceptance*. Until the
Contracts SDK exists, a contract-referenced order resolves the existing
`acceptance-requirement-unevaluable` at both guards and never falls back to an election.

**Rationale**: [05 §3.5](DESIGN.md#contract-05-3-5) read the acceptance-required declaration "via the gate slice's port",
but that port returned only contract status and party eligibility, and the Contracts gear has no
SDK, only docs. Its PRD does require the declaration. The precedence of D-107 therefore had a
first tier with no source. Rejected alternative: snapshot the declaration onto the version at
submit. That adds a version column and freezes a contract term the contract owner may correct.
It also leaves the begin-fulfillment guard reading a value that is older than the one the
contract now states. Closes slice-lens review finding M-26 (2026-09-23).

**Propagated**: [03 §3.3](DESIGN.md#contract-03-3-3), `§3.5`, `§5`; [05 §3.5](DESIGN.md#contract-05-3-5), `§3.6` *Record Acceptance* step 2,
*Evaluate Begin-Fulfillment Preconditions* step 1; `DESIGN.md §3.5`; `UPSTREAM_REQS.md §1.2`,
`§2.11`, `§3`.

### D-133 (M) Acceptance and tolerate-failure elections change only by deployment promotion

**Decision**: `orders_policy_election` is mutable only by deployment promotion; a seller's
election is requested through platform operations. `elected_by`/`elected_at` record the
promotion's change identity and instant. No Orders endpoint writes the table and no PDP action
governs it. Q-30 stays open; its mitigation, a seller-scope election of acceptance not required,
is requested through platform operations and takes effect by promotion.

**Rationale**: [05 §3.7](DESIGN.md#contract-05-3-7) said an election "is a standing policy that a seller may change" and
that it travels on "the same policy channel that carries `orders_state_ttl_policy`". `DESIGN.md
§3.8` says that channel is promoted with the deployment, not edited at runtime. No endpoint wrote
`elected_by`/`elected_at`, and Q-30's mitigation assumed a seller could elect. Rejected
alternative: a runtime seller endpoint with a new PDP action and a new [08 §4.3](DESIGN.md#contract-08-4-3) matrix row. That
adds an endpoint, a permission and an audit surface for a rarely changed commercial policy the
platform already delivers by promotion. Closes slice-lens review finding M-27 (2026-09-23).

**Propagated**: [05 §3.7](DESIGN.md#contract-05-3-7), `§4.2`, `§4.3`; `DESIGN.md §3.7`, `§3.8`; Q-30 (mitigation).

### D-134 (M) A workflow-mediated cancel always carries complete compensation evidence

**Decision**: *(Amended by D-182: the closed evidence schema gains a forced variant — `unknown`
for `at_sale_facts_emitted` and `no_active_subscription_remains`, plus `operator_attestation` —
written only by `force-fail-unreconciled`; the guards below refuse it as
`compensation-evidence-incomplete`, so it never enters through `/workflow-cancel` or a failed
acknowledgement.)*
`/workflow-cancel` requires complete compensation evidence whether or not the spawn
signal is recorded. The evidence check leaves [06 §3.6](features/06-workflow-seam.md#contract-06-3-6) *Evaluate Cancel From In-Fulfillment
(shared guard)*, which keeps only the window and actor logic, and becomes a guard registered on
the `cancel-workflow-mediated` rows ([01 §4.3](features/01-foundation.md#contract-01-4-3) rows 16 and 27). It refuses
`compensation-evidence-missing` when the evidence is absent or null and
`compensation-evidence-incomplete` when it fails the closed schema of [01 §3.7](DESIGN.md#contract-01-3-7) or does not assert
`no_active_subscription_remains`, the same two reasons the failure acknowledgement uses. Before the
spawn signal the evidence records the voided drafts and `activation_dispatched = false`. The
ordinary `POST /cancel` of [07 §4.6](features/07-hold-and-expiry.md#contract-07-4-6), which also calls the shared guard, carries no evidence and
is unaffected. `workflow-cancel-requires-evidence` named the same condition as those two reasons
and is retired. [06 §3.6](features/06-workflow-seam.md#contract-06-3-6) also gains the three algorithms its declared operations lacked: *Begin
Fulfillment* composes [05 §3.6](features/05-preconditions.md#contract-05-3-6) *Evaluate Begin-Fulfillment Preconditions* as the row-11 guards;
*Report Spawn Signal* guards on `spawn_signal_at` IS NULL (`spawn-signal-already-recorded`); and
*Workflow Cancel* is the `/workflow-cancel` handler, reusing `07`'s `cancel-reason-required` for
its mandatory cancel reason. `begin-fulfillment-preconditions-unmet` is retired, because no step
raised it and `05` refuses with its specific reasons.

**Rationale**: the shared guard admitted a pre-spawn cancel without evidence ("no spawn signal
is recorded: admit"), while row 16 and 27 said the cancel carries compensation evidence, [06 §3.3](DESIGN.md#contract-06-3-3)
described the operation "with attached compensation evidence", and the Workflow PRD requires
evidence on every workflow cancellation. Pre-spawn evidence is cheap to supply (the voided drafts,
possibly none) and lets one guard state one rule. Moving it out of the shared guard keeps the
ordinary cancel path free of a requirement it cannot meet. Rejected alternative: require evidence
only after the spawn signal, as the guard did. A pre-spawn Workflow cancel would then leave no
record of which drafts were voided, and the rows would keep contradicting their own guard. Closes
slice-lens review finding M-28 (2026-09-23). The same pass applies the mechanical items M-1 (06
part, the three missing algorithms) and M-31 (the closed compensation-evidence schema of
[01 §3.7](DESIGN.md#contract-01-3-7)), which need no decision of their own.

**Retires**: begin-fulfillment-preconditions-unmet, workflow-cancel-requires-evidence

**Propagated**: [01 §3.7](DESIGN.md#contract-01-3-7), `§4.3`, `§4.7` reason registry; [06 §3.2](DESIGN.md#contract-06-3-2), `§3.3`, `§3.6` *Begin
Fulfillment*, *Report Spawn Signal*, *Evaluate Cancel From In-Fulfillment (shared guard)*,
*Workflow Cancel*, `§3.7`, `§4.4`; D-109 (amendment note).

### D-135 (M) A denied verdict carries a denial reason, stored and never evaluated

**Amended by D-142**: a denial reason sent with any other verdict is rejected at boundary
validation with `request-invalid`.

**Decision**: [06 §3.6](features/06-workflow-seam.md#contract-06-3-6) *Reflect Verdict* takes a `denial_reason` input, required when the verdict
is `denied` and forbidden otherwise. A new reason `denial-reason-missing`
(`DENIAL_REASON_MISSING`, InvalidArgument, 400), owned by `06`, is a declared guard of step 1 and
refuses a denied verdict without one; a denial reason sent with any other verdict is a malformed
request rejected at boundary validation. `orders_approval_reflection` gains a nullable
`denial_reason text` column with a CHECK that it is present exactly when `verdict` is `denied`.
`OrderRejected` carries it. Lifecycle stores it as an opaque received fact and never parses,
classifies or evaluates it.

**Rationale**: [01 §4.4](DESIGN.md#contract-01-4-4) gave `OrderRejected` "the deciding authority and the denial reason" and
the Workflow PRD's `OrderApprovalDecision` returns a decision "with a reason", but *Reflect
Verdict* took no such input and the reflection table had no column for it, so the event promised
a field nothing supplied. Storing it opaquely keeps R2 intact: the gear records what the authority
said and does not judge it. Rejected alternative: drop the denial reason from `OrderRejected`. A
consumer then has to ask Workflow why an order was rejected, and the reason the approval authority
already gives is lost at the seam. Closes slice-lens review finding M-30 (2026-09-23).

**Propagated**: [01 §4.4](DESIGN.md#contract-01-4-4) event catalogue, `§4.7` reason registry; [06 §2.1](DESIGN.md#contract-06-2-1), `§3.1`, `§3.3`,
`§3.6` *Reflect Verdict* steps 1 and 3, `§3.7`, `§4.2`.

### D-136 (M) A fulfillment acknowledgement carries the correlation identifier and a closed failure reason

**Amended by D-142**: a value outside the enumeration, and a `failure_reason` sent with a completed
outcome, are rejected at boundary validation with `request-invalid`. **Amended by D-143**: the
failure reason is recorded on the committed audit entry's `caller_reason`, not its `reason`.

**Decision**: *(Amended by D-182: the closed `failure_reason` enumeration gains
`operator-forced-unreconciled`, fixed by the operator trigger `force-fail-unreconciled` and rejected
as `request-invalid` on a fulfillment acknowledgement; on that trigger the audit `caller_reason` is
the operator's forced-failure reason.)*
[06 §3.6](features/06-workflow-seam.md#contract-06-3-6) *Acknowledge Fulfillment* takes `correlation_id`, as every seam call does,
and `failure_reason`, required on the failed outcome. `failure_reason` is a closed enumeration:
`market-divergence`, `overlap-collision`, `identity-party-unavailable`,
`overlap-presence-unevaluable`, `line-execution-failed` and `dependency-graph-invalid`. The first
two are the re-check's `reject` reasons, the next two the port reasons an exhausted `defer`
carries (D-127), and the last two cover Workflow's own line-execution and dependency-graph
failures. A new reason `failure-reason-missing` (`FAILURE_REASON_MISSING`, InvalidArgument, 400),
owned by `06`, refuses a failed acknowledgement without one. A value outside the enumeration is
rejected at boundary validation. The failure reason is recorded on the committed audit entry and
carried in `OrderFulfillmentFailed`.

**Rationale**: [06 §3.3](DESIGN.md#contract-06-3-3) says every call carries the process correlation identifier, and the
audit-completeness test asserts it on each of the five operations, yet the acknowledgement input
omitted it. [06 §4.3](features/06-workflow-seam.md#contract-06-4-3) said the re-check reason "is carried as the failure reason", but no input
carried one. A closed set lets consumers key on the value and lets the event schema state it.
Rejected alternative: an opaque Workflow string. It would put free text on a published event that
consumers cannot match on, and it would make the re-check reasons, which the gear already
registers, indistinguishable from arbitrary prose. Closes slice-lens review finding M-32
(2026-09-23).

**Propagated**: [01 §4.4](DESIGN.md#contract-01-4-4) event catalogue, `§4.7` reason registry; [06 §3.1](DESIGN.md#contract-06-3-1), `§3.3`, `§3.6`
*Acknowledge Fulfillment* steps 1 and 4, `§4.3`, `§4.4`.

### D-137 (M) Seller-scope TTL overrides sit behind a default-off gear flag

**Decision**: `07` gains a gear-level setting, `ttl_seller_override_enabled`. It is static
per-gear configuration on the same promotion path as `orders_state_ttl_policy`, and it defaults
to **off**. While it is off, the policy channel's validation rejects every `scope = seller` row at
promotion, so no seller override reaches the table, and the seller pass of [07 §3.6](features/07-hold-and-expiry.md#contract-07-3-6) *Sweep
Expired Orders* (step 2.3) selects nothing. Only the permanent platform rows are effective. The
seller-overrides-platform mechanism of [07 §3.6](features/07-hold-and-expiry.md#contract-07-3-6) and `§3.7` stays specified in full, but it takes
effect only where Q-06 admits it and the flag is on. Q-06's override-scope half becomes a
configuration answer rather than a design change.

**Clarified 2026-09-23** (review R-L2): the flag is read by **effective-policy selection**, not
only by promotion validation. While it is off, the sweep's seller pass, the platform fallback's
exclusion of seller-policied orders, the draft auto-void pass and the engine's in-transaction
policy re-read all ignore every `scope = seller` row, so the platform rows are effective for every
seller even where seller rows exist. Turning the flag off after seller rows were promoted
therefore reverts to the platform rows without deleting them. Promotion validation still rejects
seller rows while the flag is off.

**Rationale**: [07 §2.2](DESIGN.md#contract-07-2-2) and `§4.5` listed override scope as an open Product question (PRD §15
row 7, Q-06), while `§3.6` and `§3.7` specified "seller scope overrides platform scope" as a
normative rule. The design had already answered the question it said was open. Putting the
mechanism behind a default-off flag keeps it ready to build and test, and leaves the choice with
Product. Rejected alternative: a provisional decision that seller scope overrides platform scope,
with Q-06 narrowed to the TTL values. That would make a commercial policy Product has not taken
the default behaviour, which is what [07 §2.2](DESIGN.md#contract-07-2-2) says a code default must not do. Closes slice-lens
review finding M-34 (2026-09-23).

**Propagated**: [07 §2.2](DESIGN.md#contract-07-2-2), `§3.6` *Sweep Expired Orders* (steps 2.3 and 2.3a and the engine's
policy re-read — R-L2), `§3.7` `orders_state_ttl_policy`, `§4.4` draft pass (R-L2), `§4.5`;
`DECOMPOSITION.md`; Q-06.

### D-138 (M) The hold record is the stored pre-hold state plus the hold transition's audit entry

**Amended by D-143**: the optional hold reason is the hold entry's `caller_reason`, NULL when not
supplied, and `OrderHeld` carries it only when present.

**Decision**: the hold record of [07 §3.1](DESIGN.md#contract-07-3-1) is `orders_order.pre_hold_state` plus the hold
transition's audit entry, which carries the actor, the instant and the reason. There is no
separate hold column set. The hold reason is **optional**: it is recorded on the audit entry and
in `OrderHeld` when the caller supplies one, and no guard requires it, unlike the cancel reason.
[07 §3.8](DESIGN.md#contract-07-3-8)'s hold-duration distribution reads the hold instant from that audit entry, or from
`state_entered_at` while the order is still `on_hold`. D-22's hold actor/instant/reason columns
are withdrawn.

**Rationale**: [07 §3.1](DESIGN.md#contract-07-3-1) described the hold record as a column set with the holding actor, the
instant and the audited reason, and D-22 said those columns were added. But [01 §3.7](DESIGN.md#contract-01-3-7)
`orders_order` carries only `pre_hold_state`, and [07 §3.7](DESIGN.md#contract-07-3-7) owns only that column. The actor, the
instant and the reason are already on the hold transition's audit entry and in `OrderHeld`, so
columns would store them a second time with nothing to keep the copies equal. `07`'s *Hold Then
Resume* takes a reason as input but declares no guard on it, so the reason stays optional rather
than gaining a new refusal. Rejected alternative: three new columns `held_by`, `held_at` and
`hold_reason` on `orders_order`. They duplicate the audit entry and need an engine-owned writer and
clearing rule. Closes slice-lens review finding M-35 (2026-09-23).

**Propagated**: [07 §3.1](DESIGN.md#contract-07-3-1), `§3.6` *Hold Then Resume*, `§3.8`; D-22.

### D-139 (M) An invalid cursor is `cursor-invalid`, under one cursor contract for all five paged collections

**Decision**: register `cursor-invalid` (`CURSOR_INVALID`, InvalidArgument, 400), owned by
`08-read-and-authz`. The order list, the version list, the per-line read and the audit read share
one cursor contract in [08 §2.2](DESIGN.md#contract-08-2-2): exact continuation after the last returned sort tuple, and an
opaque versioned token that is position, not authority, bound to the endpoint, the parent order
where the collection has one, the authenticated principal, the normalized filters and the sort.
A token that fails structure, version, precision or binding validation returns `cursor-invalid`
at input validation ([08 §3.6](features/08-read-and-authz.md#contract-08-3-6) common read wrapper item 1), before the access decision, so
it appends no access-log row, following D-112's boundary rule. *List Orders* gains an explicit
cursor-validation step. The audit read's live-view, retention and merge rules stay audit-only.

**Rationale**: [08 §2.2](DESIGN.md#contract-08-2-2) sent cursor failures to "the existing malformed-request response" and
forbade a new code, but the [01 §4.7](DESIGN.md#contract-01-4-7) registry has no such entry, so an implementation had
nothing to return. The token and binding rules also sat under a heading that applied them only to
the audit read, which left the order, version and line cursors with an ordering but no token
contract. Rejected alternative: toolkit-odata's canonical `INVALID_CURSOR` field violation
(`libs/toolkit-odata/src/problem_mapping.rs`). It is not in the `orders.v1` error
domain, so a client matching on Orders domain/code pairs could not recognise it, and it would be
the only Orders read failure outside that domain. Closes slice-lens review finding M-40
(2026-09-23).

**Amends**: D-101 — its "malformed-request mapping" becomes `cursor-invalid`, and its token rules
extend from the audit read to all five paged collections.

**Amended 2026-09-23** (LOW L-08.1): acceptance history is a fifth paged collection under the
same cursor contract, ordered by `accepted_version` descending over `orders_acceptance`'s
`(order_id, accepted_version)` primary key, like the version list; its read executes the common
read wrapper under `order × read`, access-logged.

**Propagated**: [08 §2.2](DESIGN.md#contract-08-2-2), `§3.3`, `§3.6` common read wrapper item 1 and *List Orders* step 3,
`§3.7`, `§4.4`; [01 §4.7](DESIGN.md#contract-01-4-7); D-101; [05 §3.3](DESIGN.md#contract-05-3-3) (acceptance read, L-08.1).

### D-140 (H) `sales_path` is `partner_placed` iff the allowed create carried a delegation proof reference

**Decision**: the create branch writes `orders_order.sales_path = partner_placed` **iff** the
create request that the step 1 authorization allowed carried a delegation proof reference, and
`self_service` otherwise. It is the same observable proxy [08 §4.4](DESIGN.md#contract-08-4-4) already uses to log a read as
delegated. The rule is stated once, in [01 §3.7](DESIGN.md#contract-01-3-7) `sales_path` and the *Create Transition*
creation-initialization table; *Create Transition* step 6, `05`'s recording-party bar and
`recording_path`, and [08 §2.1](DESIGN.md#contract-08-2-1) / `§4.4` cite it. The proxy is imprecise in both directions
(corrected by D-146): a caller who supplies a proof on its own-tenant create is recorded
`partner_placed`, and delegation that begins after create — a `resourceTenantId` edit in draft,
or a delegated partner submitting a buyer's draft — leaves a `self_service` order. `UPSTREAM_REQS.md`
`…-upreq-pdp-policy-integration` gains item 5, asking the PDP allow response to say which path it
allowed, delegated or direct; once it is delivered, the marker replaces the proxy.

**Rationale**: D-106 had the create branch write `partner_placed` "when step 1 authorized the
create through the delegated partner path", but after D-111 Orders never classifies a path as
delegated and cannot observe which PDP path allowed a request ([08 §2.1](DESIGN.md#contract-08-2-1), `§4.4`; [01 §3.7](DESIGN.md#contract-01-3-7)
*Actor class*). The value D-130's recording-party bar and `recording_path` depend on therefore
had no buildable source. A supplied proof reference is the one delegation fact Orders sees on
every request. Rejected alternative: block `sales_path`, and so the partner-placed acceptance
rule, until the PDP returns a path marker — it leaves a p1 guard unbuildable on an open upstream
ask when a conservative proxy is available now. Closes remediation-review finding R-H1
(2026-09-23).

**Amends**: D-106 — its "from the PDP-authorized access path" becomes the proof-reference proxy.

**Amended by D-146**: the claim that the proxy errs only toward applying the recording-party bar
more is withdrawn. `sales_path` describes the create only; no acceptance control keys on it
alone. The submit-time automatic acceptance keys on the submit request's own facts, the bar also
compares stored version actors' tenants with `resource_tenant_id`, and the proxy counts only the
proof the PDP accepted once `…-upreq-pdp-policy-integration` item 4 is delivered.

**Propagated**: [01 §3.6](features/01-foundation.md#contract-01-3-6) *Create Transition (D-105)* step 6 and its creation initialization, `§3.7`
`orders_order.sales_path`; [05 §3.6](features/05-preconditions.md#contract-05-3-6) *Record Acceptance* steps 4 and 6, `§4.2`; [08 §2.1](DESIGN.md#contract-08-2-1)
`…-constraint-delegation-proof-required`, `§4.3` acceptance rule, `§4.4`; `UPSTREAM_REQS.md`
`…-upreq-pdp-policy-integration` item 5; D-106.

### D-141 (M) A targeted request answers `order-not-found` on a delegation-proof denial; only untargeted requests disclose the reason

**Decision**: a targeted request — a point or child read, or a write against an existing order —
answers `order-not-found` (404) on every PDP denial, a delegation-proof denial included.
`delegation-proof-required` and `delegation-proof-invalid` are returned only on untargeted
requests: list, create and preview. On a targeted request the classified proof reason goes to
the scoped internal log/metric, and the access-log or refusal row records `order-not-found`.
[08 §3.6](features/08-read-and-authz.md#contract-08-3-6) *Read One Order* loses its delegation-proof step (old step 4; later steps renumber),
and a proof denial takes step 3's not-found arm. D-114's 403 rule still holds for every
non-proof denial of a targeted request: 403 only when the follow-up `order × read` allows. The two
compose as follows: a proof denial on a targeted request is always 404, whatever the follow-up
answers, and the follow-up is still made on it, so a proof denial and any other denial make the
same PDP calls.

**Rationale**: `08` disclosed a proof reason on a targeted read "where disclosing that reason
would not reveal a hidden target", but *Read One Order* returned it only where the row existed,
so the reason itself confirmed existence, and nothing decided when disclosure was safe. Rejected
alternative: disclose the proof reason when a non-delegated `order × read` on the target allows.
It costs a second decision on every proof denial and still needs a test for "non-delegated"
that Orders cannot run (D-111). Closes remediation-review finding R-M2 (2026-09-23).

**Amends**: D-114 — "Delegation-proof denials keep D-111's mapping and its hidden-target rule"
becomes "a proof denial on a targeted request is always `order-not-found`"; D-111 — its
disclosure half.

**Propagated**: [08 §2.1](DESIGN.md#contract-08-2-1) `…-constraint-delegation-proof-required`, `§3.6` common read wrapper
items 2–3 and route census test, *Read One Order* steps 3–9 and its snapshot note, *List Orders*
step 4, audit-read refusal logging, AC-16 note, observability, `§4.4`; [01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt
Transition* step 1 and *Create Transition* step 1; `DESIGN.md §4.2`; D-111, D-114.

### D-142 (M) A boundary validation failure with no more specific reason is `request-invalid`; a no-op administrative edit is refused

**Decision**: register `request-invalid` (`REQUEST_INVALID`, InvalidArgument, 400), owned by the
engine and listed in [01 §3.3](DESIGN.md#contract-01-3-3)'s engine-contributed reasons and the [01 §4.7](DESIGN.md#contract-01-4-7) registry. It is the
rejection of a request that fails boundary schema validation and has no more specific registered
reason: a field its variant forbids (a `denial_reason` on a non-denied verdict; a `failure_reason`
on a completed acknowledgement), a value outside a closed enumeration (a `failure_reason` outside
[06 §4.4](features/06-workflow-seam.md#contract-06-4-4)), a delta or edit key naming no authored field, and an administrative edit naming no
field. It is raised in [01 §4.7](DESIGN.md#contract-01-4-7) *Validation flow at the boundary*, before authorization, exactly
as D-112's `expected-version-required`: no audit entry, no access-log row, and no idempotency
record probed, claimed or settled. `expected-version-required`, `page-size-exceeded`,
`filter-invalid` and `cursor-invalid` keep their conditions; `request-invalid` is the fallback
and never a second name for them.

An administrative edit in which no named field's value changes is refused `request-invalid`,
never committed. The boundary refuses the edit that names no field. The edit whose named fields
already hold their new values is only recognisable after the stored values are read, so
[04 §3.6](features/04-versioning.md#contract-04-3-6) *Apply Administrative Edit* declares a last guard, "at least one named field changes",
that refuses it `request-invalid` as an ordinary audited, settled guard refusal; this is the
reason's one post-authorization use. [01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt Transition* step 22 appends one entry per
changed field for an administrative edit, at consecutive sequence numbers, and one entry for
every other transition; step 25 settles the idempotency record with the last entry appended.

**Rationale**: `06` rejected a stray `denial_reason` and an out-of-enumeration `failure_reason`
"at boundary validation, like a missing expected version", and `04` rejected an unknown delta key
"as a malformed request", but no registered reason named the rejection, so the wire answer was
unspecified and "like a missing expected version" invited 428. This is the defect M-40 fixed for
cursors, recreated for request bodies. D-136 also left a `failure_reason` sent with a completed
outcome undecided. Separately, [01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt Transition* steps 22 and 25 still appended and settled "the audit
entry", one, where D-117 appends one per changed field, and a no-op edit had no defined outcome:
per-field auditing leaves it no entry to append and settle. Rejected alternative for the no-op
edit: commit it with one entry naming no changed field. It creates an audit row that records no
change, and it makes the per-field rule conditional on the edit's content. Closes remediation-review
findings R-M3 and R-L1 (2026-09-23).

**Amends**: D-112 — its boundary is shared: `expected-version-required` is one of the specific
reasons ahead of the `request-invalid` fallback. D-117 — its per-field rule now states the
no-op case. D-135 and D-136 — their "rejected at boundary validation" names `request-invalid`.

**Propagated**: [01 §3.3](DESIGN.md#contract-01-3-3) engine error surface and `request-invalid` description, `§3.6`
*Attempt Transition* steps 22 and 25, `§4.1` transition contract, `§4.7` registry and
*Validation flow at the boundary*; [04 §3.6](features/04-versioning.md#contract-04-3-6) *Append Amendment* step 1 and *Apply Administrative
Edit* steps 1–2 and its no-op note, `§4.1` precedence; [06 §3.6](features/06-workflow-seam.md#contract-06-3-6) *Reflect Verdict* step 1 and
*Acknowledge Fulfillment* input and step 1, `§4.4`; D-112, D-117, D-135, D-136.

**Amended by D-149**: the edit whose named fields all already hold their new values refuses
`administrative-edit-unchanged` (owned by `04`), not `request-invalid`; "this is the reason's one
post-authorization use" is withdrawn, and `request-invalid` is boundary-only.

### D-143 (M) Caller-supplied explanations go in a nullable audit `caller_reason`, covered by audit hash v2

**Decision**: `orders_transition_audit` gains `caller_reason text`, nullable. It holds what the
caller supplied as its explanation, stored as received and never interpreted: the mandatory
cancel reason, the optional hold reason, the amendment explanation (the same text a committed
amendment stores on `orders_order_version.amendment_reason`), or the closed `failure_reason`
value of a failed acknowledgement. It is NULL where the trigger carries no such input, where an
optional one was not supplied, and on every refused entry. `reason` stays the registered machine
reason: D-82's vocabulary on a committed entry, the registered refusal reason on a refused one.
`OrderHeld` carries the hold reason only when present; `OrderCancelled` and
`OrderFulfillmentFailed` carry the cancel and failure reasons; each is read from the committed
entry's `caller_reason`.

The hash covers the new column through **audit hash encoding v2**, since D-99 froze v1: v2 is v1
with `caller_reason` appended after `prev_hash`, framed like every field so NULL is the explicit
`0x00` byte, under row tag `VHP-BSS-ORDERS-AUDIT-ROW-v2`; genesis is unchanged. Every writer
emits `hash_version = 2`. The verifier selects the encoding per entry from the existing
`hash_version` column and rejects any other value; a CHECK allows only 1 or 2 and requires a
v1 row to have NULL `caller_reason`. No writer has shipped, so no v1 entry exists; v1 stays
defined with its vectors so the verifier never guesses, as D-99's evolution rule requires.

**Rationale**: [01 §3.7](DESIGN.md#contract-01-3-7) declared `reason` "Registered reason", non-null, while `07` put the free-text
cancel and hold reasons there, `06` put D-136's failure reason there and `04` put D-82's vocabulary
there. One column cannot be both a registered token and caller prose, and D-138's optional hold
reason left the hold entry nothing to write, while `OrderHeld` still promised "the hold reason"
unconditionally. Using the existing `hash_version` column, rather than declaring v2 the initial
encoding, keeps D-99's frozen v1 contract and its vectors valid and shows the evolution path the
first time it is used. Rejected alternative: a per-trigger rule for what `reason` holds. It makes
the column's meaning depend on the trigger, so a reader cannot tell a registered reason from
caller text without the state table. Closes remediation-review finding R-M4 (2026-09-23).

**Amends**: D-99 — encoding v2 is added and becomes what writers emit; v1 remains frozen for
verification. D-136 — the failure reason is recorded in `caller_reason`, not `reason`. D-138 —
the optional hold reason is the hold entry's `caller_reason`, NULL when absent, and `OrderHeld`
carries it only when present.

**Propagated**: [01 §3.7](DESIGN.md#contract-01-3-7) `orders_transition_audit` (`hash_version`, `entry_hash`, `reason`,
`caller_reason`, target-shape invariants), `§3.6` *Attempt Transition* step 22, `§4.4` event
catalogue, `data` extension note and *Canonical audit hash v2*; [04 §3.3](DESIGN.md#contract-04-3-3) version reason
vocabulary, `§3.7`; [06 §3.6](features/06-workflow-seam.md#contract-06-3-6) *Acknowledge Fulfillment* step 2 and *Workflow Cancel* step 6,
`§4.3`, `§4.4`; [07 §3.1](DESIGN.md#contract-07-3-1) hold record, `§3.6` *Hold Then Resume* step 3, `§4.6` *Cancel Order*
step 3; D-99, D-136, D-138.

**Amended by D-148**: "D-82's vocabulary on a committed entry" is the closed list of one token per
trigger in [01 §3.7](DESIGN.md#contract-01-3-7) *Committed audit reason tokens*; no detail, such as an expiry's state or
policy revisions, is composed into `reason`.

### D-144 (M) The composed read carries the activation re-check's fulfillment inputs

**Decision**: [08 §4.2](DESIGN.md#contract-08-4-2)'s composed order read carries, beside the order document, the current
version's **fulfillment inputs**: each line's stored `orders_order_line.overlap_scope_key`, the
version's market (`market_currency`, `market_region`) and the version's `payer_tenant_id`. They
are carved out of §4.2's "no guard state" rule by name: they are stored commercial facts of the
version, and Workflow executes the re-check that [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) *Re-check Activation Preconditions*
specifies — step 5 compares against the frozen market, step 6 reads occupancy for each stored key
with the version's payer. `08` restricts no composed-read field by principal, and these follow
that pattern: every principal the PDP lets read the order sees them, since none is a secret.
Workflow reads them under its existing finite order-ID `order × read` scope; no Workflow-only read
is added.

**Rationale**: `03`'s re-check reads the key "as stored on `orders_order_line.overlap_scope_key`"
and compares with the version market, yet Workflow reaches Lifecycle data only through Lifecycle
reads, and the composed read carried neither while [08 §4.2](DESIGN.md#contract-08-4-2) forbade exposing guard state, so the
specified re-check had no source for its inputs. Re-resolving the key from the registry is not an
option: the re-check must test the key the claim holds. Rejected alternative: a separate
Workflow-only read. It adds an endpoint, a permission and a second projection of the same rows for
values that are not confidential. Closes remediation-review finding R-M5 (2026-09-23).

**Propagated**: [08 §4.2](DESIGN.md#contract-08-4-2); [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) *Re-check Activation Preconditions* step 1; [06 §4.3](features/06-workflow-seam.md#contract-06-4-3);
`UPSTREAM_REQS.md` §2.6.

### D-145 (M) A `PATCH` selects its trigger from field classes alone; a commercial edit outside draft is `not-admissible`

**Decision**: order and line `PATCH` select their trigger from the named fields' classes alone,
reading no state first: any commercial (or commercial-frozen) field selects `draft-mutate`
(`order × write`); administrative fields only select `administrative-edit` (`order × edit`); a
draft request naming both is refused `mixed-field-classes` (D-118). The engine then authorizes
under the selected trigger's action and checks state-table admissibility. `draft-mutate` has a row
only from `draft`, so a `PATCH` naming a commercial field outside `draft` — mixed or not — refuses
the engine's `not-admissible`, naming the state and trigger. That is the outward reason; the engine
gains no mapping. `commercial-field-immutable` stays registered as the defensive field-classification
guard of [04 §3.6](features/04-versioning.md#contract-04-3-6) *Apply Administrative Edit*, which a `PATCH` no longer reaches.

**Rationale**: `02` *Edit Order* and *Edit or Remove Line* read the order's state from the draft
snapshot to choose the trigger, but `01` states that authorization precedes any state read and
[08 §4.3](DESIGN.md#contract-08-4-3) authorizes a `PATCH` under the action of the trigger its fields select, so the trigger
depended on a read the caller had not yet been authorized for. Selecting by class removes the read.
The outward reason was chosen for fewest cross-slice changes: keeping `commercial-field-immutable`
would need the engine to map a slice's `not-admissible` to a slice reason, a mapping that does not
exist and that would put slice knowledge in the engine; `not-admissible` already names the state and
trigger, which tells the caller the order is past draft. Rejected alternative: authorize the
state pre-read as `order × read`. It adds a second authorization per `PATCH` and still leaves the
trigger dependent on a read that can go stale before the lock. Closes remediation-review finding
R-M7 (2026-09-23).

**Amends**: D-117 — a commercial line field outside `draft` refuses `not-admissible`, not
`commercial-field-immutable`, and only an administrative-only post-draft line `PATCH` maps to
`order × edit`. D-118 — the one-trigger mapping holds in every state and reads no state.

**Propagated**: [02 §3.2](DESIGN.md#contract-02-3-2) field classifier scope, `§3.3` order and line `PATCH` rows, `§3.6`
*Edit Order* steps 1–3 and description, *Edit or Remove Line* steps 1–3, `§4.3` *One request,
one trigger*; [04 §3.3](DESIGN.md#contract-04-3-3), `§3.6` *Apply Administrative Edit* step 1; [08 §4.3](DESIGN.md#contract-08-4-3) action map;
`DESIGN.md` endpoint note; D-117, D-118.

**Amended by D-147**: the `not-admissible` above is reachable because `expected_draft_revision` is
optional at the boundary for `draft-mutate` and compared only at [01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt Transition*
step 12, after step 11's admissibility check; in `draft` an absent value refuses
`version-conflict`.

### D-146 (H) The submit-time automatic acceptance keys on the submit request, and the recording-party bar also keys on stored actor tenants

**Decision**: the automatic acceptance written at submit is keyed on facts of the submit request
itself, never on `orders_order.sales_path`: it is written **only** when (1) the submit request the
engine's authorization allowed carried no delegation proof reference and (2) the submitting
principal's trusted `subject_tenant_id` equals the order's `resourceTenantId`, which is frozen at
submit. Its `recording_path` is `self_service` by construction. Otherwise the submit writes no
acceptance, and acceptance, where required, is recorded separately through [05 §3.6](features/05-preconditions.md#contract-05-3-6) *Record
Acceptance*. That step's recording-party bar applies per role version — version 1 (creator), the
submitted version (submitter) and an amendment-appended `expected_version` (amender) — and
refuses `acceptance-recording-party-barred` when the caller equals that version's actor and either
`sales_path = partner_placed` or that version's actor tenant differs from `resource_tenant_id`.
The actor tenant is a new column, `orders_order_version.actor_tenant_id` (uuid, NOT NULL), which
the engine writes from the trusted SecurityContext with `actor` on every version append, because
`actor` holds only the subject UUID. `sales_path` counts only the proof the PDP accepted once
`UPSTREAM_REQS.md` `…-upreq-pdp-policy-integration` item 4 is delivered; until then any supplied
proof reference counts. Residual consequence: a self-service client that sends a proof reference
on its own-tenant create is recorded `partner_placed`, and one that sends it on its own-tenant
submit gets no automatic acceptance. Its buyer must then record acceptance through a permitted
recording party. The remedy is not to send a proof on own-tenant requests (clients **SHOULD NOT**),
and Q-30 tracks the dead-end risk.

**Rationale**: `sales_path` is fixed at create from a proof-reference proxy (D-140), but
delegation can begin after create: a partner creates in its own tenant without a proof, edits
`resourceTenantId` (editable in draft, [02 §4.3](DESIGN.md#contract-02-4-3)) to a customer tenant and submits with a proof,
or a buyer creates and a delegated partner submits within delegated scope ([08 §4.3](DESIGN.md#contract-08-4-3)). Both
orders stay `self_service`, so the recording-party bar was skipped and the self-service automatic
acceptance could record the partner's submit as the customer's consent — what D-31 and D-130
exist to prevent. D-140's "imprecise in one direction … applies more, never less" was therefore
false. Both new keys are observable facts: the submit request's proof reference and subject
tenant, and the stored actor tenant of each role version. The bar is per role version so that a
buyer who created its own order is not barred merely because a partner submitted it. In the
other direction, an unvalidated proof on an own-tenant create over-classifies the order, which
can dead-end a self-service buyer; counting only a PDP-accepted proof closes that once item 4 is
delivered, and until then the consequence is disclosed rather than hidden. Rejected alternative:
recompute and freeze `sales_path` at submit. It still describes one request per order, so a
partner-appended amendment and a partner-submitted buyer draft would stay misclassified, and it
turns an immutable create fact into a second write. Closes re-review findings RR-H1 and RR-M1
(2026-09-23).

**Amends**: D-140 — the proxy is imprecise in both directions and no acceptance control keys on
it alone; D-130 — step 4 also bars a role actor whose recorded tenant differs from the resource
tenant; D-31 — "partner-placed" is not read from `sales_path` alone; D-16 — the submit-time
acceptance keys on the submit request.

**Propagated**: [01 §3.6](features/01-foundation.md#contract-01-3-6) *Create Transition (D-105)* creation initialization, `§3.7`
`orders_order.sales_path`, `orders_order_version.actor_tenant_id`, `orders_acceptance`, `§4.4`
`OrderSubmitted`; [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) *Run Gate and Submit* step 15; [05 §1.2](DESIGN.md#contract-05-1-2), `§3.6` *Record Acceptance*
step 4 and *Self-service submit constitutes acceptance*, `§4.2`; [08 §2.2](DESIGN.md#contract-08-2-2), `§4.3` acceptance rule
and automatic acceptance; `UPSTREAM_REQS.md` `§2.9` item 5; D-16, D-31, D-130, D-140.

### D-147 (M) `expected_draft_revision` is optional at the boundary for `draft-mutate` and compared only after admissibility

**Decision**: on a `draft-mutate` request, `expected_draft_revision` is **optional at the
boundary**: its absence is never a boundary rejection, and no validation reason (neither
`expected-version-required` nor `request-invalid`) names it. The engine compares it only at
[01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt Transition* step 12, which follows step 11's state-table admissibility check.
A commercial order or line `PATCH` against an order past `draft` therefore reaches step 11 and
refuses `not-admissible` naming the state and trigger, as D-145 states, whether or not the value
was sent. In `draft`, an absent value differs from every revision, so step 12 refuses it
`version-conflict` naming the authorized current version and draft revision, exactly as for a
mismatched value. The idempotency request fingerprint covers `expected_draft_revision` for draft
writes and submit; an absent value on `draft-mutate` takes the same fixed not-applicable sentinel
as a request to which the field does not apply. Submit is unchanged: it is admissible only from
`draft`, where reads expose `draftRevision`.

**Rationale**: D-145 answered a commercial `PATCH` outside `draft` with `not-admissible`, but
[02 §4.1](features/02-capture.md#contract-02-4-1) required `expected_draft_revision` on every commercial draft edit, and [08 §3.6](features/08-read-and-authz.md#contract-08-3-6) *Read
One Order* returns `draftRevision` only for a draft. A client of a submitted order has no value to
send, so a boundary that required the field would reject the request before the engine ran, and
D-145's outward reason was unreachable. Deferring the comparison to step 12 keeps the draft
concurrency protection of OL-4 intact — a draft edit without a revision still cannot commit — and
lets admissibility, which precedes the version check in [01 §4.1](DESIGN.md#contract-01-4-1), answer first. Rejected
alternative: expose `draftRevision` on every read so a client can always send it. It publishes a
counter that has no meaning after `draft` and still leaves a client that omits it with a boundary
reason instead of the state. Closes re-review finding RR-M2 (2026-09-24).

**Amends**: D-145 — its `not-admissible` for a post-draft commercial `PATCH` is reached because
`expected_draft_revision` is optional at the boundary and compared only at step 12.

**Propagated**: [01 §3.6](features/01-foundation.md#contract-01-3-6) *Attempt Transition* mutable-draft concurrency note and step 12,
`§4.2` request fingerprint, `§4.7` *Validation flow at the boundary*; [02 §3.6](features/02-capture.md#contract-02-3-6) *Edit Order* and
*Edit or Remove Line* inputs, `§4.1`; [08 §3.3](DESIGN.md#contract-08-3-3); `DESIGN.md` endpoint note; D-145.

### D-148 (M) A committed audit entry's `reason` is one closed token per trigger

**Decision**: *(Amended by D-182: the closed list gains a twenty-first token,
`force-fail-unreconciled`, written on rows 28 and 29; the fulfillment-outcome mapping includes it.)*
`orders_transition_audit.reason` on a committed entry holds exactly one closed
token per [01 §4.3](features/01-foundation.md#contract-01-4-3) trigger, equal to the trigger name: `create`, `draft-mutate`,
`administrative-edit`, `submit`, `cancel`, `auto-void`, `reflect-approval-required`,
`reflect-approval-not-required`, `reflect-approval-granted`, `reflect-approval-denied`,
`begin-fulfillment`, `report-spawn-signal`, `acknowledge-completed`, `acknowledge-failed`,
`cancel-workflow-mediated`, `amendment`, `hold`, `resume`, `expire` and `record-acceptance`. A
trigger owning several rows writes the same token on each. No detail is composed into the token.
D-82's version-reason vocabulary `{create, submit, amendment}` is unchanged; those three are the
same strings here, and the six PRD §6.2 reasons D-82 places on the audit column map to tokens:
approval reflection → the four `reflect-approval-*`; hold → `hold`; resume → `resume`; cancel →
`cancel` or `cancel-workflow-mediated`; fulfillment outcome → `acknowledge-completed` or
`acknowledge-failed`; expiry → `expire`, or `auto-void` for a draft. A refused entry keeps the
registered refusal reason. The audit table has no detail column, so an expiry's details are not
on `reason`: the expired state is the entry's `from_state`, and the TTL, effective policy
identity and both revisions are in the expiry contribution the request fingerprint covers and on
the `OrderExpired` payload.

**Rationale**: D-143 made `reason` a registered machine token, but [01 §3.7](DESIGN.md#contract-01-3-7) listed the
committed-entry vocabulary as `create`, `submit`, `amendment` "and the state-only reasons such as
hold, cancel or expiry" — an open list — and [07 §3.6](features/07-hold-and-expiry.md#contract-07-3-6) put "a stable expiry reason naming the
state and TTL" and the policy identity and revisions in the audit reason. A composed value is
neither registered nor closed, so a reader could not enumerate what the column holds, which is
the defect D-143 set out to remove. Using the trigger name needs no second vocabulary, and the
trigger is already a closed set checked against the state table ([01 §4.6](DESIGN.md#contract-01-4-6)). Rejected
alternative: add a jsonb detail column to the audit table for expiry details. It adds a hashed
column, and so an audit hash encoding change, for values the fingerprint and the event already
carry. Closes re-review finding RR-M4 (2026-09-24).

**Amends**: D-143 — "D-82's vocabulary on a committed entry" is the closed per-trigger token list
above, of which D-82's three version reasons are a subset.

**Propagated**: [01 §3.7](DESIGN.md#contract-01-3-7) `orders_transition_audit.reason` and *Committed audit reason tokens*,
`§4.4` `OrderExpired` payload, `§4.7` registry note; [07 §3.6](features/07-hold-and-expiry.md#contract-07-3-6) expiry contribution and fingerprint
paragraph; D-143.

### D-149 (M) An administrative edit that changes nothing refuses `administrative-edit-unchanged`, keeping `request-invalid` boundary-only

**Decision**: register `administrative-edit-unchanged` (`ADMINISTRATIVE_EDIT_UNCHANGED`,
FailedPrecondition, 400 — the registry's mapping for every other FailedPrecondition guard
reason), owned by `04` and listed in its reasons and the [01 §4.7](DESIGN.md#contract-01-4-7) registry. It is raised by the
last guard of [04 §3.6](features/04-versioning.md#contract-04-3-6) *Apply Administrative Edit*, "at least one named field changes", which
compares the named values with the stored ones under the engine's row lock; the refusal is an
ordinary audited, settled guard refusal. An administrative edit naming no field stays a boundary
`request-invalid`. `request-invalid` is boundary-only again: it is raised only by [01 §4.7](DESIGN.md#contract-01-4-7)
*Validation flow at the boundary*, before authorization, and has no post-authorization use.

**Rationale**: D-142 used `request-invalid` for both halves of the no-op edit, which made it a
post-authorization, audited refusal for the edit whose named values already hold, contradicting
its own definition as a boundary schema failure ([01 §3.3](DESIGN.md#contract-01-3-3)) and giving a well-formed request an
InvalidArgument category for what is a fact of the stored state. A dedicated FailedPrecondition
reason matches every other guard on data in the registry. Rejected alternative: accept the
unchanged edit as a success with no entry. It violates [01 §4.1](DESIGN.md#contract-01-4-1)'s "one settled record and at
least one audit entry per success" and returns applied for an edit that applied nothing. Closes
re-review finding RR-M5 (2026-09-24).

**Amends**: D-142 — the unchanged administrative edit refuses `administrative-edit-unchanged`, not
`request-invalid`, and `request-invalid` has no post-authorization use.

**Propagated**: [01 §3.3](DESIGN.md#contract-01-3-3) `request-invalid` description, `§3.6` *Attempt Transition* step 22,
`§4.1` transition contract, `§4.7` registry; [04 §3.3](DESIGN.md#contract-04-3-3) reasons, `§3.6` *Apply Administrative Edit*
step 1 and its no-op note; D-142.

## PriceBook seam remediation (2026-09-29)

D-150–D-168 govern the PriceBook target in ADR-0008; D-159–D-168 (2026-09-30) pull each ask back
onto the seam that already exists on `16705a243` where one does, and amend D-151, D-152, D-153 and
D-157 as noted. Superseded entries below are retained as history; they do not authorize use of
removed producer contracts. Counterpart status is tracked per requirement in
[`UPSTREAM_REQS.md`](UPSTREAM_REQS.md).

| Decision | Orders-side rule | Counterpart status |
|---|---|---|
| D-150 | Adopt PriceBook at `16705a243`; revision per line, common assessment date, seller-scoped facts. Supersedes D-93's single catalog version and D-122/D-123's frontier mechanism. | Rating T-D-37 / Subscriptions SUB-D-29 adopted the model; adapters deferred. |
| D-151 | Pricing owns a shared prospective-purchase assessment with complete per-item tri-state verdicts and selected bindings. Orders adds its nine local checks, never forks the commercial gate. | New owning SDK and seller authorization are required, not built here. |
| D-152 | *(Historical deadline-source qualification: D-191 replaces the local/Product interval assumption with Pricing-issued receipt validity and missing seller-policy discovery/resolution.)* Initial binding is an accepted order-version snapshot, never a renewal resolve call. It carries an absolute activation deadline and price/descriptor boundaries; no hold/resume extends it. Supersedes the TTL-based price-hold interpretation. | Joint initial-binding, clock/protection and first-period contract proposed; Product duration unset means no submit. |
| D-153 | Consume an opaque overlap key and active-count/limit/provenance. Preserve the one-in-flight-order rule; enforce subscription cardinality at Subscriptions' active commit. Supersedes D-108's removed Product-key lookup. | Key derivation/namespace and SUB-O5 amendment unagreed; `plan_id` is not a default. |
| D-154 | Rating supplies whole-order/per-line figures and net pre-tax annualized TCV over the exact selected prices. Three charge kinds; no local money arithmetic. D-40's ownership and required-evaluation failure posture remain. | Rating pre-purchase DTO/aggregation and exact-price mode require amendment. |
| D-155 | Lifecycle SDK preserves all verdict authorities, explicit per-line completion mappings, version/idempotency/correlation and compensation evidence. REST and SDK enter the same application service. | Local design fixed; SDK implementation and Workflow consumer still pending. |
| D-156 | Resolve item quantities/optional selections, own/default dimension pins, descriptor versions and currency/rounding evidence explicitly. One acquisition line still spawns one subscription. | Mirrored in proposed producer DTOs and provisioning contract. |
| D-157 | Subscriptions validates an accepted binding and protection at activation; preserve versioned creation keys, actual start, fee-free compensation and topology completeness. Rechecks are early aborts only. | SUB-O1/O2/O5/O9/O10 and topology/protection requirements remain open. |
| D-158 | Expanded pins are obtained through authorized immutable-version reads; events carry bounded projections and a version reference. Submit/amendment require a bounded aggregate purchase and receipt contract. | Consumer/event and capacity validation required before runtime release. |

**Alignment with existing seams (2026-09-30).** Each row names the seam already built or decided on `16705a243` that the Orders rule adopts.

| Decision | Orders-side rule | Existing seam adopted | Amends |
|---|---|---|---|
| D-159 | *(Read contract assumptions amended by D-187; historical rationale retained.)* The stored pin nests `items[].chains[]` exactly as resolve answers the matrix: one slot per `(item_id, dim_value)`, each `{dim_value, uncovered, binding?}` with the binding's DTO fields verbatim; `selected_dim_value` stays at item scope and selects the consumed slot. 1,000 bound slots per line is an added admission cap (Pricing's pin cap is per request, one revision); a matrix is never truncated. | `PricingResolveItemDto.chains[]` (dto.rs:66–119); SUB-D-29 stores one binding per `(item, chain)`; `MAX_PINS = 1_000` | D-156, D-158 |
| D-160 | *(Read assumptions amended by D-187; identity construction superseded by D-194; historical rationale retained.)* Pricing access follows D-424: a `bss-orders.system` subject with `plan:read` and `price:read`, the adapter building that subject's context for the order's seller tenant (Pricing's `reference_ticker::system_actor` pattern, in-process only). No seller parameter on the Pricing API, no delegation proof to Pricing. The `get_sku` row needs its own Products SKU read grant (round 2, R2-7). | Pricing D-424; `read_contract.rs` door (`subject_tenant_id`, `PolicyEnforcer`); `reference_ticker.rs` | D-151 |
| D-161 | *(Read contract assumptions amended by D-187; historical rationale retained.)* Catalog predicates are evaluated from `PricingReadV1 { resolve, price, current_revision }` over the three existing reads; currentness is the `revisions[]` entry with `id == plan_revision_id` having `state == published` and `rev_no == published_rev` (round 2, R2-5); the consumed slot is the chain whose `dim_value` equals `selected_dim_value` (R2-4); only market applicability and the residual owner rule remain the `…-pricing-purchase-assessment` ask. `PricingPurchaseV1::assess` is withdrawn; the deadline is local (D-162). | `GET /resolve`, `GET /prices/{id}`, `GET /plans/{id}` (`PricingPlanDto.published_rev: Option<i32>`, `revisions[]`); the 15 golden files (resolve, price); `Sku.sellable`/`Sku.lifecycle` in products-sdk | D-151 |
| D-162 | *(Historical: initial-activation comparison and no-receipt assumptions superseded by D-188/D-190; deadline policy superseded by D-191 and descriptor mapping pending R06.)* The accepted prices are verified by a **pinned** comparison at activation (round 2, R2-2): Subscriptions encodes the accepted consumed slots as pins, runs the ordinary `resolve(revision, activation date, pins)`, and compares `binding.price_id` per consumed slot; the walk keeps the accepted price across `new` successors and moves only for an `all` successor or an ended price, so refusal (`accepted-price-mismatch` → `order-binding-expired`) happens exactly when the promise cannot be kept. A signup resolve is not used. No initial-acceptance mode, no receipt, no determinism claim (R2-6); `activation_deadline` is derived locally from `min(ends_on, temporary_until)` and a seller-scoped Orders `max_acceptance_interval` (R2-1). The comparison runs at the `applied` commit and its bindings are the first period's pins; SUB-P5 must admit a superseded accepted revision at `create`. | Pricing D-420 rules 2–4 (`resolve.rs` `walk`, `binds_on`, `keep_for_bound`); D-422; T-D-37 | D-152 |
| D-163 | The overlap key stays Subscriptions' registry-owned `catalogSubscriptionProductKey` (SUB-G1); the PriceBook derivation proposed to that owner is the SKU of the line's paid `recurring` item(s). `plan_id` is not proposed. Orders stores the key as answered. "Never derived from plan/SKU IDs" is withdrawn. | SUB-G1, SUB-O5/SUB-P8 shape; resolve `items[].sku_id`, `charge_kind` | D-153 |
| D-164 | SKU protection is inherited from the accepted revision's `plan_item` references; Orders reserves nothing and asks for no owner, kind or receipt. The residual ask was a release report counting non-terminal accepted orders; D-196 now requires Pricing-coordinated admission closure/drain and fenced usage proof, including potentially committable preparations. `ReferenceKind::OrderLine` is withdrawn. | Pricing D-414 (references outlive the revision), D-410; Products P-D-189/P-D-194, `SKU_REFERENCED`; registry owner check | D-157 |
| D-165 | Provisioning maps onto Subscriptions' existing contract: acceptance instant → `customerAcceptedAt`, actual activation → `serviceActivatedAt`, `contractEffectiveAt` from the Contract; `activated` in the completion acknowledgement means `applied`, not `approved`; `oss_unconfirmed` is a provisioning failure; a stalled `active → cancelled` compensation is escalated by Workflow while the order stays `in_fulfillment`. | 01-foundation-lifecycle.md (create key, OSS-async activate, three instants); SUB-O3; Workflow PRD "escalates rather than a third leg" | D-157 |
| D-166 | The approval policy owner is Workflow's approval adapter, which may embed `cf-gears-bss-approval`; "Generic Approval service" is withdrawn from PRD and DESIGN. Lifecycle still only reflects verdicts with their authority. | `gears/bss/libs/approval` as used by Pricing and Products | D-73, Q-09 |
| D-167 | The Rating request is the accepted matrix in resolve's vocabulary (`lines[{line_id, plan_revision_id, items[{item_id, quantity, chains[]}]}]`, `assessment_id`, `resolve_date`); periods are `month` or `year`; an unsupported cycle refuses evaluation. Whole-order figures and TCV remain a Rating ask (no existing seam supplies them). | Pricing periods; T-D-36/37/38; D-415 | D-154 |
| D-168 | The billing chain is not `gears/bss/ledger`: invoicing/at-sale valuation, indicative tax and Payments are unowned capabilities registered for whichever specification takes them; the Ledger is the GL posting and settlement target and generates no invoices. External references travel order → create → billable fact → invoice, snapshotted at the first handoff. | `LedgerClientV1` (ledger-sdk/src/api.rs) | UPSTREAM_REQS §4 |
| D-169 | Revision currency follows Pricing D-454/D-460 (fork tip `7d3544156`): the sale's revision is the plan's `in_effect` revision on the assessment date, not `current`, which may name a revision waiting for its sale date; resolve's 409 `REVISION_NOT_YET_AVAILABLE` is `catalog-predicate-failed` with detail `revision-not-yet-available`; its 503 `REGISTRY_UNAVAILABLE` (D-469) is `pricing-revision-unavailable`. | Pricing D-446–D-454, D-460, D-469 (served contract at `7d3544156`) | D-161; Atlas row 03a; F36, F37 |
| D-170 | Item model follows Pricing D-467: a plan item is a SKU and its entry; there are no optional or included items and no `treatment`, `qty_min` or `included_qty`. The pin stores those members as null when resolve omits them and no predicate evaluates them; the submit gate's plan-scoped add-on rule bounds are withdrawn for `new_sale` (an add-on is an ordinary line) pending the Change Orders decision (Q-33, Atlas ticket T9). | Pricing D-467 (`b5b88fbe8`; goldens re-recorded) | D-156, D-159; PRD §6 add-on note; Changes PRD §17 |
| D-171 | *(Orders subject choice superseded by D-194; historical rationale retained.)* Products P-D-222 is recorded: only in-process code may act as `bss-pricing.system`; every Pricing and Products REST door refuses it with 403 `SYSTEM_ACTOR_RESERVED`. Consequence: the gate's SKU read runs as this gear's own `bss-orders.system` subject under its own grant (D-160) and never borrows Pricing's identity; the resolve-echo alternative is withdrawn. | Products P-D-222 (`5973c5d52`) | D-160, D-177; UPSTREAM_REQS §2.10 |
| D-172 | Failure-reason catalog aligned with the Workflow branch (`bss/orders-workflow` @ `3ccf7793c`): `dependency-graph-invalid` is withdrawn as an emitted value because Workflow D-196 removed the dependency graph; boundary validation keeps accepting it for replayed payloads. Workflow emits `order-binding-expired` and an interim `line-execution-failed` for a stale payment authorization; the D-152 mapping is the one that lands. | Workflow D-193, D-196; W/design/06:1156-1187 | D-136, D-142, D-152; 06 §4.4 |
| D-173 | Idempotency receipts for workflow-class triggers are retained at least as long as the issuing Workflow process may retry under the same key: the Workflow branch keeps a key for 30 days (W/design/05:229-233), so the Lifecycle receipt floor is 30 days and any shorter TTL is superseded for those triggers. A retry after day one replays the original receipt; it is never a new transition. *(Extended by D-203: the D-201 internal `replace-fulfillment-grant` continuation joins this retention class.)* | Workflow `upreq-lifecycle-workflow-key-retention` | 01 §3.1; Atlas row X22; F43 |
| D-174 | Q-12 closed: on `OrderAmended` Workflow must obtain the new version's requirement verdict and reflect it before any `begin_fulfillment` on that version; Lifecycle holds the order `submitted` with its TTL running until then (`…-upreq-workflow-amendment-verdict`). | Workflow PRD amendment owed | Q-12; UPSTREAM_REQS §2.6 |
| D-175 | *(D-198 narrowly permits durable Workflow pause/revoke coordination; it adds no verdict/authorization-read dependency.)* Verdicts and authorization outcomes are adapter-owned facts, not provider-verified evidence: Lifecycle checks that a deciding authority is named and stores the verdict (D-166); it consumes Workflow's three-valued `AuthorizationOutcome` and never stores it (D-131); it calls no `ApprovalReadV1` or `AdmissionReadV1`. Both gears agree; Seam Atlas C02/D05 carry the correction (Atlas ticket T11). | Workflow D-197 (adapter port); W/DESIGN:707-712 forbids a Lifecycle → Workflow call | D-131, D-166 |
| D-176 | `category = change` stays refused `category-not-admitted` in this DESIGN for the acquisition slice although the PRD admits it for increases: the change path's document model, gate and application belong to the Change Orders PRD §17 and its design has not landed; Workflow D-199 defers Change Orders and Subscriptions has designed no apply-on-live-subscription. Phase-two obligations are recorded in Q-33. | Changes PRD §17; Workflow D-199 | Capture constraints; Atlas ticket T9 |
| D-177 | *(Subject choice superseded by D-194; retirement trigger amended by D-189; historical rationale retained.)* `ProductsClient::get_sku` under the `bss-orders.system` grant is the interim source of `sellable` and `lifecycle` for predicate row 6 until Pricing publishes `SellabilityV1`; on that day the read is retired and Seam Atlas P9 holds. Until then P9 is an atlas correction, not an Orders change. | Pricing: no sale-time sellability read at `7d3544156` (revision availability only, D-454/D-460; publish-time SKU checks, D-466) | D-161, D-171; UPSTREAM_REQS §2.10 |
| D-179 | Q-05 route (a) for the in-flight claim (design review 2026-10-04, Artifizer, HIGH): the claim tuple and its partial UNIQUE become `(payer_tenant_id, resource_tenant_id, overlap_scope_key)`, cap still one. A partner's orders for different customers no longer collide or run serially, and a refusal names only an order of the caller's own resource tenant, closing the cross-customer existence leak. `resource_tenant_id` is the order's immutable-after-submit axis; Orders still never computes `overlap_scope_key`. The occupancy read is asked for on the same tuple (`resourceTenantId` as a default dimension of Subscriptions' key); until Subscriptions enforces it, predicate 7 applies the tuple Subscriptions answers and never re-buckets locally. | Subscriptions `design/03-plan-changes.md` §4.4 already admits extra key dimensions; `Subscription.resourceTenantId` exists | D-83, D-126, Q-05; `SUB-O5` amendment |
| D-180 | *(Mechanism extended by D-198: reserve before OSS dispatch; receiver barriers fence admission and confirmation. Historical rationale follows.)* Subscription-side cardinality is **advisory at order time** (design review 2026-10-04, finding #2, HIGH): a predicate-7 or re-check pass is a pre-check, not an admission guarantee, and Workflow MUST handle `overlap-collision` on the failure-acknowledgement path. `…-upreq-overlap-activation-atomicity` becomes a release gate: submit/activation is not production-ready until Subscriptions agrees and delivers it. Orders' contribution is narrowed and stated: at most one in-flight order per claim tuple, and while the occupancy answer is per payer predicate 7's `proposed` also counts the lines of this payer's other in-flight orders claiming the same key under another resource tenant (own claim data, not re-bucketing). The residual race is with entries into `active` that bypass Orders (direct, `resume`, `transfer`, key-altering `changePlan`). Recommended upstream mechanism: an in-transaction `(overlapScopeKey, slot)` claim mirroring ADR-0007 (alternative: a per-key row lock); `coord` leases and toolkit-db advisory locks rejected. | ADR-0007 claim pattern; Subscriptions `design/03-plan-changes.md` §4.4 (per-aggregate point lookup at `activate`/`resume`/`transfer`/`changePlan`); `gears/bss/libs/coord/README.md` "Don't use `coord` when…"; `libs/toolkit-db/src/advisory_locks.rs` (session-level) | D-89, D-126, D-179 interim; Q-05; 03 §2.2, §4.2 predicate 7; 06 §4.3; UPSTREAM_REQS §2.1 |
| D-181 | Every in-flight state is bounded in production by **provisional platform TTLs shipped as data, not code** (design review 2026-10-04, finding #3, MEDIUM). The migration seeds the five permanent `orders_state_ttl_policy` platform rows with `draft` **90 days**, `submitted` **14 days**, `pending_approval` **14 days**, `approved` **30 days**, `on_hold` **30 days**, at `policy_revision` 1 with the new `provisional = true` marker; Product refines them under Q-06/Q-07(b) by promoting a new revision through the existing policy channel, which sets `provisional = false`. Readiness gating on an unset TTL is **rejected**: platform readiness means "can serve traffic" and an unanswered Product question must not become an outage (D-90 kept). Instead the policy channel **refuses to promote a NULL `ttl_duration` to a production environment** — a release gate, not an outage. `ttl_duration` stays nullable for non-production only, where the sweep's unset branch still skips the scope; a NULL row observed in production can arise only by bypassing the channel and pages as an integrity alert, and the sweep still skips it rather than invent a value. The no-configured-TTL production alert becomes a **provisional-default-in-effect** gauge and alert; a configuration-read failure stays a health signal. With every expirable state finite, resume ≤ 5 and amendment ≤ 20 now bound total configured dwell: 74 entries × T_max 30 days = **2,220 days** plus scheduler delay, and `draft` ≤ 90 days from `created_at`. `in_fulfillment` stays exempt (unchanged). Q-27 closes. | Subscriptions ships a 90-day draft auto-void platform default while Product's value is TBD (`gears/bss/subscriptions/docs/PRD.md` §15 "Draft retention TTL", SUB-D-11 amendment); Orders already reads an unset election as its safe value (`features/05-preconditions.md` "Where the election is stored", D-107/D-133); Orders Workflow ships a default 72-hour escalation window (`gears/bss/orders-workflow/docs/PRD.md` §6.2 *Escalation Timer*); promotion-time channel validation as for seller rows under D-137 (`DESIGN.md` §3.7 table `orders_state_ttl_policy`); readiness semantics `docs/arch/toolkit-oop/ADR/0005-cpt-cf-adr-eventual-readiness.md` | D-90; Q-06, Q-07(b), Q-17, Q-27, Q-30; PRD §15 rows 5 and 7 |
| D-182 | A wedged `in_fulfillment` order gets an owner and a bounded, operator-initiated end (design review 2026-10-04, finding #4, MEDIUM). (a) The §4.7 residual row and ADR-0007 are corrected: Workflow's terminals do release the claim, but none is reachable without complete evidence and a healthy Workflow, and the claim is per `(payer_tenant_id, resource_tenant_id, overlap_scope_key)` (D-179). (b) Workflow's overdue escalation to the fulfillment operator, and its handling of a forced terminal, is `…-upreq-workflow-overdue-escalation`, a production release prerequisite; 07 §3.8 adds the Orders-side `fulfillment_overdue_orders` / `fulfillment_overdue_oldest_age_seconds` gauges over `in_fulfillment` and exempt holds past expected fulfillment time + 24 h, alerting when non-zero. (c) Engine change under 01 §4.6: trigger `force-fail-unreconciled` (the twenty-first) and rows 28 `in_fulfillment → fulfillment_failed` and 29 `on_hold` (pre-hold `in_fulfillment`) `→ fulfillment_failed`, endpoint `POST /forced-failure` owned by 07. Guards in order: `forced-failure-reason-required`, row 29's `prehold-not-in-fulfillment`, `spawn-signal-not-recorded` (pre-spawn the ordinary cancel is open), `overdue-window-not-elapsed`, `second-approver-required`. Two-person without a new table: the requester's attempt refuses `second-approver-required`, committed and audited, returning `requestAuditId`; a different principal approves by naming it within a 24 h approval window and before any state change. Authorization is a distinct break-glass, user-only `order × force-fail-unreconciled` grant (new matrix row and Fulfillment Operator column) that neither Seller Operator nor the Workflow principal holds. It lands in the existing `fulfillment_failed` with `failure_reason = operator-forced-unreconciled` and `OrderFulfillmentFailed`, so no state or event is added (ADR-0004 holds). The closed evidence schema gains a forced variant, written only by this trigger: `activation_dispatched = true`, `at_sale_facts_emitted` and `no_active_subscription_remains` = `unknown`, and `operator_attestation {requested_by, request_audit_id, requested_at, approved_by}`. It never asserts compensation, no consumer may read it as compensated, and the Workflow evidence guards refuse it. The claim is released at step 17.1 like any terminal. Every committed use alerts. Workflow must keep the orphan-subscription manual task open. PRD §6.1, §6.3, §6.6, §9.1 and AC 8h are amended: still no *automatic* terminal. | Products force-completion of a stuck freeze under the two-person rule, recording missing participants as not-frozen (`gears/bss/products/docs/PRD.md` `cpt-cf-bss-products-fr-freeze-recovery`, AC 22) and break-glass writes gated by two persons and a distinct alert (`…-fr-breakglass-action-scope`); Ledger's dual-controlled `unknown_final` refund disposition and `DUAL_CONTROL_REQUIRED` refusal (`gears/bss/ledger/docs/design/05-adjustments-notes-refunds.md` `inst-rf-dual-control`, `inst-rf-unknown-final`); this design's repair-as-a-row rule (`cpt-cf-bss-orders-constraint-single-writer`; 06 §4.1) | D-109, D-134, D-136, D-148, D-165; ADR-0004, ADR-0007; DESIGN §4.4, §4.7; Q-26; `UPSTREAM_REQS` §2.6 |
| D-183 | Database object names follow the platform database ADR-0001, object namespacing (design review 2026-10-04, finding #5, MAJOR): the gear declares the stable alias `db_namespace = "bss_orders"`; every `orders_*` table name in the design set is a **logical** name whose physical name is `bss_orders__<local>`, `<local>` being the logical name minus its leading `orders_` (`orders_order` → `bss_orders__order`); an explicitly named index or constraint is `idx_\|uq_\|fk_\|ck_<physical table>__<purpose>`. The alias is used because `bss_orders__audit_checkpoint_member` (45 bytes) leaves 13 bytes for a `uq_` purpose under 63 and bare `orders` is ambiguous across the Orders gears; the longest physical table is 35 bytes. Prose, headings and anchors keep the logical names; the §3.7 inventory carries the physical names. The ADR's gear-macro attribute and Dylint prefix check do not exist yet, so review enforces the rule until they land; no BSS gear complies yet. This settles the review disagreement — round 1 flagged the single-underscore names (LOW), round 2 passed them as logical: they are logical, and the physical names are now declared. | `docs/arch/database/ADR/0001-cpt-cf-database-adr-object-namespacing.md` (`<db_namespace>__<table>`, `usage_tsdb` alias); `gears/system/policy-engine/docs/DESIGN.md` (`cpt-cf-policy-engine-constraint-db-namespace`, §3.7 declaration, `uq_policy_engine__decision_record__evaluation`) | DESIGN §3.7 inventory; Foundation constraints (`…-constraint-db-namespace`) |
| D-184 | Worker discovery scopes are structurally unable to write (design review finding #6, MEDIUM): the five maintenance workers hold the broad scope only as an Orders-local `DiscoveryScope` (private field, constructor visible only in the discovery module) that builds scoped selects, never yields `&AccessScope`, and runs inside a `TxConfig::read_only()` transaction; every write, the private worker engine entry and the cleanup/purge/checkpoint repositories take only a `TargetScope` built from a discovered row's persisted ID and stored seller/payer/resource-tenant properties (per-table constructors for registry, retention and audit-namespace rows); candidate locking (`SKIP LOCKED`) and rechecks move to the target phase. `AccessScope::allow_all` is a crate-local clippy `disallowed-methods` entry with `#[allow]` only on the `DiscoveryScope` constructor (the crate file carries the whole workspace `clippy.toml`; CI grep fallback). "Public callers cannot reach the internal path" and "discovery scopes never reach writes" are blocking CI gates with `trybuild` compile-fail cases. Not closed by the types: `scope_unchecked` inserts ignore scope (worker inserts use `scope_with_model` plus grants) and the worker connection can still write; role separation stays. No toolkit change. | Pricing `infra/jobs.rs:42-47`, `jobs/window_activation.rs:413` vs `:453` (convention hardened); Ledger `approval_repo.rs:310-331` `expire_due_all` writes under `allow_all` (drift evidence); `TxConfig::read_only()` `tx_config.rs:134` + `SecureConn::transaction_with_config` `secure_conn.rs:650`; workspace `clippy.toml:16-27`, `libs/.clippy.toml` (nearest file wins); `trybuild` in `libs/toolkit-db-macros/tests/ui.rs:16-22` | 08 §3.5 *Trusted internal maintenance* and verification plan; 01 §3.6 *Internal worker entry*, 01 §4.2 *Idempotency-window cleanup executor*; 07 §3.6 `inst-es-request-expiry` and draft worker; 08 feature §3.2, §6; D-115 unchanged |
| D-185 | Unbounded audit growth (design review finding #7, MEDIUM): (1) the `retention-purge` worker becomes the named Foundation component `cpt-cf-bss-orders-component-retention-purge` and a **phase 0/1 deliverable** with algorithm `…-algo-foundation-retention-purge` (daily, bounded batches per store: Preview outcomes 7 d, refused audit rows 90 d, read-log rows 90 d, through the existing grants and partial/time indexes) and DoD `…-dod-foundation-retention-purge`; refusal auditing is not enabled without it. (2) The audit store is sized by request and refusal volume, not transition rate: §4.1 adds 250/s engine-entering requests and 200/s refusal writes (4 : 1), retained refused rows ≈ mean refusal rate × 90 d, and ~2.33 × 10⁷ rows per caller per gateway replica (limiter × retention). (3) §3.8 alerts on refusal-audit write rate (absolute and ratio), purge backlog / oldest overdue refused row > 1 day, and no successful purge pass > 26 h; the §4.2 threat row names the monitor. (4) Q-26 split: the **per-caller** limit (200/60 s) adopts the platform api-gateway limiter — identity-keyed zone `rl_orders_caller_write` (`3/s`, burst 20) bound via `ThrottlingSpec { require_security_context: true }` on the caller-facing engine-entering operations, workflow-only operations on their own zone `rl_orders_workflow_write` (50/s) — and closes without an Architecture decision; gateway state is per replica (N ×) until throttling ADR-0001 lands. The **per-(caller, order)** limit (20/min) stays open as narrowed Q-26 with `…-upreq-gateway-path-param-throttle-key` and a gear-local REST-edge limiter as fallback. The limiter is a pre-engine request limiter: it also counts legitimate retries. | api-gateway `gears/system/api-gateway/src/middleware/throttling.rs` (governor keyed buckets, 429, `Retry-After`), `ThrottlingSpec` in `libs/toolkit/src/api/operation_builder.rs`, zone shape in `config/quickstart.yaml` and its `mini-chat` `create_chat` binding; `KeyType { Identity, Ip }` and deferred variants in `docs/arch/throttling/DESIGN.md` D1/D2, per-replica state in its ADR-0001 (proposed); the idempotency-cleanup executor's metrics and overdue-backlog alert (`features/01-foundation.md` §4.2) | D-49, D-05, ADR-0005 Consequences, Q-26 |
| D-178 | Event contract position against Seam Atlas C00/C09: eleven events (not three) on the platform `EventV1` envelope with GTS-registered type ids; aggregate version, correlation and causation are `data` fields; a dead-lettered message leaves a partition gap that the consumer's inbox tolerates and recovers through the authorized reads, so no gap-free stream sequence is promised. *(Amended by D-186: "the consumer's inbox" reads "a consumer-owned processed-event store per the event consumer contract" (Foundation §4.4); the platform supplies no inbox.)* | event-broker SDK envelope; ADR-0006; Workflow branch consumes nine events (W/design/10:274-276) | D-95, D-158; Foundation events |
| D-186 | One normative event consumer contract (design review, finding #8, MEDIUM; Foundation §4.4 `cpt-cf-bss-orders-constraint-event-consumer-contract`) binds Workflow, Subscriptions and Billing: C1 de-duplicate by event ID in a consumer-owned processed-event store, marking complete only when the effect commits; C2 reconcile `orderVersion`/state through `get_version` and the current-order read before any business effect, under a declared per-event applicability rule; C3 tolerate unknown `state`/event-type values (D-69); C4 never reconstruct order state or assume every prior event was observed; C5 an unavailable or denied read keeps work durably pending with bounded retry and escalation, never retired or read as empty. Q-25's §9.2 half is closed: a read is always required before a business effect; the event is a trigger and version reference; only effect-free consumers (audit, notification, analytics) act on payload alone. Conformance is an `orders-events` golden corpus family for `gears/bss/fixtures`, specified in Foundation §4.4 and built with the first consumer integration; passing it is each consumer's integration sign-off gate (`…-upreq-event-consumer-conformance`). Dead-letter alerting and SDK republication stay `p1` release prerequisites. Q-25's §6.5 `OrderAmended` trigger wording moves to Q-41. | Event Broker consumer contract (`gears/system/event-broker/docs/features/0002-consumer-subscription-lifecycle.md` §2.3: dedup by `event.id` on the consumer side; no platform inbox — `event-broker-sdk` outbox and `libs/toolkit-db/src/outbox` are producer-side); joint golden fixtures `gears/bss/fixtures` (README; `bss-fixtures-conformance`) and Pricing's fixture-gated sign-off (`gears/bss/pricing/docs/design/06-consumer-contracts.md` K5); named consumer-contract docs `gears/bss/subscriptions/docs/design/09-consumer-contracts.md`, `gears/bss/rating/docs/design/11-consumer-contracts.md`; PRD §9.2 PB-2026-09-29 | D-67, D-87, D-178, Q-25; Foundation §4.4; UPSTREAM_REQS §2.7 |
| D-187 | **R01, selected Option A (2026-10-05): Orders-owned typed read adapter.** Main `a35dfc3e21b57a0ab453e01898a0e42fa6d569ec` implements and registers `PricingReadV1`. Orders consumes it through its own catalog port/adapter, not Pricing internals or REST DTOs. Narrowly supersedes D-159/D-161's missing-SDK/REST-shaped read assumptions and D-160's no-tenant-argument wording: queries carry the seller `CatalogRef`, checked against PDP constraints, alongside the configured seller-scoped subject. Exact, derived and missing evidence are enumerated in DESIGN §4.1; revision availability/roster and legacy pin provenance remain explicit gaps, never fabricated or silently relaxed. `current_revision` uses the provider clock and can catch up revision state with audit/events despite `SafeRead` classification. Adapter conformance and grants remain open. Rationale: isolate Orders assessment semantics while reusing the implemented producer contract; direct SDK coupling and an upstream metadata extension were considered, but no Pricing API change is selected. No receipt/Preview/hold/deadline/descriptor or activation decision is implied, and §4.3's required legacy evidence remains pending reconciliation. | [Typed contract](../../pricing/pricing-sdk/src/read.rs); [provider](../../pricing/pricing/src/api/pricing_read.rs); [registration](../../pricing/pricing/src/module.rs); [provider test source](../../pricing/pricing/tests/pricing_read_sdk.rs); [SDK boundary](../../../../docs/toolkit_unified_system/03_clienthub_and_plugins.md) | D-159/D-160/D-161 read assumptions only; [DESIGN §4.1 mapping](DESIGN.md#contract-03-pricing-read-mapping); `…-upreq-pricing-read-sdk` remains unchecked; D-162 and the other commercial reconciliation decisions unchanged |
| D-188 | **R02, selected Option A (2026-10-05): permanently reserve/burn commercial version candidates.** Engine-owned durable immutable attempts and aggregate allocation high-water precede Pricing commands; exact retries reuse candidate and per-line keys, terminal failures burn it. Only successful final commercial transactions append history/move current_version; explicit supersedes_version replaces arithmetic predecessors. Submit may commit above 2. Operational ownership is a narrow exception to single-transaction idempotency claims; fencing and marker-generation linkage survive crashes/key reuse. No Pricing SDK change. Alternatives considered: separate upstream acceptance-attempt identity; prepare/confirm/cancel. R03 subsequently resolved by D-189; R04 subsequently resolved by D-190. | [Normative schema, subflow and tests](DESIGN.md#contract-01-commercial-attempt); Pricing check business uniqueness and replay | Supersedes D-19/D-64 fixed-version wording and contiguous commercial-version examples, not contiguous audit sequences; documentation only, implementation pending |


| D-189 | **R03, selected Option A (2026-10-05): separate nonbinding Pricing assessment sharing admission rules.** Proposed `PricingAssessmentV1::assess` is a missing typed SafeRead, with basket-local correlation rather than acceptance order/version/line identities. Pricing owns shared evaluation; all independent predicate results, unavailable observations, proposed bindings and observation time flow into one Orders composition for Preview/submit/amendment. Preview never calls check/hold or creates an attempt; acceptance revalidates authoritatively through D-188. Buyer-axis authorization and seller-scoped least-privilege grants remain required. Diagnostic-only persistence, TCV withholding and allowed catalog catch-up are preserved. Alternatives considered: expanded read projection plus SDK evaluator; explicitly limited Preview. | [Normative contract and conformance](DESIGN.md#contract-03-nonbinding-assessment); [Graph Storage shared dry-run evaluator](../../../graph-storage/graph-storage/src/domain/service.rs); [Stripe nonbinding preview](https://docs.stripe.com/api/invoices/create_preview) | Narrowly supersedes D-161 withdrawn-assess/residual-only assumption and D-177 command-existence trigger; D-187 metadata gaps remain; R04 subsequently resolved by D-190, with policy/deadline/activation prerequisites still open; documentation only |

| D-190 | **R04, selected Option A (2026-10-05): honor committed accepted prices through the implemented Pricing hold protocol.** Subscriptions alone holds the selected committed receipt at its durable chosen activation instant, then checks fresh fulfillment eligibility and fences the committed order/version/attempt before activation intent. Successors do not replace the initial accepted price. Exact hold replay is historical, never admission permission; `FulfilmentQuery` has no caller deadline. Preview/submit/amendment do not hold. Lifecycle `on_hold` is distinct. Alternatives: keep the stricter catalog-equality veto; freeze a per-seller choice of both behaviors. | [Contract and pending tests](DESIGN.md#contract-03-accepted-price-activation); [existing Pricing SDK](../../pricing/pricing-sdk/src/acceptance.rs); [hold conformance source](../../pricing/pricing/tests/fulfilment_holds.rs); [industry quote price locking](https://docs.commercetools.com/learning-model-b2b-commerce/implement-b2b-purchase-flows/quotes-price-locking-and-renegotiation) | Narrowly supersedes D-162 initial comparison and closes Q-32. D-191 subsequently resolves deadline/policy authority; its implementation, R06 frozen-binding mapping and R12 concrete admission/recovery remain prerequisites; remote check is not atomic with activation. Documentation only; no runtime delivery asserted. |

| D-191 | **R05, selected Option A (2026-10-05): Pricing owns the issued commercial deadline.** Orders copies each selected receipt's finite `hold_until` verbatim to `activation_deadline`; no local interval/catalog-end formula. Pricing enforces exclusive server-time bounds; fresh `valid_before` can narrow eligibility without rewriting evidence. Preview has no issued deadline. Seller-specific policy discovery/resolution is proposed missing Pricing work: actual config is deployment-wide v1/86400 by default. Exact retries freeze policy/input and preserve deadlines; new executions can refetch after refusal. Alternatives: independent earlier Orders business deadline; caller-requested deadline capped by Pricing at new acceptance. | [Normative contract and pending conformance](DESIGN.md#contract-03-commercial-deadline); [existing policy config](../../pricing/pricing/src/config.rs); [existing policy-change test](../../pricing/pricing/tests/acceptance_transaction/mod.rs) | Narrowly supersedes D-162 deadline ownership/formula; no uniform-policy downgrade or Settings provider chosen. D-192 subsequently resolves frozen snapshot shape; its implementation and R12 remain separate. Documentation only; implementation/grants/conformance pending. |

| D-192 | **R06, selected Option A (2026-10-05): adopt Pricing's complete frozen receipt as the commercial pin.** Orders schema 2 preserves the complete issued query, BillingTerms, selected AcceptedBindings, original digests and deadline. Subscriptions materializes identical accepted inputs after matching hold and fresh eligibility; HeldBindings alone is incomplete. Unsupported legacy provenance/full unselected matrix requirements are explicitly retired from the commercial envelope, not fabricated; real invoice, meter, coverage and terms obligations remain fail-closed. Current UI metadata is separate. Alternatives: extend Pricing receipts with legacy evidence; store receipt references and retrieve immutable facts rather than materializing the complete receipt in Orders. | [Schema, field reconciliation and pending tests](DESIGN.md#contract-03-frozen-commercial-snapshot); [SDK bindings](../../pricing/pricing-sdk/src/read.rs); [hold tests](../../pricing/pricing/tests/fulfilment_holds.rs); [industry line-item snapshots](https://docs.commercetools.com/api/carts-orders-overview) | Narrowly supersedes D-152/D-159/D-162 legacy pin shape and activation descriptor refresh, and resolves D-187's persisted-provenance question. D-190/D-191 admission/expiry still apply; Rating owns its broader composite snapshot. Downstream mappings, required additional facts and R12 remain implementation prerequisites. Documentation only. |

| D-193 | **R07, selected Option A (2026-10-05): Subscriptions owns a pre-subscription SafeRead billing-terms resolver.** Authorized explicit intent/versioned seller policy becomes complete BillingTerms with canonical SDK digest and exact invoice-period Term; Orders freezes the result into D-188/D-192 and Pricing validates without defaults. No subscription/acceptance/hold creation; optional incomplete Preview remains diagnostic-only. Alternatives: Subscriptions-owned local resolver library; explicit-terms-only initial scope without policy fallback. | [Contract, exact term mapping and pending tests](DESIGN.md#contract-03-billing-terms-resolution); [Pricing terms](../../pricing/pricing-sdk/src/terms.rs); [industry subscription anchors](https://docs.stripe.com/billing/subscriptions/billing-cycle) | Resolves R07 producer ownership; narrowly qualifies authored interval/term mapping. SDK/provider, seller-policy source/grants, supported-profile and joint period conformance remain unchecked upstream prerequisites. No Settings backend chosen; no runtime delivery asserted. |


| D-194 | **R08, selected Option A (2026-10-05): authenticated platform service principals with explicit seller-scoped PDP grants.** Orders and Subscriptions have distinct ordinary service identities; Orders owns acceptance create/read, Subscriptions owns hold/read. Proposed assessment, policy and terms reads need their own agreed grants. Buyer/resource/proposed-payer authority remains independent and rechecked; durable attempts preserve authenticated caller identity across retries/rotation. Alternatives: extend custom system-subject provider rules; delegated caller/token-exchange path. | [Normative call/grant matrix and pending evidence](DESIGN.md#contract-08-commercial-service-authorization); [platform S2S](../../../../docs/arch/authorization/DESIGN.md); [OAuth client credentials](https://www.rfc-editor.org/rfc/rfc6749#section-4.4) | Supersedes D-160 custom subject/context construction and corresponding D-171/D-177 assumptions. Existing Products ordinary PDP path is used; privileged Pricing branch is unchanged. Provisioning, policies, proposed API grants and real-provider conformance remain unchecked. Documentation only. |


| D-195 | **R09, selected Option A (2026-10-05): Pricing-owned versioned predicate results and explicit Orders mapping/coverage registry.** D-189's full assessment retains every observed subresult; Orders verifies independent expected coverage, preserves producer/profile/mapping identity and uses a blocking-only primary-error set. Optional-context TCV unevaluability preserves successful Preview. First-error acceptance is never expanded into a fictitious full vector. Alternatives: shared cross-BSS diagnostic vocabulary; provider-native public report sections. | [Normative mapping, schema and pending conformance](DESIGN.md#contract-03-diagnostic-mapping); [existing first-error validator](../../pricing/pricing/src/domain/commercial_terms.rs); [RFC 9457 structured validation](https://www.rfc-editor.org/rfc/rfc9457.html#section-3) | Refines D-187/D-189 adapter ownership and diagnostic completeness, narrows unavailable-first to blocking outcomes, and replaces D-159's ambiguous diagnostic default sentinel with tagged native selection. Profiles, provider results, registry, schema/response work and tests remain unchecked. Documentation only. |
| D-196 | **R10, selected Option A (2026-10-05): Pricing coordinates revision-reference release using fenced Orders and Subscriptions usage.** Keep Pricing as sole Products reference owner. Durably close admission and drain old work before authoritative zero-holder proof; include D-188 attempts that could commit and gap-free Subscriptions handoff. Unknown usage retains protection. Current revision release is deferred, not an unsafe running cleanup path. Alternatives: Subscriptions aggregates usage; retain references indefinitely as the initial product scope. | [Normative release boundary and pending conformance](DESIGN.md#contract-03-revision-reference-protection); [Pricing deferred revision release](../../pricing/docs/DECISIONS.md); [Products retirement fence](../../products/products/src/domain/approvals/retire.rs); [Kubernetes finalizers](https://kubernetes.io/docs/concepts/overview/working-with-objects/finalizers/) | Refines D-164's residual count/report ask and selects Pricing coordination. New cross-owner protocol, SDKs, grants and implementation remain open; R12 activation/cardinality remains separate. Documentation only. |

| D-197 | **R11, selected Option A (2026-10-05): Rating-owned exact-binding pre-purchase evaluation.** Rating computes all monetary outputs and aggregation, returning exact amounts plus major-unit display estimates (D-216); Orders freezes and stores them verbatim. Distinguish uncommitted usage from zero, use one TCV carrier with finite/rolling/mixed basis, and enforce exact acceptance-input equality. Alternatives: shared evaluator library; restricted initial purchase subset. | [Normative contract and pending conformance](DESIGN.md#contract-03-rating-purchase-evaluation); [Rating target requirement](../../rating/docs/PRD.md); [Stripe preview](https://docs.stripe.com/api/invoices/create_preview); [TCV definition](https://www.chargebee.com/resources/glossaries/total-contract-value/) | Refines D-40/D-167/D-192/D-193 and narrowly replaces Rating's caller-summation/catalog-prefix purchase seam. Provider/SDK/grants and real integration remain missing. Preview withholding and separate tax ownership stay unchanged. Documentation only. |
| D-198 | **R12, selected Option A (2026-10-05): receiver-owned durable capacity slots and fulfillment-attempt fencing.** Reserve capacity at activation-intent admission through OSS confirmation/settled compensation; serialize pause/revoke with admission and active confirmation. Choose resource tenant in canonical scope, peer adoption pending. Stage post-spawn control internally before effective hold/ordinary terminal, preserving public states/still-processing and five workers. D-182 forced unknown exit explicitly retains uncertainty/claims. Alternatives: per-key count lock; serializable predicate admission, both still requiring receiver fence. | [Normative protocol and conformance](DESIGN.md#contract-06-activation-admission); Pricing SDK check_fulfilment; Subscriptions async intent contract; PostgreSQL row-locking/saga sources in protocol | Extends D-180, selects Q-40 target without claiming SUB-G1/SUB-O5 delivery; narrows immediate post-spawn hold semantics and ordinary single-transaction claim rule. Documentation only; peer SDK/runtime/grants remain unchecked. |

| D-199 | **R13, selected Option A (2026-10-05): owner-specific commercial contracts with evidence-based production gates.** Account Management/PDP, Contracts, Payments and Workflow retain their separate authorities; missing required facts fail closed. Pricing acceptance, customer consent and approval are distinct. Workflow owns version-bound policy/authorization orchestration and durable payment request recovery; TCV is not authorization amount. Alternatives: aggregating BSS facade; explicitly reduced initial scope. | [Normative owner/readiness contract](DESIGN.md#contract-05-commercial-owner-readiness); [existing approval-source port](../../approvals/approvals-sdk/src/lib.rs); [durable payment intent precedent](https://docs.stripe.com/payments/payment-intents); [delegation context precedent](https://www.rfc-editor.org/rfc/rfc8693.html) | Refines upstream delivery/readiness while preserving D-175 adapter-owned verdicts and D-194 service identities. Does not prescribe token exchange, supply missing providers/grants, alter payment policy or claim real-provider conformance. Documentation only. |
| D-200 | **R14, selected Option A (2026-10-05): integrate the existing EventBrokerApi/DbProducer/toolkit outbox.** Correct obsolete missing-runtime and initial-cursor-retry defect claims; preserve actual schema, deployment, grants, regression, producer recovery, monitoring and consumer conformance gates. Enqueue atomically and wake after commit; broker acceptance is not downstream completion. Alternatives: platform external transport adapter; defer event-producing production release while developing against doubles. | [Integration contract and capability matrix](DESIGN.md#contract-01-event-platform-integration); [existing Pricing integration](../../pricing/pricing/src/infra/events.rs); [AWS transactional outbox guidance](https://docs.aws.amazon.com/en_en/prescriptive-guidance/latest/cloud-design-patterns/transactional-outbox.html) | Refines platform-readiness evidence, preserves D-186 and stable upstream requirement IDs. Production wiring/recovery work remains open; documentation only. |


Round-2 review (2026-09-30):
D-160, D-161, D-162 and D-164 were amended as their rows now say; the pin cap of D-159 is 1,000
**bound** slots per **line** (R2-10); the Subscriptions principal's `get_version` read at activation
is granted through the same finite order-ID set as Workflow's (R2-8, 08 §4.3).

Commercial questions still **unagreed**, not silently defaulted: concrete seller policy values/provider delivery
behind Pricing-issued `activation_deadline` (authority resolved by D-191), the partner/customer dimension of the subscription-side overlap key
(Q-05; the in-flight claim side is closed by D-179),
forced-retirement treatment, the `ends_on` cut owner between Subscriptions and Rating, and the
approval-policy host inside Workflow. The implementation must fail closed at the affected seam
until the owning contract is supplied. A concrete draft is available in the reciprocal amendments;
no team has been contacted or represented as accepting it.

Atlas overlay alignment (2026-10-02):
D-169–D-178 align the design with the written-down overlay of Seam Atlas v2 rev 8, the Pricing and
Products fork tip `7d3544156` (685 commits past the atlas's `01f670fa4`) and the Workflow branch
`bss/orders-workflow` @ `3ccf7793c`. Where the two Orders gears agree with each other and not with
the atlas, the atlas carries the correction (C02, C06, C09, D05, P9); where a provider has not written
its half, the ask in UPSTREAM_REQS carries the field shape the provider can transcribe.

## High-register reconciliation (2026-09-23)

The 2026-09-23 High-finding disposition maps all 33
High entries in the external review register to the current design, as recorded below. It supersedes conflicting
older narrative on these specific points; it does not assert runtime implementation or upstream
owner agreement.

- **Draft concurrency (OL-4):** commercial version is not a mutable edit token. Foundation §3.6
  adds `draft_revision`, a coherent pre-resolution snapshot and post-lock comparison, following
  Pricing's mutable-row-version distinction using existing scoped transactions. Replay precedes
  new-execution revision checks, including the unavailable-input path.
- **Commercial gate (OL-27/45–47/51/52/62):** Preview authorizes buyer resource and payer use
  without demanding ownership of the seller. *(Payer use superseded by D-215: the payer
  relationship is a business guard, not a PDP condition.)* Slices contribute guards/results, never return a
  business refusal before the engine. Amendments provide the complete proposed claim set.
  Pricing's actual tri-state predicates are normalized with retained diagnostics. Frontier and
  composition are explicit operations; typed SDK readiness remains open. Date-policy snapshots
  precede version creation, and day rollover cannot silently alter inputs to a completed gate.
- **Acceptance and Workflow (OL-26/37/57/58/61):** acceptance is version-bound, Lifecycle alone
  evaluates tolerate-failure, and completion checks stored current-version line membership.
  Workflow owns durable pending continuation and must obtain the pre-dispatch spawn-signal
  commit. Payments/Workflow contracts and the conflicting cancellation wording still require
  upstream implementation/reconciliation, recorded in `UPSTREAM_REQS.md` §§2.5–2.6.
- **Authorization (OL-38/64/65):** retain the agreed three-property PDP model without a
  designated owner tenant, separate from broker-root tenancy. Seller-only grants do not confer
  commercial edit or Preview rights. Workflow reads require explicit finite order-ID scopes,
  not an unpersisted correlation relationship; deployment grant provisioning remains open.
- **Expiry (OL-10/68–70):** use generation- and policy-bound stable requests, configured internal
  service identity, explicit due/exemption/staleness guards, pre-LIMIT exemption filtering and
  bounded keyset traversal. Permanent platform rows in the existing TTL policy table serialize
  override changes with expiry through standard row locks. Session advisory coordination is
  not treated as fencing. Product-owned TTL values remain Q-06/Q-07/Q-27.
- **Platform-first corrections:** partial overlap reservations are released by exact inserted
  IDs using existing scoped update/returning support, not a nonexistent savepoint API. Broker
  sequencing, retry and vacuum remain platform-owned; no local drain or re-drive is restored.
  GTS and canonical errors follow the supplied SDK, not divergent guideline examples.

**Verification ownership:** Orders implements the specified transaction, mapping, race and scope
tests. Platform owners supply missing SDK operations, broker recovery/observability and deployed
authorization evidence. Product/Workflow owners reconcile cross-gear semantics. This separation
is deliberate: recording a prerequisite does not make the dependency available.

## Medium-register reconciliation (2026-09-23)

The 2026-09-23 Medium disposition records the
remaining review corrections below. These are design changes and required tests, not runtime delivery.

- **Schema/writers:** category is the same GTS text in aggregate and version; amendment explanation
  has its own bounded column. Engine alone assigns version links/pointers. Category admission is
  shared by all commercial writes. Acceptance path/provenance and reflection correlation have
  explicit writers. Line identities are permanent reservations; membership determines visibility.
- **Gate diagnostics:** one durably recorded assessment ID groups the complete outcome vector and
  trusted principal/tenant scope. Preview returns it and retains seven days; operational access
  must be scoped. Foundation defines refusal diagnostic writes and stores the assessment/response
  snapshot on the settled idempotency record; engine-only refusals carry no assessment. No public
  diagnostics API or independent assessment service is introduced.
  Subscriptions must supply effective cardinality, not merely presence. Missing input fails closed.
- **Workflow:** begin-fulfillment commits before plan/recheck work; Workflow reuses owning SDKs
  for rechecks, voids drafts on false predicates, and acknowledges failure from in_fulfillment.
  Its already-specified progress read owns live execution progress; Lifecycle stores acknowledgements.
- **Reads:** one shared PDP/query/logging wrapper covers every read surface and all collection
  bounds. Delegated and direct cross-tenant reads log under the §4.4 decision table, including
  mixed/empty scoped pages; direct grants require no fabricated delegation proof. Invalid credentials are distinguishable internally even when outward detail is hidden.
- **Expiry:** the full graph yields 74 TTL-covered dwell entries, not 26. Configured TTL budgets
  are not hard wall-clock bounds without bounded scheduling delay. Draft discovery uses the shared
  five-minute/500-row worker contract. Missing-TTL gauges are replaced from current configuration.
- **Requirement qualification:** cross-principal duplicate creation qualifies the idempotency
  NFR, not AC-4. Its Product/Architecture ratification remains open alongside Q-28's cross-seller
  restriction and the previously registered PRD divergences; neither is silently declared compliant.

The live PR description was read and remains stale. A replacement is prepared locally; publication
is separate from document remediation. No external PR body or reviewer register was changed.

## Open questions

Not decided here. Each carries a named owner and the design position taken in the meantime.

| ID | Question | Owner | Interim position |
|----|----------|-------|------------------|
| Q-01 | PRD §15: should buyer-decided trial conversion and re-negotiated renewal be reclassified as commercially initiated, and if so is it a third `category` or a widening of `change`? | Product with Architecture | `category` remains `new_sale` \| `change` and the enum is documented as **open to a third value**, so the schema no longer forecloses it |
| Q-02 | **Closed — answered no by design decision D-84.** Several order lines may not compose into one subscription; the 1:1 mapping holds and is now enforced by a partial UNIQUE on `(order_id, subscription_id)` and a distinctness guard on acknowledgement. The premise had already moved — a subscription is *already* multi-product via `PlanLink` and `AddOn` — so the question was about the order-side mapping, and both shapes that would want composition (add-on selection, a line targeting an existing subscription) are already out of scope for this phase | Design | Closed; no external decision required |
| Q-03 | PRD §15: does the order line gain commercial soft and hard bounds and an overuse price reference, and what are `qty` semantics per charge kind? | Product with Architecture and Rating | No commercial cap on the line; the PRD's own note that routing it to the quota subsystem places a negotiated term outside the order is now reproduced rather than suppressed |
| Q-04 | **Documentation ambiguity resolved for implementation (S1-01).** Existing numbered seams retain their source-qualified meaning; semantic identity governs across registers. | Architecture | [Canonical semantic alias map](UPSTREAM_REQS.md#subscription-seam-alias-map) distinguishes both SUB-O6 meanings and historical branch-only aliases. Provider SDK design and delivery remain S5-01/05/06 work. |
| Q-05 | **Partly closed by D-179 (2026-10-05).** Route (a) is taken for the Orders in-flight claim: it is held per `(payer_tenant_id, resource_tenant_id, overlap_scope_key)`, so a partner's orders for different customer tenants no longer collide or run serially, and a refusal names only an order of the caller's own resource tenant. **Target selected by D-198/Q-40, producer delivery pending:** Subscriptions must make `resourceTenantId` a default dimension of `overlapScopeKey`, so that the occupancy read and the active commit also count per customer. Raised as a `SUB-O5` amendment (`UPSTREAM_REQS.md` `…-upreq-overlap-presence-read`); the key's shape is agreed on PR #4177, the venue `SUB-G1` names | Subscriptions, with Architecture | Predicate 7 applies the tuple Subscriptions answers, as `provenance` states it, and never re-buckets a per-payer count locally (D-83: no local fork). While the answer is per payer, a partner's second customer on the same key is refused at submit at cardinality one rather than passed into an activation refusal — whether the first customer's subscription is already `active` or its order is still in flight, since predicate 7 also counts this payer's other in-flight orders on the key (D-180). Route (b) and why it alone does not resolve this are recorded in D-83 |
| Q-06 | Per-state TTL defaults and whether seller scope may override platform scope | Product | **Provisional defaults seeded (D-181); Product refines values.** Still no code default: the migration seeds the platform rows as revisioned data — `submitted` 14 days, `pending_approval` 14 days, `approved` 30 days, `on_hold` 30 days, marked `provisional` — and Product's answer is a new revision promoted through the policy channel, which clears the marker. The channel refuses a NULL duration in production, so no production state is unswept. The override mechanism is specified and ships disabled behind the gear-level `ttl_seller_override_enabled` flag, default off (D-137), so the override-scope answer becomes configuration: a yes turns the flag on, a no leaves it off, and neither needs a design change |
| Q-07 | **PRD §15 row 5 carries two values and this row tracks both.** (a) The program retention period for completed and cancelled orders. (b) The **`draft` auto-void TTL**, which [07 §4.5](DESIGN.md#contract-07-4-5) routes here and which was the more urgent half: unset, the draft sweep does no work and basket accumulation is **unbounded**, with no fallback since D-90 withdrew the absolute-lifetime backstop. Unlike the §15 row-7 TTLs it bounds storage rather than a commercial promise | Product | (a) Append-only with no destructive path; retention deferred to the policy. (b) **Provisional default seeded (D-181); Product refines the value.** The `draft` platform row ships at **90 days**, `provisional`, matching the Subscriptions draft auto-void platform default (SUB-D-11 amendment); it is data rather than a code default, per [07 §2.2](DESIGN.md#contract-07-2-2), and Product's value is a new revision through the policy channel. The draft-age distribution ([02 §3.8](DESIGN.md#contract-02-3-8)) and the provisional-default alert ([07 §3.8](DESIGN.md#contract-07-3-8)) show while the value is still provisional |
| Q-08 | The minimum payment-outcome surface: a declined instrument's exit (currently expiry only) and refund-as-reversal after capture | Architecture with Product | Authorization-only, stated as a limitation in [05 §4.4](DESIGN.md#contract-05-4-4) |
| Q-09 | Where does the approval policy live, given that no approval service exists and `cf-gears-bss-approval` is a library Pricing and Products embed? | Architecture | Workflow's approval adapter is the owner and may embed the library (D-166); stand-in behind the expectations contract until it does; every verdict carries its deciding authority (D-73) |
| Q-10 | The durable-execution engine for the sibling Workflow gear | Architecture | Out of scope for this gear; nothing in the engine depends on it |
| Q-11 | **End-to-end submit latency needs boundary clarification and validation.** The PRD requires durable write plus event publish at p95 < 1 s (§7.1, §12), while [03 §2.2](DESIGN.md#contract-03-2-2) allows up to **2.25 s** for pre-transaction submit port resolution (2.5 s on Preview, which also calls the tax port; both raised by 250 ms for the catalog product-key operation, D-108). Those timeout budgets do not prove achieved p95 and do not exempt guard resolution from caller-visible latency | Product with Architecture | Measure request-to-commit and the complete request-to-broker path per `DESIGN.md §4.1`; attribute guard-resolution time as required by [03 §1.2](DESIGN.md#contract-03-1-2). Resolve the governing boundary jointly with Q-16. The design claims neither sub-second buyer response nor PRD compliance from commit-only measurements. |
| Q-12 | **Closed as an algorithm choice by D-174; stale PRD wording reconciled in S1-01.** An admitted amendment from submitted, pending_approval or approved commits the new version in submitted. Workflow obtains and reflects a fresh version-specific requirement verdict before fulfillment. | Product with Architecture | Two-step reapproval is selected: no direct approved → pending_approval amendment edge and no old verdict reuse. Provider delivery remains open in UPSTREAM_REQS; this correction does not claim deployment or broader Product acknowledgement. |
| Q-13 | **Closed — no Product decision required; the PRD's own usage settles it.** §12 requires rejecting a superseded version "with a machine-readable **stale-version** reason", and D-59 read that as a descriptor rather than a minted identifier, resolving it to the engine's `version-conflict`. That reading is not an interpretation among several: §12 uses the **identical construction** four more times — "a machine-readable **business-level** reason code", "a machine-readable **business** reason", "a machine-readable **business-level** reason indicating the mixed-currency basket", "a machine-readable **business-level** reason indicating fulfillment has already spawned a subscription" — and in none of those four is the adjective phrase a reason name. A reading that mints `stale-version` would have to mint `business-level` too. So the phrase names the *kind* of reason, `version-conflict` is a reason of that kind, and the `MUST` is met | Design | [01 §4.2](features/01-foundation.md#contract-01-4-2) holds the descriptor-to-identifier mapping so a later reader does not restore the descriptor as a name and reintroduce the duplication D-38 removed. Product is **notified, not asked**: if §12 is ever intended to mint identifiers it should say so for all five phrases, which would be a PRD change and not a design one |
| Q-14 | **Closed — no Product decision required.** PRD §6.1 gives the contract-effective date's default as **submit time**. The design now retains an explicitly authored date in draft and, where it remains absent, resolves it at submit from the commit instant before deriving dependent dates | Design | [02 §4.2](features/02-capture.md#contract-02-4-2) now implements the PRD rule; the resolved values are stored with the admitted version and no authoring-time/default divergence remains |
| Q-15 | ADR-0003's fail-closed posture means **no submit passes the gate** until `SUB-O5`, GA/prepaid and registry inputs, and the batched fixed-version Pricing SDK operations exist; bundle purchases additionally require frozen composition and component-conjunction integration, so operators will see submits refused for predicates not yet built upstream. That behaviour is designed, but it is product-visible and nobody outside this design has agreed it | Product with Architecture | Fail closed is specified in D-72 with ADR-0003; the phase map in `DECOMPOSITION.md` states the consequence |
| Q-16 | **Delivery target reopened.** Confirm the measurement boundary and feasibility of the PRD's p95 < 1 s durable-write-plus-publish baseline (§7.1, §12 AC-17), and decide whether a separate publication budget is justified. D-41's 30 s p95 was borrowed from Workflow and is unapproved for Lifecycle; asynchronous publication alone does not make sub-second delivery impossible | Product with Architecture | The PRD baseline remains governing. `DESIGN.md §4.1` separates request-to-commit, commit-to-broker acceptance and downstream processing, requires full-path measurement at expected load with backlog/retries, and keeps incomplete deliveries visible. Confirm population, observation window and tail criteria; approve and propagate any changed target only after reviewing the evidence. Delayed-delivery and dead-letter monitoring remain mandatory. |
| Q-17 | **Authority resolved by D-191; seller-policy discovery/implementation and product-selected window remain pending.** The accepted commercial deadline is the selected receipt's exact hold_until. | Architecture with Product | State dwell TTLs are operational expiry policy, not substitutes for receipt validity. Do not extend the receipt deadline on hold/resume or infer it from TTL. S3-05 delivers owner policy discovery and exact projection; S5-07 validates the held receipt at activation. |
| Q-18 | PRD §11's Order Console step 4 lists **hold** among a Partner Admin's actions, while §5.1 and §6.3 name only the seller operator and Orders Workflow as hold actors. This design follows the stricter reading and denies Partner Admin hold | Product | Recorded in [08 §4.3](DESIGN.md#contract-08-4-3); either §11's step list or the §5.1/§6.3 actor set needs correcting |
| Q-19 | **Closed — no Product decision required.** The design-introduced Orders outbox re-drive endpoint had no PRD basis | Design | Removed by D-17/D-58 when the design adopted the platform producer outbox. Operations use toolkit-db dead-letter facilities; no Orders business API remains to acknowledge |
| Q-20 | The **audit read** has no FR basis (D-70): PRD §6.1 and the audit NFR oblige the system to *record*, not to *expose*, and §9.1 contains no retrieval operation. It exposes actor identities, delegation-proof references and correlation identifiers | Product | Specified in the [audit interface contract](DESIGN.md#contract-08-3-3); acknowledge it, or scope what the surface may return |
| Q-21 | **Technical behavior selected by D-189/D-195; wording reconciled in S1-01.** Preview creates no order, receipt, hold or commercial attempt, but persists bounded diagnostic results. | Product | Diagnostic-only persistence has 7-day retention; no tax or preview total is stored on an order. The nonbinding assessment provider and diagnostic implementation remain S3-03/10/11 work. |
| Q-22 | Row 6, **`draft → expired`** on the auto-void TTL, is an edge PRD §6.1's normative state diagram does not contain; §7.1 says only that abandoned drafts *SHOULD* be auto-voided (D-14). `OrderExpired` consequently carries two commercially different facts | Product with Architecture | Implemented and disclosed in [01 §4.3](features/01-foundation.md#contract-01-4-3); amend the §6.1 diagram, or introduce a twelfth state and a twelfth event, which ADR-0004 rejected |
| Q-23 | **Ten endpoints the PRD describes in §6 but omits from §9.1** mean §9.1 is no longer the normative operation set (`DESIGN.md §3.3`). Each has an FR basis, so this is a wording gap rather than a scope extension | Product | The endpoints are specified and inventoried; §9.1 needs the amendment `DESIGN.md §3.3` already calls for |
| Q-24 | The **version reason vocabulary** is `{create, submit, amendment}` (D-82) against PRD §6.2's MUST-level eight-value list; the other six name state-only transitions and live on `orders_transition_audit.reason` | Product | The split is implemented and stated in [04 §4.5](features/04-versioning.md#contract-04-4-5); amend §6.2 so its enumeration matches the version/audit split PRD §1.4 already draws, or a conformance run against §6.2 fails on six values |
| Q-25 | **Closed by D-186 (2026-10-05); its §6.5 half is split to Q-41.** The bundled question was (a) whether freshness reads before consumer business effects may depart from §9.2's "without a callback read" wording (D-67), and (b) `OrderAmended`'s §6.5 trigger. (a) is answered: **a read is always required before a business effect**; the event is a trigger and a version reference (enough to route, de-duplicate, partition, filter and decide whether to fetch); only effect-free consumers (audit, notification, analytics) may act on the payload alone, still de-duplicating and never building state. This is D-158, D-178 and PRD §9.2's PB-2026-09-29 amendment, which already superseded the no-callback wording. Read traffic, service grants and unavailable-read recovery are carried by `…-upreq-event-consumer-conformance` and §2.9, not by this row. (b) is Q-41 | Design (closed) | The [event consumer contract](DESIGN.md#contract-01-event-consumer-contract) (Foundation §4.4) and its `orders-events` golden corpus; integration sign-off for Workflow, Subscriptions and Billing gates on the corpus (UPSTREAM_REQS §2.7) |
| Q-26 | *(Narrowed by D-185: the per-caller 200/min half is closed by adopting the platform api-gateway identity-keyed zone; the per-(caller, order) 20/min half stays open — wait for `cpt-cf-bss-orders-upreq-gateway-path-param-throttle-key` or build the gear-local REST-edge fallback — together with the new §4.1 request/refusal baselines, the Workflow zone value and the purge batch size.)* The **operational limits this design set as working baselines** need ratifying against real capacity: the per-(caller, order) request limit (20/min), the per-port bulkhead (32 in-flight), the breaker ratio (0.5 over 30 s, open 10 s), submit and Preview rate limits (10/min and 60/min per caller), the line cap (200), the toolkit producer queue count (16), high-throughput profile and 64 KiB envelope bound, and the operator-forced exit's 24-hour approval window (D-182). Each was previously named as a mechanism with no value, so five separate risk mitigations rested on thresholds nobody had set | Architecture | Values are set in [01 §3.7](DESIGN.md#contract-01-3-7), [02 §4.5](features/02-capture.md#contract-02-4-5), [03 §2.2](DESIGN.md#contract-03-2-2), `ADR/0006` and measured by the load tests those sections name; they are baselines to revise, not guesses to keep |
| Q-27 | **Closed by D-181 (2026-10-05): provisional defaults seeded; Product refines values.** Every expirable state now ships a finite provisional platform TTL as a revisioned row, and the policy channel refuses a NULL duration in production, so no production order is in a state without a TTL; the requirement no longer waits on Q-06/Q-07, which now refine values rather than supply them. The original question follows. **An order in a state with no configured TTL never expires.** PRD §6.3 requires bounded lifetime; PRD §15 row 7 leaves the TTL values open; and this design takes no code default, so the requirement is unmet for exactly the states Product has not yet valued — including `draft`, whose auto-void TTL is Q-07. D-90 records why the absolute-lifetime backstop that previously masked this was withdrawn. This is therefore a **requirement blocked on an unanswered question**, not a design gap | Product | Disclosed in [07 §4.2](features/07-hold-and-expiry.md#contract-07-4-2), `§4.4` and `§4.5`, alerted per [07 §3.8](DESIGN.md#contract-07-3-8), and bounded on the one axis this design can close — the resume cap of D-90 stops the dwell being restarted without limit. Answering §15 row 7 and Q-07 closes it; no mechanism changes when they are answered. **Closed (D-181)**: the provisional rows bound every state now, the caps bound total dwell at 74 × 30 days plus scheduler delay, and an answer from Product is a new policy revision, not a mechanism change |
| Q-28 | **D-62 refuses a cross-seller payer rebinding that PRD §6.1 requires be honoured.** The PRD says a payer change crossing seller scope "**MUST** follow the paired payer/seller rebinding semantics (ownership-transfer alignment, manifest §4.11)", and §12's acceptance criterion repeats it — "paired with seller rebinding where the change crosses seller scope". D-62 freezes `sellerTenantId` as commercial-frozen, which makes the paired half unexpressible, and refuses the cross-seller payer change with `payer-rebinding-requires-seller`. The divergence is deliberate and was taken to close a real hole (an unguarded amendment could rebind the selling party), but it narrows a PRD MUST and the register recorded it as a decision rather than routing it | Product + Architecture | The freeze and the refusal are implemented as D-62 states ([04 §2.2](DESIGN.md#contract-04-2-2), `§3.3`, `§3.6`); a cross-seller payer change is therefore **not supported** and a caller must cancel and re-place. Closing it needs one of three: amend §6.1 to match, specify an ownership-transfer transition that rebinds both axes together under its own guard and event, or accept the refusal as the answer. Nothing changes in this design until it is answered |
| Q-29 | **PRD §1.1 claims the order does "double duty as quote and order" with "validity/expiry [as] the per-state TTL", and no state in this design is a quote.** A commercial quote is a *priced, non-binding, time-bounded offer*. A `draft` carries no price ([02 §3.2](DESIGN.md#contract-02-3-2)); submit is where the price appears and on the self-service path submit **is** the commitment ([05 §4.2](features/05-preconditions.md#contract-05-4-2)); Preview prices a basket but persists only its per-predicate verdicts, so the figure it quoted is unrecoverable and bound for no period ([03 §4.6](features/03-gate-and-pin.md#contract-03-4-6)). A partner-led sale needing "valid for thirty days" must hold that price outside this SoR with its validity unenforced — the outcome §1.1 gives as the reason no separate quote artifact is needed | Product + Architecture | Disclosed in [03 §4.6](features/03-gate-and-pin.md#contract-03-4-6). This design **MUST NOT** close it locally by storing Preview's total and calling it an offer: an offer needs a validity rule, an expiry actor, a re-price rule and a binding-on-acceptance rule, none of which any document in this set carries. Closing it means amending §1.1 to stop claiming quote coverage, or specifying a priced offer artifact — a scope decision, not a design one |
| Q-30 | **The partner path cannot complete without a `resourceTenantId` principal who can reach an acceptance surface.** [05 §4.2](features/05-preconditions.md#contract-05-4-2) bars the placing and selling parties from recording acceptance — the only technical control against manufactured consent, and kept. But where the end customer has no platform credential at the point of sale, nobody may record it: begin-fulfillment refuses, the order rests in `approved` until the `approved` TTL expires it (provisional 30 days, D-181; before D-181 an unset TTL meant it never left). A partner-placed order can be commercially agreed offline and still be unfulfillable | Product + whoever owns partner onboarding | Disclosed in [05 §4.2](features/05-preconditions.md#contract-05-4-2). The platform **MUST** be able to present an acceptance action to a `resourceTenantId` principal for any order the partner path produces — a capability this gear does not own. No delegated or operator-attested route is offered, deliberately, since an attested route is the authority artifact D-31 found unspecified. The available mitigation is a seller-scope election of acceptance **not** required ([05 §4.1](features/05-preconditions.md#contract-05-4-1)), which the seller requests through platform operations and which takes effect by deployment promotion, not a runtime call (D-133) — a decision about evidence, to be made knowingly |
| Q-31 | **PRD §6.3 says "A held order **MUST** be resumable", and the design caps resume.** [07 §4.1](features/07-hold-and-expiry.md#contract-07-4-1) refuses row 22 with `resume-cap-exhausted` once `resume_count` reaches the design-owned cap (baseline 5; [07 §4.5](DESIGN.md#contract-07-4-5) admits no "unlimited" value). The cap closes the hold/resume loop that restarts the dwell (D-90), and D-109 kept it unconditional. So PRD §6.3's resumability is qualified by a design-owned count cap: a held order at the cap exits only by cancel, expiry or, for an `in_fulfillment`-origin hold, Workflow's rows 26 and 27 (fail or mediated cancel), and never by completion | Product with Design | Cap kept for every resume (D-90, D-109) and disclosed in [07 §4.1](features/07-hold-and-expiry.md#contract-07-4-1) and `§4.5`. Product either accepts the qualification or amends PRD §6.3; if the design instead exempted some resumes from the cap, it would reopen the uncapped loop D-90 removed |
| Q-32 | **Resolved by D-190:** an accepted order survives an ordinary `all` successor for initial activation, subject to fresh Pricing eligibility and receiver admission. Fixture F-B2 expects 10 after catalog successor 12. | Orders / Pricing / Subscriptions | Existing hold protocol adopted; no caller-supplied `hold_until`. D-191 deadline/policy, D-192 selected descriptor mapping and D-198 selected activation/cardinality contracts still require implementation and real-provider conformance. |
| Q-33 | **Where do add-ons and change orders live after Pricing D-467?** The PRD admits `change` for increases per the Change Orders PRD §17 and names plan-scoped add-on rules; Pricing removed optional and included items and their bounds; Workflow D-199 defers Change Orders; Subscriptions has designed no apply-on-live-subscription (Atlas ticket T9). | Product with Pricing and Subscriptions | An add-on is an ordinary line (D-170); `change` stays refused in this slice (D-176). Phase two needs a change-aware fence at intent acceptance, an overlap exemption for the target key, `Applied \| Held` outcomes and a Subscriptions apply-change with an expected revision |
| Q-40 | **Design choice resolved by D-198; producer adoption and conformance remain pending. Target includes resourceTenantId.** Historical question: **Should Subscriptions make `resourceTenantId` a default dimension of `overlapScopeKey`?** (split from Q-05 by D-179). The Orders in-flight claim is already held per `(payer_tenant_id, resource_tenant_id, overlap_scope_key)`, but Subscriptions' default key is `(payerTenantId, catalogSubscriptionProductKey)` (`subscriptions/docs/design/03-plan-changes.md` §4.4), so its occupancy read and its `active` commit still count per payer, and a partner may hold only one active subscription per product across all its customers. Adding the resource tenant is permitted by that same §4.4 ("extra dimensions"), uses a field `Subscription` already carries, and changes nothing on self-service sales, where payer and resource tenant are one tenant. Raised as the second `SUB-O5` amendment (`UPSTREAM_REQS.md` `…-upreq-overlap-presence-read`); the key's shape is agreed on PR #4177, the venue `SUB-G1` names | Subscriptions, with Architecture | Predicate 7 applies the tuple Subscriptions answers, as `provenance` states it, and never re-buckets a per-payer count locally (D-83: no local fork); it also counts this payer's other in-flight orders on the key (D-180), so a partner's second customer on the same key is refused at submit at cardinality one rather than passed into an activation refusal. Closing Q-40 lets predicate 7, the occupancy read and the active commit count the same tuple as the claim, and closes Q-05 |
| Q-41 | **`OrderAmended`'s PRD trigger no longer holds** (split from Q-25 by D-186). PRD §6.5 emits it "on creation of a new order version", and D-64 makes creation and submit version-appending rows that publish no `OrderAmended` — creation is event-less, submit publishes `OrderSubmitted`. It fires only on the three amendment rows (18, 19, 20) | Product | Specified and disclosed in [01 §4.4](DESIGN.md#contract-01-4-4) and [04 §3.3](DESIGN.md#contract-04-3-3); amend §6.5 to name amendment as the trigger, or a conformance run against §6.5 fails on two of the five version-appending rows |

## Traceability

- **PRD**: [`./PRD.md`](./PRD.md)
- **DESIGN**: [`./DESIGN.md`](./DESIGN.md) and [Decomposition](DECOMPOSITION.md)
- **ADRs**: [`./ADR/`](./ADR/) — `cpt-cf-bss-orders-adr-transition-through-engine`, `cpt-cf-bss-orders-adr-slice-decomposition`, `cpt-cf-bss-orders-adr-fail-closed-gate`, `cpt-cf-bss-orders-adr-closed-enumerations`, `cpt-cf-bss-orders-adr-refusals-commit`
- **Review waves**: the 2026-09-08 wave (`R-01`…`R-74`) and the 2026-09-09/10 waves (`Rc2-`, `Rc3-`, `F2-`, `F3-` ids); records held with the team rather than in this set

## Documentation review history

This record preserves the design review caveats recorded before the layout migration.
It is historical context, not evidence that runtime integration or implementation passed.

**No claim is made that the set has converged.** Wave 5 produced two CRITICALs, and a subsequent
sweep found seven further instances of one of them that wave 5 had not reached. Wave 6 reviewed
wave 5's *own fixes* and found five more CRITICALs, **all five introduced by those fixes** — an
Orders-owned outbox scheduling column that was indexed but never declared, monthly partitioning
incompatible with the ordering constraint it sat on (both since superseded by the platform
producer outbox), a savepoint that hid a phantom version rather than
preventing it, an absolute-lifetime backstop rendered inert by a shared idempotency key, and a
timed validity window no declared interface could carry. That is the honest shape of this set's
state: each wave has found real defects in the previous wave's remediation, and the rate is not
yet falling. Editors must review counts, citations, reason-name uniqueness, table/index
references and agreement with the transition table. Mechanical checks alone do not establish
semantic correctness or runtime integration.

**Structural review is not proof of correctness.** Step ordering, transaction scope,
cardinality coupling and whether each declared `MUST` has an implementable interface require
semantic review and runtime verification. A 2026-09-10 review found five defects of those kinds.

Phase 0/1 is [`01-foundation.md`](DESIGN.md#contract-01-1-1), the correctness core: the transition
contract, the idempotency semantics with their four exhaustive outcomes, the state machine as a
**twenty-seven-row** table with its normative exclusions, the audit and platform producer-outbox rules, and the
canonical schema for the Orders-owned Foundation tables inventoried in `DESIGN.md §3.7`.

Two slices carry a dependency on an unagreed upstream ask rather than a gap in their own design.
[`03-gate-and-pin.md`](DESIGN.md#contract-03-1-1) needs the occupancy read (`SUB-O5`, amended by D-126) for the
against-existing-subscriptions half of the overlap rule; until it lands that half is unevaluable
and therefore a refusal, which fails closed. [`06-workflow-seam.md`](DESIGN.md#contract-06-1-1)
depends on six: the compensation cancel reason (`SUB-O1`), the order reference on `create`
(`SUB-O2`), the occupancy read (`SUB-O5`, amended by D-126), correlation propagation (`SUB-O9`) and the
explicit subscription start instant (`SUB-O10`). Each is stated as a constraint naming its ask, so the
design is complete and the boundary is honest, and the **Workflow-side amendment verdict** (`…-upreq-workflow-amendment-verdict`). That last one is not upstream *code* but an upstream **document**: the sibling Workflow PRD restricts verdict acquisition to `OrderSubmitted`, so it needs amending before the two-step re-approval seam of [`04-versioning`](DESIGN.md#contract-04-1-1) §4.3 can be implemented at all (`p1`; see [`../DECISIONS.md`](DECISIONS.md) Q-12).

One thing remains outstanding for the gear, and it is not a slice. The **`SUB-O*` register has
forked** — the Subscriptions seam map defines `SUB-O1` through `SUB-O6` while the sibling
Workflow PRD cites `SUB-O5` through `SUB-O9`, with `SUB-O6` carrying different
meanings on the two sides — and reconciling it is a document diff rather than a dependency on
code. It is tracked as Q-04 in [`../DECISIONS.md`](DECISIONS.md), which treats the seam-map
numbering as canonical in the meantime.

Configuration values are split by owner rather than left uniformly blank. Values this design can
choose — sweep cadence, batch size, page sizes, port budgets — carry working baselines set in
[`07-hold-and-expiry.md`](DESIGN.md#contract-07-1-1) §4.5,
[`08-read-and-authz.md`](DESIGN.md#contract-08-1-1) §4.5 and
[`03-gate-and-pin.md`](DESIGN.md#contract-03-1-1) §2.2. Values that are commercial policy — the
per-state TTLs and the override scope — remain **PRD open questions owned by Product** and have
no code default, so that an unset value is visible as absent behaviour rather than silently
becoming the platform answer. The override scope's mechanism is specified but ships disabled
behind the default-off `ttl_seller_override_enabled` flag, so Product's answer becomes
configuration (`DECISIONS.md` D-137).

- **ADRs**: [`ADR/0002`](ADR/0002-cpt-cf-bss-orders-adr-slice-decomposition.md) the foundation-plus-seven-slices decomposition


## D-201 — Executable receiver contracts and forced-request integrity (S5-01)

**Status:** selected local normative correction, 2026-10-06; owner deployment conformance pending. *Amended 2026-10-18 by D-217: the actual service start is the receiver's `activate` commit instant, not the OSS applied-outcome time.*

D-198 gains the authenticated internal rebuild continuation with closed-predecessor settlement,
fixed commercial/active members, fresh generation and atomic grant/audit/response. It adds one
internal audit token, no public route/state/event/state-machine trigger. D-182 request freshness
uses an observed committed sequence/state/version object; frozen audit v1/v2 cannot cover that
new column, so new writers use explicit audit v3 while old encodings and genesis remain frozen.
Refusals remain outside the committed chain. Existing two-user/24-hour/overdue guards remain.
D-172 flat fulfillment replaces Workflow's stale Catalog DAG/bundle prose. Failure mappings
separate verified expiry from retirement, identity/digest corruption and infrastructure errors.
Pricing's immutable held activation identity remains distinct from asynchronous service start;
unknown or incompatible receiver/Rating geometry fails closed without rewriting accepted terms.

[Normative rebuild](DESIGN.md#contract-06-replace-fulfillment-grant),
[audit storage/encoding](DESIGN.md#contract-01-table-orders_transition_audit),
[clock boundary](DESIGN.md#contract-06-activation-clocks), and
implementation evidence/operation matrix.
S2-02 owns migrations, S2-06 the production audit encoder, S5-02 SDK/contribution registration,
S5-08 rebuild execution and S5-15 full forced-exit integration. This contract-only package
implements none of those runtime paths and is not peer production sign-off.

## D-202 — `X-Delegation-Proof-Ref` is the REST carrier of the delegation proof reference (S2-03)

**Status:** decided 2026-10-06 by the user ("do the recommended way"), confirming the S2-03
coordinator decision of the same day, which was interim until this confirmation. Amended by
D-221: the credential check also refuses well-known opaque token prefixes.

A REST caller presents the delegation proof reference in the optional request header
`X-Delegation-Proof-Ref`, on every operation, reads included. The value is opaque, 1–512
printable non-space ASCII characters, and credential-free (no compact JWS/JWE, PEM armour, URL
user-info or secret-named parameter). A malformed, credential-bearing or repeated header is
`request-invalid` before authorization, idempotency resolution or any read. A valid header maps to
the same `CallMeta.delegation_proof_ref` field an in-process SDK caller supplies; both reach one
PEP, which forwards it unvalidated as PolicyEnforcer request context (D-111). The value is never
logged, echoed or forwarded on any other outbound call; it persists only as supplied evidence in
the audit or access-log column (08 §4.4). Proof verification and the deny codes stay with PDP
policy under `cpt-cf-bss-orders-upreq-pdp-policy-integration` (UPSTREAM_REQS §2.9).

[REST carrier](DESIGN.md#contract-08-4-4), [SDK/REST metadata](DESIGN.md#33-api-contracts),
boundary fixtures B44–B52,
implementation evidence.

## D-203 — `replace-fulfillment-grant` receipts use the workflow retention class (S2-05)

**Status:** decided 2026-10-06 by the user ("do the recommended way").

The D-201 internal `replace_fulfillment_grant` IdempotentWrite is issued and retried by Workflow
under one key, exactly like the workflow-class triggers. Its registry receipts therefore get the
D-173 window of at least 30 days instead of the ordinary 24 hours, so a Workflow retry after day
one replays the committed grant rather than being refused or executed again. The **workflow
retention class** is the workflow-trigger class of Foundation §4.1 plus this one operation token.
It adds no trigger to the workflow-trigger class and does not change the continuation's closed
guard order; `force-fail-unreconciled` stays ordinary (D-182).

[Retention class](DESIGN.md#contract-01-4-1),
[idempotency window](DESIGN.md#contract-06-replace-fulfillment-grant),
implementation `RegistryOperation::retention`
(`orders/src/domain/idempotency.rs`), unit test
`retention_class_is_exactly_the_workflow_trigger_class` and PostgreSQL test
`claims_stamp_db_time_and_operation_windows_and_never_extend_them`.

## D-204 — Administrative free text is minimized with a keyed HMAC (amends the D-96 interim; S2-06)

**Status:** decided 2026-10-06 by the user ("do the recommended way"); Privacy/Legal approval of
the retained content remains open under `cpt-cf-bss-orders-upreq-audit-identity-lifecycle`.

The S2-06 interim stored `display_label` and `internal_notes` before/after values as an unkeyed
`sha256:<hex>`, from which a short label can be recovered by dictionary search. They are now
`hmac-sha256:v1:<key_id>:<hex>`: the lowercase-hex HMAC-SHA256 (aws-lc-rs, the repository's
FIPS-capable crypto provider) of the exact UTF-8 value under the active Orders minimization key.
`external_reference` stays verbatim and NULL stays NULL. The key is the required gear setting
`audit_minimization { key_id, key }`, where `key` is a secret reference (`"${ENV_VAR}"`, resolved
at load time through the toolkit `#[expand_vars]` pattern) and is never committed, logged or
echoed. A missing, unresolved or shorter-than-32-byte key, or a `key_id` outside 1–64 of
`A-Z a-z 0-9 . _ -`, fails startup. The `key_id` in each value makes a rotation visible; rotation
installs a new `key_id` with a new key and leaves stored rows as written. Verification hashes the
stored text and never needs the key. A whitespace- or case-only change still yields distinct
values, so the `prior_value IS DISTINCT FROM new_value` CHECK admits it. The minimizer remains
one replaceable function. No conformance vector changes: the admin-edit vectors cover only the
verbatim `external_reference`.

[Minimization contract](DESIGN.md#43-data-protection-residency-and-retention),
[audit column](DESIGN.md#contract-01-3-7), implementation evidence.

## D-205 - Orders declares no broker partition count (amends DESIGN §3.7; S2-08)

**Status:** decided 2026-10-06 by the user ("do the recommended way"), confirming the S2-08
coordinator decision.

The Event Broker SDK registers one producer id per producer outbox partition, so each id
carries exactly one sequence space, and the broker places each event on a topic partition by
its partition key. The producer therefore needs no knowledge of the topic's partition count:
Orders has no partition-count setting, and the count of the Orders topic is broker
configuration alone. Any broker partition count stores every event.

[Producer persistence contract](DESIGN.md#contract-01-platform-managed-producer-persistence),
[upstream requirement](UPSTREAM_REQS.md#27-event-broker), implementation evidence.

## D-206 — Authoring wire bindings: `snake_case` bodies, typed create/line results, removal body, unauthored draft cycle (S2-09)

**Status:** selected 2026-10-06 by the S2-09 implementation, at the coordinator's request to settle
the stored-body casing before any route stores replayable bodies (S2-04 gap review, limitation
3). Items 5 and 6 and the guideline and migration citations were added by the independent S2-09
gap review on 2026-10-07. Pending design review. Event payloads are unchanged.

1. **One casing.** Every Orders REST and SDK body member — requests, settled success bodies and
   RFC 9457 `context.data` members — is `snake_case`, matching the S1-02 machine catalog
   (`models.json`: `draft_revision`, `request_audit_id`) and the ToolKit `api_dto` rule. The
   design's earlier response annotations `draftRevision` and `requestAuditId` are now
   `draft_revision` and `request_audit_id`; refusal context members are `current_version`,
   `draft_revision`, `request_audit_id`, `blocked[].overlap_scope_key` and
   `blocked[].conflicting_order_id`. Replay is verbatim, so the settled shape is fixed from the
   first stored body. Event `data` keeps its declared camelCase contract. This follows the
   platform REST guideline (`guidelines/DNA/REST/API.md` §4 *JSON Conventions*: "Naming:
   snake_case") and the ToolKit `api_dto` macro, which fixes `snake_case` on every REST DTO
   (dylint DE0203; the BSS Ledger DTOs record the same ruling over illustrative camelCase).
2. **Typed results.** Create answers 201 with `OrderView` (`order`, `version`, `lines`,
   `draft_revision`) of the committed empty draft, plus `ETag: "1"` and `Location`. A line insert
   answers `TransitionResult` with the server-reserved `line_id` (02 *Author Line* step 6), which
   is part of the settled body, so a same-key retry returns the same identity.
3. **Line removal body.** `DELETE …/lines/{lineId}` accepts an optional JSON body carrying only
   `expected_draft_revision` (the S1-02 binding extracts it from the body; DELETE has no other
   member). An absent body is an omitted revision, which the engine refuses `version-conflict` in
   `draft` (D-147).
4. **Unauthored draft cycle.** `models.json` `AddLine` and CONTRACTS.md allow term and cycle to be
   absent in draft. `orders_draft_content.billing_cycle` is therefore nullable; a whole-period term
   authored without a cycle keeps its intent with no interval, which is derived when the cycle is
   authored (`bss_orders__term_valid`). Committed lines keep the non-null cycle. Migrations 01
   and 07 were edited in place rather than corrected forward: the Orders gear has never been
   committed, released or deployed, so no environment carries an earlier migration history.
   S2-02's forward-only rule (Orders `down()` refuses; deployed corrections are compensating
   forward migrations, DESIGN §3.7 *Migration and schema versioning*) governs applied schemas
   and applies from the first deployment.
5. **SDK settled refusals keep their diagnostics.** A refusal the engine settled reaches an SDK
   caller as `OrdersError::Settled { reason, problem }`, carrying the stored canonical Problem
   with its permitted `context.data` (for example `current_version` and `draft_revision` on
   `version-conflict`), exactly as REST renders and replays it (DESIGN *Workflow SDK contract*:
   "with its registered reason and allowed diagnostics"). `OrdersError::reason()` reads the
   reason of a boundary or settled refusal alike.
6. **Fingerprint axes of an arrangement change.** For a request that proposes a resource or payer
   change, the "three tenant axes in force" of the Foundation §4.2 request fingerprint are the
   arrangement the request establishes: the PEP-authorized current facts completed with the
   delta, exactly the arrangement the engine authorizes and writes. Hashing the replaced axes
   would make the same-key retry of a committed axis edit an `idempotency-mismatch`, because the
   retry reads the arrangement that edit wrote. Amendment payer changes use the same rule. Only
   `draft-mutate` (resource and payer) and `amendment` (payer only) may carry a proposed
   arrangement; the engine refuses any other proposal as an integration defect before
   authorization, a read or a write (DESIGN 02 §2.2 *One order, one payer*, §4.3).

Capture implementation evidence, [field classification](DESIGN.md#contract-02-4-3).

## D-207 — The date-policy channel is a startup promotion; the date basis is strict and frozen (S2-10)

**Status:** selected 2026-10-07 by the S2-10 implementation. Pending design review.

1. **Policy channel carrier.** A deployment promotes `orders_date_policy` rows through the
   optional gear configuration `date_policy { platform_default, overrides[], retired_overrides[] }`.
   Each switch is explicit and none defaults. Startup validates the block, applies it in one
   transaction through the S2-02 secure repositories, then verifies the platform default.
   - An unchanged promotion only reads, so a runtime-role boot carrying the same configuration
     needs no write privilege.
   - A changed promotion requires the `bss_orders_policy` database role. Without it, startup
     fails and nothing is written.
   - Revisions are assigned, not configured. The default advances by one. An inserted or changed
     override takes the namespace high-water mark plus one. The database trigger then advances
     the default past it, and a retirement advances the default too. A re-created override
     therefore gets a fresh `policy_id` and a revision above any the scope has carried
     (DESIGN 02 §3.7).
   - Removing an override needs an explicit `retired_overrides` entry, for two reasons.
     Enumerating unnamed override rows would need an unscoped read, and D-184 confines
     `AccessScope::allow_all` to maintenance discovery. An omitted override is therefore kept,
     not deleted.
   - No Orders endpoint writes the table.
2. **Scoped addressing.** The entity gains the PEP property `resource_tenant_id`. The effective
   read and the promotion address the permanent default by its migration identity, and a
   tenant's override by its resource tenant.
3. **Snapshot shape** (`date_policy_switch_state`, `snake_case` per D-206):
   `{service_activation_required, acceptance_due_required, scope: platform_default|resource_tenant, resource_tenant_id, policy_id, revision}`.
   - Migration 07 adds `bss_orders__date_snapshot_valid` and
     `ck_bss_orders__order_line__dates`. An admitted line must carry both dependent dates and a
     well-formed snapshot (02 §4.2: "an admitted line never stores NULL for any of the three
     dates", D-60). The DESIGN column table showed the two dependent dates as nullable until D-208
     corrected it to `date` (not nullable), matching the migration-07 CHECK.
   - The migration was amended in place under D-206 item 4's rationale: no deployed history.
4. **Proposed date source.** The proposed UTC date is the database clock (`clock_timestamp()`),
   read in the same statement as the policy rows. This is the source the engine's transition
   timestamp `t` uses, so application/database clock skew cannot cause a spurious refusal
   near midnight. Tests inject a clock.
5. **Strict basis check.** The `capture.date-basis` guard refuses `date-cascade-invalid` whenever
   the proposed UTC date differs from `UTC-date(t)`, even when every line date was supplied.
   The run's assessment was evaluated at the proposed date (03 step 15: "checks defaults and
   assessment date"), and this check is a superset of 02 §4.2's transition-default check.
   - Missing required dates are not refused by this guard. They are reported through the
     composite gate's all-failures report (`DateBasis::failures`, 03 §4.2 predicate 8), which
     is registered after it.
   - A missing or invalid effective policy binds a guard that always fails.
6. **Frozen basis.** `commercial_attempt.date_policy_basis` stores the complete versioned
   `DateBasis`: format, the order's resource tenant, proposed date, snapshot and per-line
   resolution with each value's source. A recovered execution restores it exactly and never
   re-reads current policy. Restoring re-validates that every resolved line follows from its
   own supplied values, the snapshot and the proposed date, and that a tenant snapshot belongs
   to the basis's resource tenant. The admitted-line builder refuses a basis prepared for
   another resource tenant or one whose supplied values are not the line's authored dates.
7. **Amendment carry-forward.** An amendment resolves its carried-forward lines with their admitted
   values as supplied values. They remain values under the new snapshot, so a newly required field
   that already holds a carried value is satisfied (02 §4.2: "carried-forward resolved dates remain
   values unless changed by the amendment").

Date implementation evidence, [date cascade](features/02-capture.md#contract-02-4-2).

## D-208 — Admitted-line dates are never NULL; the stale-basis refusal carries no context data (S2-10 gap review)

**Status:** selected 2026-10-07 by the S2-10 gap review at the coordinator's request. Pending design review.

1. **Nullability (amends the DESIGN 01 `orders_order_line` column table).**
   - **What changed.** `service_activation_date` and `acceptance_due_date` are typed `date` (not
     nullable) on `orders_order_line`. The table previously said `date, nullable`.
   - **Why the old text was stale.** D-60 (clarified 2026-09-23) and the normative cascade
     (02 §4.2: "an admitted line never stores NULL for any of the three dates") both forbid NULL:
     - every admitted line stores either the authored value or, where the snapshot does not
       require the field, the contract-effective default;
     - a missing required date is refused at the gate, so a line is never admitted without it.
   - **No exceptions.** `orders_order_line` holds only admitted lines; draft content is
     `orders_draft_content`, where the dates stay nullable. There are no pre-S2-10 rows, because
     there is no deployed history. NULL is therefore legal for no admitted line.
   - **Enforcement.** The physical columns stay nullable. `ck_bss_orders__order_line__dates`
     (migration 07, D-207 item 3) enforces non-NULL together with a well-formed snapshot. The
     generated catalog keeps the draft projection `nullable in draft`.
2. **Stale-basis refusal context.**
   - **No variant fields.** The engine's `date-cascade-invalid` refusal for a changed UTC day
     (`capture.date-basis`) carries no `context.data` variant fields. The DESIGN Problem contract
     allows only explicitly permitted variant fields and never policy diagnostics. It names
     `second-approver-required` as the one refusal with such a field. No field is registered
     for the stale basis.
   - **How the basis is identified.** 02 §4.2's "identifying the stale date basis" is met by
     the settled evidence:
     - the D-188 attempt keeps its frozen `date_policy_basis` (with its `proposed_utc_date`);
     - the refusal audit entry records the transition timestamp at which it was refused;
     - the same key replays the identical settled Problem, over REST, through the SDK's
       `OrdersError::Settled` and from the stored response alike.
   - **Missing-date failures.** Per-line missing-date failures (predicate 8) travel in the
     composite gate's all-failures report in `context.data` (S3-12). They are not added to this
     guard.
   - **If a wire field is wanted.** Adding one requires registering it here first. It then
     applies to the REST body, the SDK and the stored replay together.

Date implementation evidence, gap review (`artifacts/orders-lifecycle-s2-20261006/S2-10-gap-review.md`, local run artifact, not in the repository).

## D-209 — Worker class connections, the conditional sweep lock and streamed checkpoint publication (S2-11)

**Status:** selected 2026-10-07 by the S2-11 implementation under the coordinator's authority; items 6–9 added by the S2-11 gap review the same day. Pending design review. Worker instruments amended by D-223: no audit-namespace metric attribute, roster-wide age gauges.

1. **Per-class restricted connections.** Each Orders-owned worker reaches storage only through a login connection that is a member of exactly its migration-08 group role (`bss_orders_discovery`, `_maintenance`, `_retention`, `_verifier`, `_checkpoint`), configured as the secret DSN references `maintenance.connections.<class>`. A task whose class connection is absent fails validation; startup attests every connection with zero-row probes, plus a `has_table_privilege` check of the checkpoint append right (the only grant that tells the checkpoint and verifier logins apart), and refuses a connection that lacks a granted statement **or** holds an ungranted one (the superuser as verifier, or the two logins swapped, is refused). The host connection keeps the only advisory-lock session (`lock_route`); no lock is taken on a class connection. This is the implementation of CAPABILITIES' "separate verifier/checkpoint/retention/discovery credentials" and of "database privilege is runtime-owned": the runtime never widens a role through `SET ROLE`, which the toolkit's secure runner does not expose and which would require the host login to be a member of every class.
2. **The cleanup "row lock" is the conditional delete's own.** The maintenance role holds SELECT and DELETE on the registry and no UPDATE, so `SELECT … FOR UPDATE` is unavailable to it. The sweep classifies the current row (replaced generation, live window or lease, unresolved linked execution: each preserved) and deletes with one statement whose predicates (exact discovered generation, `expires_at < clock_timestamp()`, lease lapsed or absent) PostgreSQL re-evaluates on the current row version under the row's lock after any concurrent writer commits; the registry guard trigger rechecks live windows and unresolved executions on that same row. The DESIGN roster's "recheck expiry and settlement under row lock; never delete a live/reclaimed in-flight record" is met without an UPDATE grant.
3. **Members stream before the header.** D-100 item 1 streams members into the checkpoint transaction, and the header digest covers them, so the header is written last. Migration 09 therefore replaces `fk_bss_orders__audit_checkpoint_member__0` with the `DEFERRABLE INITIALLY DEFERRED` `fk_bss_orders__audit_checkpoint_member__header`; the link and the cardinality trigger are checked at commit, so header and members still publish atomically and a competing append still rolls every member back.
4. **Schedule bounds.** `maintenance.schedule` carries the design baselines and may tighten them but never exceed: cleanup 1–3600 s / 1–500 rows; purge 60–86400 s / 1–5000 rows / 1–10000 batches per pass; checkpoints 60–86400 s; verification 1–3600 s / 1–500 orders per slice; sweeps 1–3600 s / 1–5000 rows.
5. **Undelivered bodies are explicit ports.** Expiry/auto-void (`TransitionSweep`, Stage 5) and D-188/D-198 recovery (`ExecutionRecovery`, S3/S5) default to `unavailable`: the slot is scheduled and measured, nothing is read, transitioned, abandoned or purged.
6. **The audit roster is the union of live orders, checkpoint headers and committed rows, never surviving audit rows alone.** D-100 item 2 enumerates orders "so an empty/deleted trail is a finding", and every order a previous checkpoint recorded. Namespace discovery therefore lists the distinct `audit_tenant_id` of every live order (any counter), of every checkpoint header and of every committed audit row, each keyset-paged on its own index and merged exactly. A namespace whose whole committed trail was removed, or whose only order was removed after a checkpoint recorded it, stays scheduled under its `audit/<namespace>` key and raises `head_missing`/`order_missing` findings; enumerating `orders_transition_audit` alone, as first implemented, let it vanish from the schedule silently. An empty tenant (live orders, no committed evidence) is checkpointed with no members, as the D-100 acceptance list requires; a counter rolled back to zero above surviving rows is an overflow finding.
7. **Cleanup passes carry a keyset cursor, and recovery precedes the sweep.** The bounded pass (500 rows) continues behind the last marker and the last lapsed execution it visited and restarts from the oldest once a page comes back short, so markers the sweep must preserve (live leases, unresolved executions) and executions the continuation cannot progress cannot occupy every slot of every pass and starve the deletable or recoverable work behind them. The cursor is replica-local; another replica taking the key starts from the oldest, which only revisits. The backlog count and oldest-age figures are whole-set reads, not page figures, so the D-188 alerts do not depend on where the cursor stands. Within a pass the D-188/D-198 continuation runs before the marker sweep, as D-188 orders it ("recovered/terminated before marker cleanup").
8. **A checkpoint mismatch withholds the next capture for one checkpoint interval.** The checkpoint phase runs on every audit tick (verification cadence, baseline 60 s) and captures only when the last checkpoint is older than the interval. After a mismatch it does not re-run the full namespace reconciliation on every tick against evidence known to be wrong: the capture is withheld until the interval elapses or the worker restarts, the last successful checkpoint stands, the verifier keeps alerting on every slice, and the reported checkpoint age crosses the 24-hour alert line. A conflict or a store failure retries on the next tick. The checkpoint metric distinguishes `appended`, `current` (no capture due), `mismatch`, `withheld`, `conflict` and `failed`, so the appended count is the number of checkpoints actually published.
9. **Retention discovery cannot use `SKIP LOCKED`.** 01 §3.5 item 2 selects batches "under `SKIP LOCKED`"; every `SELECT … FOR` row-lock clause requires the UPDATE privilege, which the retention role deliberately lacks (SELECT and DELETE only). Overlapping passes are made safe by the deletion statement itself: it deletes only the discovered IDs that still satisfy the store's class predicate and window at execution, PostgreSQL re-evaluates those predicates on the current row version under the row's lock, and the migration-07 retention guard rechecks the window on the same row, so a row is never deleted twice and no live row is deleted (proved by the two-replica lock-session-loss test). Advisory keys are released on every exit path: on completion and cancellation `DbLockGuard::release` is awaited and its error reported; on a panic the guard's `Drop` queues the generation-checked unlock to the toolkit's lock-session task, so a crashed pass never pins its key beyond the session.

**Amended text (S2-FINAL cleanup, 2026-10-07).** Items 2 and 9 diverged from the DESIGN roster cell "recheck expiry and settlement under row lock" ([Foundation contract §3.8](DESIGN.md#contract-01-3-8)) and from the Foundation feature text that selected cleanup and purge batches "under `SKIP LOCKED`" (01 §3.1 item 6, §3.5 item 2). The implementation is kept: every PostgreSQL row-locking clause requires the UPDATE privilege, the migration-08 maintenance and retention roles deliberately hold SELECT and DELETE only (CAPABILITIES "separate credentials"; attested at startup), the conditional DELETE's predicates are re-evaluated on the current row version under the row's lock, and the two-replica lock-session-loss test proves no double or live deletion. DESIGN §3.8 (roster), the D-184 discovery text and 01 §3.1/§3.5 now state the conditional-delete mechanism and cite this decision.

Maintenance implementation evidence, implementation report (`artifacts/orders-lifecycle-s2-20261006/S2-11-implementation.md`, local run artifact, not in the repository), gap review (`artifacts/orders-lifecycle-s2-20261006/S2-11-gap-review.md`, local run artifact, not in the repository).

## D-210 — Draft read wire bindings, the cursor token, and the snapshot/access-log transactions (early S6-01/S6-04)

**Status:** selected 2026-10-07 by the early S6-01/S6-04 implementation under the coordinator's authority. Pending design review. Scope: the draft subset of [08 §3.3](DESIGN.md#contract-08-3-3) (`get`, `list`, `list_lines`) and the whole of [08 §3.7](DESIGN.md#contract-08-3-7)/[§4.4](DESIGN.md#contract-08-4-4) read-access logging; versions, acceptance, audit and the committed-version composition remain later packages.

1. **Wire shapes.** The S1-02 query names stand (`page_size`, `cursor`, `state`, `created_from`, `created_to`, `state_entered_before`, `contract_id`). `OrderPage` is `{orders, next_cursor?}`; each element is an `OrderSummary` `{order, current_version, draft_revision?}`, the aggregate header at its current version (the composed detail is the point read, so the list needs no version-row join). `LinePage` is `{lines, next_cursor?, current_version, draft_revision?}`: `draft_revision` is added to the catalog's optional members (`models.json` supplement) because a line page of a mutable draft is incoherent without the revision of its snapshot. `OrderView` keeps S2-09's members; `resolved_total`, `acceptance`, `admin`, `expected_fulfillment_time` and `contract_read_through` are **absent** until their producers ship rather than typed as placeholders. Every body is `snake_case` (D-206). The two per-order reads return the current commercial version as the strong `ETag` — the value a write sends back as `If-Match`; the list returns none. The `get` method sits on the one delivered SDK trait (`OrdersV1`), which serves the Workflow consumer read too; a separate Workflow trait is not introduced for one method.
2. **Cursor token (D-139).** Opaque base64url (no padding) JSON, version 1, binding the collection, the parent order (lines) or none (orders), the authenticated subject and subject tenant, a SHA-256 of the canonical UTC JSON of the typed filters (the empty filter set for the line collection) and the declared sort, with the position as `(created_at` in microseconds since the Unix epoch, UUID`)`. Any structural, version, precision or binding failure is `cursor-invalid` before the access decision and appends no access-log row. The token is not signed: it is position only, carries no authority, and a forged position still meets the current PDP scope (tested). Precedent: the Approvals cursor; its composite-source semantics are not reused.
3. **Boundary order and the page-size rule.** `X-Delegation-Proof-Ref` (`request-invalid`, D-202) → `page_size` → filters and unknown query names → `cursor`. `page_size` outside 1–200 **or not an unsigned decimal integer** is `page-size-exceeded`, following the S1-02 oracle (`contract_checks.py`); an unknown query name, a repeated filter, an unregistered `state` token, a non-RFC 3339 instant or a malformed `contract_id` is `filter-invalid`; the line collection accepts no filter. The SDK path applies the same credential-bearing check to `ReadMeta.delegation_proof_ref`.
4. **Transactions (S2-02/S2-11 hand-off).** The scoped read is one `REPEATABLE READ`, `READ ONLY` transaction started after the PDP decision; it holds no lock and can write nothing. The required served access-log row is therefore committed in its **own** short transaction after the snapshot and before the payload is returned (`read-store-unavailable` with no payload if it fails); the refused row likewise after the decision (its failure keeps the refusal and raises `orders_read_access_log_write_failures_total`). `repo::private::insert_read_access_log` keeps accepting any `DBRunner`; the read service always passes that transaction. Rows carry the D-115 actor class from the configured identities, the subject UUID text as `actor`, the catalog operation identifier (`get`, `list`, `list_lines`), the requested/resolved target columns of [08 §3.7](DESIGN.md#contract-08-3-7), the public reason and the operational-only proof detail (D-141), the supplied proof reference as supplied, and `accessed_at` stamped by the writer from the database clock (`clock_timestamp()` read inside the log transaction) like every other evidence timestamp (OL-2); no application or caller clock reaches the log (gap review 2026-10-07, which replaced the application clock the first delivery used).
5. **Restart bound.** Changed authorization facts between the decision and the snapshot restart prefetch and decision, at most three times; a target whose facts keep changing is refused `authorization-context-changed` (logged refused, nothing disclosed). A row the decided scope no longer admits at the snapshot (the resource or payer axis moved under the decision, or the row vanished) is treated the same way — it restarts rather than answering `order-not-found` from inside the snapshot — so the fresh decision's refusal arm appends its refused row and no refusal bypasses the access log (gap review 2026-10-07; the first delivery refused that arm without a row). The refusal arms perform the same discarded scoped point lookup on the hidden and the missing target (D-68).
6. **Collection logging is conservative.** A collection scope is "provably confined to the subject's resource tenant" only when every OR path carries an in-memory-representable `resource_tenant_id` filter whose values are all the subject tenant; an unconstrained scope, a seller/payer/ID-only path, a subquery filter or a foreign tenant logs one served row per request, empty pages included. A service principal's finite-ID scope is never confined, so it always logs.
7. **Undelivered compositions fail closed.** An order outside `draft` has committed versions whose pins, totals, fulfillment projection, acceptance and administrative values are later packages: the point and line reads refuse it **unavailable** (503) after authorization and disclose nothing; the list still summarizes it (header, current version, no draft revision).
8. **Readiness.** The read service binds its database, PEP and identities when `serve` binds the engine and drops them on stop, so no read answers before readiness or after shutdown (consistent with S2-09's writes); boundary validation still answers first.

Read implementation evidence, implementation report (`artifacts/orders-lifecycle-s2-20261006/S6-01-draft-S6-04-implementation.md`, local run artifact, not in the repository).

**Amended 2026-10-12 (D-212).** Items 1 and 3's query names and page shapes are replaced by the shared list format: `limit` replaces `page_size`; the order-list filters are an OData `$filter` (`state eq`, `created_at ge`/`lt`, `state_entered_at le`, `contract_id eq`, joined by `and`); `OrderPage` is `{items, page_info}` and `LinePage` is `{items, page_info, current_version, draft_revision?}`. The cursor token (item 2), the boundary order with its registered reasons, the transactions and the restart bound stand.

## D-211 — The foundation integration milestone: pre-engine throttling bindings, the delivered-route census, real readiness and the live E2E host (S2-12)

**Status:** selected 2026-10-07 by the S2-12 implementation under the coordinator's authority. Pending design review. Item 2 amended by D-221: `throttling.per_order_max_keys` is 1–100000, its baseline the maximum. Scope: [Foundation contract §3.7](DESIGN.md#contract-01-3-7) request limiter (D-185, Q-26 fallback), [08 §3.3](DESIGN.md#contract-08-3-3) route exposure, [§3.8](DESIGN.md#contract-01-3-8) health reporting, and the live HTTP/PostgreSQL E2E host of the draft milestone. Observability extended by D-223: the edge limiter counts its refusals by kind, gauges its tracked keys and warns when its key store is full. Items 1, 5 and 6 amended by D-228 (2026-10-23): the live E2E host, its fragment `config/e2e-orders.yaml`, its gear test and `make e2e-orders` were removed from PR #5259 before merge; the zone values of item 1 stay deployment gateway configuration, the migration 10 history grant of item 5 stays, and item 6's live-E2E evidence is no longer part of this PR.

1. **Per-caller zone bindings.** Every delivered caller-facing engine-entering operation (`create`, `patch_order`, `add_line`, `patch_line`, `remove_line`) binds the api-gateway zone `rl_orders_caller_write` through `ThrottlingSpec { require_security_context: true, dry_run: false }`; the five workflow-only operations bind `rl_orders_workflow_write` when S5-08 mounts them (`api::rest::throttle::workflow_write_throttling`). Reads are not engine-entering and bind no zone. The zone values of D-185 (`3/s`, burst 20; `50/s`, burst 100; 429; automatic `Retry-After`; identity key) are gateway configuration carried by the E2E fragment (`config/e2e-orders.yaml`, `api-gateway` block) and pinned by a gear test; a deployment that omits a bound zone fails gateway startup, which is the intended fail-closed shape.
2. **Per-(caller, order) fallback.** Until `cpt-cf-bss-orders-upreq-gateway-path-param-throttle-key` lands, the gear-local REST-edge limiter `api::rest::throttle::PerOrderLimiter` keys `(subject_id, orderId)` with a keyed token bucket (`governor`, as the gateway) at the §3.7 baseline of 20 per minute, consulted **first** in the four existing-order write handlers — before `If-Match`, the key, the proof header, the body, authorization, the registry and the engine — so it counts every request (admitted, boundary-refused and replayed alike) and a rejection writes nothing. `create` has no order and is bounded by the caller zone alone. The rejection is the gateway's canonical `resource_exhausted` Problem (quota violation subject `throttling`, description `per-order request limit exceeded`, `retry_after_seconds`, `Retry-After`), scoped to the order resource type. The limiter adds no `RateLimit-*` headers of its own; on the live wire the gateway, which admitted the request, still decorates the response with the caller zone's `RateLimit-Remaining`, and the gateway's own rejections name `rate_limit` in the violation and carry no `RateLimit-Remaining`, so the two refusals are told apart by the violation, not by the headers. Configuration `throttling.per_order_per_minute` (1–20; the baseline is the maximum) and `throttling.per_order_max_keys` (bounded key store: while full, a **new** `(caller, order)` pair is refused with the same 429 after idle keys are reclaimed at most once per second; an already-tracked pair is never refused by the bound and spends its own budget, as under the gateway's `max_keys`; the bound is approximate under concurrency, like the gateway's). The limiter is per replica, like the gateway zones, and does not apply to the in-process SDK. Orders removes it when the gateway variant lands.
3. **Delivered-route census.** All 27 catalog operations remain registered contracts (`catalog::OPERATIONS`, the permission census); only `api::rest::DELIVERED_OPERATIONS` (the five writes and the three draft reads) are mounted. `validate_delivered` refuses startup if a delivered identifier is not a catalog operation or would mount a service-only Workflow seam or the break-glass action; the gear test proves every other catalog path is unrouted (404/405) and that no mounted route carries a seam action. The local SDK keeps answering `unavailable` for undelivered methods. Draft cancel is Stage 5 and is **not** mounted.
4. **Readiness.** The gear's readiness check is real: healthy only while the lifecycle task runs, the managed producer is bound, the engine and the read parts are mounted, and the store answers a scoped read of the mandatory platform date-policy default within 2 s. Any loss makes the instance **not ready** (`orders-store-unavailable`, or the producer's bind-failure code) without stopping the lifecycle task, so traffic stops and nothing restarts the process into the same unavailable dependency (§3.8); readiness returns when the dependency does. `/healthz` stays the host's shallow liveness probe. The README's earlier "always reports `orders-not-ready`" statement is superseded.
5. **Live E2E host (runtime login separation).** The E2E suite `testing/e2e/suites/bss/orders` is self-managed: it starts the workspace-pinned PostgreSQL image, applies every gear's migrations with `cf-gears-example-server … migrate` under a schema-owner login (`orders_admin`, `CREATEROLE`), provisions the restricted logins per the README contract (`orders_runtime` ∈ `bss_orders_runtime` only; one `LOGIN` per worker class), and runs the server under `orders_runtime`. This required two owner-side changes: the toolkit migration runner now probes the history table before `CREATE TABLE IF NOT EXISTS` (PostgreSQL checks schema `CREATE` before table existence, so a read-only login could never boot), and Orders migration 10 grants the runtime role `SELECT` on its own history table; a pending migration still fails closed under the runtime login. The gear block, partition count, producer grants and throttling zones come from the checked-in fragment deep-merged into the suite's host config, so the live server runs exactly what the gear's own tests pin. The suite runs through `make e2e-orders` (`run_e2e.py --suite bss-orders`), not the shared SQLite `make e2e-local` server.
6. **Evidence boundaries.** The S2-12 PostgreSQL suite proves the per-caller zone with the real gateway throttling map and middleware over the mounted router, the per-order fallback, the canonical malformed-path refusal and the audit-failure rollback on the wire; the live E2E proves the same seams over TCP with the real AuthN/AuthZ pipeline, the restricted runtime login and `psql` read-back of persisted effects. The E2E host alone runs the per-order limiter at 10 per minute (`testing/e2e/suites/bss/orders/config.yaml`), below the gateway burst of 20, so the fallback is observable over TCP without exhausting the caller zone; production keeps the §3.7 baseline. Outbox-failure rollback stays the S2-08 proof because create and draft mutation are eventless. Successful production submit is not claimed; Stage 3 real-provider gates remain.

7. **Canonical path parameters (gap review 2026-10-07).** Every mounted route extracts `orderId`/`lineId` with the toolkit's `toolkit::api::rest::extract::Path`, the platform precedent (mini-chat), so a malformed identifier is the canonical `invalid_argument` Problem with a `path` field violation, answered before the edge limiter, authorization and any store or log access; axum's plain-text rejection is never on the wire.

Milestone evidence, implementation report (`artifacts/orders-lifecycle-s2-20261006/S2-12-implementation.md`, local run artifact, not in the repository), gap review (`artifacts/orders-lifecycle-s2-20261006/S2-12-gap-review.md`, local run artifact, not in the repository).

## D-212 — REST guideline conformance after the PR #5259 review: 412 for a stale `If-Match`, the replay marker, the refusal-replay exception and the shared list format

**Status:** selected 2026-10-12 by the user on the maintainer review of PR #5259 (the four REST-guideline findings). Pending design review. Scope: the delivered draft writes and reads ([02 §3.3](DESIGN.md#contract-02-3-3), [08 §3.3](DESIGN.md#contract-08-3-3)) against the DNA REST guideline as published on DNA `main` (`REST/API.md` §5 and §8, `REST/PAGINATION.md`); the repository's pinned `guidelines/DNA` submodule predates its 2xx-only caching rule.

1. **Stale `If-Match` is 412.** A public trigger whose present, well-formed expected version is not the current one is refused with the new registered reason `precondition-failed` (`PRECONDITION_FAILED`, canonical FailedPrecondition, HTTP 412, `gts.cf.bss.orders.err.v1~cf.bss.orders.precondition_failed.v1~`). It is an engine reason, settled, audited and replayable like `version-conflict`, and names the same facts (`current_version`, plus `draft_revision` for draft operations). Two conditions stay `version-conflict` (409): a stale draft revision, which is a separate conflict from the version precondition, and a stale version on a workflow-class trigger, whose Workflow seam contract (D-110) names `version-conflict` and which never reaches REST. The stale-authorization arm uses the same split. A Problem body now carries its registered wire status, so 412 and 428 bodies agree with the response line.
2. **Replay marker.** A replayed settled outcome carries `Idempotency-Replayed: true`. The engine already distinguishes replays (`OutcomeKind::Replayed`); the capture service returns that flag beside the stored response and the REST adapter renders it, so the stored response is unchanged and never contains the header. Under item 3 a replayed refusal carries the marker too.
3. **Refusal replay is an approved exception to "cache only 2xx".** The guideline re-executes failed requests on retry. Orders keeps ADR-0005: a refusal is a committed, audited business outcome settled under its idempotency key, and its replay returns the same answer, so a retry cannot turn a refused attempt into a different outcome after the facts move and every refusal has exactly one audit entry. The refusal's audit record is already separate from the response cache: the entry is in the hash-chained transition audit, the response in the idempotency registry. Unsettled outcomes (`authorization-context-changed`, `still-processing`, `idempotency-mismatch`, 5xx and boundary refusals such as 428 or `request-invalid`) are not cached, as the guideline requires.
4. **Shared list format.** The order and line lists use `limit` (default 50, 1–200), the opaque `cursor`, and on the order list an OData `$filter` parsed by the platform parser over a derived field enum (the ledger precedent) and lowered to the existing filters: `state eq` (a registered state token), `created_at ge` (inclusive), `created_at lt` (exclusive), `state_entered_at le` (in the current state since that instant or earlier), `contract_id eq`, each at most once and joined only by `and`. Pages are `items` plus `page_info {next_cursor, prev_cursor, limit}`; `prev_cursor` is always absent because reads page forward only; the line page keeps `current_version` and `draft_revision` of the same snapshot. Kept from D-210: the D-139 cursor token and its request binding, the fixed sorts (`$orderby` is refused), and the boundary order with its registered reasons: `limit` → `page-size-exceeded` (400, not the guideline's 422 `INVALID_LIMIT`), unknown names, `$filter` failures and the retired names → `filter-invalid`, cursor → `cursor-invalid`, all before the access decision and without an access-log row. The OData extractor is not used because it requires the platform `CursorV1` envelope and answers its own problems before the delegation-proof check; adopting `CursorV1` and the guideline's 422 is left to the design review.

## D-213 — Decimal quantities and prices use the shared `bss_money` canonical text

**Status:** selected 2026-10-12 by the user on the maintainer review of PR #5259. Pending design review. Scope: every exact decimal Orders reads or writes as text: the authored item `Quantity` and the schema-2 commercial pin's quantity, price amounts, tier bounds and rates, package size and price, and minimum fee ([03 frozen commercial snapshot](DESIGN.md#contract-03-frozen-commercial-snapshot)).

1. **One codec.** Orders uses `bss_money::serde_text` (and `serde_text::option`) from `gears/bss/libs/money`, the codec Pricing and Ledger already use under the BSS money ADR, instead of its own `scalars::decimal`. The receipt Pricing issues is therefore read with the same rules it was written with.
2. **Normalized spelling.** The canonical text is the normalized decimal: trailing fractional zeros are dropped (`1.2300` is written `1.23`), and a non-canonical spelling (`12.500`, `01`, `1.`) is refused at the boundary. The former codec kept the scale, so `12.500` and `12.5` were distinct texts of one value; that distinction is gone, and "preserve exact decimals" in contract 03 now means the exact value in its canonical text, not the producer's scale. Currency scale is a producer fact carried with each price (item 5).
3. **Range.** A coefficient is at most 28 digits, the `bss_money` contract bound; a wider `rust_decimal` value is refused when written, so it cannot be persisted and then fail to read back.
4. **No migration.** No schema-2 pin or authored quantity is persisted outside tests; the SDK golden fixtures are restated in canonical text. Issued Pricing digests are carried, not recomputed, so they are unaffected.
5. **Currency with its scale (amended 2026-10-15).** Pricing's exact-money change (pricing `958b5e2f1`) makes a price's `currency` a `bss_money::CurrencySpec` (code and the scale it was priced at) and removes `InvoiceInputs.currency_scale`, binding the scale into the money and terms digests. The schema-2 pin carries the price currency as one object `{"code", "scale"}`, validated by `CurrencySpec` on read, and no longer carries `invoice.currency_scale`. Every binding of a receipt must share one code (the market's) and one scale (`CurrencySpec::ensure_same`). The SDK golden fixtures and their digest preimages are restated with Pricing's digest helpers; still no persisted pin exists.

## D-214 — The gear is named `orders`

**Status:** selected 2026-10-15 by the user on the maintainer review of PR #5259. Pending design review. Item 1's E2E suite and `make e2e-orders` were removed from PR #5259 by D-228; a follow-up that restores the suite keeps those names.

1. **Names.** The gear formerly called Orders Lifecycle is `orders`: directory `gears/bss/orders` with crates `cf-gears-bss-orders` (`bss_orders`) and `cf-gears-bss-orders-sdk` (`bss_orders_sdk`), gear and module name `bss-orders`, REST base path `/bss-orders/v1`, Cargo feature `bss-orders`, E2E suite `bss-orders` (`make e2e-orders`). The SDK traits are `OrdersV1` and `OrdersClientV1` and the gear type is `OrdersGear`; the planned Workflow-facing seam trait is `OrdersWorkflowSeamV1`, so it is not read as the Orders Workflow gear's own API.
2. **Unchanged.** GTS identifiers (`gts.cf.bss.orders.*`), database objects (`bss_orders__*`, the `bss_orders_*` roles) and the sibling gears (`orders-workflow`, `orders-changes`) keep their names. The migration history table follows the gear name, and migration 10 grants the runtime role on `toolkit_migrations__bss_orders__*`.
3. **Documentation identifiers.** The system slug follows the directory, so every `cpt-cf-bss-orders-lifecycle-*` identifier is now `cpt-cf-bss-orders-*`, including the ADR file names. Earlier entries in this log and other documents that say "Lifecycle" refer to this gear. Other gears' documents keep their own identifiers and wording; only their links into this gear's documents are updated. The external source PRD (`PRD-orders-lifecycle-202608101404`) and the implementation evidence folders keep their names.

## D-215 — The payer relationship is a business rule, not a permission

**Status:** selected 2026-10-16 by the user on the maintainer review of PR #5259 (thread 13 and a reviewer comment that tenant interactions belong in business logic, not in authorization). Pending design review. Supersedes the payer-use authority of contract 08 (§3.5, §4.3), introduced with the slice-lens review resolution (2026-09-24) and recorded only as a bullet of the high-register reconciliation; no earlier numbered decision covered it. Item 6's live-E2E half (the E2E policy and the E2E foreign-payer test) was removed from PR #5259 by D-228; the storage tests stand.

1. **What changes.** Authorization answers who may see and act on an order: the action, the resource, seller and current-payer access paths, delegation proof, and finite ID sets for services. It no longer decides which payer an order may name. The proposed arrangement, payer included, is still sent to the PDP on create and on axis changes, because the current payer is an access path; the policy simply carries no payer condition.
2. **Where the payer rule lives.** Whether the named payer may pay for the resource tenant is a commercial relationship between those tenants, owned by Account Management. Preview, submit and amendment confirm it from the payer commercial profile (`cpt-cf-bss-orders-upreq-payer-commercial-profile`, extended with `in_relationship_with_resource`), alongside the payer ↔ seller relationship of D-128, **before** resolving or disclosing any payer-related commercial fact. An unconfirmed relationship refuses `payer-relationship-unconfirmed`, registered when the Stage 3 gate implements the check; an unavailable or late answer is the existing `identity-party-unavailable`.
3. **Drafts.** A draft may name any payer. It commits nothing, no payer-related fact is resolved in Stage 2, and no earlier draft edit or Preview substitutes for the check at submit.
4. **Why.** The payer choice is a fact about two tenants, not about the caller; keeping it in the policy put BSS vocabulary (`payer_use`) into the shared rules plugin, and because that condition was attached to every path of a rule, it also narrowed reads: a customer's users lost sight of their own order once its payer left the rule's payer set. Rejected alternatives: keeping `payer_use` (the reviewer objections stand), and renaming it to a generic "proposed value in set" condition (it removes the vocabulary but keeps a business fact in authorization and the read side effect).
5. **Not adopted now.** A caller-specific authority to commit another tenant's money (for example, only finance users may name a parent company as payer) is not required by the PRD. If a deployment needs it, it is an ordinary PDP check on that tenant's billing-account resource, not a condition of the order policy.
6. **Delivered.** The rules plugin drops `payer_use` (a leftover key now fails startup); the Orders test policies and the live E2E policy name no payer; a foreign payer on a draft is accepted and the order stays readable by its resource tenant (storage and E2E tests); refusal coverage moves to an unauthorized resource tenant. The submit-time check is Stage 3 work (`features/03-gate-and-pin.md`).

## D-216 — Resolved totals carry exact decimals, display estimates and explicit discount availability

**Status:** selected 2026-10-18 by the user on the maintainer's follow-up review of PR #5259 (the money and discount comments on `orders_resolved_total`). Pending design review. Refines D-197; the BSS money ADR and Rating's `fr-pre-purchase-evaluation` contract define the producer side.

1. **Money is exact, not minor units.** `orders_resolved_total` stores every producer amount (gross, net, discount, TCV) as **exact canonical decimal text in major units** (`*_exact`) plus the producer's **major-unit display estimate** at the currency scale (`*_display`). Integer minor units cannot hold an exact sub-scale amount: exact EUR 0.015 and its display estimate EUR 0.02 are different values and both are kept. The text is the `bss_money` canonical spelling (no exponent, no leading or trailing zeros, no `-0`), at most 28 significant digits; exact values allow up to 28 fractional digits and display estimates at most `currency_scale`. A database check (`bss_orders__decimal_valid`, after the Ledger's validator) enforces it without float arithmetic.
2. **Scale, not rounding.** `currency_minor_digits` becomes `currency_scale` (0 to 28, the `bss_money` bound). `rounding_policy` is removed: Rating returns no rounding mode, and rounding for payment is the billing chain's. Orders performs no valuation, conversion or summation; display estimates are never payment instructions.
3. **Discount availability is explicit.** `discount_status` is `available` or `unavailable`, and `discount_exact`/`discount_display` are present exactly when it is `available`, independently of usage versus committed amount status. An unavailable discount is stored as unavailable, never as zero; a supported zero discount is stored as `0`. This replaces the former rule that a non-usage row needed a numeric discount and that an unavailable required discount refused, which would have refused every submit while Rating reports discount unavailable (promotions deferred in Pricing).
4. **Unchanged.** Usage rows still carry no committed amount (gross and net NULL), the TCV is still carried only on the order-scope recurring row, and the table stays append-only and producer-verbatim. No resolved total is written before the Stage 3 gate, so no data migration is involved.
5. **Evidence.** Storage tests cover a fractional-cent round trip, an unavailable discount and a supported zero discount, and the refusal of a zero stored as unavailable, a missing discount value, a display estimate beyond the currency scale and non-canonical text.

## D-217 — Service start is the receiver's activation commit, not the provider's ready time

**Status:** selected 2026-10-18 by the user on the maintainer's follow-up review of PR #5259 (the activation-time comment). Pending design review and Subscriptions owner confirmation. Amends D-201's [activation clocks](DESIGN.md#contract-06-activation-clocks); aligns with the Subscriptions PRD rework (#5131, SUB-D-05) and the Subscriptions direction of 18 October.

1. **The rule.** The actual service-effective instant is the `serviceActivatedAt` T that Subscriptions' guarded local `activate` transaction assigns when it commits active, once the required service-readiness confirmations are in. The OSS applied-outcome or callback time is **evidence**, persisted with the applied transition identity, never the commercial start. Billing, entitlement and the term are never backdated to it or to a quoted date. Example: OSS reports ready at 10:00, final readiness, commercial and financial checks complete and activation commits at 10:05: the service starts at 10:05. D-201 previously took the OSS applied-outcome time (10:00), which would have given a different accrual boundary for the same purchase than Subscriptions.
2. **Separate clocks stay separate.** Pricing's frozen `activation_at` (the accepted business activation identity), the receiver's intent-admission instant, the actual start T and the customer-acceptance instant remain distinct. D-201's accepted-terms compatibility rule is unchanged: an actual T that differs from the frozen identity is acceptable only when the supported Subscriptions/Rating profile preserves every accepted term and anchor.
3. **Workflow and Orders read T from the durable applied receipt.** They take T from the receiver's stored applied receipt, never from a provider callback. A delayed callback cannot move T, and a replay after a lost commit response returns the same receipt and the same T rather than assigning a new one.
4. **Open with the owner.** What exactly the "required service-readiness confirmations" are is Subscriptions' to define. #5131 gates `activate` on the OSS confirmation of every resource and keeps license readiness as a separate readiness state; the broader "complete readiness manifest" wording of 18 October is to be confirmed with the Subscriptions owner before this is treated as settled.
5. **Scope.** Documentation only: activation is Stage 5 and no S2 operation is affected. Propagated to DESIGN (activation clocks), features 06, UPSTREAM_REQS (`…-upreq-subscription-start-instant`, the instant mapping), the PRD activation criteria, and the mirrored D-201 notes in the Subscriptions and Rating documents, whose clock sentence now points to this rule.

## D-218 — Worker passes have a deadline; worker class connections bound statements and lock waits

**Status:** selected 2026-10-18 by the user on the PR #5259 code review. Pending design review. Refines D-209 and the [Foundation contract §3.8](DESIGN.md#contract-01-3-8) "bounded pass". Item 3 extended by D-222: on the request path a lock wait or statement those bounds end is a retryable 503.

1. **The defect.** A worker pass held its advisory key for as long as its body ran, and no Orders connection set `statement_timeout` or `lock_timeout`. One statement stuck on a row lock or a half-open connection kept the key indefinitely; every other replica reported `contended` for that worker, so that maintenance stopped everywhere. No data was at risk (every effect is protected by its transactional recheck, D-209), but expiry, cleanup, purge or verification could stall silently.
2. **Per-pass deadline.** `guarded_pass` runs the body under `tokio::time::timeout` with a per-worker budget: **45 s** for expiry, draft auto-void and idempotency cleanup (60 s baseline tick); **5 min** per audit namespace (a verification slice plus, once per checkpoint interval, the streamed checkpoint capture); **30 min** for the retention purge (daily tick, up to 24 batches of 5,000 rows per store). An elapsed pass is `WorkerError::DeadlineExceeded`, counted and recorded as a `failed` pass (`orders_worker_*` `result="failed"`) and retried on the next tick. Dropping the body rolls back its open transaction, and the key is released on the same path as completion and cancellation. The budgets are constants (`MINUTE_PASS_BUDGET`, `AUDIT_PASS_BUDGET`, `RETENTION_PASS_BUDGET`), not configuration: `maintenance.schedule` may only tighten cadences (D-209 item 4), and a shorter tick than a budget only delays the next tick (missed ticks are delayed, never burst).
3. **Session bounds on the class connections.** Every class connection the gear opens itself (`maintenance.connections.<class>`) starts its sessions with `statement_timeout = 30 s` and `lock_timeout = 5 s`, passed as PostgreSQL startup runtime parameters on the class DSN. 30 s is below the shortest pass budget, so a hung statement fails its pass before the deadline does; 5 s lets a conditional delete or checkpoint append blocked behind another writer give up and retry next tick (the checkpoint uniqueness and conditional-delete rechecks are unchanged). A deployment DSN that already names either parameter keeps its value. The host connection (request path and the advisory-lock session) is not changed here: it is toolkit-configured through deployment `params`, as for every other gear.
4. **Evidence.** A storage test drives a pass past a shortened deadline and asserts `failed` and that another replica then acquires the key; a unit test pins the DSN session parameters. Neither runs on this branch until the split-out dependency PRs merge (compile-checked only).

## D-219 — The SDK error is transport-neutral; the REST error gear owns the Problem, the status table and the routes

**Status:** selected 2026-10-21 by the user on the PR #5259 code review (HIGH 13, TOOLKIT-ERR-002, with two related MEDIUM findings under TOOLKIT-CORE-001). Pending design review. Follows the accepted platform [ADR-0005 `cpt-cf-errors-adr-sdk-canonical-projection`](../../../../docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md) and the contract-error decode of the [cluster SDK](../../../system/cluster/cluster-sdk/src/convert.rs). Refines the [Workflow SDK contract](DESIGN.md#orders-workflow-sdk) and the [canonical wire contract](DESIGN.md#contract-01-refusal-reasons-are-derived-gts-error-types); the REST wire is unchanged. Consumer dispatch amended by D-220: `OrdersError` is `#[non_exhaustive]`, so a consumer `match` ends with a fail-closed wildcard arm.

1. **The defect.** `OrdersError::Settled` and `OrdersError::Remote` carried a full RFC 9457 `Problem`, `into_problem` stamped the HTTP status, the SDK `ReasonMapping` held `http_status`, and the SDK `Operation` published the REST method and path, which the gear's route census and authorization read from there. In-process SDK callers received an HTTP envelope as their error, and the local client decided an idempotent replay's result from the stored response's HTTP status.
2. **SDK shape (ADR-0005 projection form).** `OrdersError` keeps `Refused(Reason)`, `Unavailable`, `Integration`, `Internal` and `OutcomeUnknown`; `Settled { reason, diagnostics: RefusalDiagnostics }` replaces the boxed Problem with the typed permitted `context.data` members (`state`/`trigger` for `not-admissible`, `current_version`/`draft_revision` for `version-conflict` and `precondition-failed`, `request_audit_id` for `second-approver-required`, `blocked` for `overlap-collision`; every member optional, unknown members ignored); `Remote` becomes the mandatory catch-all `Other { canonical: CanonicalError }` behind an infallible `From<CanonicalError>` (service-unavailable → `Unavailable`, internal → `Internal`, every other category → `Other`). `ReasonMapping` keeps reason, code, GTS key and canonical category; `Operation` keeps id, PDP resource and action. The trait keeps returning `OrdersError` rather than `CanonicalError` (ADR-0005's trait rule): the gear uses `OrdersError` as its internal error throughout, so switching the trait is a larger API change than these findings, and the projection form is the one the ADR also allows.
3. **Gear REST layer.** `orders/src/api/rest/error.rs` is the one encoder (`problem`, `refusal_problem`, `settled_refusal_problem`) and the engine builds its stored refusal snapshot through it. `orders/src/api/rest/wire.rs`, generated by `generate_catalog.py` from the same reviewed `catalog.json`, holds `reason_status` (including the 412 and 428 same-class overrides) and the `ROUTES` method/path table keyed by operation id; the route census, `is_targeted`, the idempotent-write test and startup `validate_catalog` read it.
4. **Decode by contract error, not by status.** The local client takes success or refusal from the engine's recorded outcome (`Committed`, `Refused { reason }`, or the replayed record's outcome), never from the stored status. A settled refusal decodes through `RefusalEnvelope`'s `TryFrom<Problem>`, keyed on `(error_domain, error_code)` and reading `context.data`, which bounces any other Problem back unchanged: the shape `#[derive(ContractError)]` generates for the cluster SDK. It is hand-written because the derive's encoder would change the frozen wire (its `detail` is the Rust variant name and its status the category default, where Orders answers the reason token and 412/428), and a derived encoder that must never be used would be a trap. A stored refusal that does not decode, or whose code disagrees with the recorded reason, stays `Integration` and is now logged with the recorded reason; it is never reported as success or as an unknown outcome, and a same-key retry replays the same settlement.
5. **Why not `TryFrom<Problem> for CanonicalError`.** Orders refusals are contract errors (`Problem::contract_error` with `context = {"data": …}`), and the canonical decode cannot carry them without a wire change: measured over all 90 registered reasons, 70 are rejected (the failed-precondition, aborted, invalid-argument, permission-denied and resource-exhausted contexts require `violations`, `reason` or `field_violations`), and the 20 not-found, already-exists and service-unavailable reasons convert without their `error_code` and `context.data`, so the reason and diagnostics are lost. Rejected alternatives: adding canonical context to every Orders Problem (a REST wire change), and changing `toolkit-canonical-errors` to keep the contract-error members (a platform change outside this gear). For the same reason a `request-invalid` refusal does not name the failing member in the Problem; the member is logged at debug instead (D-225 item 11).
6. **Evidence.** `orders/tests/rest_problems.rs` compares every boundary refusal, every settled-refusal diagnostic shape and every infrastructure error with `orders/tests/fixtures/rest-problem-golden.json`, captured from the pre-D-219 encoder at `53091e0d5`: status and body bytes are identical. It also decodes every reason back to the same reason and diagnostics and re-encodes them to the same bytes, pins the decode against foreign domains, unregistered codes and mistyped diagnostics, measures the canonical round trip (70 of 90 rejected), and checks the wire bindings against `catalog.json`. The frozen `boundary-fixtures.json` `problem` cases stay REST-boundary fixtures and pass against the gear mapping. The SDK `contracts.rs` pins the projection and the diagnostics' wire members. The storage tests of the local client (outcome-driven decode, integration on a bad refusal) are compile-checked only on this branch until the split-out dependency PRs merge.

## D-220 — SDK contract round: typed codec causes, named selection evidence, one term unit, a closed sales path and a non-exhaustive error

**Status:** selected 2026-10-21 by the user on the PR #5259 code review ("go with the recommendations"; findings #16, #17, #18, #58–#62, #64–#66, #85, #96, #114, #201). Pending design review. Amends D-219's consumer dispatch guidance; refines the [Workflow SDK contract](DESIGN.md#orders-workflow-sdk) and the schema-2 pin codec of D-192/D-193. The REST wire and stored pin bytes are unchanged.

1. **Codec causes (#16, #17, #18, #114).** `CodecError` stays `Copy`/`Eq`. `Malformed` now carries an SDK-owned `MalformedKind` (`Syntax`, `Data`, `Eof`, `Io`: the `serde_json` error category), so a caller can log why stored evidence failed without the decoder's message, which may quote stored values. Line and column are not carried: an encoded pin is one compact line, so the column is a byte offset into commercial content and the category is what an operator acts on. An encoder refusal (a value the strict scalars cannot write, such as a date outside the four-digit year encoding) is `Unsupported`, not `Malformed`: nothing was decoded. `TermIntent::resolve` returns `Incomplete` for a missing billing cycle on `Periods` and `Calendar`, beside the missing intent it already reported so; `Unsupported` stays for zero counts, day/time remainders, nonintegral years and overflow.
2. **Named selection evidence (#64).** `OrderPin::verify_selection(frozen, SelectionEvidence { acceptance_id, request_digest, terms_digest, evaluated_bindings })`: the two issuer digests are named fields, so swapping them no longer compiles.
3. **One term unit (#65).** `TermIntent::Calendar` carries `microseconds`, the unit of `AuthoredTerm::Calendar`, and `impl From<Option<AuthoredTerm>> for TermIntent` is the one conversion: absence is `Missing` (never rolling) and every unit widens losslessly with `u64::from`. A 1000× unit slip can no longer compile.
4. **Closed sales path (#62).** `OrderHeader::sales_path` is `enum SalesPath { SelfService, PartnerPlaced }` with the same `snake_case` tokens, so the serialized `OrderView` is byte-identical and stored responses replay. The create path writes the `SalesPath` token, both reads decode the stored column through `SalesPath::from_token` and fail closed as an integration error on any other spelling (the column's CHECK constraint already admits only the two). The REST OpenAPI DTO keeps `String`: it describes the wire, which is unchanged.
5. **One billing-cycle parser (#85, #96).** `BillingCycle::from_token` sits beside `token()`; the capture prefetch and the draft read use it instead of their own string matches.
6. **Exhaustive patch field lists (#60, #61).** `HeaderPatch::named_fields` and `LinePatch::named_fields` destructure `Self` exhaustively, so a new patch field fails to compile until it is named (trigger selection reads these lists, D-145).
7. **Borrowing serialization (#201).** The schema-2 `vec_*` serialize helpers wrap each element in a borrowing `Ref` and feed `collect_seq` instead of cloning every binding, selection and tier into a temporary `Vec`. Serde emits the same sequence, so the encoded bytes are unchanged; the golden and conformance pins still round-trip.
8. **`#[non_exhaustive]` `OrdersError` (#66; amends D-219).** A later Orders may add a disposition, as the platform SDK errors that are expected to grow already allow (`llm-gateway-sdk` `LlmGatewayError`, `model-registry-sdk` `ModelRegistryError`, `settings-service-sdk`). D-219's consumer dispatch table now ends with a wildcard arm: a consumer treats an unknown disposition as fail-closed. The gear's REST encoder does exactly that: its `_` arm renders the sanitized canonical internal 500. `orders/tests/rest_problems.rs` maps every known variant to its own status and pins which ones share the internal body.
9. **Accepted, not changed (#58, #59).** `patch_line` and `remove_line` keep `(order_id, line_id)` as two `Uuid` parameters. Platform SDK traits pass raw positional `Uuid` identities the same way (`ledger-sdk` `read_account_balance(ctx, tenant_id, account_id)` and `get_entry(ctx, tenant_id, entry_id)`, `products-sdk` `get_sku(ctx, tenant_id, sku_id)`); a `LineRef` or newtypes would be an SDK trait change for every consumer for a swap the engine already refuses (`line-not-found` under the order's own scope).
10. **What the Orders Workflow PR #4787 mirrors.** SDK contract changes: (a) `OrdersError` is non-exhaustive, so every Workflow `match` on it needs a wildcard arm (fail closed); (b) `OrderHeader::sales_path` is `SalesPath`, not `String` (wire unchanged); (c) `verify_selection` takes `SelectionEvidence`; (d) `TermIntent::Calendar` is in microseconds with `From<Option<AuthoredTerm>>`; (e) `CodecError::Malformed(MalformedKind)` and the `Incomplete` missing-cycle result; (f) the additive `BillingCycle::from_token` and `SalesPath::{token, from_token}`. Not contract changes: items 6 and 7.
11. **Evidence.** The SDK tests pin the exact `CodecError` per mutation and per rejected term intent (including `Periods(12).resolve(None)` → `Incomplete`), and `rest_problems.rs` pins the variant mapping; the REST golden, boundary fixtures and `rest_bodies` pass unchanged.

## D-221 — Input bounds: well-known token prefixes in references, the limiter key-store ceiling and a bounded overlap scope key

**Status:** selected 2026-10-21 by the user on the PR #5259 code review ("go with the recommendations"; findings #40, #190, #191). Pending design review. Amends D-202 (the credential check) and D-211 item 2 (the limiter key bound). The REST wire is unchanged: a refused reference is the existing `request-invalid`.

1. **Well-known token prefixes (#40; amends D-202).** `domain::audit::carries_credential` also refuses a reference that contains a well-known opaque secret: GitHub `ghp_`, `gho_`, `ghs_`, `github_pat_`; GitLab `glpat-`; Stripe `sk_live_`, `sk_test_`, `rk_live_`; Slack `xoxa-`, `xoxb-`, `xoxp-`, `xoxr-` (each followed by at least 16 token characters); an AWS access key id (`AKIA` or `ASIA` plus exactly 16 upper-case letters or digits); and a Google API key (`AIza` plus 35 token characters). A token counts where it starts the value or follows a non-alphanumeric separator, so `proof:ghp_…` is refused and an ordinary word containing a prefix is not. The check covers every supplied delegation proof reference and external reference, because both persist into the hash-chained audit preimage, which cannot be scrubbed later without breaking verification. It stays a denylist: the review's allowlisted reference grammar (a registered prefix plus a bounded opaque id) would define what a valid proof reference is, which belongs to the PDP policy owners under `cpt-cf-bss-orders-upreq-pdp-policy-integration` (D-111); Orders adopts it when they publish one.
2. **Limiter key-store ceiling (#190; amends D-211).** `throttling.per_order_max_keys` is the only memory bound on the caller-keyed limiter state (`(subject_id, orderId)` keys, with `orderId` taken from the path before any lookup). It is now validated as 1–100000 at boot, the declared `PER_ORDER_MAX_KEYS_BASELINE` being the maximum, like `per_order_per_minute` (1–20): a deployment may tighten it, never widen it. A value outside fails startup with a sanitized operator error.
3. **Overlap scope key length (#191).** `OverlapScopeKey` (Subscriptions' registry-owned `catalogSubscriptionProductKey`) is at most 1024 UTF-8 bytes, beside the existing empty and NUL refusals. The key is stored in the unique btree index on `(payer_tenant_id, resource_tenant_id, overlap_scope_key)`, whose PostgreSQL row limit is about 2700 bytes; an oversized key is now a validated `InvalidKey` before the claim insert instead of a raw index error on submit. The bound is recorded on the Subscriptions key requirement (UPSTREAM_REQS §2.10).
4. **Evidence.** `orders/tests/input_bounds.rs` runs the production rules: every prefix with a token body is refused at the start of a reference and after a separator, a bare or short prefix and an in-word occurrence are not; `per_order_max_keys` 1, 50000 and 100000 validate and 0, 100001 and `u64::MAX` fail; a 1024-byte key is admitted and a 1025-byte or 1026-byte (multi-byte) key is `InvalidKey`.

## D-222 — Request-path lock-wait and statement timeouts are retryable; deployments bound them

**Status:** selected 2026-10-21 by the user on the PR #5259 code review ("go with the recommendations"; finding #51). Pending design review. Extends D-218 item 3 to the request path; the REST wire is unchanged (503 is the existing `Unavailable` Problem). Item 5 amended by D-229 (2026-10-23): the lib test target now compiles and those unit tests run and pass.

1. **The finding.** A transition takes the aggregate row lock with `SELECT … FOR UPDATE` on the host connection, with no `lock_timeout` and no async deadline, so an SDK or REST writer queued behind a slow holder can wait indefinitely and hold a pooled connection. The review suggested `SET LOCAL lock_timeout` per transition and a `tokio::time::timeout` around engine attempts.
2. **Accepted as platform-wide behaviour.** The host connection is toolkit-configured through deployment `params` with neither bound set by default, as for every other gear; toolkit-db documents exactly that (`libs/toolkit-db/src/config.rs`, `params`: unknown keys are PostgreSQL session runtime parameters, `statement_timeout` and `lock_timeout` named with no default). A per-transaction lock bound is a toolkit-db change (a `TxConfig` option), not an Orders one. The [Foundation topology](DESIGN.md#contract-01-3-8) now recommends `lock_timeout = 5s` (the D-218 worker class value) and `statement_timeout = 30s` in the Orders gear's `database.params`.
3. **Retryable classification.** When a transition statement fails before COMMIT with SQLSTATE `55P03` (`lock_not_available`: the lock was not granted within `lock_timeout`) or `57014` (`query_canceled`: `statement_timeout`), the transaction rolled back and the same request may succeed when sent again, so the engine answers `Unavailable` (503) instead of `Internal` (500). The read-only authorization prefetch and replay disclosure classify the same two codes as temporary. Decided by SQLSTATE, never by message text. Precedent: the graph-storage gear maps `55P03` to its retryable `Serialization` and `57014` to `Deadline` (`gears/graph-storage/graph-storage/src/infra/store.rs`). A failure at COMMIT keeps the D-218/§3.6 rule: a definite server answer is `Internal`, anything else `OutcomeUnknown`.
4. **Never an async timeout around a commit.** Dropping a commit future loses the acknowledgement and turns a definite outcome into an unknown one, so the engine does not wrap attempts in `tokio::time::timeout`; the server-side bounds end the statement and the transaction rolls back cleanly.
5. **Evidence.** Unit tests in `infra/engine/errors.rs` build `55P03` and `57014` driver errors in every abort wrapping (`Store`, `Db`, `DbError::Other`, `Registry`) and assert 503 before COMMIT, `Internal` at COMMIT and a temporary prefetch failure; type-checked only on this branch until the split-out dependency PRs merge.

## D-223 — Worker and limiter metrics carry no tenant attribute; the edge limiter is observable

**Status:** selected 2026-10-21 by the user on the PR #5259 code review ("go with the recommendations"; findings #55, #68, with #105 and #185 in the same round). Pending design review. Changes the S2-11 worker instruments (D-209) and extends the S2-12 limiter (D-211). No REST wire, stored data or log redaction rule changes. Item 6 amended by D-229 (2026-10-23): the lib test target now compiles; the limiter `kind` unit tests and the worker recording-sink tests run and pass.

1. **No namespace attribute (#55).** The audit worker labelled five instruments with `namespace`, the audit tenant UUID, so their series grew without bound with tenants, in the OpenTelemetry SDK's aggregation state and in the backend. The attribute is dropped: `orders_audit_integrity_findings_total{kind}`, `orders_audit_checkpoints_total{result}` and `orders_audit_orders_verified_total` are totals across namespaces. The platform keeps tenant identities out of metric attributes in the same way (the api-gateway throttling middleware: "the high-cardinality bucket key stays a structured field, never a metric attribute"). The namespace stays in the `orders.audit` logs: every integrity finding (error), checkpoint append and append conflict carries it, and a failed namespace pass is logged with its advisory key `audit/<namespace>`.
2. **Roster-wide age gauges (#55).** `orders_audit_checkpoint_age_seconds` and `orders_audit_verification_coverage_age_seconds` keep their names and lose the attribute: at the end of each audit cycle the worker records the oldest checkpoint age and the oldest coverage age among the namespaces it visited (`WorkerMetrics::audit_cycle`). A namespace another replica held that cycle is reported by that replica, so a max-across-instances query gives the roster-wide maximum, and the 24-hour checkpoint alert line of D-100 reads the same way. An interrupted cycle reports nothing; the gauge then keeps its last value.
3. **Edge limiter signals (#68).** The gear-local per-(caller, order) limiter (D-185) adds, on the `bss-orders` meter: `orders_throttle_rejections_total{kind}` with `kind` `per_order` (the pair's budget is exhausted) or `max_keys` (the key store is full and the pair is new), and the `orders_throttle_tracked_keys` gauge, updated on every admission. A full key store logs one warning (`orders.rest.throttle`, with the tracked count and the bound) at most once per one-second reclamation interval, paced by the existing reclamation timestamp. The `(subject_id, orderId)` key is never a metric attribute and never logged (D-44). Precedent: the api-gateway `throttling.rejections` counter with its bounded `kind` (`rate_limit`, `max_keys`, `in_flight`), `gears/system/api-gateway/src/middleware/throttling.rs`.
4. **Workers scheduled (#185).** The new `orders_workers_scheduled` gauge reports how many Orders workers this replica scheduled: 0 without a `maintenance` block (logged at warn, since cleanup, purge and audit verification are then off), and 0 again after stop. Readiness does not change: serving does not depend on the workers.
5. **Marker sweep failures (#105).** A failed marker sweep is logged with the marker's `execution_id`, `operation` and `order_id` and the chained error; never its idempotency key or principal scope.
6. **Evidence.** The production instruments are on the process meter only; the recording sink in the worker tests gains the `AuditCycle` and `Scheduled` observations, and the limiter unit tests pin the two `kind` values. Both are in the lib test build, type-checked only on this branch until the split-out dependency PRs merge.

## D-224 — Orders test tiers: Docker-backed tests are ignored in the unit tier and run by `make test-integration LANE=orders-pg`

**Status:** selected 2026-10-21 by the user on the PR #5259 code review ("go with the recommendations"; findings #133, #142). Pending design review. No production code, REST wire or stored data changes. Items 2 and 3 amended by D-228: the E2E fragment checks and `make e2e-orders` were removed, so the unit tier of those two modules is 13 tests and there is no separate live E2E in this PR. Item 5 amended by D-229 (2026-10-23): the lib test target now compiles without #5381; the unit tier runs (165 passed) and the Postgres tier runs except the 69 #5381-dependent tests compiled out behind the custom `orders_event_broker_test_support` cfg, which nothing sets, until #5381 merges.

1. **The finding.** The storage suite (`orders/src/infra/storage/tests/`) and the gear-boot tests in `orders/src/gear/tests.rs` start a PostgreSQL container through `pg_container::start_postgres`, and they ran in the default unit tier, so `cargo test` and `make test` needed a Docker daemon and a container per test.
2. **Two tiers, the BSS precedent.** Every Orders test that starts a container, directly or through a fixture (`Pg`, the storage `T`, `Env` and `R` harnesses), carries `#[ignore = "requires Docker (testcontainers)"]`, the exact attribute of the ledger and products Postgres tiers: 169 tests, all in the lib test binary. The attribute marks nothing else. The unit tier keeps every test that needs no container: in those two modules the config, date-policy and maintenance-identity validation, the E2E fragment checks, the prerequisite, registry and unsupported-backend refusals and the bind backoff (15 tests); every other in-crate unit module (codec, interval, term, SQL, engine error classification, limiter); and every `orders/tests/*.rs` integration test, none of which starts Docker.
3. **Running the Postgres tier.** `make test-integration LANE=orders-pg` runs `cargo nextest run -p cf-gears-bss-orders --run-ignored ignored-only --no-fail-fast`, the shape of `test-ledger-pg` and `test-products-pg`: no binary or name filter, because the Docker reason is the only use of `#[ignore]` in the gear, and `--no-fail-fast` for the reason the Makefile records on `test-pricing-pg` (a tier whose purpose is to surface PostgreSQL-only defects reports every failure). CI's Ubuntu integration job runs it as "Test bss-orders with Postgres (integration)", beside the ledger and products steps. `make e2e-orders` stays the separate live HTTP/PostgreSQL E2E.
4. **Rule for new tests.** A test that starts a container gets the attribute; one that does not must not, so the unit tier stays runnable without Docker and the Postgres tier stays exactly the container-backed set.
5. **Evidence.** `cargo test -p cf-gears-bss-orders --lib --no-run` still fails with only the known errors of the split-out dependency PRs (four since D-227, all from the Event Broker test-support PR #5381), so on this branch the lib binary, and with it the ignored tier and the in-crate unit tests, is type-checked only until those merge; the `orders/tests/*.rs` integration tests run and pass. `make -n test-integration LANE=orders-pg` prints the nextest command above.

## D-225 — Code-review findings accepted without a code change

**Status:** selected 2026-10-21 by the user on the PR #5259 code review ("go with the recommendations"; findings #54, #58, #59, #166, #167, #168, #172, #182, #192, #253, #257, #258, #259; amended 2026-10-22 with #19, #30, #31, #32, #109, #187 and #237, items 11 to 14, whose choices were agreed in the same round but recorded only in commit messages and code comments, and amended with #238, #246 and #247, item 15). Pending design review. Records why each finding is accepted as the code stands; nothing here changes code, the REST wire or stored data. Items 4, 6 and 7 wait on owner approval; the questions are drafted for the ToolKit, AuthZ, PDP and api-gateway owners and not yet sent.

1. **Checkpoint verification is bounded by the audit pass budget (#54).** The review: the first slice of every full verification pass re-reads and re-hashes every member of every checkpoint in the namespace, outside `verification_orders_per_pass`, and the history grows daily. True, and accepted for now. The pass is bounded: it runs once per full pass (`VerifyCursor::checkpoints_checked`), paged by `MAX_NAMESPACE_PAGE` (500), each statement under the verifier connection's 30 s `statement_timeout`, and the whole namespace pass under the 5-minute `AUDIT_PASS_BUDGET` (D-218 items 2 and 3). A history that outgrows the budget does not stall silently: the pass fails with `DeadlineExceeded`, is counted as a `failed` pass, releases `audit/<namespace>` for the next tick and the coverage age stops advancing toward the 24-hour line of D-100 (D-223 item 2). A checkpoint cursor across slices would change D-100's rule that a full pass re-verifies the whole checkpoint chain in one read-only snapshot, so it waits until measured passes approach the budget. Then the remedy is the review's: a separate resumable checkpoint cursor with a per-slice member bound.
2. **Two `Uuid` line parameters (#58, #59).** Already accepted in D-220 item 9: platform SDK traits pass positional `Uuid` identities the same way, and the engine refuses a swapped pair as `line-not-found` under the order's own scope.
3. **The list `$filter` is parsed by the platform parser, not published through `with_odata_filter` (#166).** The review: `OrderList` derives `ODataFilterable` but the route declares `$filter` as a free-text parameter, so the spec carries no `x-odata-filter` extension. `with_odata_filter::<T>()` (`libs/toolkit/src/api/operation_builder.rs`) publishes, per field, every operator the field kind admits: for a `String` that includes `ne`, `contains`, `startswith`, `endswith` and `in`, for a `DateTimeUtc` all six comparisons. Orders admits exactly the five D-212 terms (`state eq`, `created_at ge`/`lt`, `state_entered_at le`, `contract_id eq`), each once, joined by `and`; anything else is 400 `FILTER_INVALID`. Publishing the builder's table would advertise a superset that the route refuses. So the route keeps the platform parser (`parse_odata_filter::<OrderListFilterField>`, the derive's field table and `MAX_FILTER_LEN`) and lists the terms on the parameter; a comment at the route in `api/rest/read.rs` says why. When the toolkit can publish a restricted operator set, the route adopts it; that question goes to the ToolKit owners.
4. **`CaptureService` and `ReadService` stay in `infra` (#167, #172).** They are the transactional application services REST and the local SDK share: they own the engine, the PEP calls, the `Db` transactions and snapshot configuration, the outbox-bound engine and the access-log transaction, and `edited_row` maps a patch onto the stored `draft_content` row model. The pure rules are already in `domain` (`domain/capture.rs`: trigger selection, capture guards, term geometry; `domain/read.rs`: page bounds, the cursor codec, scope confinement and the access-log decision table). Precedent: Ledger keeps its transactional posting service in `infra` (`gears/bss/ledger/ledger/src/infra/posting/service.rs`, `PostingService`), with pure rules in its `domain`.
5. **The local client's refusal decode is settled by D-219 (#168).** The client takes success or refusal from the engine's recorded outcome (`Committed`, `Refused { reason }`, the replayed outcome), never from the stored status (D-219 item 4). A refusal's diagnostics are decoded from the stored Problem because the stored Problem is the settled response a same-key retry must replay byte for byte; a second typed store would be a second source of truth for one settlement. A stored refusal that does not decode, or disagrees with the recorded reason, is `Integration` and logged (D-219 items 4 and 5).
6. **`PrivateScope` is the 08 §3.5 private-persistence exception, pending owner approval (#182).** The review: `PrivateScope::for_refusal`, `for_read_log` and `for_principal` build `AccessScope`s from the authenticated caller, with no `PolicyEnforcer` decision, and the platform rule is that every `AccessScope` comes from `PolicyEnforcer`. This is the exception [08 §3.5](DESIGN.md#contract-08-3-5) records, following Pricing's integration at the business-operation boundary: audit append, request-bound idempotency and outbox enqueue are private persistence effects, not grantable caller actions. They run under restricted service database roles (migration 08) and scopes bound to the authorized order, the authenticated principal or the refusal subject; they never widen the business decision, return another principal's outcome or authorize a read (`infra/storage/scoped.rs`, `PrivateScope`). Refusal evidence is appended under the authenticated subject tenant without granting the caller the target (08 §3.5, keeping D-104's subject-tenant isolation). The review asks for ToolKit and AuthZ owner sign-off, which is requested; until then the exception stands as designed and the code does not change. M3C Task 4 extends `for_payer_count` to two bounded cross-order overlap reads: D-180's other-resource line count (predicate 7) and `held_same_tuple` (predicate 9), which returns only requested keys held live for the same payer and resource tenant, excluding the requesting order. Neither read discloses holder identities; the engine's step-17 claim remains authoritative.
7. **Security headers belong to the gateway (#192).** The review: neither Orders router mounts a security-header layer, the api-gateway adds none, and the tenant reads carry no `Cache-Control: no-store`. Correct: no gear and no gateway layer sets `X-Content-Type-Options`, `X-Frame-Options`, a CSP or `Referrer-Policy` (the api-gateway layer stack in `gears/system/api-gateway/src/gear.rs` has CORS, body limits, timeouts, auth, throttling and error mapping, none of these). A per-gear layer would give Orders headers no other JSON API sends, and the review itself prefers one gateway-wide layer, so the set is deferred to the api-gateway owners. `Cache-Control: no-store` does have per-gear precedent for caller-specific responses (`gears/system/types-registry/types-registry/src/api/rest/handlers.rs`, `no_store`; account-management's one-time secrets); it is held with the same question so the read responses change once, whether the gateway sets it for authenticated JSON or the gear does.
8. **The sealed `TransactionRunner` stays on the row-lock path (#253, #258).** Single-statement repository leaves take `&impl DBRunner`. The bound remains only on `LockedOrder`, the scoped lock entry points and the `FOR UPDATE` functions, because a `SELECT … FOR UPDATE` on a connection runs in its own autocommit transaction and releases the lock at once. toolkit-db has no transaction-only runner bound, so the gear seals one over `DbTx` and `SecureTx`. The code says so at `infra/storage/repo/mod.rs` (the `transaction_seal` comment) and `infra/storage/scoped.rs` (`lock_authorized`). A toolkit-db transaction-only bound would replace it; that question goes to the ToolKit owners.
9. **`prefetch` takes `&Db` (#257).** It owns its read-only transaction, which must finish before the PDP call: the authorized snapshot starts after the decision (08 §3.6), and a caller's runner could keep one snapshot open across it. Documented on `infra/storage/scoped.rs` `prefetch`.
10. **The mounted-route census is a test (#259).** The review: the startup check `api::rest::validate_delivered` reads the hand-kept `DELIVERED_OPERATIONS`, not what the routers register. The mounted set is checked against that list in `gear/tests.rs` (`assert_delivered_census`): it compares the method and path of every operation the real capture and read routers registered, and the OpenAPI document, with `DELIVERED_OPERATIONS`, and checks every delivered write binds the caller zone and no read is throttled. The routers are static code, so a route added without the list fails that test on the same commit; the startup check keeps its own job, refusing a delivered id that is not a catalog operation or that would mount a service-only or break-glass action. The census runs in the Postgres tier (D-224), because it boots the gear on PostgreSQL.
11. **A `request-invalid` refusal does not name the failing member in the body (#19, #30, #31, #32).** The review: a body that fails to decode, and a `HeaderPatch`, `AddLine` or `LinePatch` that fails `validate()`, become `request-invalid` with the serde error or the `AuthoringInvalid` member discarded, so neither the client nor the operator learns which field was wrong. The operator half is fixed: the REST body decoders (`api/rest/capture.rs`, `refused_body`) and the capture `validate()` sites (`infra/capture.rs`) write one debug record on `orders.capture` with the operation, the static member name where there is one (the `AuthoringInvalid` name, `expected_draft_revision`, `fields`) and the serde error category, never the serde text, which can echo the caller's values. The client half is accepted: the refusal body stays `context.data: {}`. That body is frozen by the REST golden (`orders/tests/rest_problems.rs`) and the boundary fixture, DESIGN maps refusals to RFC 9457 "with no internal diagnostics on the wire", and D-219 item 5 already declined adding canonical context (`field_violations`) to Orders Problems because it changes the wire. Naming the member on the wire would be a new decision with a fixture change, taken together with the platform's `InvalidArgument` field-violation shape.
12. **A capability that is not delivered yet still answers 503 (#187).** The review: undelivered capabilities reuse `Unavailable`, which a caller reads as a retryable outage. Internally they are now distinct: `EngineError::NotDelivered(&'static str)` (`domain/error.rs`) and `Abort::NotDelivered` carry the capability name and log one info record on `orders.not_delivered` instead of the error-level abort line, so operators can tell them from an outage. The answer stays `OrdersError::Unavailable` (503) on REST and in the SDK (`domain/error.rs`, the `From<EngineError>` mapping). A 501 or a registered refusal would change the REST wire and add an SDK variant that the Orders Workflow consumer (#4787) would have to handle, for paths that exist only until their slices ship (D-188, D-198, the S4-05 admin edit, the committed-version reads, SDK `submit`). Each delivery replaces its `NotDelivered` with the real path.
13. **One `#[allow]` keeps its exact text (#237).** The review asks every `#[allow]` to carry a `reason` or become `#[expect]`. `infra/maintenance/scope.rs` keeps `#[allow(clippy::disallowed_methods)]` on the discovery constructor, with its reason in the comment above it, because it is the single D-184 exception and `orders/tests/d184_lint_gate.rs` locates it by that exact attribute text (`rfind("#[allow(clippy::disallowed_methods)]")`) to prove the crate-wide `deny` has only this one hole. Adding `reason = ...` or switching to `expect` would need the gate test changed in the same commit; it is left as is so the gate keeps matching what D-184 records.
14. **`authz` stays public but hidden (#109).** The review: the internal PEP module is exported, so the gear's PDP mapping looks like public API. `lib.rs` marks it `#[doc(hidden)] pub mod authz` with a comment that it is not a consumer contract, as `domain` already is. It cannot become `pub(crate)`: the integration tests that run today (`orders/tests/boundary.rs`, `orders/tests/rest_problems.rs`) check the operation census and action mapping through it, and the in-crate tests cannot run until the split-out dependency PRs merge. Ledger, Products and Pricing also expose `pub mod authz`.
15. **Three module-level `dead_code` expectations stay (#238, #246, #247).** The review asks to replace the module-wide lint attribute on `infra::dates` (`infra/mod.rs`), `infra::storage::entity` and `infra::storage::repo` (`infra/storage/mod.rs`) with item-level `#[expect(dead_code, reason = ...)]` on each undelivered item. The module-wide `allow` without a reason that the review saw is gone: each of the three modules carries `#[cfg_attr(not(test), expect(dead_code, reason = ...))]`, whose reason names the undelivered slices (the date preparer and admitted-line builder until Preview, submit S3-12 and amendment S4; the date-policy and state-TTL policy entities until policy administration; the repository functions for fulfillment children, policy administration, audit pages and registry receipts), plus a test-build `allow` with its reason, because the tests reach only part of each module and `expect` cannot be verified per test target. These modules hold code for slices not yet delivered, and which items are unused changes with every slice: item-level attributes would be churn added now and removed again item by item as each slice ships, while the module-level `expect` turns into an unfulfilled-expectation warning, which the `-D warnings` clippy gate fails, as soon as the last undelivered item in the module gains a caller, so it cannot outlive its reason.

## D-226 — One actor class for every authorization decision: the configured identities decide, the subject type only tightens

**Status:** selected 2026-10-22 by the user on the PR #5259 code review (finding #188, option B of three). Pending design review. Refines D-115 and the D-182 user-only rule. No REST wire, stored data or audit `actor_class` changes. Item 8 amended by D-229 (2026-10-23): the lib test target now compiles; the PEP test runs and passes.

1. **The finding.** The D-182 user-only rule for rows 28 and 29 was decided twice from different inputs. The PEP (`Pep::admit_user_only`, and the 08 §4.3 finite-ID rule in `requires_finite_ids`) classed the caller by its token subject type (`user` → user, anything else → service), while the engine's declared row actor rule (`check_actor_rule`) classed it through the configured identities (D-115: the configured worker actor → `system`, a configured service principal → `service`, everyone else → `user`). A user-typed subject that is also configured as a service principal passed the PEP and then failed in the engine as `EngineError::Integration` (500), after the PDP call and with no refusal row.
2. **One function.** `authz::decision_class(identities, ctx)` is the class every authorization decision reads: the PEP's break-glass user-only check, the PEP's finite-ID requirement (every class but `user`), and the engine's `check_actor_rule`. The configured identities decide first (`ActorIdentities::configured_class`, the same matching `ActorIdentities::classify` uses for the audit row). A subject they leave as `user` stays `user` only when its subject type is the platform user type (`gts.cf.core.security.subject_user.v1~`); any other type, or none, is `service`. The subject type can only tighten the class, never loosen it. The PEP holds the identities (`Pep::new(enforcer, identities)`); the gear builds the PEP and the engine from one `config.actor_identities()` value.
3. **What changes.** Every correctly configured caller, and every unconfigured service-typed or untyped token, gets the answer it got before. A user-typed subject configured as a service principal or as the system actor is a deployment misconfiguration; it is now decided by its configured class at the PEP. On `force-fail-unreconciled` it gets the ordinary break-glass denial, mapped by its read access like any targeted denial (D-114: 403 `operation-not-permitted-for-actor`, or the non-disclosing not-found), instead of the engine's 500. On every other action it is held to the finite order-ID rule like any service principal, so a PDP scope without a finite ID set is the existing `orders.authz.integration` failure. `decision_class` logs that misconfiguration at warn on `orders.authz` with the configured class only, never the principal (D-182).
4. **The backstop stays.** `check_actor_rule` reads the same class, so a caller the PEP admits can no longer fail its user-only rule. It is kept: the `system` rule (expire, auto-void) is never decided by the PEP, because the worker path does not authorize through it, so the engine check is the only enforcement of that rule. Its integration log keeps the rule and class and never the principal.
5. **The audit row is unchanged.** `orders_transition_audit.actor_class` and `orders_read_access_log.actor_class` still record `ActorIdentities::classify` (D-115), from the same identities. The decision class differs from it only where an unconfigured non-user token is tightened to `service`, which the audit row records as `user` as before.
6. **Rejected: the configured identities alone (option A).** It makes the PEP and the audit row agree exactly, but an unconfigured service-typed token would become `user` at the PEP: break-glass would rest on the PDP grant alone, and a service decision without a finite order-ID set would be admitted where it is refused today. Both loosen a security rule for callers that are not misconfigured, and change REST answers for them.
7. **Rejected: refusing unconfigured service-typed subjects outright (option C).** It closes the gap from the other side, but it is a new refusal for callers D-115 classes as `user` and that the PDP may legitimately grant, so it changes the API contract and DESIGN, beyond a review fix.
8. **Evidence.** `orders/tests/actor_class.rs` runs every configured identity × subject type combination through `decision_class`, checks it against the audit class, and checks the misconfiguration log carries the class and not the principal. The PEP test `break_glass_is_never_honored_for_a_service_principal` (`orders/src/authz/tests.rs`) adds user-typed subjects configured as the Workflow principal and the system actor; it is in the lib test build, type-checked only on this branch until the split-out dependency PRs merge.

## D-227 — In-crate tests use a gear-local fake PDP; the live E2E keeps the real rules provider

**Status:** selected 2026-10-23 by the user (the in-crate tests must not depend on the split-out `rules-authz-plugin`, PR #5375, which is not merged). Pending design review. Test code and docs only: no production code, REST wire, stored data or E2E configuration changes. Items 4 and 5 superseded and item 6 amended by D-228 (2026-10-23): the live E2E suite, the fragment and its producer-grant check were removed from PR #5259, so the provider evidence item 6 assigns to the live E2E is not part of this PR. Item 7 amended by D-229 (2026-10-23): the four #5381 sites are compiled out behind the custom `orders_event_broker_test_support` cfg, which nothing sets, so the lib test target compiles and runs; the `FakePdp`-driven unit tests and the Postgres-tier `authz` suite run, while the `engine`, `capture` and `reads` suites stay compiled out until #5381 merges.

1. **The problem.** The PostgreSQL-tier storage tests (`orders/src/infra/storage/tests/{authz,engine,capture,reads}.rs`) answered PDP calls through the rules provider of #5375, and a gear-boot test parsed the E2E policy fragment into that provider's config type. The provider is not a dependency of the Orders crate, so those sites were three of the seven known `--lib --no-run` errors (E0433), and the in-crate tests could never run before #5375 merged.
2. **The fake.** `authz::test_pdp::FakePdp` (compiled only under `cfg(test)`, inside the existing test-only `test_pdp` module) implements the platform `AuthZResolverApi` that `PolicyEnforcer` consumes, so the tests still drive the real `PolicyEnforcer`, the real Orders PEP and real PostgreSQL scopes. A test lists `Grant`s: a subject, Orders `Action`s (matched on the exact registered resource type and PDP action name from `gts::PERMISSIONS`) and OR paths of `property IN values` predicates. It denies by default. The grants that name the subject and the requested resource/action contribute all their paths as constraints, and the PEP and scoped queries enforce them. A grant marked delegated contributes only when the request's `delegation_proof_ref` is one of the fake's accepted proofs; when nothing contributes and a delegated grant would have, the denial is coded `delegation_proof_required` (no proof) or `delegation_proof_invalid` (a proof it does not accept), the Orders interim codes. Nothing else: the finite order-ID rule for services, the break-glass user-only rule and the hidden/missing follow-up read stay the PEP's own and are exercised as before. The semantics were derived from what the tests assert, written from scratch; no content of #5375 was used.
3. **Precedent.** Rating's `FakePdp` (`rating/src/test_providers/authz.rs`, PR #5390), an `AuthZResolverApi` double answering `In` constraints, and the in-tree `cfg(test)` resolver doubles such as account-management's `domain/tenant/test_support/auth.rs` and settings-service's `test_support.rs`. Rating gates its double behind a `test-providers` feature because integration tests and other crates use it; Orders needs it only in the lib's own tests, so `cfg(test)` is enough and nothing reaches a production build.
4. **The E2E fragment check.** `e2e_fragment_grants_event_broker_produce_to_exactly_the_orders_producer` now reads the parsed YAML of `config/e2e-orders.yaml` instead of the provider's config type. It still pins what Orders owns: the block holds exactly one unconditional grant and one rule, both name exactly the configured producer principal (`events.producer_*`), the grant is `event_type` × `produce`, and the rule is `request` × `produce` with one `owner_tenant_id` predicate on one tenant. Checking the provider's schema and its `validate` is left to the provider itself, which loads this fragment when the live E2E host starts. The alternative of asserting that the fragment names every census permission was rejected: the fragment grants only Event Broker production, and the Orders grants of the live suite live in `testing/e2e/suites/bss/orders/`.
5. **The live E2E is unchanged.** `make e2e-orders`, `config/e2e-orders.yaml` and the example server's rules-authz feature keep the real provider of #5375 (UPSTREAM_REQS §2.9, "Integration and E2E provider").
6. **What the fake does not prove.** That the real provider's policy language expresses these grants, or that the provider evaluates them the same way: rule matching beyond subject ID and exact resource/action, subject tenant, token scopes, tenant context, per-rule proof lists, deny precedence, priorities, unconditional grants, and its handling of resource properties other than the proof reference. It also proves nothing about the deployed production provider (the open §2.9 requirement). Those remain the live E2E's and the provider owners' evidence.
7. **Evidence.** `cargo test -p cf-gears-bss-orders --lib --no-run` fails with four errors, all from #5381 (one E0432 for the Event Broker `test_support` imports, three E0599 `with_types_registry_client`); the three E0433 are gone. The lib test binary therefore stays type-check only on this branch. In a scratch copy with those four #5381 sites removed (not stubbed with any #5381 content) and the Docker-only storage module left out, the rewritten E2E fragment check passed, and a throwaway test drove `FakePdp` through the real PEP over the S2-03 authorization matrix (actor/axis visibility, targeted 403/404, proposals, delegation required/invalid/revoked, finite IDs, service break-glass, resource-type separation) on in-memory facts: all passed.

## D-228 — The live E2E suite is removed from PR #5259

**Status:** selected 2026-10-23 by the user (PR #5259 must no longer depend on the split-out PRs #5375, `rules-authz-plugin`, and #5376, toolkit-db restricted login and the `PostgresSidecar` E2E helper). Pending design review. Test code, build wiring and docs only: no production code, REST wire, stored data or migration changes. Amends D-211 items 1, 5 and 6, D-214 item 1, D-215 item 6 and D-224 items 2 and 3; supersedes D-227 items 4 and 5 and amends its item 6; amends the UPSTREAM_REQS §2.9 "Integration and E2E provider" paragraph and the §2.12 fallback status. Items 3 and 6 amended by D-229 (2026-10-23): the lib test target compiles and runs without #5381; of the in-process evidence item 3 lists, the mounted-router throttling, read-route, audit-rollback and malformed-path tests and the gear-boot producer bind and readiness-loss tests are compiled out until #5381 merges, while class-connection attestation still runs.

1. **What is removed.** The self-managed live HTTP/PostgreSQL suite `testing/e2e/suites/bss/orders` (`conftest.py`, `config.yaml`, `e2e.yaml`, `test_draft_capture.py`: 14 tests), the gear's E2E host fragment `config/e2e-orders.yaml`, the `make e2e-orders` target, and the four gear tests that read the fragment: the broker partition-count pairing, the Event Broker producer grants (D-227 item 4), the gateway zone values and the fragment's identity classification. Two keep their fragment-free coverage: `configured_identities_classify_system_service_and_user_distinctly` builds the same distinct system, service and producer identities inline, and `throttle_zone_bindings_and_edge_limiter_settings_are_bounded` keeps the code's zone names and the edge limiter's bounds. The unit tier of `gear::tests` and the storage module is now 13 tests (D-224 item 2 counted 15).
2. **Why.** The suite was the last part of the PR that needed split-out work: its `e2e.yaml` selected the `rules-authz` example-server feature of #5375 (the configurable fail-closed PDP provider), and its `conftest.py` needed #5376's `PostgresSidecar` and restricted runtime login (the migration runner's history probe, D-211 item 5). Since D-227 the in-crate tests authorize through the real `PolicyEnforcer`, the real Orders PEP and real PostgreSQL scopes with `FakePdp`, so without the suite the PR needs neither PR. The remaining split-out dependency is #5381 (Event Broker test support).
3. **What is no longer proven.** The 14 tests ran against `cf-gears-example-server` over TCP: the api-gateway with static-authn tokens and identity-keyed throttling, the authz-resolver with the real fail-closed rules provider, the server under the restricted runtime login on a schema migrated by an owner login, and persisted effects read back through `psql`. They proved:
   - an authenticated create persists the trusted identity, version 1, draft revision 0 and one committed audit row, and the same key replays byte for byte;
   - caller-supplied actor fields (`actor_subject_id`, `sales_path`, `created_by`) cannot replace the trusted identity, and nothing is created;
   - the authoring round trip (header and line edits, line removal) keeps identities and revisions, and the persisted membership and version/revision match;
   - retry and conflict on the wire: only the replay is marked, key reuse with a changed payload, a stale draft revision, a stale `If-Match` (412), a missing `If-Match` (428), and which of them are audited;
   - reads: the `ETag` a write sends back as `If-Match`, the shared list format's pages, and an access-log row before a cross-tenant disclosure but none for an own-tenant read;
   - the authorization matrix and tenant isolation under the real provider: foreign tenant hidden, same-tenant membership alone grants nothing, the seller reads but cannot author, the payer axis reads, a foreign resource tenant is refused, any payer on a draft (D-215);
   - service principals need finite order-ID constraints (fail closed, sanitized), and service-only Workflow seams are unreachable from any token;
   - the delegation-proof header is forwarded to the PDP, never echoed and recorded on the served row; missing, wrong and malformed proofs;
   - the gateway caller zone (`rl_orders_caller_write`, 3/s, burst 20) throttles before the engine with the canonical 429 and its policy headers, and a throttled attempt writes no audit row;
   - readiness is real (`/healthz` then `/readyz`) and reads survive a server restart;
   - a failing required access-log write serves no protected data;
   - a failing audit append rolls back the admitted write over HTTP;
   - the per-order edge limiter is keyed per order over HTTP (10 per minute on that host) and refuses a replay at the edge;
   - malformed path parameters are canonical `invalid_argument` Problems before authorization, the limiter and the store.

   Beyond those seams, nothing in this PR now proves: the real provider's policy language and evaluation (D-227 item 6); real AuthN tokens through the gateway; the restricted runtime login booting a migrated schema (migration 10's history grant with #5376's probe); the worker-class logins and the managed Event Broker producer in a real host; the gateway caller zone over a real listener; readiness across a process restart; and that a checked-in deployment fragment (partition pairing, producer grants, zone values) is the one a server runs. The in-process evidence stands: the Postgres tier (D-224, `make test-integration LANE=orders-pg` in CI) drives the mounted router over real PostgreSQL with the real gateway throttling map and middleware, the per-order limiter, the read routes on the wire, the audit-failure rollback and malformed paths, and the gear-boot tests cover class-connection attestation, the producer bind and readiness loss.
4. **Where it comes back.** A follow-up PR once #5375 and #5376 merge restores the suite, the fragment, its gear tests and `make e2e-orders`. The removed files are in git history at `f113a0e17` (the S2 milestone commit; the producer-grant check in its D-227 form is at `cab1c72e1`) and on the local branch `backup/orders-lifecycle-impl-pre-squash-20261023`.
5. **Kept.** The `bss-orders` example-server feature with its Event Broker, standalone cluster and SQLite event-log wiring (`cargo check -p cf-gears-example-server --features bss-orders`), the workspace members, `make test-integration LANE=orders-pg` and its CI step, migration 10 and the README's restricted-login provisioning contract. The D-185 zone values stay deployment gateway configuration.
6. **Evidence.** `cargo test -p cf-gears-bss-orders --lib --no-run` still fails with exactly the four #5381 errors (one E0432, three E0599). In a scratch run with those sites and the Docker-only storage module left out, the eleven non-Docker `gear::tests` tests, the two rewritten ones included, passed. No file outside the gear references the removed suite, fragment or target.

## D-229 — The #5381-dependent in-crate tests are compiled out until #5381 merges

**Status:** selected 2026-10-23 by the user (the lib test target must compile and run without the split-out Event Broker test-support PR #5381, which is not merged; the gate is a custom cfg, not a Cargo feature, so `--all-features` never re-enables it). Pending design review. Temporary. Test code, a cfg-declaring build script and docs only: no production code, REST wire, stored data or migration changes; the default build is unchanged. Amends the evidence of D-222 item 5, D-223 item 6, D-224 item 5, D-226 item 8, D-227 item 7 and D-228 items 3 and 6.

1. **The problem.** Since D-227 the lib test target failed to compile with exactly four errors, all from #5381: one E0432 (the `event_broker::test_support::{FaultInjectingBroker, TransientFault, DeliveredOutcome, DeliveredPublish}` imports) and three E0599 (`EventBrokerHarnessBuilder::with_types_registry_client`). No in-crate test had therefore ever run on this branch: the unit tests under `orders/src/**` and the whole Postgres tier were type-checked only.
2. **The gate.** Exactly the code that needs #5381 carries `#[cfg(orders_event_broker_test_support)]`; nothing of #5381 is copied, stubbed or replaced by a local double. It is a custom cfg rather than a Cargo feature because the PR clippy lane runs `cargo clippy --workspace --all-targets --all-features` (`make clippy`, pass 1; the gear-scoped clippy passes `--all-features` too), which would turn a feature on and bring the four #5381 errors back. Nothing sets the cfg (no `RUSTFLAGS`, workspace, example-server, Makefile or CI reference). A minimal `orders/build.rs` (wired by `build = "build.rs"` in `orders/Cargo.toml`) prints `cargo::rustc-check-cfg=cfg(orders_event_broker_test_support)` so `unexpected_cfgs` accepts it (the `cargo::` form needs Cargo 1.77; the pinned toolchain is 1.98.1). The in-repo precedent for an expected custom cfg is `cfg(coverage_nightly)`, declared through `check-cfg` in `[workspace.lints.rust]` and in event-broker-sdk's own `[lints.rust]`; Orders inherits the workspace table (`[lints] workspace = true`), which cannot be extended per crate, so the declaration goes in the build script. To compile the gated tests in once #5381 is in the base: `RUSTFLAGS="--cfg orders_event_broker_test_support" cargo test -p cf-gears-bss-orders --lib`.
3. **What is gated (69 tests, all Docker-tier).**
   - Whole storage test modules `events` (12), `engine` (19), `capture` (15), `reads` (14) and `throttling` (4), declared under the gate in `infra/storage/tests/mod.rs`. Every test in them runs over the shared `events::Env`, whose broker loads the Orders contract through `with_types_registry_client` and is wrapped in `FaultInjectingBroker`; `capture::T` and `engine::T`, which `reads`, `throttling` and `dates` use, are built on it. A per-item gate would cover every item in those files with the same effect, so the module is the narrowest gate that reads.
   - In `dates`, the "Admission through the engine (row 4)" section: `admission_persists_the_resolved_triple_and_snapshot_immutably`, `a_utc_day_rollover_refuses_without_admission_and_a_fresh_attempt_commits`, `a_required_unauthored_date_refuses_at_the_gate_without_admission`, their helpers and the imports only they use. The five date-policy channel tests stay compiled.
   - In `gear/tests.rs`: `serve_binds_the_real_producer_and_stops_its_workers` and `store_loss_stops_readiness_without_a_restart_loop`.
   - Test-only helpers whose only callers are gated: `events::test_support::{Root, root}` and `TxEvents::enqueue_unchecked`. The test-time `dead_code` expectations on `BssOrdersGear::engine` and `EventDetail` also hold while the feature is off.
4. **What is now unproven until #5381 merges.** No test in this PR exercises:
   - the real-registry contract load: the in-process broker's Orders topic and event types loaded from the `types-registry` commit of the Orders contract, with every event stored whatever the topic's partition count;
   - event delivery through the managed producer: committed transitions deliver platform-root-tenant events routed by order; the enqueue is atomic with the transition and once per attempt; a failed enqueue never commits even if the error is dropped; maximal envelopes fit and oversize events roll back; the root-tenant grant is required and a rejection advances; binding fails closed without each prerequisite;
   - event fault injection: transport and rate-limit outages on cursor reads and publishes retry in order and roll nothing back; a lost acknowledgement retries after a restart with cursor recovery and without a duplicate; a publish delivered without a reply (accepted, persisted or duplicate) is acknowledged once; an unknown producer re-registers and its message is delivered;
   - the S2-04 engine on PostgreSQL (row effects, audit and event cardinality per key, refusals, version precedence, replays, concurrent creates, lease and changed-facts handling, rollback at every persistence boundary, commit rejection, claims, post-spawn hold, administrative and draft edits, the outbox-insert rollback);
   - S2-09 draft authoring through the capture service, REST and the local SDK, and the date admission through the engine (S2-10 row 4);
   - the early S6-01/S6-04 reads (point and list reads, cursors, the access-log decision table, hidden/missing, service finite IDs, the mounted read routes and the undelivered compositions);
   - the in-process wire checks of D-228 item 3: the real gateway caller zone, the per-order edge limiter, the audit-failure rollback and malformed path parameters on the mounted router;
   - the gear boot binding the real producer and stopping its workers, and readiness loss on a lost store.

   What stands: the lib unit tier (now run, item 6), the remaining Postgres tier (migrations, repositories, roles, audit, authz, claims, constraints, executions, idempotency, maintenance, workers, policies/retention, platform, the date-policy channel, the other gear-boot tests) and the `orders/tests/*.rs` integration tests.
5. **Undo.** When #5381 merges into this branch's base: optionally first run the lib tier with `RUSTFLAGS="--cfg orders_event_broker_test_support"`; then delete `orders/build.rs` and the `build` key in `orders/Cargo.toml`, every `cfg(orders_event_broker_test_support)` attribute (`infra/storage/tests/mod.rs`, `dates.rs`, `gear/tests.rs`, `infra/events/test_support.rs`, `infra/events/mod.rs`) and the `not(orders_event_broker_test_support)` arm of the two `cfg_attr` expectations in `gear.rs` and `infra/engine/documents.rs`; run the lib tier, `make test-integration LANE=orders-pg` and `cargo clippy -p cf-gears-bss-orders --all-targets -- -D warnings`; then mark this decision superseded. Until then, setting the cfg reproduces exactly the four #5381 errors, which proves the gate covers nothing else.
6. **Evidence (first run).** `cargo test -p cf-gears-bss-orders --lib`: 165 passed, 0 failed, 100 ignored (the remaining Docker tier). No test failed on its first run. The first lint of the target (`cargo clippy -p cf-gears-bss-orders --lib --tests -- -D warnings`) found 14 test-time findings, fixed in test code: a stale `dead_code` expectation on `DiscoveredExecution` (no test reads its accessors, so the expectation is now unconditional), and 13 clippy findings in `audit_tests`, `read_tests`, `contributions_tests`, the `engine::errors` test module, `interval_tests`, `constraints` and `workers`. The previously type-check-only unit tests cited by D-222 item 5 (`infra/engine/errors.rs`), D-223 item 6 (the edge-limiter `kind` tests) and D-226 item 8 (`break_glass_is_never_honored_for_a_service_principal`) ran and passed. `make test-integration LANE=orders-pg` (`cargo nextest run -p cf-gears-bss-orders --run-ignored ignored-only --no-fail-fast`, Docker): 100 run, 100 passed, also on the first run. `cargo test -p cf-gears-bss-orders --lib --no-run` compiles with no error, and `cargo clippy -p cf-gears-bss-orders --all-targets --all-features -- -D warnings` is clean; with `RUSTFLAGS="--cfg orders_event_broker_test_support"` the lib test target fails with exactly the four #5381 errors.

## D-230 — Event schema reason patterns use `\xHH` escapes (GTS 0.16 regular-expression profile)

**Status:** selected 2026-10-23 while merging `main` into PR #5259. Pending design review. Changes the published text of four event schema fields, not the set of values they accept.

1. **The problem.** `main` moved the workspace to `gts` 0.16 (from 0.12), whose types registry checks every schema `pattern` against the GTS safe regular-expression profile (gts-spec §11.0.1). The profile accepts `\xHH` but refuses `\u` escapes. The caller-text pattern of the Orders events (`cancelReason`, `forcedReason`, `holdReason`, `denialReason`; `infra/events/schema.rs` `reason()`) was written with `\u` escapes, so the registry refused the four `cf.bss.orders.*` event schemas at its ready commit. Orders registers them at init, so the gear would not start on current `main`; the unit test `the_real_registry_commits_and_resolves_the_event_contract` failed with `ReadyCommitFailed … unsupported escape '\u'`.
2. **The fix.** The pattern is now `^[^\x00-\x08\x0B\x0C\x0E-\x1F\x7F-\x9F]+$`. Every excluded code point is at most U+009F, so the two spellings name the same class: compared over every Unicode scalar value, they accept exactly the same characters. The rule is unchanged: caller text has no control characters other than tab, CR and LF (the audit `caller_reason` rule; `payload.rs` `reason_text` enforces it in code before publication).
3. **Effect.** The published schema JSON of those four fields changes text, not meaning; consumers validating against it accept and refuse the same events. No payload, wire or stored data changes.
4. **Evidence.** `cargo test -p cf-gears-bss-orders --lib`: 165 passed, 0 failed, including the real-registry contract test.

## D-231 — Stage 3 port inventory, typed unavailability and the S3-F01/F02/F08 reconciliation

**Status:** selected 2026-10-22 by the Stage 3 implementation (S3-01). Pending design review. No REST wire, stored data or SDK change.

1. **The outbound inventory (F03 §3.3, DESIGN `interface-gate-ports`, D-187…D-199).** One row per logical operation; the Rust `GatePort` enum encodes this table verbatim and `orders/tests/gate_ports.rs` asserts it.

   | Port | SDK method | Status on 2026-10-22 | Feeds | Unavailable reason | Missing → | Deadline |
   |---|---|---|---|---|---|---|
   | current-revision | `PricingReadV1::current_revision` | exists (main) | PricingRevision | `pricing-revision-unavailable` | grants: `…-upreq-commercial-service-provisioning`, `…-upreq-pricing-catalog-tenant-reads` | 250 ms |
   | resolve | `PricingReadV1::resolve` | exists | CatalogPredicates, PinComposition | `catalog-predicates-unavailable` (predicates), `catalog-pin-composition-unavailable` (pin) | same grants | 250 ms |
   | sku-read | `ProductsClient::get_sku` | exists | CatalogPredicates | `catalog-predicates-unavailable` | `…-upreq-products-sku-read-grant` | 250 ms |
   | assessment | `PricingAssessmentV1::assess` (D-189) | **missing** | CatalogPredicates | `catalog-predicates-unavailable` | S3-03; `…-upreq-pricing-purchase-assessment`, `…-upreq-pricing-bundle-sellability` | 250 ms (unratified, Q7) |
   | billing-terms | Subscriptions resolver (D-193) | **missing** | PinComposition | `catalog-pin-composition-unavailable` | S3-04; `…-upreq-pre-subscription-billing-terms` | 250 ms (unratified) |
   | acceptance-policy | Pricing policy discovery (D-191) | **missing** | PinComposition | answered-missing: `order-binding-policy-missing`; unavailable: `catalog-pin-composition-unavailable` | S3-05; `…-upreq-pricing-acceptance-policy` | 250 ms (unratified) |
   | identity-profile | AM payer profile | **missing** (`get_tenant` only) | Identity | `identity-party-unavailable` | S4-02; `…-upreq-payer-commercial-profile` | 250 ms |
   | contract-resolution | Contracts batched status/eligibility | **missing** | ContractResolution | `contract-resolution-unavailable` | S4-02; `…-upreq-contract-party-eligibility` | 250 ms |
   | overlap-key | Subscriptions SUB-G1 keys | **missing** | OverlapKey | `overlap-key-unavailable` | S5-06a; `…-upreq-catalog-subscription-product-key` | 250 ms |
   | occupancy | Subscriptions SUB-O5 | **missing** | Occupancy | `overlap-presence-unevaluable` | S5-06a; `…-upreq-overlap-presence-read` | 250 ms |
   | evaluation | Rating exact-binding evaluation (D-197) | **missing** | Evaluation | `evaluation-unavailable` | S3-07; `…-upreq-rating-evaluation` | 500 ms |
   | indicative-tax | unowned (D-168) | **missing** | none (Preview only) | `indicative-tax-unavailable` | S3-08; `…-upreq-indicative-tax-read` (p2) | 250 ms |

   The rule: a port that feeds two engine inputs reports both unresolvable (D-113 lets the engine pick the earliest-registered). The pin outcome of a line whose resolve failed is `unevaluable(catalog-pin-composition-unavailable)` and the adopted predicates of that line are `unevaluable(catalog-predicates-unavailable)`; neither duplicates the other in the failure list (F03 step 11).
2. **Typed unavailability.** Every port answer is `Answered(T)` or `Unavailable{Outage, Denied, Deadline, Malformed, Missing}`; `Missing` is the production answer of an undelivered provider. Every `Unavailable` kind maps to the reason of each engine input the port feeds (item 1's "Feeds" column; `resolve` feeds two inputs, so it carries `catalog-predicates-unavailable` and `catalog-pin-composition-unavailable`, amended 2026-10-22); the kind is operator diagnostics only (log field `orders.gate.unavailable_kind`, item 9), never public.
3. **S3-F02.** DESIGN's port-budget prose is narrowed: reads are the typed `PricingReadV1` (D-187), not REST; `current_revision` uses the Pricing clock and may promote (D-187); D-189, not command availability, retires the interim `get_sku` read.
4. **S3-F08 supported purchase mapping.** One line is one subscription acquisition (`plan_id`, `plan_revision_id`, `selected_items[]{item_id, quantity, selected_dim_value}`); `quantity` is the exact positive decimal sent as `NewSaleQuery.quantity` (D-192); there is no line-level multiplier; bundles stay one line; unsupported shapes refuse `reference-unresolvable` at assessment and are never widened locally (Q-03 stays open).
5. **S3-F01 budgets.** The F03 §3.3 values are recorded as unratified baselines; config `gate.deadline_ms` may override per port and `gate.authority_exchange_deadline_ms` the one `AuthN` exchange per catalog read (250 ms baseline); the 2.25 s / 2.5 s ceilings are measured, not promised. The 200-subrequest and 8-concurrent caps are per port per run (DESIGN L7005, F03 §3.3; amended 2026-10-22): a ninth concurrent call on one port waits for a slot until the port's run deadline, while the gear-wide 32-per-port bulkhead never queues (backpressure).
6. **Blocking rule by operation (D-195).** `tcv-only` and `preview-forecast` outcomes are non-blocking only for Preview; on submit and amendment `preview-forecast` outcomes are not produced and `tcv-only` unevaluability blocks (TCV is required, D-197).
7. **Prerequisites.** Decided by the user: Q1 `payer-relationship-unconfirmed` is a gate predicate result, FailedPrecondition/400, registered in `catalog.json` by a later task (D-232); Q2 is one new optional `RefusalDiagnostics.assessment` member (later D-233); Q13 is `AuthNResolverClient::exchange_client_credentials` with per-seller `commercial_identities[]` configuration, the D-194 S2S path. Still open: Q6 (shared breaker) and Q7 (budget ratification).
8. **Evidence.** `orders/tests/gate_ports.rs` asserts the table (Task 2).
9. **Catalog-read vocabulary and precedence (added 2026-10-22).** `catalog-predicates-unavailable` is the reason when a port gave **no answer**: an outage, a denial, a deadline, a malformed envelope or answer (including a `resolve` answer for another plan or revision, and a `current_revision` failure other than `PLAN_UNPUBLISHED`), or a missing provider. `catalog-predicate-unevaluable` is the reason when a port **answered** but the answer cannot evaluate the predicate (an unknown `PriceState`, an answered SKU of another id, the slot predicates after `resolve` answered `REVISION_NOT_PUBLISHED`, `REVISION_NOT_YET_AVAILABLE` or `INCOMPLETE_COMMERCIAL_INPUTS`). The engine inputs follow the same view: `pricing_map::normalise` is the one normalisation the mapping and the assessment's input resolution share, so an answer the mapping treats as unavailable also marks the inputs that port feeds unresolvable. Precedence when the two Pricing ports disagree: when `current_revision` is unavailable and `resolve` answers "revision absent", predicate 1 reports `pricing-revision-unavailable` and its dependents carry `pricing-revision-absent`; when `current_revision` answers `PLAN_UNPUBLISHED` and `resolve` is unavailable, predicate 1 reports the business failure (`catalog-predicate-failed`, `upstream_code` `PLAN_UNPUBLISHED`) and its dependents carry `catalog-predicates-unavailable`. The `orders.gate.unavailable_kind` log field lands with S3-11 (the orchestrator); nothing emits it yet. Evidence: `orders/tests/gate_pricing_map.rs`, `orders/tests/gate_assess.rs` `malformed_catalog_answer_marks_the_fed_inputs_unresolvable`.

## D-232 — The payer relationship refusal is a gate predicate result

**Status:** selected 2026-10-22 by the user (Q1). Pending design review. Wire change: one new registered reason.

1. **The decision.** An unconfirmed payer ↔ seller or payer ↔ resource-tenant relationship (D-215 item 2, D-128) is a result of Orders gate predicate 1, axis validity (F03 §4.2), owned by the sellability composite guard (`gate.sellability-composite`). It refuses the new reason `payer-relationship-unconfirmed`, category FailedPrecondition, HTTP 400: the request is well formed, but the tenants' commercial relationship does not admit it, the same class as `contract-party-ineligible` and `market-inconsistent`. Preview, submit and amendment report it; an unavailable or late payer profile stays `identity-party-unavailable`. This fulfils D-215 item 2 ("registered when the Stage 3 gate implements the check"). Rejected: keeping it as `axis-invalid` with an `upstream_code` (the interim of S3-12 rule (e)), because a client cannot tell a malformed axis from a relationship that a commercial change could establish, and `upstream_code` is diagnostics, not a decodable reason; InvalidArgument, because the request itself is valid.
2. **What changes on the wire.** One new registered reason, decoded like every other by `(error_domain, error_code)` (D-219): `(orders.v1, PAYER_RELATIONSHIP_UNCONFIRMED)`, GTS key `gts.cf.bss.orders.err.v1~cf.bss.orders.payer_relationship_unconfirmed.v1~`, Problem type `gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~`, title "Failed precondition", status 400. `catalog.json` `errors[]` grows from 90 to 91 (entry after `axis-invalid`); `orders-sdk/src/catalog.rs` and `orders/src/api/rest/wire.rs` are regenerated by `generate_catalog.py`. No existing reason, status or body changes. The gate's rule (e) now returns `failed(payer-relationship-unconfirmed)` with no upstream detail; `PredicateId::AxisValidity` lists both failure reasons; the guard registry lists the reason under `GATE_COMPOSITE`; the GTS bootstrap registers it with the other reason identities (91).
3. **Golden, fixtures and counts.** `orders/tests/fixtures/rest-problem-golden.json` gains one boundary and one settled entry for the reason, written from its catalog row in the shape of the captured FailedPrecondition/400 entries (the capture of D-219 item 6 predates the reason; its comment says so). The canonical round-trip measurement of D-219 item 5 becomes 71 of 91 rejected (the new reason is a failed-precondition context, so it is rejected; the 20 that convert are unchanged). `boundary-fixtures.json` enumerates no full reason list and is unchanged. DESIGN's reason table, the gate's registry contribution and the payer-relationship rule, F03's predicate 1 row and the README counts (91 reason mappings and identities) are updated.
4. **Workflow mirror.** Orders Workflow (#4787, `bss/orders-workflow`) must add the reason wherever it enumerates or maps Orders refusal reasons, so a step that receives `(orders.v1, PAYER_RELATIONSHIP_UNCONFIRMED)` decodes it rather than treating it as unknown. Not done on this branch.
5. **Evidence.** `orders/tests/rest_problems.rs` asserts the reason decodes from its token with code `PAYER_RELATIONSHIP_UNCONFIRMED` and status 400, compares its boundary and settled bodies with the golden, round-trips it through the SDK error, and measures 71 of 91; `the_rest_wire_bindings_match_the_reviewed_catalog` and the startup checks (`gts::validate_catalog`, the guard registry) pass with it. `orders/tests/gate_assess.rs` `e_axis_validity_reads_the_payer_profile` asserts the unconfirmed seller or resource relationship fails predicate 1 with the reason and no upstream detail. The gear-boot census (GTS acknowledges exactly the requested identifiers) is in the Postgres tier and is type-checked only on this branch until the split-out dependency PRs merge.
6. **Withholding (added 2026-10-22; D-215 item 2, D-113).** While the payer profile answers with either relationship unconfirmed, no payer fact is resolved or disclosed and no payer-dependent port decides the primary: `orders.contract-active`, `orders.market-consistency`, `orders.overlap-uniqueness`, `orders.in-flight-per-key` and every composed `orders.pin-composition` row (its billing terms are withheld: the terms query carries the payer and the contract, so the orchestrator does not ask it, D-215 item 2) are `unevaluable(payer-relationship-unconfirmed)` for every applicable scope (the withheld rows carry predicate 1's reason, `axis-invalid` when the axes are also invalid and `payer-relationship-unconfirmed` otherwise, so the failure list holds one entry per order and per line), the contract, overlap-key, occupancy and billing-terms answers are ignored (unasked terms are withheld, not a down port: the pin row is `required`, never tcv-only, and never `catalog-pin-composition-unavailable`; a pin that is itself unresolvable or unevaluable keeps its catalog reason), and the `ContractResolution`, `OverlapKey`, `Occupancy`, `PinComposition` and `Evaluation` engine inputs are `Resolved`: the composite gate resolves them by withholding and its own predicate carries predicate 1's failure (predicate 1 lives inside the composite gate, the last registered guard, so D-113 `Precluded` does not apply: it is reserved for inputs an earlier-registered guard makes moot, and the engine refuses to reach a guard with one). The override wins over a down `Resolve`, `AcceptancePolicy` or evaluation port, which would otherwise mark the input `Unresolvable` (`Resolve` and `AcceptancePolicy` still feed `PinComposition` and are still asked). No payer-dependent port decides the primary of Preview, submit and amendment. Resolved by D-234: the withheld rows and predicate 1 outrank catalog unavailability in primary selection. The orchestrator must not call the contract, overlap-key, occupancy or billing-terms ports before the relationship is confirmed (documented on `ContractReads`, `OverlapKeyReads`, `OccupancyReads` and the orchestrator, M2-2). Evidence: `orders/tests/gate_assess.rs` `unconfirmed_payer_relationship_withholds_payer_dependent_rows` and `unconfirmed_relationship_with_invalid_axes_withholds_under_axis_invalid` (pin rows and `PinComposition` included) and `confirmed_payer_with_terms_unavailable_keeps_the_pin_port_reason` (the confirmed terms outage keeps `catalog-pin-composition-unavailable`), `orders/tests/gate_orchestrate.rs` `a_port_not_asked_is_missing_and_unconfirmed_payer_inputs_are_resolved_by_withholding`; at the engine, `orders/tests/gate_assess.rs` `engine_settles_unconfirmed_payer_as_the_gate_refusal` and `engine_settles_invalid_axes_as_the_gate_refusal` (D-234 item 6).

## D-233 — The all-failures gate report is one optional member of the refusal diagnostics

**Status:** selected 2026-10-22 by the user (Q2). Pending design review. Wire change: one new optional context.data member.

1. **Shape.** DESIGN L5093 ("For an all-failures response, retain the complete authorized report in `context.data`") gets one typed member, `assessment: Option<Box<GateReport>>`, on the SDK `RefusalDiagnostics` (`orders-sdk/src/errors.rs`; boxed so the report does not grow every `OrdersError`, clippy `result_large_err`). `GateReport` in the new SDK module `orders-sdk/src/gate_report.rs`: `assessment_id` (the stored gate assessment), `mapping_version` (D-195, `orders-gate-mapping/1` today), `outcomes: Vec<GateOutcome>` and `failures: Vec<GateFailure>`. `GateOutcome` carries the public fields of one gear vector row (`predicate`, `line_id`, `item_id`, `has_catalog_selection`, `catalog_scope_key`, `verdict`, `reason`, `applicability`) and `GateFailure` the gear's `Failure` (`line_id`, `reason`, `predicate`); every enum is its stored token, and producer results and upstream evidence are never in it (F03 §3.1 public projection). The gear builds it with `Assessment::report(assessment_id)` (`orders/src/domain/gate/assess/mod.rs`), mapping the vector's outcomes and the failure list 1:1 (amended 2026-10-22: the gear's separate `PublicOutcome`/`Vector::public()` projection is gone; `report()` reads the vector directly). The three types are `#[non_exhaustive]` with constructors (`GateReport::new`, `GateOutcome::new(..).with_scope(..).with_reason(..)`, `GateFailure::new`): consumers decode them and never build them. Rejected: a second Problem extension or a separate endpoint, because the DESIGN puts the report in `context.data` and every other diagnostic is a typed `RefusalDiagnostics` member already (D-219); an untyped `serde_json::Value`, because a caller would re-implement the decoder.
2. **When present.** Every composite-gate refusal of a submit or amendment (`gate.sellability-composite` reasons: catalog, axis, contract, overlap, date and pin results) carries it; the outer Problem stays the primary blocking result selected per D-234 then D-195 (the first failed definitive local refusal, otherwise the first unevaluable, otherwise the first failed), so status, `error_code` and every existing body are unchanged. Non-gate refusals never carry it. Producing it at settlement lands with the engine's gate integration; this decision fixes the wire and the conversion.
3. **Decoding rules.** The member and its optional fields are `#[serde(default, skip_serializing_if = "Option::is_none")]`: a payload from an Orders that predates D-233 decodes with `assessment = None`, and an absent `line_id`/`item_id`/`catalog_scope_key`/`reason` stays absent on re-encode. `predicate`, `verdict` and `applicability` are open strings, so a newer Orders may add a predicate or a token without breaking older decoders; `reason` is the closed `Reason` (user decision 2026-10-23). An unknown reason inside `assessment` fails the whole diagnostics decode (D-219 item 4: `Integration`), so consumers such as Workflow #4787 must take every new registered reason in lockstep. The outer refusal's own reason stays the closed `(error_domain, error_code)` decode of D-219. The REST encoder needs no change: `api/rest/error.rs` serializes `RefusalDiagnostics` through serde, so the new member is encoded and decoded generically.
4. **Golden and fixtures.** `orders/tests/fixtures/rest-problem-golden.json` gains one settled `payer-relationship-unconfirmed` entry whose `context.data` carries an `assessment` (three outcomes, two failures), written by hand in the shape of the captured settled entries; its `_comment` and the `rest_problems.rs` header say so. The reason set and every count are unchanged. `boundary-fixtures.json` enumerates no `context.data` members (its Problems carry `{}`) and is unchanged. DESIGN's refusal-reasons section names `assessment` as a permitted `context.data` member next to `request_audit_id`; `catalog.json` embeds none of that text and is unchanged (`generate_catalog.py --check` passes).
5. **Workflow mirror.** Orders Workflow (#4787, `bss/orders-workflow`) must add the `assessment` member (and the `GateReport` types) to its refusal-diagnostics decoder so a step receiving a composite-gate refusal keeps the report. Not done on this branch.
6. **Evidence.** `orders-sdk/tests/contracts.rs` `refusal_diagnostics_carry_the_optional_gate_report` (round-trip under the wire names, an older payload decodes with `None`, an unknown reason token in an outcome or a failure fails); `orders/tests/gate_assess.rs` `report_is_the_public_vector_and_failure_list_one_to_one` (the vector's stored tokens, failures 1:1, `MAPPING_VERSION`, survives the SDK diagnostics); `orders/tests/rest_problems.rs` compares the new settled entry byte for byte, decodes it into `OrdersError::Settled` with the typed report and re-encodes the same bytes.

**Amended 2026-10-26 (milestone 3b, D-238 item 6b).** Item 2 is now implemented and tightened: the stored Problem of a composite-gate refusal carries `context.data.assessment`, so a same-key replay carries it too. **Amended 2026-10-27 by D-240 item 4 (Q-A):** `order-in-flight-for-key` is shared by the composite gate and the authoritative step-17 claim guard. Both refusals carry `assessment`; step 17 also carries `blocked` and replaces predicate 9 in the stored diagnostics and response snapshot before settlement. The report is stored in the Problem body and also in the stored response's top-level `assessment` field; the duplication is accepted, and only the body copy is read (`api/local_client.rs` `sdk_result` decodes it from the body).

## D-234 — Definitive local refusals outrank unavailability in the gate's primary

**Status:** selected 2026-10-23 by the user. Pending design review. No wire change: the outer Problem's reason is chosen differently; the report is unchanged.

1. **The rule.** Primary Problem selection considers only blocking outcomes (D-195, D-231 item 6), in declared predicate order, in three steps: (1) the first **failed** outcome of a definitive local predicate, one whose verdict is final whatever any provider answers: `orders.axis-validity` (`axis-invalid`, `payer-relationship-unconfirmed`), `orders.reference-resolution`, `orders.single-currency`, `orders.quantity-floor` and `orders.dates-resolvable`; the payer-dependent rows withheld under D-232 item 6 carry predicate 1's reason as unevaluable and rank with it, so an outcome whose reason is `axis-invalid` or `payer-relationship-unconfirmed` is a definitive local refusal too; (2) otherwise the first unevaluable outcome (D-195 unchanged); (3) otherwise the first failed outcome. A buyer with an unconfirmed payer relationship therefore gets 400 `payer-relationship-unconfirmed`, not 503 `catalog-predicates-unavailable`, even when the D-189 assessment port is Missing or a catalog port is down. `PredicateSpec::definitive_local` (`orders/src/domain/gate/predicates.rs`) marks the five predicates; `Vector::primary` (`orders/src/domain/gate/vector.rs`) implements the steps.
2. **Why.** A 503 tells the buyer to retry later; retrying never clears these refusals, which need a buyer action (a valid axis, a confirmed relationship, a resolvable reference, one currency, a positive quantity, resolvable dates). Reporting unavailability first sends the buyer into a retry loop that ends in the same refusal. D-113 applies the same principle between guards: an unavailable input never masks an earlier-registered failing guard; D-234 applies it between gate predicates.
3. **What stays.** Catalog and provider-backed rows (the eight adopted predicates, contract activity, market consistency, overlap uniqueness, the in-flight check and the pin) keep the D-195 order: a failed provider-backed row never outranks an unevaluable one, and with no definitive local refusal the primary is the first unevaluable blocking outcome, otherwise the first failed. The all-failures report (D-233) and the failure list are unchanged; only the outer Problem's reason, status and `error_code` can differ.
4. **Preview `tcvWithheld`.** Untouched: `tcv-only` and `preview-forecast` outcomes still never block a Preview (D-195, D-231 item 6), so they never enter primary selection and a Preview that D-125/D-197 withholds TCV for still succeeds.
5. **Evidence.** `orders/tests/gate_vector.rs`: `d230_failed_local_refusal_outranks_an_earlier_unevaluable_catalog_row` (a failed quantity floor on line 2 beats an unevaluable `catalog.revision-available` on line 1), `d230_failed_catalog_row_keeps_the_d195_order`, `d230_withheld_row_with_predicate_1_reason_outranks_catalog_unavailability`, `definitive_local_flags_exactly_the_five_d230_predicates`, and `primary_is_first_unavailable_in_the_blocking_set_else_first_failed` (no local refusal: D-195 holds). `orders/tests/gate_assess.rs`: `unconfirmed_payer_relationship_withholds_payer_dependent_rows` and `unconfirmed_relationship_with_invalid_axes_withholds_under_axis_invalid` assert `primary(Submit)` and `primary(Preview)` are `payer-relationship-unconfirmed` / `axis-invalid` with the assessment port `Unavailable(Missing)` and a catalog port down; `provider_outages_alone_keep_the_unavailable_primary` keeps the 503 port reason; `k_failures_are_deduplicated_by_line_and_reason_in_vector_order` now selects the failed `orders.reference-resolution` over the earlier unevaluable catalog row. DESIGN L5093 (all-failures report), the R09 diagnostic mapping (and its copy in `orders-sdk/contracts/catalog.json`) and F03 §3.4 step 4 state the rule.
6. **Engine presentation (added 2026-10-23).** When the primary is a definitive local refusal, every composite-gate input is presented `Resolved` so the engine settles the gate's refusal rather than an unresolvable input's 503 (D-113 order applies between guards; inside the gate D-234 applies). Without it the engine, which checks a guard's unresolvable inputs before its predicate, would answer item 1's own example (an unconfirmed payer with a catalog port down) with `catalog-predicates-unavailable`. A primary that is not a definitive local refusal keeps the per-port `Unresolvable` marking (D-231 item 1), so a provider outage alone still settles `Unevaluable`. `inputs` in `orders/src/domain/gate/assess/mod.rs` implements it. Evidence: `gate_assess.rs` engine-level tests (`engine_settles_unconfirmed_payer_as_the_gate_refusal`, `engine_settles_invalid_axes_as_the_gate_refusal`, `engine_settles_a_confirmed_catalog_outage_as_unevaluable`) and `confirmed_payer_catalog_outage_keeps_catalog_predicates_unresolvable`.

## D-235 — Milestone 2: development delivery, engine diagnostics hook, submit protocol bindings

**Status:** selected 2026-10-23 by the Stage 3 implementation (milestone 2). Pending design review. Wire changes: `OrdersV1::preview` (SDK method), the Preview types; no new reason.

1. **Development delivery switch (M2-1, M2-3).** Preview and submit are delivered only when the `gate-dev-stubs` cargo feature is compiled **and** `gate.dev_stubs = true` (`gear.rs` `gate_dev_delivery`: `cfg!(feature = "gate-dev-stubs") && config.dev_stubs`; config validation already refuses the flag in a build without the feature). Under the switch the local `OrdersV1::preview`/`submit` delegate to `PreviewService`/`SubmitService` and `api/rest/gate.rs` mounts `POST /bss-orders/v1/orders/preview`. Otherwise nothing changes from milestone 1: `preview` and `submit` answer `NotDelivered` (`Unavailable`), the Preview route is unmounted and `api::rest::DELIVERED_OPERATIONS` is unchanged. The example server's `bss-orders` feature never enables `gate-dev-stubs`, readiness keeps reporting every stubbed port as `stubbed (development only)`, and stubbed answers are never evidence of a delivered producer. Per-caller limiters (`gate.preview_per_caller_per_minute` default 60, `gate.submit_per_caller_per_minute` default 10, both `1..=600`) guard the two entries.
2. **Engine diagnostics hook (M2-2/M2-4).** `Prepared.diagnostics: Option<Arc<dyn DiagnosticsContribution>>` (`infra/engine/documents.rs`, `infra/engine/mod.rs`). The engine calls it inside the transition transaction: on a gate refusal (`Refusal.assessment`, set for the composite gate), on an overlap conflict (`conflict_refusal` goes through `refuse_settled`) before refusal settlement and audit, and on admission after the documents step and before the aggregate write and sealed audit, with the settled version (the candidate for a submit). It is never called for engine-only refusals (not-admissible, version conflict, precondition failure, resume-target-missing) or for a non-gate guard such as `capture.date-basis`. The engine, not the slice, supplies the subject identity: `DiagnosticsSettled` carries `subject_tenant_id` and `subject_id` from the caller's `SecurityContext`, and the write scope is `AccessScope::for_tenant(subject_tenant_id)` built by the engine (`gate_outcome` declares no PEP properties, so the order's PDP scope cannot resolve on it). `GateRun` (`infra/gate/writer.rs`) is the one implementation. No engine step was reordered and no `FaultPoint` was added; every other caller builds `Prepared` with `diagnostics = None`.
3. **E1 to E4 (M2-6, engine).** (E1) A new execution re-runs `transition::admit` under the reservation lock, after the registry claim and before the attempt is begun: state admissibility, expected version, and the client and prepared draft revisions equal to the locked one; a refusal rolls back the claim and answers unsettled with unresolved evidence. (E2) `Engine::recover_commercial_attempt` finds a settled key (replay), a reserved attempt (`Existing`) or a miss, **before any assessment**; a miss and a replay write nothing; resuming an existing attempt goes through the authoritative registry gate under the aggregate lock (a lapsed lease is reclaimed with a rotated owner and fence, a live one answers `still-processing`), because resuming under the probe's stale owner would bypass the fence. A live record of the key under another request fingerprint is `Probe::Mismatch` and answers the unsettled `idempotency-mismatch` before the gate or the date basis runs; an expired record or one bound to another order stays a miss. (E3) `check_refusal_owner` makes a non-commercial refusal never settle over a recovered `prepared`/`running` attempt (`Abort::Integration`, nothing written); a phase-C refusal names the reserved candidate and so settles the attempt `refused` and burns the candidate, as D-188 requires. (E4) Submit's fingerprint document is `{}` (the draft revision is already a fingerprint input, as capture's documents never repeat it). `TransitionRequest::with_execution(owner)` carries the owner into phase C.
4. **Frozen attempt members beyond D-188's list.** D-188 names the canonical per-line Pricing requests, their hashes and stable command keys, and the captured date and policy basis. This delivery also freezes, in the attempt's `line_requests` under its `"order"` member (`FrozenOrder`): the run's assessment identity and instant, the Rating evaluation, the authorized gate report, the request correlation and the complete outcome vector (`Vector`, re-validated on decode), plus each line's overlap scope key. Why: recovery settles the same evidence. The commit builds its settled report and its diagnostics run (`GateRun`) only from this member, so a recovered same-key commit settles the same report and writes the same gate rows (naming the committed version) as the commit of the call that assessed, without re-running the gate or a catalog read; this is the "non-receipt assessment evidence" D-188 step 1 lets only a new execution prepare. No column is added.
5. **`ReceiptRecorder` is an engine entry.** The per-line receipt transaction locks the authorized aggregate and verifies the attempt's fence. `tests/engine_single_writer.rs` reserves `lock_authorized(` to the engine and storage, so the recorder is `Engine::receipt_recorder(caller, request)` (authorized once per phase B, denial evidence as every path) and `ReceiptRecorder::record` (one short transaction per line, `reg::record_receipt`; a moved fence is `StaleOwner` and the owner stops; a different first result fails closed; a `line_key` that is not one of the attempt's frozen lines, or is `"order"`, is an integration failure). It is engine code beyond the E-list and adds no step to `transition`. Each new result renews the owner's lease on the marker (`lease_expires_at`) and the attempt (`lease_until`) together, to the database time plus the configured lease, without rotating owner or fence; an identical repeat writes nothing. A line records `{"accepted": <receipt, schema-2 spelling>}` or `{"refused": {"reason": "<Pricing code>"}}` for the seven `aborted` reasons that describe the sale (`NOT_SELLABLE`, `RESOLUTION_CHANGED`, `ACTIVATION_OUTSIDE_ACCEPTED_WINDOW`, `HOLD_EXPIRED`, `PRICE_CLOSED`, `SKU_RETIRED`, `MARKET_CHANGED`) and the eight terminal term refusals listed in item 7 (M3C Task 3 amendment). Acceptance budget: `gate.acceptance_deadline_ms` (absent = 500): one deadline per line command covering both attempts, one retry for a transient failure while time remains, cancel-aware, a 32-permit gear-wide bulkhead taken without queueing; it is not a gate port and `GatePort::ALL` is unchanged. `SellabilityV1::check` is named only by `infra/gate/submit.rs` (phase B) and the development stub (a grep test pins it); Preview never reaches phase B.
6. **Money (D-216, D-213, D-188).** `resolved_total.*_exact` must be canonical decimal text (`bss_orders__decimal_valid(..., 28)`, `ck_bss_orders__resolved_total__9`) and Rating's exact amount is a fraction (`ExactMoney`, #5390). Submit renders it with the platform money library: `bss_money::exact::ExactAmount::from_fraction` (an **additive** constructor in `gears/bss/libs/money/src/exact.rs`; it takes no currency, because `ExactAmount` carries none) and `canonical_at_scale`, so `21/2` is stored as `10.5`. `*_exact` is rendered at scale 28; `*_display` at the currency scale through the same function (`10.50` is stored as `10.5`). A fraction that does not terminate within 28 places (`1/3`), text beyond 28 significant digits, or a display finer than the currency scale cannot be stored and is never rounded: the evaluation is unusable and the gate refuses `evaluation-unavailable` (before reserving in phase A; in the commit for a recovered attempt). Displays are normalised per D-213 (no trailing zeros). This is formatting, not arithmetic: Orders still sums, converts and rounds nothing. An evaluation in another currency than the frozen market is a producer contract defect (`Integration`). Usage rows: a usage-flagged line also states `LineEvaluation.usage_declaration`, and `OrderEvaluation.order_usage_declaration` is required whenever a line has usage; per line and for the roll-up submit writes the committed component under its declared kind (`committed`) and a `usage` row with `amount_status = uncommitted_usage`, NULL gross/net, an unavailable discount and the usage declaration (its `exclusions` included) copied verbatim (`ck_bss_orders__resolved_total__5`); the TCV rides only on the order/recurring row. **Producer-declared totals members are required and never derived** (DESIGN §3.7: "Orders neither derives a horizon nor converts cycles"): `TotalsDeclaration` (charge kind, `amount_basis`, `period_evidence`, `item_breakdown`, `recurring_by_cycle`, `exclusions`) on `LineEvaluation.declaration` and `OrderEvaluation.order_declaration`, and `Tcv.basis`/`Tcv.evidence`. They are optional serde members of the mirror; an evaluation without them, usage charges without the usage declaration, or the reverse, is `evaluation-unavailable` for submit. **Pending Rating sign-off (#5175)** on all of these members, the usage declarations and the display normalisation. The development fixture states cents (`21/2`, display `10.50`; TCV `126`, display `126.00`), and Preview copies them verbatim.
7. **Refusal mapping and exhaustion.** A recorded `NOT_SELLABLE`, `RESOLUTION_CHANGED`, `MARKET_CHANGED`, `SKU_RETIRED` or `PRICE_CLOSED` fails the composite gate `catalog-predicate-failed`. `HOLD_EXPIRED` and `ACTIVATION_OUTSIDE_ACCEPTED_WINDOW` fail `gate.binding-admission` `order-binding-expired` (the accepted binding no longer admits the order; the composite gate does not register that reason). A receipt whose exclusive `hold_until` is not after the transition time fails `gate.binding-admission` `order-binding-expired`; a version whose persisted bytes exceed 1 MiB fails it `purchase-capacity-exceeded` (bytes counted column by column in `domain::gate::commercial::persisted`: binary width for fixed-width types, UTF-8 length for text, serialized length for `jsonb`, over every admitted line row with its pin, every `resolved_total` row and the `order_version` row). A receipt that does not verify against the frozen request (D-192) is an integration failure. Pricing contract defects fail closed: `IDEMPOTENCY_CONFLICT` and `ACCEPTANCE_MISMATCH` (the frozen key met another request or acceptance, impossible between a correct Orders and Pricing) are not recorded and answer `Integration`, never a settled `catalog-predicates-unavailable`; infrastructure failures and permission denials are likewise never recorded. **M3C Task 3 amendment (Q-B option a):** the exact terminal term refusals `UNSUPPORTED_TERMS`, `UNSUPPORTED_MODEL`, `UNSUPPORTED_WINDOW`, `UNSUPPORTED_SCOPE`, `MISSING_BILLING_TERMS`, `BILLING_CYCLE_MISMATCH`, `UNALIGNED_BILLING_ANCHOR` and `TERM_COUNT_INVALID`, decoded from Pricing's InvalidArgument `commercial_terms` field violation, are recorded and settle through the composite gate as `billing-terms-unsupported`. The attempt becomes refused and its candidate stays burned; an authorized same-key retry replays the settled refusal. Digest, money, currency, market, selection and all other invalid-argument defects remain unrecorded `Integration` failures. `UNSUPPORTED_TERMS` may reflect a policy race (D-191), but is never automatically retried as policy drift; fresh intent requires a new execution/candidate. **Candidate exhaustion** (`version_allocation_high_water` at `i32::MAX`) aborts the reservation before any Pricing command and answers, unsettled, the existing canonical resource-exhaustion Problem D-188 names: `resource_exhausted` (429) on the Orders order resource with the quota violation subject `version-allocation` and no retry hint, built as the edge limiter builds its refusals (`api::rest::throttle::version_allocation_exhausted`). It is an engine error mapping (`EngineError::VersionAllocationExhausted`), not a registered reason.
8. **Preview (M2-1, M2-3).** `OrdersV1::preview(ctx, PreviewBasket, ReadMeta) -> PreviewResult` (no idempotency, `orders-sdk/src/preview.rs`). The SDK refuses an empty basket (`request-invalid`) and over 200 lines, and reuses `AddLine` validation per line. A line lacking an authored term or billing cycle makes the result carry `tcv_withheld` (serde `tcvWithheld`, `reason: "preview-term-or-cycle-missing"`, `lineIds`) and no `tcv` (D-125); TCV-only lines carry the marker and a Preview that D-125/D-197 withholds TCV for still succeeds (D-234 item 4). Tax lines are named objects with one assessment id; unknown enum kinds are tolerated on results. Every money view is text copied verbatim. A Preview writes its run standalone (`run_id == assessment_id`, under the subject-tenant scope) and no audit row (it is not a transition); a denial writes none either.
9. **`OrderSubmitted.acceptance: None` (carried).** The committed `OrderSubmitted` event leaves `acceptance` as `None` (`infra/gate/commit.rs`): D-146 automatic acceptance is not wired by milestone 2 and is carried to the acceptance-policy and acceptance-recording slices (S4-07/08a). S3-05 is Pricing acceptance-policy discovery for commercial bindings; it does not deliver D-146 automatic acceptance.
10. **Workflow mirror (#4787, `bss/orders-workflow`).** Not done on this branch. M3C Task 7 adds optional `ResolvedTotalView.exclusions` (producer order declaration, verbatim JSON), `PreviewResult.tcv_basis` (producer token; absent when TCV is unavailable or withheld), and `TransitionResult.assessment_id` (successful submit frozen run identity, preserved by replay); mirror these in the corresponding Workflow decoders. M3C Task 2 adds the registered `billing-terms-unsupported` / `BILLING_TERMS_UNSUPPORTED` refusal reason (FailedPrecondition, HTTP 400); mirror it in both the outer refusal and the gate-report reason decoder. Orders Workflow must mirror: the new `OrdersV1::preview` method and the Preview types (`PreviewBasket`, `PreviewLine`, `PreviewResult` and its members, `tcvWithheld`); the D-232 reason `PAYER_RELATIONSHIP_UNCONFIRMED`; the D-233 `assessment` member (`GateReport`); and the D-234 primary selection (outer reason only). D-235 originally added no reason; the M3C amendment adds the reason above. M3C Task 4 also allows the existing `order-in-flight-for-key` reason in composite-gate diagnostics, including the assessment; consumers must preserve that report as well as the existing engine `blocked` data. Subscription-side cardinality is advisory at order time, not an admission guarantee (D-180); Workflow must retain its failure-acknowledgement/compensation path. Added by D-237 item 9 (not wire changes): adopt `orders/tests/fixtures/activation-recheck-v1.json` as Workflow's activation re-check conformance set; and carry the ask that Subscriptions encode an unlimited `maxConcurrentActive` explicitly, so a missing limit defers `overlap-presence-unevaluable` instead of decoding as unlimited.
11. **Evidence.** Running (pure), verified at 256acb8c6 (`artifacts/orders-stage3/M2-VERIFY.log`): `orders/tests/gate_orchestrate.rs` (20), `gate_preview_shape.rs` (10), `gate_assess.rs` (23), `rest_problems.rs` (6), `conformance_vectors.rs` (7), `gate_submit_shape.rs` (20), `gate_ports.rs` (10), `gate_vector.rs` (14), `gate_pricing_map.rs` (14), `gate_answers.rs` (10), `gate_compose.rs` (3); the SDK test binaries (44 across them); `gears/bss/libs/money` (`--all-features`, 69 passed, including `from_fraction`). Type-check only (D-224, the lib-test crate has the 7 known errors until the split-out PRs #5375/#5376/#5381 merge): `infra/gate/tests/preview.rs`, `infra/storage/tests/engine.rs` (hook, E1 to E3), `infra/storage/tests/submit.rs` (the D-188 failure matrix) and the local-client unit test, which only maps `not_delivered` to `OrdersError::Unavailable` (there is no test that calls `LocalOrdersClient::preview`). None of the Postgres tests has run.
12. **Open: mixed-quantity bundle lines.** A bundle line whose selected items carry different quantities passes the gate (`domain/gate/commercial.rs`) and then fails `build_line_requests` in `infra/gate/submit.rs` with `Unsupported("mixed item quantities")`, which submit answers as `Integration` (HTTP 500). Options: (a) refuse at `AddLine::validate` with `request-invalid`; (b) a registered business reason. Decision pending with the Q-03 quantity policy.

## D-236 — Commercial attempt recovery: scope and the open abandonment question

**Status:** SELECTED 2026-10-26 by the user: option C (defer). Raised OPEN 2026-10-23 by the milestone-2 implementation. The abandonment reason (item 3a) remains an open question inside this entry; it is decided when the worker body is built.

1. **What the worker must do (D-188 step 6).** A maintenance recovery pass finds a commercial attempt that is `prepared` or `running` with a lapsed lease, finishes what the frozen inputs allow and, for an attempt that can no longer complete, abandons it: it "records an operational reason ... and settles an authorized refusal only through the engine's existing refusal path". D-188 names no reason for that refusal.
2. **What exists.** A same-key retry recovers and completes the attempt through E2 (`Engine::recover_commercial_attempt`, D-235 item 3): the lapsed lease is reclaimed under a rotated fence and phase C commits from the frozen inputs, settling the frozen report. A lost phase B reply is re-sent by the caller's retry under the same frozen key and recorded once. The `ExecutionRecovery` worker slot from S2-11 is present and answers `RecoveryUnavailable`, so every lapsed execution is preserved and reported unavailable.
3. **What is missing.** (a) No registered reason settles an abandoned execution: `still-processing` never settles a key (it is not a final answer in any case); `order-binding-expired` fits only the case of expired receipts, not an age-based abandonment; `precondition-failed` and `not-admissible` mean a stale draft revision or an inadmissible state and would mislead on replay; the unavailability reasons (`catalog-predicates-unavailable`, `evaluation-unavailable`) are retryable by D-231 and settling one as terminal contradicts it; `authorization-context-changed` is an unsettled-only class. (b) The worker path needs an internal `ReceiptRecorder` over the maintenance grant (today's recorder is caller-bound: `TargetAuthorization`, `Caller`, late-conflict evidence; the worker needs a second engine entry over `TargetLock::Internal`). (c) It needs a `TargetScope` constructor for discovered executions: `TargetScope::from_discovered_order` takes a `DiscoveredOrder`, `DiscoveredExecution` carries no resource, seller or payer axes, and `maintenance/scope.rs` is included unchanged by the D-184 compile-fail fixture, which must keep compiling. (d) It needs a system-actor audit rule for abandonment: the worker actor is class `system`, the trigger `Submit` or `Amendment` has no system-actor rule in `check_actor_rule`, and `finish_execution` needs an `audit_id` from the resolved refusal the engine appends. `ExecutionTerminal::Abandoned` has no production caller today (only `storage/tests/idempotency.rs`, with `not-admissible` as a placeholder).
4. **Options.**
   - **A. Re-issue-only recovery (no abandonment).** The worker re-issues missing `check` commands under the frozen keys and records receipts, then reports `Deferred`; the caller's same-key retry commits. It never settles. Consequence: needs (b) and (c) but not (d) or a reason; an attempt whose caller never retries stays unresolved (and its candidate version unsettled) until a decision on abandonment, so unresolved attempts persist.
   - **B. Register `commercial-attempt-abandoned`.** The full body: re-issue, then abandon under fence after an abandonment window (the milestone-2 plan proposed `gate.attempt_abandon_after_seconds`, default 86400 s; no such setting exists in code) and settle the new reason through the engine refusal path. Consequence: a wire change (catalog, GTS identity, status mapping, goldens, the 91 to 92 reason counts), a Workflow #4787 mirror, a new decision (D-237), and the work in (b), (c) and (d).
   - **C. Defer the body to milestone 3.** `RecoveryUnavailable` stays: every lapsed execution is preserved and reported unavailable, nothing is abandoned, nothing settles. Consequence: none now; the choice between A and B is taken with the Stage 3 delivery of the other providers.
5. **Note on the window.** The planned default abandonment window of 86400 s (not implemented) equals the 24 h idempotency key retention, so by the time abandonment fires the key record is at or past `expires_at` and a settled replay of the abandonment is practically unreachable: the reason matters mostly for audit, not for what a caller sees.
6. **What milestone 2 ships.** Nothing for the worker body (M2-7 stopped before coding): no recovery pass, no new reason, no abandonment path. The same-key retry recovery of item 2 is delivered; `RecoveryUnavailable` is unchanged.
7. **Resolution (2026-10-26, option C).** The worker body is deferred to the next Stage 3 milestone, with the other Orders-owned closure work (observability signals, fault injection, the shared breaker, the Workflow #4787 mirror). Nothing changes in code: the `ExecutionRecovery` slot keeps answering `RecoveryUnavailable`, every lapsed execution is preserved, nothing is abandoned and nothing settles without a caller. Runtime consequence: a submit that dies after the attempt is written is completed by the caller's same-key retry (item 2); an attempt nobody retries stays as it is until the idempotency key retention purge removes it, and the order remains submittable under a new key. No caller is ever told "abandoned". Reasons for choosing C over A and B: item 5 (the reason is audit-facing, a settled replay is practically unreachable), B is a wire change on an unreviewed branch, and A delivers half a worker whose other half needs the same decision. When the body is built, the implementer takes (a) to (d) of item 3 as the task's prerequisites, decides the reason question first (a new registered reason is a wire change with a Workflow mirror under the next free decision number; D-237 was taken by the milestone-3 observability decision), and adds the abandonment window setting then.

## D-237 — Gate observability, the gear-local breaker, alert baselines, fault injection and the activation re-check vectors

**Status:** selected 2026-10-26 by the Stage 3 implementation, milestone 3 (user-approved plan PLAN-M3; pending design review). No wire change: no catalog, GTS, REST status or SDK type changes; no new reason. Code: the breaker state machine `domain/gate/breaker.rs` (62be44dad, 4901d3ba9, a12081806), run signals `domain/gate/signals.rs` (a33ced216), the breaker in `budget::bounded` with `gate.breaker` and readiness (a6d9ed2cd), the `GateSignals` sink `infra/gate/signals.rs` (28a74145d), the activation re-check `domain/gate/recheck.rs` with its vectors (75fe50a15). It closes the Orders-owned 03 §3.8 signals, the 03 §3.3 breaker and the 4.4 Observability rule "Alert thresholds, evaluation windows and operational ownership MUST be defined and tested before production" as far as Orders alone can.

1. **Instruments.** All on the existing meter `bss-orders` (`opentelemetry::global::meter`), D-223 naming (`orders_gate_<thing>[_total|_ms]`), built once in `OtelGateSignals::new()` with descriptions naming the labels (the `OtelWorkerMetrics` pattern); `NoGateSignals` is the sink of `Budget::baseline()`. Label values are the existing tokens (`GatePort::token()`, `PredicateId::token()`, `Reason::token()`, origin `adopted`/`delta`/`pin`); `flow` is `preview` when the plan asks tax, else `submit`.

   | Name | Kind | Labels | Reported from |
   |---|---|---|---|
   | `orders_gate_port_calls_total` | counter | `port`, `outcome` ∈ `ok`, `deadline`, `transient`, `permanent`, `backpressure`, `cancelled`, `subrequest_cap`, `breaker_open` | `bounded` exit, every call including those that never reached the callee |
   | `orders_gate_port_call_duration_ms` | histogram | `port` | `bounded` exit, the whole call including the retry; only calls that reached the callee |
   | `orders_gate_port_retries_total` | counter | `port` | `bounded`, second attempt |
   | `orders_gate_breaker_state` | gauge | `port`; 0 closed, 1 half-open, 2 open | set to 0 per port at construction (no transition counted), then on every transition |
   | `orders_gate_breaker_transitions_total` | counter | `port`, `to` ∈ `closed`, `open`, `half-open` | real transitions only: `record` moving to a new state, the first probe admission (`half-open`), an abandoned probe (`open`) |
   | `orders_gate_port_unavailable_total` | counter | `port`, `kind` ∈ `outage`, `denied`, `deadline`, `malformed`, `missing` | once per run per port, every one of the 12 ports including the stubbed ones |
   | `orders_gate_catalog_denials_total` | counter | `catalog_tenant` (the seller tenant id; item 4) | runs with a denied catalog read, per seller: once per run however many catalog ports answered `Unavailable::Denied` |
   | `orders_gate_runs_total` | counter | `flow` ∈ `preview`, `submit`; `outcome` ∈ `passed`, `blocked` | orchestrator end |
   | `orders_gate_refusals_total` | counter | `flow`, `predicate`, `origin` ∈ `adopted`, `delta`, `pin`; `reason` | orchestrator end, per item 3 |
   | `orders_gate_predicates_unevaluable_total` | counter | `flow`, `predicate`, `origin`, `reason` | orchestrator end, per item 3 |
   | `orders_gate_runs_unevaluable_total` | counter | `flow` | a run with any unevaluable predicate (the item 5 rate's numerator) |
   | `orders_gate_pins_total` | counter | `flow`, `kind` ∈ `unresolvable`, `unevaluable` | gate runs with a pin that did not compose: adds 1 per run per kind with any such line, never the line count |
   | `orders_gate_occupancy_incomplete_total` | counter | `flow` | a run whose occupancy answer was incomplete (D-126) |
   | `orders_gate_resolution_duration_ms` | histogram | `flow` | orchestrator end, the run's elapsed time |
   | `orders_gate_resolution_over_ceiling_total` | counter | `flow` | elapsed above 2500 ms (preview) / 2250 ms (submit) |
   | `orders_gate_requests_total` | counter | `flow` | `PreviewService::preview` / `SubmitService::submit` entry, before the per-caller throttle |

   `outcome="deadline"` is one token for both an admitted call that timed out and a call that found the run's port deadline already elapsed. `orders_gate_port_call_duration_ms` records only calls that reached the callee (an admitted attempt was started); exits before the first attempt (`breaker_open`, `backpressure`, a pre-admission `cancelled`, a run's port deadline already elapsed) count on `orders_gate_port_calls_total` and add no latency sample. The construction-time gauge value is not a transition, so a restart does not move `orders_gate_breaker_transitions_total`. The Preview rate against its limit (03 §3.8) is `orders_gate_requests_total{flow="preview"}` against the existing `orders_throttle_rejections_total{kind="per_caller"}`; no second rejection counter is added.
2. **The gear-local breaker (03 §3.3; plan M3-1, M3-2 as amended 2026-10-26).** No shared platform breaker exists (`libs/` has none; the OIDC plugin's host breaker is bound to `AuthNError`), so Orders ships one behind `bounded` and a platform breaker replaces it by changing `budget.rs` only. One breaker per port, gear-wide (shared by every run, as the bulkhead). Semantics: a sliding window of 30 one-second buckets; a closed breaker opens when the window holds at least `min_calls` outcomes and failures / outcomes ≥ `failure_rate`, judged exactly in basis points (0.3 opens at 3 of 10); it stays open for `open_seconds`; half-open admits exactly one probe; a probe success closes it and clears the window, a probe failure or an abandoned probe re-opens it for another `open_seconds`. A failure is `CallError::Deadline` or `CallError::Transient` (the callee did not answer, or answered a retryable fault); `Ok` and `Permanent` (a callee answer) are successes; `Backpressure`, `Cancelled`, `SubrequestCapExceeded` and `BreakerOpen` are local and not counted. **At most one `Deadline` per run per port counts**: a run's port deadline is cumulative, so its later deadline exits on that port are abandoned, not counted (otherwise one slow or oversized basket records up to 8 failures at one instant and trips the gear-wide breaker). The clock is clamped monotonic inside the breaker. An open breaker answers `CallError::BreakerOpen`, mapped to `Unavailable::Outage`, before any subrequest is spent, so the port's existing fail-closed reason applies (03 §4.5 "mapping to that port's existing fail-closed reason"); `bounded` checks in the order cancel → in-run slot → bulkhead → breaker → attempts. **Config** `gate.breaker { enabled: true, failure_rate: 0.5, window_seconds: 30, open_seconds: 10, min_calls: 10 }`, the defaults when the block is absent; `enabled: false` admits every call. 0.5 / 30 s / 10 s are the design's baseline; `min_calls` is the one number the design omits and is unratified (next to Q7). Validated through `BreakerPolicy::validate` at config validation; errors read `bss-orders: gate.breaker.<field> …`. **Readiness:** the JSON manifest carries `breaker: closed|open|half-open` per port (absent when disabled); the one-line health summary lists only non-closed breakers within a 30-character budget (`, resolve: breaker open`), else `, breakers not closed: N`, because the toolkit caps the health message at 256 characters.
3. **Signals are run-level, not row-level (M3-3).** A refused or unevaluable predicate counts once per run per `(predicate, origin, reason)`, never once per line; pin outcomes count once per run per kind; a port's unavailability counts once per run per port. A 200-line basket must not look like 200 refusals, and every alert rate in item 5 is a rate of runs. A cancelled run or a run ending in an integration error emits no run signal (its port-call signals stand).
4. **The `catalog_tenant` exception to D-223 (M3-4).** `orders_gate_catalog_denials_total{catalog_tenant}` carries the seller tenant id: the only attribute with a tenant value in the gear. D-223 forbids tenant attributes because the audit namespace set is unbounded; this set is bounded by `gate.commercial_identities[]`, one series per configured seller. Why: 03 §3.8 asks for PEP denials "counted per catalog tenant as the operator diagnostic behind `pricing-revision-unavailable`". No other instrument may add a tenant, subject or order attribute.
5. **Alert baselines (M3-5).** The conditions are the design's (03 §3.8); every threshold and window is an unratified baseline, recorded like Q7, to be measured and revised.

   | Alert | Condition | Window | Severity | Status |
   |---|---|---|---|---|
   | Breaker open | any increase of `orders_gate_breaker_transitions_total{to="open"}` | 5 min | page | unratified |
   | Pin unresolvable | any increase of `orders_gate_pins_total{kind="unresolvable"}` (03 §3.8: the price-integrity guarantee is refusing real purchases) | 5 min | page | unratified |
   | Unevaluable rate | `orders_gate_runs_unevaluable_total / orders_gate_runs_total` > 0.05 | 15 min | warn | unratified |
   | Resolution ceiling | p95 of `orders_gate_resolution_duration_ms{flow}` above the flow's ceiling (2500 ms preview, 2250 ms submit) | 15 min | warn | unratified |

   Operational ownership of the alerts is a deployment matter and is not assigned here.
6. **Fault-injection matrix (plan Task 5).** Rows are specifications bound to the named in-crate tests in `infra/gate/tests/faults.rs`; like every `infra/gate/tests` file they type-check only until the split-out PRs merge (item 8). Rows 3 and 8 prove a diagnostic and an exclusion. *(Amended 2026-10-28 by [D-244](#d-244--pr-5536-toolkit-pr-review-findings-fixes-accepts-deferrals) item 42: these tests now run in the orders-pg CI lane with `--features gate-dev-stubs --run-ignored all`; D-241 item 5 had already recorded that they compile and execute.)*

   | Row | Injected condition | Signal that must move | Alert | Test |
   |---|---|---|---|---|
   | 1 | Pricing unavailable for 10 calls (two runs of a 5-line basket) | `breaker(resolve, open)` once; `port_call(resolve, transient)` ×10; the next run `port_call(resolve, breaker_open)` and `port_unavailable(resolve, outage)` | breaker open | `alert_1_transient_failures_open_the_breaker` |
   | 2 | Pricing hangs | `port_call(current_revision, deadline)`; `port_unavailable(current_revision, deadline)`; a blocked run with unevaluable entries | unevaluable rate | `alert_2_hung_port_hits_its_deadline` |
   | 3 | Pricing denies | `catalog_denial(seller)` exactly once for the run, although both `current_revision` and `resolve` are denied; `port_unavailable(resolve, denied)`; the buyer-facing primary is `pricing-revision-unavailable`, never a 403 | (diagnostic, no alert) | `alert_3_denied_catalog_read_counts_per_seller` |
   | 4 | stub fixture without `assessment_profile` | `port_unavailable(assessment, missing)`; a run whose unevaluable entries include the catalog market predicate (`catalog-predicates-unavailable`); its pins still compose, since the pin does not read the D-189 assessment | unevaluable rate | `alert_4_missing_assessment_makes_predicates_unevaluable` |
   | 5 | fixture with no occupancy entry for the line's overlap key (an entry omitting `max_concurrent_active` is now malformed, see item 9) | `port_unavailable(occupancy, missing)`; a run with occupancy incomplete and an overlap-uniqueness unevaluable entry `overlap-presence-unevaluable` (D-126); the independent in-flight-per-key pre-check fails `order-in-flight-for-key` when another order holds the same payer/resource/key, and passes otherwise; occupancy unavailability never suppresses this check (the engine claim remains authoritative) | unevaluable rate | `alert_5_missing_occupancy_entry_sets_the_d126_flag` |
   | 6 | fixture `resolve` answer with an uncovered selected slot | a run with one unresolvable pin | pin unresolvable | `alert_6_uncovered_slot_makes_a_pin_unresolvable` |
   | 7 | Pricing `resolve` delayed 2.6 s under `gate.deadline_ms.resolve = 3000`, Preview-shaped run | `run(preview)` with elapsed above 2500 ms and the over-ceiling observation | resolution ceiling | `alert_7_slow_resolve_crosses_the_preview_ceiling` |
   | 8 | 33 concurrent runs on one port (bulkhead + 1) | one `port_call(_, backpressure)` and `port_unavailable(_, outage)`; the breaker stays closed | (none; proves item 2's exclusion of backpressure) | `alert_8_backpressure_is_not_a_breaker_failure` |

   Row 5 uses a fixture with no occupancy entry for the key. The residual Task 5 presence fix now also rejects an entry missing `max_concurrent_active`; malformed decoded answers can use the existing unavailable/unevaluable path. Explicit `null` still means unlimited in the development mirror; the producer agreement in item 9 remains open.
7. **Scope.** (a) The D-127 activation re-check *signals* (outcomes by kind, defer retries, retry-budget exhaustion) are deferred to S3-13's live integration: no code path executes the re-check in Orders, so there is nothing to instrument (M3-6); the pure decision of item 9 emits nothing. (b) The shared platform breaker remains a platform ask, `cpt-cf-bss-orders-upreq-shared-circuit-breaker` (p2, UPSTREAM_REQS §2.13); this breaker is the interim. DESIGN 03 §3.3's "missing shared breaker capability remains a release prerequisite" is amended to "the gate's breaker is gear-local until the platform ships one", and so is the feature doc's matching sentence ([F03 §3.3](features/03-gate-and-pin.md): "Unavailable required shared breaker capability remains a prerequisite" now reads "The gate's breaker is gear-local until the platform ships one (D-237; `cpt-cf-bss-orders-upreq-shared-circuit-breaker`)"), so the release no longer waits on an ADR nobody owns (`docs/arch/toolkit-contract-binding/DESIGN.md` says a breaker "needs a separate ADR"). (c) D-236 item 7's forward reference to "a D-237 wire change" for an abandonment reason now means the next free decision number; D-236 is otherwise unchanged and the recovery body stays deferred.
8. **Evidence tiers (D-224, M3-7).** Running (pure, integration tier): `orders/tests/gate_breaker.rs` (16), `gate_signals.rs` (6), `gate_recheck.rs` (3). Type-check only until the split-out PRs #5375/#5376/#5381 merge (the lib-test crate keeps exactly the 7 known errors): the in-crate tests of `infra/gate/tests/` `budget.rs` (the breaker in `bounded`, the recording sink), `signals.rs`, `orchestrate.rs`, `preview.rs`, `readiness.rs` and the item 6 matrix. No live, Postgres or alert-rule evaluation has run; the alert rules exist only as item 5. *(Amended 2026-10-28 by [D-244](#d-244--pr-5536-toolkit-pr-review-findings-fixes-accepts-deferrals) item 42: the type-check-only tier above is superseded; the orders-pg CI lane runs these in-crate tests, gated ones included, with `--features gate-dev-stubs --run-ignored all`.)*
9. **Activation re-check (03 §3.6, D-127, D-144; S3-13 Orders-side, M3-8).** The re-check is a Workflow integration algorithm, not a Lifecycle endpoint. Orders ships the pure decision `domain/gate/recheck.rs` `recheck(&RecheckInput) -> Recheck` over the D-144 stored inputs (state, current version, frozen market currency and region, payer, lines with overlap scope key and binding deadline) and the identity and occupancy answers, with `now` as a parameter: `Proceed`, `Reject { lines }` (`market-divergence`, `overlap-collision` or `order-binding-expired`, every affected line of the first rejecting step only), `NotDispatchable(Held | Terminal | Superseded { current_version })` or `Defer(reason)` (`identity-party-unavailable`, `overlap-presence-unevaluable`). Steps in the algorithm's order: held → terminal → superseded; identity (unavailable defers; a currency or region differing from the stored market rejects every line); occupancy (unavailable, or a stored key absent from the answer, defers: never default the limit; `active_count + pending > n` rejects the key's lines; unlimited passes); binding deadlines (`deadline <= now` rejects). When no stored line carries an overlap key the occupancy step is skipped entirely: no read is needed, so an unavailable occupancy answer cannot defer. In the occupancy answer the first row per key wins. `RETRY_BUDGET` (3 attempts within 60 s, the D-127 baseline) is documented Workflow-side; Orders does not retry. Frozen vectors: `orders/tests/fixtures/activation-recheck-v1.json` (version 1, the `conformance-v1.json` shape, 22 vectors: `proceed-two-lines`, `not-dispatchable-held`, `not-dispatchable-terminal-cancelled`, `not-dispatchable-superseded`, `defer-identity-outage`, `defer-identity-deadline`, `reject-market-currency`, `reject-market-region`, `defer-occupancy-outage`, `defer-occupancy-key-missing`, `reject-overlap-collision-one-key`, `proceed-unlimited-limit`, `proceed-at-the-limit`, `proceed-line-without-overlap-key`, `reject-binding-expired`, `proceed-binding-not-yet-expired`, `held-wins-over-identity-outage`, `market-wins-over-collision`, `collision-wins-over-binding-expired`, `terminal-wins-over-superseded`, `held-wins-over-superseded`, `proceed-no-keys-occupancy-unavailable`); `gate_recheck.rs` runs them all and asserts every outcome and every reason appears. Workflow #4787 adopts the vectors as its conformance set (D-235 item 10 mirror list; not a wire change). The composed read's fulfillment inputs (D-144) stay S6-01; the live integration stays S5-04/08/09. **Finding and residual Task 5 amendment (2026-10-27):** the proposed `MaxConcurrentActive` wire form (`domain/gate/answers.rs`, from the #5131 design, which leaves the encoding unspecified) uses explicit `null` for unlimited in the development mirror. Orders now requires the containing occupancy object's `max_concurrent_active` member: absence fails decoding, as do missing count or provenance and malformed limits. This closes the local missing-field fail-open bug; malformed answers use existing typed unavailability and `overlap-presence-unevaluable`, without defaulting a limit. Explicit `null` continues to decode as `Unlimited` for compatibility. Subscriptions must still agree how unlimited differs from unknown, including the meaning of `null`; the explicit-unlimited encoding ask remains open on `cpt-cf-bss-orders-upreq-overlap-presence-read` and the #4787 mirror list. The local presence fix is not producer agreement.

## D-238 — The submit REST route in the development delivery and the end-to-end development build

**Status:** selected 2026-10-26 by the user (build the milestone-3b scenarios); pending design review. No production wire change.

1. **The route.** `POST /bss-orders/v1/orders/{orderId}/submit`, operation `bss_orders.submit` (e4e0254f1, bd8990bec), is mounted beside Preview only when the development delivery is on (cargo feature `gate-dev-stubs` and `gate.dev_stubs`), outside `DELIVERED_OPERATIONS`. The boundary order is the per-order edge limiter, then `If-Match` (428 when absent), the `Idempotency-Key`, the delegation proof header, the body (exactly the independent `expected_draft_revision`, `deny_unknown_fields`), then `SubmitService::submit`. It shares capture's per-order limiter and its renderer: 200 with the stored transition result, a replay byte-identical with `Idempotency-Replayed: true`. The per-caller throttle (`SubmitError::Throttled`) answers 429 with `Retry-After`, as Preview does. Production is unchanged: the delivered-route census still sees 404/405, the OpenAPI is unchanged and the SDK `submit` answers `NotDelivered`.
2. **The build line.** The suite enables `bss_orders/gate-dev-stubs` and `bss-orders-test-authz` from its own `testing/e2e/suites/bss/orders/e2e.yaml`, never from the example server's manifest (`grep gate-dev-stubs apps/cf-gears-example-server/` stays empty). The build is `cargo build --bin cf-gears-example-server --no-default-features --features bss-orders,bss-pricing,bss-orders-test-authz,static-tenants,static-authn,static-authz,bss_orders/gate-dev-stubs`.
3. **Two boots and the seed.** After `migrate`, boot 1 runs without the gate block. The seed (`seed_pricing`, conftest) authors real Products and Pricing data over REST as a catalog-author user in the seller tenant: a SKU carrying the invoice inputs (GL code, tax category, invoice line template and billing timing; Pricing's SDK resolve refuses a line without them), a price book entry and a plan that is created, scheduled and submitted under approval quorum 0. The seed fails fast if any of the four invoice inputs resolves null. The suite then writes the stub fixture keyed by the seeded ids (`write_fixture`: terms and evaluations by the plan revision, overlap keys and occupancy by the SKU, payer profiles for the confirmed payer and one unconfirmed foreign payer, the catalog currency in every money member). Boot 2 stops the server, re-renders the config with the `gate` overlay (`dev_stubs`, the fixture path, `commercial_identities`, Preview 20 per minute, submit 10 per minute) and starts it again. The overlay is the suite's top-level `development_delivery:` block, merged for boot 2 only because `dev_stubs` requires a fixture that boot 1 cannot yet have (4efd158fe, c4166a7bc).
4. **Identities and policy.** The seller's commercial identity is a static-authn S2S credential with its own subject `0e0e0000-0000-4000-8000-000000000006`, distinct from the event producer `…0005`; its `client_secret` is a per-run value that the static authn plugin reads literally (it does not expand variables) while Orders' `commercial_identities` entry expands the same value from `ORDERS_SELLER_CLIENT_SECRET`. The policy grants it `read` only, on `gts.cf.bss.pricing.plan.v1~`, `gts.cf.bss.pricing.price.v1~` and `gts.cf.bss.products.sku.v1~`, constrained to the seller tenant; the development acceptance double needs nothing more. The eight buyers gain the `preview` and `submit` actions on the order type (resource-tenant constraint unchanged). The catalog author holds wildcard rules on the Pricing and Products types for the seller tenant, and the Pricing and Products event producers hold `produce` grants like the Orders producer.
5. **Scenarios.** Preview (`test_preview.py`, 7): `test_preview_happy_path_returns_lines_totals_and_tcv_and_writes_only_diagnostics` (lines, totals, TCV, pin, assessment rows; only gate-outcome rows are written); `test_preview_withholds_tcv_when_a_line_has_no_term` (`tcvWithheld`, no `tcv`); `test_preview_reports_an_unconfirmed_payer_relationship` (the catalog row is `payer-relationship-unconfirmed`); `test_preview_reports_unavailable_when_a_fixture_port_is_missing` (a payer with no profile maps to `identity-party-unavailable`); `test_preview_denies_seller_only_and_foreign_callers` (403, no audit or diagnostics row); `test_preview_is_throttled_per_caller` (request N+1 gets 429 with `Retry-After`); `test_readiness_lists_stubbed_ports_and_closed_breakers` (the health message names nine stubbed ports and no open breaker). Preview never refuses on gate results: `PreviewService::run` returns 200 with the failures inside `assessment.failures`, so the unconfirmed-payer and missing-port tests assert 200 and read the reason from the assessment (the Preview commit's subject says "400"; the test asserts 200). Submit (`test_submit.py`, 10): `test_submit_is_throttled_per_caller` (429 with `Retry-After` after N); `test_submit_commits_a_version_with_pin_totals_audit_and_one_event` (version, pin, totals, audit row, one outbox event); `test_submit_replays_the_same_key_without_new_rows_or_events` (byte-identical replay, `Idempotency-Replayed`); `test_submit_refuses_a_different_payload_under_the_same_key` (`idempotency-mismatch`, also for a committed key); `test_submit_refuses_a_stale_draft_revision_and_a_stale_version` (`version-conflict`, then the 412/stale `If-Match` path); `test_submit_after_preview_yields_the_same_assessment` (Preview parity of the stored gate rows); `test_a_new_key_on_a_committed_order_is_not_admissible` (400 `not-admissible`, replayable); `test_an_open_overlap_claim_blocks_a_second_commit_on_the_same_scope` (`order-in-flight-for-key` with `blocked`, D-180); `test_seller_cannot_submit` (403 for the actor, one unresolved audit row; the foreign caller's non-disclosing 404 `order-not-found` with its own audit row); `test_submit_refuses_an_unsellable_line_with_the_gate_report` (the refusal carries `context.data.assessment`).
6. **Defects found by writing these scenarios, and fixed.** (a) A same-key replay after a committed submit answered 503, because the draft-only read ran before the recovery probe; the probe now runs first, so a committed key replays and another payload is an `idempotency-mismatch` (21095e19b, D-188 step 1). (b) A submit composite-gate refusal omitted `context.data.assessment`; it is now in the stored Problem, so replays carry it (156613666, D-233 amended). (c) A new key on a committed order answered 503; it is now the settled `not-admissible` refusal through `ReadService::get_draft` and the engine's state table (156613666). Test-only fixes: SQL boolean rendering (Postgres concatenates a boolean as `true`, not `t`; f80f7ec7a), and the Stage 2 census expects the dev-mounted submit route (360195113); the claim-release helper is narrowed to the test's own orders and the SKU restore is a fixture (d4fcc58d6). Two further product and seed defects were found by task reviews: submit's per-caller 429 lacked `Retry-After` (bd8990bec, product fix), and the catalog seed lacked the invoice inputs (c4166a7bc).
**M3C amendment (D-240 item 4, Q-A; amends item 6b).** The composite gate now also owns `order-in-flight-for-key`, exactly alongside the engine claim guard. A held same-tuple key refuses before any attempt or receipt; a concurrent step-17 collision remains authoritative and replaces predicate 9 in the diagnostics and stored assessment, which the refusal and replay carry.

7. **Evidence.** The suite builds and collects 31 tests (14 draft and capture, 7 Preview, 10 submit); all skip until PR #5376 provides the Postgres sidecar (`lib.sidecars.PostgresSidecar`). No HTTP evidence is claimed: every expectation is reasoned from code. Running evidence is the Rust tier recorded with each commit (fmt, clippy with and without the feature, the pure integration tests; the in-crate tier type-checks only, D-224, D-239 item 4).

## D-239 — A declarative test-host PDP for the Orders end-to-end suite

**Historical evidence:** items 4, 6 and 7 describe the checkpoint when this decision was recorded. D-241 supersedes their runtime blockers and evidence boundary; the PDP remains development-only.

**Status:** selected 2026-10-26 by the user (build our own e2e-only PDP); pending design review. No wire change. D-238 (milestone 3b) is the submit REST route and the e2e development build.

1. **Why.** The Orders permission matrix needs constraint-bearing decisions on resource, seller and payer tenant properties. The platform's static plugin emits only `owner_tenant_id` tenant predicates and the tenant-hierarchy plugin only tenant-subtree predicates, and the Orders enforcement point can compile neither. The earlier rules PDP PR (#5375) was closed unmerged on 2026-10-23, so the Stage 2 suite's `rules-authz` feature had no provider. The design of the plugin is derived from the SDK models, the enforcement point's requirements and the suite's own rule content.

**2026-10-28 ownership update:** the user requested relocation from the system AuthZ plugins to Orders-owned test support. The names below reflect that move. The crate is non-publishable, Orders uses it only as a dev-dependency, and the explicit host feature enables Orders without changing production authorization or the fixture policy semantics.

2. **What it is.** Crate `cf-gears-bss-orders-test-authz-plugin` (lib `bss_orders_test_authz_plugin`, gear `bss-orders-test-authz-plugin`, instance `cf.builtin.bss_orders_test_authz_resolver.plugin.v1`, default priority 10, vendor `constructorfabric`) under `gears/bss/orders/test-authz-plugin`; example-server feature `bss-orders-test-authz`, never default. The config schema is in the plugin README. Rules match on subject id (plus an optional tenant), resource type (exact or trailing-`*` prefix) and action (exact or `*`). Rule paths become constraints (`values` becomes `in`, `value` becomes `eq`); only `eq` and `in` are emitted. An unconditional grant answers allow without constraints only when the enforcement point does not require constraints. Rules are decided locally for enforcement points that do not require constraints (such as Event Broker's tenant check, which discards the scope): a rule then applies only when one of its paths is satisfied by the request's resource properties, so the Orders producer's tenant-scope `produce` rule allows only the platform root tenant. Delegation proof failures answer with the codes `delegation_proof_required` and `delegation_proof_invalid`. The configuration is validated at boot (ten error kinds), and a linked plugin whose block declares no rules and no grants fails its init. Commits 643055791, 9586febdb, cad9bd8c3, 5a018e8b7, e75e9fd02, 01e1899cc, 36f5093b2.
3. **What it is not.** Not a production PDP and never deployed. It is selected wherever the feature is linked and its config block is present, because its default priority 10 is lower than the static plugin's 100; the feature is never default, so only hosts that opt in link it. Production provisioning of the Orders grants stays with the UPSTREAM_REQS authorization requirement. A linked feature with no config block, or an empty policy, fails init rather than running as a deny-everything PDP. When the enforcement point does not require constraints, a rule the request does not satisfy is skipped before its delegation check, so it records no delegation reason; blank predicate values are refused at validation.
4. **Orders dev-dependency and the lib-test invariant.** Orders dev-depends on the plugin (452fadd71) and its in-crate rules PDP double runs on it. This **amends the invariants in D-224, D-235 and D-237 item 8** ("the seven known errors"): the lib-test crate (`cargo test -p cf-gears-bss-orders --lib --no-run`) now has exactly **4** known compile errors, down from 7, all from the Event Broker test-harness methods of the still-open split-out PR #5381: the `event_broker::test_support::{DeliveredOutcome, DeliveredPublish, FaultInjectingBroker, TransientFault}` import and three `with_types_registry_client` calls. The in-crate tiers stay type-check only until #5381 merges.
5. **The suite switch.** The Orders e2e suite selects the plugin (c91548091): feature `bss-orders-test-authz`, block key `bss-orders-test-authz-plugin`, rule content unchanged. The dev build resolves to `cargo build --bin cf-gears-example-server --no-default-features --features bss-orders,bss-orders-test-authz,static-tenants,static-authn,static-authz`; it builds and the suite collects 14 tests.
6. **Evidence.** Running: the plugin's 55 unit tests. **The e2e suite has not been run**: the local Docker engine was unresponsive on 2026-10-26. No end-to-end evidence is claimed; it is to be recorded here when the suite runs.
7. **Relation to milestone 3b.** The `PLAN-M3B` PDP blocker (#5375 closed) is resolved by this decision once the suite passes; until then milestone 3b can build but nothing is shown green over HTTP.

## D-240 — Milestone 3c gate behavior, wire additions and remaining prerequisites

**Residual follow-through:** D-241 supersedes the runtime-blocked/type-check-only statements in items 8, 10 and 13 and records further local implementation. Item 12 remains an explicit deferral register; a proposed local mirror does not establish owner adoption.

**Status:** selected 2026-10-27 through the user-approved `PLAN-M3C`; Q-A, Q-B and Q-C all option (a). Pending design review. This records Tasks 1–11; whole-branch verification and audit closure remain Task 12. Production Preview remains unmounted and submit remains `NotDelivered` without development delivery.

1. **Order contract (Task 1, D-132/D-199).** Planning and predicate 2 use the sorted, distinct union of the order's `contract_id` and line `contract_ref` values, with one order-scope outcome. No reference means no read. The payer relationship still gates this read (D-215/D-232). Outside development stubs, an order with `contract_id` now fails closed with 503 `contract-resolution-unavailable` until Contracts delivers its provider, subject to the existing withholding and primary-refusal precedence; it no longer silently passes.
2. **Authored terms (Task 2, Q-B, D-193).** `billing-terms-unsupported` / `BILLING_TERMS_UNSUPPORTED` is a new registered FailedPrecondition reason, HTTP 400, owned by the composite gate and its required pin-composition rows. It is registered through `orders-sdk/contracts/catalog.json`, generated SDK/REST bindings, Problem goldens and the DESIGN reason table. Authored intent is `Stated`, `Unstated` or `Unsupported`; only `Unstated` requests seller policy. The SDK's exact term resolver validates the authored calendar interval before projecting to the proposed resolver's month-based query. Unsupported/incomplete authored terms cause no terms query and fail definitively before reservation. The selected/inferred evaluation cycle must equal the resolved snapshot cycle and every recurring binding period. Preview withholds TCV for these failures. Missing snapshot content remains fail-closed at submit's pre-reservation build probe.
3. **Terminal Pricing term refusals (Task 3, Q-B, D-191/D-235 item 7).** The exact InvalidArgument `commercial_terms` reasons `UNSUPPORTED_TERMS`, `UNSUPPORTED_MODEL`, `UNSUPPORTED_WINDOW`, `UNSUPPORTED_SCOPE`, `MISSING_BILLING_TERMS`, `BILLING_CYCLE_MISMATCH`, `UNALIGNED_BILLING_ANCHOR` and `TERM_COUNT_INVALID` are recorded and settle as `billing-terms-unsupported`. The attempt becomes refused and its candidate remains burned; exact replay preserves that refusal. `UNSUPPORTED_TERMS` is not a reliable policy-drift discriminator and never triggers an automatic retry with refreshed inputs. Fresh intent needs a fresh execution/candidate. Digest, money, currency, market, selection and other invalid-argument contract defects stay unrecorded `Integration` failures (500).
4. **Predicate 9 and authoritative claims (Task 4, Q-A; amends D-238 item 6b and D-233's M3b amendment).** Exactly `order-in-flight-for-key` has shared ownership between `GATE_COMPOSITE` and the engine claim guard; the single-owner rule remains for other reasons. The friendly read finds live claims on the same payer/resource/key tuple, excludes the requesting order and discloses no holder identity. It runs independently of occupancy completeness. An already-held tuple refuses through the gate before an attempt or receipt. Engine step 17 remains authoritative against races: a collision replaces every blocked line's predicate-9 row with failed `order-in-flight-for-key`, rebuilding both stored diagnostics and the frozen assessment in the settled response. The refusal carries the assessment and engine `blocked` details, and exact replay returns that snapshot. The separate D-180 cross-resource in-flight count remains part of predicate 7.
5. **Consumed-item capacity (Task 5).** The 200-item per-order bound is the sum of authored `selected_items.len()` across all working draft lines, without deduplication or quantity multiplication. Submit refuses before ports or reservation, after authorized replay/read/revision handling, using existing `gate.binding-admission` / `purchase-capacity-exceeded`; the engine recomputes from the locked draft and commit repeats the check alongside its byte budget. Preview has no admission guard and no cap annotation.
6. **Keyless lines (Task 6, Q-C).** A composed selected pin proving no paid recurring SKU (one-time-only or usage-only) requires no overlap key or claim. Predicates 7 and 9 remain required passed rows with `upstream_code = not-applicable-no-paid-recurring-sku`, preserving complete Vector coverage. Unknown/uncomposed or recurring lines without a key still fail closed. Empty acquiring proposals are valid: `Some(empty)` replaces and releases former claims; `None` on an acquiring transition remains invalid. Internal frozen attempt lines allow explicit JSON `null` for the key; old strings still decode, while absence is malformed. Receipt-bearing keyless rows retain mandatory pin and receipt links. Re-check already skips keyless lines. No local key derivation or multi-key schema is introduced.
7. **Public members (Task 7; D-235 item 10 Workflow #4787 mirror).** Preview adds optional `resolved_total.exclusions` copied from the producer's order declaration, and optional `tcv_basis` (`finite_term`, `rolling_annualized`, `mixed`) only with a present TCV and declared basis. Successful submit adds optional `assessment_id` from the frozen run; newly settled same-key replay preserves it. The design's assessmentId is `snake_case` on the actual SDK/REST wire. Absent metadata is omitted for compatibility; legacy transition bodies still decode. Orders derives neither exclusions nor horizon nor a replacement assessment identity. Workflow must mirror these three members and the new reason in both outer and report decoders; existing `order-in-flight-for-key` can now arrive with composite-gate diagnostics. Workflow code is not changed here.
8. **Parallel dependency waves (Task 8).** A joins identity, catalog and acceptance policy. After payer confirmation, B joins contracts, billing terms and the overlap key → occupancy → in-flight-read chain. C joins assessment and evaluation when prerequisites allow a request. D is Preview-only indicative tax over a verified evaluation. Cancellation checkpoints prevent starting the next wave; already-started siblings may finish. Collected diagnostics retain historical order and catalog denial is counted at most once per run. An in-flight DB error can abort before overlap, terms and policy diagnostics are recorded: failed runs do not guarantee complete port signals. Paused-time concurrency/cancellation tests are built but runtime-blocked; this is not measured performance evidence.

   **A26 amendment (2026-10-27): completed diagnostics on local read failure.** The historical dropped-diagnostics limitation in item 8 is closed locally. The overlap chain retains completed provider answers separately from the failed in-flight count/tuple result. Before propagating the original local failure, the orchestrator records the completed identity/contracts/catalog/overlap/occupancy/terms/policy unavailable diagnostics in historical order, once per called unavailable port and catalog denial. Failed local reads are never represented as empty successful counts or claim sets, and are not labelled as a Subscriptions provider outage. Preview and Submit retain their existing sanitized `unavailable` (503) projection; their structured warning identifies only the bounded failed-read category, not the database cause. Cancellation keeps precedence over that failure and no following dependency wave starts. A failed run emits no assessed-run signal, fabricated assessment, durable outcome vector, commercial attempt, acceptance request or claim. This closes completed port log/signal retention for local in-flight failures; it does not promise durable diagnostics during database failure or completion of work never started. In-crate/service and PostgreSQL regressions execute under the current D-241 harness; no performance or provider delivery claim follows.

9. **Envelope and persistence integrity (Task 9).** The common Preview/submit call site verifies evaluation assessment echo, exact once-only line coverage, frozen currency and coherent amount scale (at most 28); malformed answers become `evaluation-unavailable` and cannot feed indicative tax. Commit compares materialized order/version/line, plan/revision, currency, selection and each exact quantity with the frozen query already verified against the receipt; divergence is `Integration`, never repaired or re-evaluated. D-195 requires envelope profile `orders-gate/1`; producer subresult profiles remain independent evidence. Every admitted row requires the commercial attempt, acceptance identity, request/terms digests and pin by SQL CHECK; commit enforces semantic validity and allows a NULL key only without recurring receipt bindings. Unasked evaluation retains its prerequisite's source reason through internal `Skipped` evidence, subject to payer withholding and local-refusal precedence, without a fictitious outbound failure signal.
10. **Regression evidence (Task 10).** Added tests cover concurrent identical Preview runs with independent identities and writes, immutable replay across mapping-version change, distinct candidates with at most one commit for racing different-key submits, changed-principal recovery refusal, private seller-denial details, frozen dates across UTC midnight and bindings/totals across mid-run publication. Collision replay compares the entire stored response. Runnable pure regressions check coherent evaluation scales 28/29 and the shipped envelope profile. The development envelope is `orders-gate/1`, independent of subresult `dev-stub`; recurring monthly fixtures have monthly periods and one-time/usage fixtures clear them. Malformed-evaluation/profile HTTP cases are collected but skip with the development-host prerequisite.
11. **Consumer contract (Task 11, D-180).** README, development OpenAPI operation descriptions and SDK docs state that subscription-side cardinality is advisory at order time. A gate or activation re-check pass is not an admission guarantee. Subscriptions must enforce cardinality atomically at activation, and submit/activation is not production-ready until `cpt-cf-bss-orders-upreq-overlap-activation-atomicity` is agreed and delivered. Workflow must handle later overlap collisions through failure acknowledgement and compensation. Orders' claim is not a substitute for that admission. D-235 item 9 is corrected: D-146 automatic acceptance remains S4-07/08a, not S3-05 Pricing acceptance-policy discovery.
12. **Explicit deferrals.** Multi-key lines remain unresolved until Subscriptions adopts SUB-G1 derivation; the current helper selects the first recurring SKU and multiple distinct keys remain unresolvable. Residual Task 5 closes the local occupancy presence bug: a missing limit now fails decoding, while explicit `null` retains the documented development-mirror `Unlimited` meaning. D-237 item 9's producer agreement on unlimited versus unknown (including `null`) remains required; the local decoder fix does not close it. Evaluation request-digest verification and `promotion_ref` await Rating #5175. Producer request-mirror completeness for D-189/D-191/D-193 must expand when each owner's SDK lands. Shipped alert rules and commit/publication latency instruments remain S6-06; a scoped diagnostics inspection read remains S6 reads. The D-236 maintenance recovery body and its abandonment-reason decision remain deferred. These limits are not closed by the gate integrity checks or development fixtures.
13. **Audit and evidence limits.** Scope comes from `artifacts/orders-stage3/AUDIT-S3-DESIGN.md` and `PLAN-M3C.md` (audit baseline `e81edaba0`); the local original ledgers are `AUDIT-S3-ledger-{A,B,C}-recovered.md`, with the full feature-03 ledger `AUDIT-S3-ledger-feature03.md` in the same directory. These local audit artifacts are intentionally uncommitted. Prior task validation includes runnable pure integration/SDK evidence; the latest prior full V at Task 10 records 209 integration and 47 SDK tests and 38 collected e2e cases. In-crate tests remain runtime-unrun under exactly four D-239 Event Broker lib-test errors (#5381); PostgreSQL also requires Docker, and HTTP scenarios skip pending #5376. No live PostgreSQL, HTTP, shipped alert-rule or latency evidence is claimed. Final whole-branch review, V and reclassification of each ledger's MISSING/PARTIAL rows belong to Task 12; this decision does not claim audit closure.

### Residual Task 10 — explicit occupancy count scope (2026-10-27)

The proposed development `Occupancy` mirror requires `count_scope`: `per_payer` or
`full_tuple` (payer/resource tenant/key). `LimitProvenance` still names the limit's
Catalog/Contract/platform authority; it does not identify count scope. Missing, null or
unknown scope fails decoding and uses existing malformed/unevaluable handling. No scope
is defaulted or inferred from the request's resource tenant. Predicate 7 adds other-resource
live-claim lines only for `per_payer`; predicate 9 always checks the same-tuple live claim.
Both explicit scopes retain the activation recheck's own-order pending basket only.
Existing development and frozen conformance fixtures explicitly declare their former
per-payer scope. The required limit-presence fix and explicit-null unlimited meaning remain.
Subscriptions must agree and deliver scope/unlimited semantics in its provider SDK; no
production producer is supplied here. Workflow #4787 must adopt this required count-scope
mirror and updated activation conformance fixtures. Those owner and Workflow obligations
remain outstanding, as does D-198 receiver admission and atomic enforcement.


## D-241 — Residual follow-through, proposed mirrors and runtime evidence

**Status:** implemented locally on 2026-10-27 under the user-authorized residual plan following D-240. Cross-gear contracts remain pending owner adoption. Combined local verification has passed; this decision records delivered semantics and observed evidence without closing the owner dependencies below.

1. **Exact evaluation request (D-197).** Fresh attempts freeze the actual request passed to the evaluation port before dispatching Pricing acceptance, beside the returned evaluation. The request preserves available native accepted bindings using the existing Orders receipt serializer: all native models/tiers/minimums, exact decimal text, meter and usage policy, invoice and producer digests. It also retains authorized seller/resource/payer axes, assessment identity and the actual captured instant, plan identity, selected items/quantities, confirmed market and contract, actual resolved start (distinct from earliest-start constraint), complete available resolved BillingTerms and known policy versions. Recovery reuses frozen evidence without another evaluation or reconstruction. Legacy attempts missing the request/new context decode with explicit absence. The native binding spelling is an Orders receipt format, not an adopted Rating wire contract.
2. **Product and scope are independent facts (D-163/D-180).** Frozen line inputs and committed immutable `order_line.catalog_subscription_product_key` preserve the product identity exactly as returned, independently of `overlap_scope_key`; persisted-byte accounting includes the product. Keyless lines remain keyless. Legacy frozen lines missing product identity retain `None`/NULL through recovery; Orders never derives it from a known scope. This closes local persistence loss, not Subscriptions' canonical key/provenance delivery. Multi-key lines still require SUB-G1 derivation/adoption: selecting the first recurring SKU does not resolve distinct product/scope keys.
3. **Required occupancy presence and explicit count scope.** The proposed development mirror requires count, `max_concurrent_active`, limit provenance and `count_scope` (`per_payer` or `full_tuple`). Missing/malformed fields fail decoding; explicit `null` retains the existing local `Unlimited` representation. Limit authority and count scope are independent. Predicate 7 adds D-180's other-resource in-flight lines only to per-payer counts; full-tuple counts use active plus proposed. Predicate 9 retains same-tuple live-claim exclusion for either scope. The activation re-check adds only this order's pending lines under either declared scope. No count is re-bucketed locally. The explicit tag is a local proposal, not proof of producer scope/provenance or owner agreement. Subscriptions still must agree unlimited versus unknown, including `null`, deliver its provider and adopt the scope contract; Workflow must adopt its mirror and conformance vectors.
4. **Producer-declared components and TCV carrier (D-216).** The proposed evaluation mirror adds optional line `committed_components`, `order_committed_components` and `order_tcv_carrier`. Explicit components supply recurring and/or one-time totals with their declarations; absent lists retain legacy single-component inputs, while explicit empty lists represent no committed components. Orders checks unique kinds, complete line/order coverage, currency/scale, representability and conflicting declarations, retaining producer usage rows as uncommitted. For one-time-only/usage-only baskets with TCV but no recurring order component, the producer must explicitly supply the order/recurring carrier's totals and declaration. Missing/invalid carriers fail closed. Preview/tax continue using producer aggregates. Orders never splits aggregates, sums components, invents carrier money, derives exclusions or chooses a horizon. Rating adoption and real-provider monetary conformance remain external prerequisites.
5. **Runtime harness replaces the historical compile prerequisite.** Orders-local test support assembles the current real Event Broker with migrated SQLite metadata, committed Types Registry client, specification manager, actual backend and handler/outbox wiring. The fault wrapper delegates all normal calls and injects deterministic transport, rate-limit, cursor and post-acceptance lost-ack faults. Its persisted-response variant is explicitly simulated after actual acceptance; backend assertions independently check stored appends. It supplies no broker synchronous-persistence contract. No prerequisite PR content was copied. This supersedes D-239 item 4 and earlier type-check-only claims in D-224/D-235/D-237/D-240: in-crate tests now compile and execute, including PostgreSQL producer and gear-boot tests. Broker partition-equality diagnostics remain evidence of the existing shared limitation, not its repair.
6. **Suite-local PostgreSQL and restricted startup.** Orders E2E owns a sidecar built on the existing generic Docker lifecycle, using pinned `postgres:18-alpine` (`GEARS_TEST_PG_TAG` override), dynamic loopback port, actual SQL readiness and surfaced SQL failures. It no longer skips for missing shared PR #5376 support. Binary/Docker gates, admin migration, restricted runtime/worker roles, real Products/Pricing seed, two boots and teardown remain. Restricted-runtime startup uses the shared migration-history existence check already merged in PR #5376. The Orders branch adds no migration-runner implementation or duplicate regression test. The upstream PostgreSQL regression verifies startup on a migrated schema and refusal of pending migrations or missing history without granting runtime DDL privileges.
7. **Malformed input and replay correctness.** Answered but invalid assessment envelopes (wrong profile, missing/unknown/duplicate required rows) now mark the assessment engine input unavailable, producing the existing canonical 503 path before reservation while preserving diagnostics, definitive-local-refusal precedence and payer withholding. They no longer fall through into an unrelated internal error. The Orders response renderer recursively sorts JSON object keys, including nested objects inside arrays, before serialization. First responses and JSONB-reloaded replays therefore share response bytes even when the host enables `serde_json/preserve_order`; status, semantic headers and replay marker retain their existing semantics. This is response normalization, not a canonical Rating request digest.
8. **Evidence and open work.** Local task reports and logs live under ignored `.superpowers/sdd/PLAN-RESIDUALS/` and `artifacts/orders-stage3/`; they are local audit artifacts, not committed deliverables. Final verification on 2026-10-27 passed all 32 recorded commands and 559 tests across the feature-enabled library, integration and SDK tiers (`RESIDUAL-VERIFY.log`), including formatting, production-library clippy, toolkit-library clippy, generated catalog/schema inventory and CFS validation. The separate Docker PostgreSQL run passed all 220 tests (`RESIDUAL-PG-FINAL.log`), superseding the earlier six-fixture-failure checkpoint. HTTP execution exposed malformed-assessment classification and exact-byte replay defects, then two non-recurring fixtures missing the new component/carrier declarations. After the runtime fixes and fixture-only correction `21923ea66`, both affected cases passed and the full suite passed all 43 cases: 38 HTTP scenarios and 5 sidecar checks (`TASK9-CHARGE-FIXTURE-FOCUSED.log`, `TASK9-CHARGE-FIXTURE-FULL.log`, under `artifacts/orders-stage3/`). Runtime source remained `0dc61e474`. These are local development-delivery results, not all-target clippy success, live-provider conformance or owner signoff. Use README's runnable commands for the current harness rather than treating the historical four compile errors or shared sidecar dependency as active blockers.
9. **Retained deferrals and production boundary.** D-240 item 12 remains in force except for specifically closed local omissions: exact request capture, available native inputs, product persistence, missing-limit decoding and declared-component/carrier support. Rating still owes the agreed schema/profile/model declaration, canonical whole-request digest/verification, evaluation/aggregation/promotion policy versions and `promotion_ref`; no Pricing profile is substituted and no missing authority is fabricated. SUB-G1 multi-key adoption, producer unlimited/unknown encoding and occupancy provenance/scope agreement remain open. Production providers, service identities/grants, Workflow decoders and activation conformance, receiver-owned atomic admission (D-198), shipped alert rules/latency instruments, scoped diagnostics reads and D-236 maintenance recovery/abandonment remain distinct external prerequisites or approved future slices. Development stubs establish none of them. Preview/submit remain development delivery only.

**Propagation:** [README](../README.md#residual-delivery-and-owner-contracts), D-237 occupancy amendments, D-240 deferrals, DESIGN/F03 occupancy scope notes and UPSTREAM_REQS owner adoption notes. Source/test details and final review belong to the local residual audit; owner requirements are not closed by this decision.


## D-242 — Adopt PR5516 protected semantics and publish proposed integration contract

**Date:** 2026-10-27. **Status:** Orders semantic direction adopted; local validation
implementation delivered in `a81090675` and `cd82261cf`; producer/consumer ratification
and provider runtime delivery remain open. No bilateral agreement claimed.

Adopt pinned PR5516 S-Q02/S-U10 as detailed in
[SUBSCRIPTIONS_CONTRACT.md](SUBSCRIPTIONS_CONTRACT.md), including source permalinks, ownership,
item-selection coverage, current local line-count unit, protected versioned observations,
confirmation-before-capture, retained suspension/resume, own reservation conversion, cancellation
and unknown-payment fences, and lower-limit preservation. Concrete Orders DTO/wire spellings are recorded in the contract and remain
a proposed owner interface; the proposed `item_keys_v1`, `line_scope_v1` and `protected_full_tuple_v1` profiles
require exact evidence and reject incompatible capacity-unit mappings. Positive quantity retains
the local one-line-per-scope rule without claiming provider agreement. Orders in-flight claims remain distinct from Subscriptions commercial reservations.

For the target contract, supersede D-180's active-only/release-on-leaving-active recommendation
and D-198's activation-intent reservation minimum, two-count observation and linked predecessor
exemption. Three distinct counts and confirmation-time protection are required; linked replacement
and legacy migration are deferred. D-198's receiver fencing and atomic admission obligations remain.
D-237/D-241 development mirrors and historical verification remain evidence of legacy local behavior;
they do not close upstream requirements. The new Orders collection and strict protected
observation branches implement the local validation slice, not atomic provider admission. All real provider/Workflow/financial delivery and joint conformance gates remain
open. Propagation: README, DESIGN, feature 03 and UPSTREAM_REQS precedence amendments.


**Local implementation follow-through (Tasks 2/3):** `a81090675` preserves full selected-item
identity, SKU version, product/scope/provenance and frozen receipt coverage; migration 11 stores
collections with legacy/new mutual exclusion. Every scope reaches the existing single claim
transaction and collision fanout. `cd82261cf` adds explicit protected/legacy decoding, trusted
full-tuple batch validation, complete scoped own-membership, checked arithmetic and reserved-only
conversion. Legacy v1 vectors stay unchanged; 16 protected v2 vectors are proposed local evidence.
No actual provider requirement checkbox is closed. HTTP then exposed malformed protected
observation input producing 500 instead of canonical 503 unavailable (46 passed, two failed).
Independently reviewed source fix `39648a630` corrects unavailable-input mapping. Fresh final-source
verification passed all 33 commands, with 34 summaries recording 583 tests passed, zero failed and
224 ignored. The separate PostgreSQL run passed all 224 tests, zero failed/ignored (112.00 seconds);
the corrected host rebuilt successfully and the HTTP suite passed all 48 cases (43 HTTP scenarios
and five sidecar helpers, 85.34 seconds), including both former failures. The earlier Task 2
43-case HTTP result remains historical evidence.

## D-243 — A maintenance block must schedule the retention purge

**Status:** selected 2026-10-28 by the user. Pending design review. Refines the [Foundation retention purge](features/01-foundation.md) rule "Refusal auditing MUST NOT be enabled in an environment where the worker is not running".

1. **The gap.** Orders audits every refusal; there is no switch that disables refusal auditing. Startup accepted a `maintenance` block whose `tasks` omitted `retention_purge`, so a deployment could run workers while refused audit rows, Preview outcomes and read-access-log rows grew without bound.
2. **The rule.** `OrdersConfig::validate` refuses a `maintenance` block whose `tasks` do not include `retention_purge` ("maintenance tasks must include retention_purge"). Startup fails before any connection is opened.
3. **Unchanged.** No `maintenance` block at all keeps the review-finding-185 behavior: no worker is scheduled, a warning names the disabled work and `orders_workers_scheduled` reports 0; readiness is not affected. Production deployments must configure the block; the alert on `orders_workers_scheduled` = 0 is deployment work (S6-06).
4. **Evidence.** `maintenance_block_without_retention_purge_is_refused` (storage tests, unit tier) refuses a block scheduling only `audit_verification` and accepts one scheduling `retention_purge`.

## D-244 — PR #5536 toolkit-pr-review findings: fixes, accepts, deferrals

**Status:** selected 2026-10-28 by the user on the PR #5536 toolkit-pr-review (`REVIEW_bss-orders-stage3.md`, 175 findings at head `334801e5e`; "review row n" is the `#` column of its Summary table). Triage: 121 fixed, 15 accepted (review row 55 counts as accepted, item 46), 9 deferred, 30 false positives, no open owner question. Pending design review. The fixes are the commits `0a25a091e`…`16b64f53f` on `bss/orders-stage3`. No new reason, no REST status change, and existing REST and SDK JSON payloads keep their bytes. The wire-visible changes are additive or a stricter decode (items 12, 28, 34, 39), and every public-type change is on the Workflow #4787 mirror list. Amends D-231 item 5 (items 14 and 16), D-240's acceptance budget (item 16), D-233 item 1's constructor sentence (item 35) and the scope of D-225 item 7 (item 44). Amends the type-check-only evidence lines of D-237 items 6 and 8 (item 42).

**(a) User decisions (2026-10-28) and accepted defaults**

1. **Catalog selection is one enum (decision 1; review rows 29, 74, 79).** Against the defer recommendation, the user chose one `CatalogSelection` enum in the SDK, used by the SDK `GateOutcome`, the domain `OutcomeKey` and `ProducerResultWithKey`. The JSON wire, the REST golden, the DB columns, CHECKs and index, and the frozen vectors stay unchanged (item 34). Review row 81 (the verdict carrying its reason) stays deferred (item 57): it is a separate shape and was not asked for.
2. **The AuthN context cache is deleted (decision 2; review rows 108, 113).** It stays out until the AuthN SDK exposes token expiry. The phase-B re-exchange that D-194 requires stays (items 15 and 52).
3. **The acceptance retry is log-only (decision 3; review row 26).** A `warn` with line and key identity, and no new D-237 instrument (item 16).
4. **CI clippy debt is fixed, not allowed (decision 4).** The unflagged test clippy errors are fixed at their sites, with no crate-level allow (item 40). The production `large_enum_variant` named in the plan was already boxed at the review head (`Recovery::Existing`, 28473a77f), so it needed no production change (4eee711e5).
5. **Gated tests run in CI (decision 5).** The orders-pg lane gains `--features gate-dev-stubs --run-ignored all` (item 42).
6. **`compose` over no subresults fails closed (decision 6; review row 140).** It returns `ComposeError::NoSubresults`, which the caller maps to invalid coverage (D-195) (item 36).
7. **Accepted defaults.** (a) The gate deadline caps are 1..=60000 ms, matching `dependency_timeout_ms` and the largest value existing tests validate. (b) `client_id` is checked as trimmed non-empty only, as toolkit-auth `OAuth2Config::validate` does. There is no length cap and no charset allowlist: no platform precedent exists and the IdP owns the format. (c) The `dev_stubs` + `dev_stub_fixture` double opt-in stays, behind one accessor, so the README, D-235 item 1 and the e2e `config.yaml` are unchanged. (d) Security headers stay gateway-owned (D-225 item 7, item 44).

**(b) Behavior, wire and public-type changes made**

8. **Gate config bounds (A-T1, 4a33dfa36; review rows 18–22, 36).** The operator config contract is tighter. `gate.deadline_ms`, `authority_exchange_deadline_ms` and `acceptance_deadline_ms` accept 1..=60000 ms; before, any nonzero `u32` was accepted. The bound is unratified, like D-231 item 5. `commercial_identities` refuse an empty or unresolved (`${`) `client_secret`, a blank `client_id` and a blank scope. The error names the seller and never the secret. `gate.deadline_ms` is a `BTreeMap<GatePort, u32>` parsed through the new additive `pub GatePort::from_token` and `Deserialize`, so an unknown port token now fails at parse, with the same message. No wire change.
9. **One development-delivery value (A-T2, 9ad8e3191; review rows 37, 48).** A new `GateConfig::dev_delivery_fixture` accessor, and `gear.rs` holds one `Option<DevDelivery>`. `bind_dev_stubs` drops its duplicate bail in favor of a `debug_assert`. The config keys are unchanged, and there is no wire change.
10. **Limiter and canonical errors leave `api::rest` (A-T3, 67a8a1ccb; review rows 40, 41, 59, 60).** The per-caller limiter, `ThrottleSettings`, `Rejection` and `rejection_error` move to `infra::throttle`. `OrdersOrderError` and `version_allocation_exhausted` move to `infra::error_mapping`. The limiter identity is a typed `CallerLimiter` enum (`token()` = `preview` | `submit`, metric labels unchanged). `api::rest::throttle` re-exports them. No wire change. Two older domain/infra → `api::rest` edges remain, `domain/idempotency.rs` `wire::ROUTES` and `infra/engine/mod.rs` `settled_refusal_problem`/`wire::reason_status`; they are out of scope and the `tests/layering.rs` matcher whitelists them.
11. **The `gate_outcome` insert moves into storage (A-T4, f609bc4ed; review row 172).** Row building and the insert now live in `infra/storage/repo/gate_outcome.rs`, and `RunRecord`/`RunTarget` are re-exported from `gate::writer`. The layering gate also catches grouped and glob imports of `api::rest::throttle`.
12. **Committed-total scale, Preview identity and shared line rules (C9 with A-T5, 70f072307; review rows 32, 34, 35, 38, 110).** This is a public-type and OpenAPI change. `CommittedTotal.currency_scale` (SDK) and `OrdersCommittedTotal.currency_scale` (REST) are `u8`; they were `i32`. The JSON bytes are unchanged (0..=28), and the OpenAPI integer format goes from int32 to uint8 (the repo has no spec snapshot). `infra/read.rs` `committed_total` is fallible on the `committed_integration` check `"currency_scale"`, which never fires under the DB CHECK. The SDK `PreviewResult` decode is stricter: `#[serde(try_from = "PreviewResultWire")]` refuses a body whose `assessment_id` differs from `assessment.assessment_id`. Its fields stay `pub`, unknown members are still tolerated, and the serialized bytes are unchanged. `CommittedTotal` gains field docs, and its tokens stay open strings (item 53). `PreviewLine::validate` no longer clones into an `AddLine`: both call the shared `pub(crate) authoring::validate_line_parts`, with the same rules and error names.
13. **Lint hygiene (A-T6, 69f7b4aed; review rows 111, 112, 114, 119).** The two snake-case allows in `dto.rs` carry a `reason`. The module-level `dead_code` suppression on `infra::gate` is removed, because the gate is delivered in development (D-225 item 15 kept module-level expectations only for undelivered modules). test-authz-plugin clones its service with `Arc::clone`.
14. **Every commercial-authority exchange is bounded (B-T1, 3059e3a2e, c3c71c164; review rows 1, 2, 102).** A new `AuthorityError::Deadline` and a `DeadlineAuthority` decorator wrap the authority once in `CatalogBinding::new`. The catalog reads, the phase-A attempt freeze and the phase-B re-exchange are all bounded by `authority_exchange_deadline`, and phase B also selects on cancel. For a hung exchange the metric `orders_gate_port_unavailable_total{kind}` now reports `deadline` instead of `outage`. This **amends D-231 item 5**: the bound covers every exchange, not only the catalog read. The REST and SDK wire is unchanged (503).
15. **The dead AuthN context cache is removed (B-T1, 7adc55e44; review row 113).** `token_expiry` always returned `None`, so the cache, `EXPIRY_MARGIN` and `cached()` could never hold an entry. Every call exchanges, which is what the code already did, and each submit phase still resolves its own context (D-194). Re-add the cache when the AuthN SDK exposes token expiry (decision 2).
16. **One jittered retry loop (B-T2, ad02f80d9; review rows 25, 26, 58, 67).** Gate ports and Pricing acceptance share one loop, `retry_transient`. Before the retry it waits a full-jitter pause over (0, `RETRY_PAUSE` = 20 ms], cut at the deadline, and it does not retry when the pause reaches the deadline (result `Transient`). This **amends D-231 item 5 and D-240's acceptance budget**: still at most two attempts, never a retry after a deadline, and the pause spends the per-port run deadline (D-94). The worst case is 20 ms or less per retried call; serial retried calls on one port add up against the shared per-port run deadline. The acceptance retry is log-only (decision 3): a `warn` on target `orders.gate.submit` with `attempt`, `max_attempts`, `line_id` and `key`. There is no new instrument, so D-237's instrument list is unchanged. The `outcome` label is a typed `CallOutcome` with the same tokens. No wire change.
17. **Submit and commit failures name their identities (B-T3, c900a66ca; review rows 7–11, 116, 117, 118, 175).** Submit and commit integration failures log the order, attempt, line and the cause's `Display` through one `gate::failure` helper. The cause is a static member name, a codec category or a component name, never stored content. The lost-lease arm gains attempt identity. Logs only, no wire change.
18. **The D-180 in-flight reader takes a named tuple (B-T4, ca04dc971; review rows 65, 66, 99, 100).** `InflightClaims` and the repo's `count_other_orders`/`held_same_tuple` take `ClaimParties` instead of a positional `(payer, resource)`. The new `pub InflightReadError::{Connection, Store}` replaces a stringified `DbErr`: its `Display` is now the label, and the cause is on the source chain. No wire change.
19. **Shared structural guards (B-T5, d19ce4d1d; review rows 62, 173).** Both row-4 preparations share `structural_guards`, and the blocked-disposition wildcards are exhaustive matches. No behavior change.
20. **A failed line-requests snapshot aborts first (B-T6, 02f17e802; review row 17).** This is a behavior change. A failed commercial line-requests snapshot now aborts before the attempt insert and answers `Integration`. Before, the `Value::Null` placeholder hit `ck_bss_orders__commercial_attempt__7`, which answered `Internal` as a store failure and dropped the snapshot error. No wire change. The remaining gap is item 67.
21. **Engine control-flow types (B-T6, c7cdcd9e9, 98731f473; review rows 63, 64).** `Abort::Unreserved(Option<Reason>)` splits into `Abort::UnreservedRefusal(Reason)` and `Abort::NothingToResume`. `Refusal`'s two correlated bools become `GateReach { NotReached, Assessed, Reported }`. No behavior change.
22. **Committed reads (B-T7, 71cf4cd45, f02bb93ed; review rows 16, 45, 46, 47, 101, and 92 for the safety-net tests written first).** This is a behavior change. A stored total charge kind or scope outside `TotalScope`/`EvaluationChargeKind` is now an `Integration` refusal. Before, an unknown kind that happened to be present on both scopes passed. The committed line no longer forges a `draft_content::Model`. Digests compare as typed `Digest` values through `digest_hex`. The eleven-way condition is a table of named checks, and the integration log names `order_id`, `line_id` and the first failing check, never stored content.
23. **Preview and writer failure context (B-T8, 96c19410c, 751d00f07; review rows 98, 103, 115).** Preview logs the refused authoring member (debug), as `infra/capture.rs` does (D-225 item 11). `run_failure` logs `run_id` and `correlation_id`, and the predicate-9 replacement logs the `VectorError`. Logs only.
24. **The development sellability stub answers a race with the conflict (B-T9, 9f041e332; review row 27).** In `SellabilityStub`, a concurrent first command with the same key and a different query now answers `IdempotencyConflict` instead of the winner's receipt. This affects only the `gate-dev-stubs` stub.
25. **Batched child documents (B-T11, 592f8729d; review row 107).** `ChildDocuments`/`LockedOrder` replace `insert_order_line`/`insert_resolved_total` with the batch `insert_order_lines`/`insert_resolved_totals` (`secure_insert_many` on the transaction). The rows written are identical.
26. **One resolve answer per revision (B-T10, 349c20294; review row 106).** `CatalogAnswer.resolved`, `Resolved.resolve_answers` and `CommercialFacts.resolved_bindings` hold `Arc<ResolvedBindings>`. This is a type change inside the crate, with no wire change.
27. **`MalformedRequests` keeps its codec cause (C1, 73048a780; review rows 12, 13, 14).** It is now `{ member, codec: Option<CodecError> }`, a domain error only. `Display` keeps the member and appends the codec category. The legacy key is bound by one slice match. No wire change.
28. **Protected occupancy validation is typed (C3, c0c88dce2; review rows 71, 87, 104).** `validate_batch` returns `Result<ProtectedBatch { Legacy, Protected }, ProtectedInvalid>` (a `Copy` thiserror enum), and `admits()` returns `Result<bool, ProtectedInvalid::Overflow>`. This is an additive wire change: `overlap-presence-unevaluable` rows gain optional `upstream_code` tokens `protected-{incomplete-rows, mixed-profiles, unexpected-key, invalid-binding, mixed-revisions, invalid-own-context, invalid-own-right, rights-exceed-count, membership-mismatch, duplicate-row, overflow}`. The reason, REST, SDK and the re-check vectors are unchanged. The cause is now available for review row 105, which stays deferred (item 61).
29. **An unencodable evaluation binding is a logged local defect (C2, 11203517a; review row 15).** The domain `evaluation_request` returns `Result<Option<_>, CommercialError>`. An unencodable native binding is `Unsupported { line_id, "native binding encoding" }`, logged at `tracing::error` on `orders.gate`. The outcome is unchanged (`Skipped`), and there is no wire change.
30. **Infallible claims and one cursor decode (C10, c4ac21595; review rows 42, 43).** `ProposedClaims::new` is infallible and returns `Self`. `domain::read` gains `DecodedCursor` and `decode_cursor_bound`, and `cursor_version` is removed. The token's wire is unchanged.
31. **Total-row tokens in one place (C8, f3ed3d02d; review rows 51, 52).** `TcvBasis::token`, `EvaluationChargeKind::token`, and `from_token` on `EvaluationChargeKind`, `TotalScope` and the new `AmountStatus`. `TotalRow.charge_kind`, `amount_status` and the TCV basis are typed, a public-type change with no wire change; `EvaluationChargeKind` gains `Ord`. The nil-UUID line id half of review row 52 is deferred (item 62).
32. **Frozen-attempt typing (C4, d9f3bf5a1; review rows 53, 54, 55, 57).** `LineRequest.request_digest` is a `Digest`, which `line_requests_value` writes as hex. `FrozenOrder.report` is a typed `GateReport`, serialized where `Prepared::assessment` is set at the submit refusal and the commit. `RECORDED_REFUSALS` is built at const evaluation from the private `SALE_REFUSALS` and `RECORDED_TERMS_REFUSALS`. This is a public-type change. The `line_requests` bytes are unchanged, as the legacy fixture `tests/fixtures/frozen-attempt-legacy-v1.json` shows, and there is no wire change. `command_key` gains a literal golden test (review row 55, accepted in item 46).
33. **Gate-run inputs carry their invariants (C6, 6355d8947, 3620c12b0; review rows 49, 83, 85, 120, 121).** `Basket.target: GateTarget { Preview, Submit { order_id }, Amendment { order_id } }` replaces `operation` + `order_id` (read through `.operation()`/`.order_id()`). `Basket.dates` and `AssessmentInput.dates` are required `DateBasis`, and a line the basis does not cover stays unevaluable `date-cascade-invalid`. `AssessmentInput.occupancy_axes` is `Option<TenantAxes>` with named axes. `GatePort::baseline_deadline` lists every port, and `assess` maps each line once. This is a public domain-type change, with no wire change.
34. **`CatalogSelection` (C11, 3da5ec9ad; review rows 29, 74, 79; decision 1).** This is a public-type change with no wire change. One `CatalogSelection` enum (`NotSelected` / `SelectedNull` / `Selected(String)`) lives in `orders-sdk::gate_report`, with `from_parts`, `has_catalog_selection()`, `catalog_scope_key()` and `into_parts`. It replaces `has_catalog_selection` + `catalog_scope_key` on the SDK `GateOutcome` (field `selection`, accessors kept), on the domain `OutcomeKey` and on `ProducerResultWithKey`. The JSON wire, the REST DTO and golden, the DB columns, CHECKs and index, and the frozen vectors stay byte-equal through flat `…Wire` structs and `#[serde(try_from, into)]`, and the variant order is the old `(bool, Option<String>)` order. This is a behavior change: a `false` + key payload now refuses to decode (SDK `GateOutcome`, `ProducerResultWithKey`, and the frozen `OutcomeKey` as `KeyWithoutSelection`), where before it decoded and was refused later or not at all. For a producer assessment answer this means the whole answer fails to decode, where before it set the malformed-coverage flag (item 68).
35. **One report projection and named scope setters (C5, f97f11087; review rows 31, 80, 86).** This is an SDK API change, producer-only per D-233 item 1. `GateOutcome::with_scope` is removed, and the named setters `with_line(Uuid)`, `with_item(Uuid)` and `with_selection(CatalogSelection)` replace it. This **amends D-233 item 1's constructor sentence** to name the three setters. In the domain, `OutcomeKey::cmp` destructures every field (D-220 item 6). `Failure` moves to `domain::gate::vector`, where it is re-importable as `vector::Failure` and no longer as `assess::Failure`. The new `Vector::failure_list()` is the one failure dedup that `assess` and `Vector::report` both use. No wire change.
36. **`compose` fails closed on no subresults (D7, b7bb8adc5, 102b7805f; review rows 140, 141; decision 6).** This is a behavior change. `compose(.., vec![])` no longer returns `Passed`: the new `ComposeError::NoSubresults` refuses it, and `assessed_row` maps every `ComposeError` variant (exhaustive match) to invalid coverage per D-195. Observable production behavior is unchanged, because the caller already filtered out empty subresults. 102b7805f pins first-wins order, the item-key text and the duplicate bounds.
37. **Validated exact money (C7, fd49ca498, dc7ddce66; review rows 75, 76).** The money lib gains an additive public `bss_money::exact::is_canonical_integer(text, signed)`, shared with `canonical_integer`; it has no Pricing or Ledger impact. `ExactMoney` (Orders' development mirror of Rating's exact money) has private fields and the accessors `numerator()`, `denominator()`, `currency()` and `currency_scale()`. `new` uses `is_canonical_integer`, and `canonical_digits` is deleted. Wire unchanged.
38. **`MaxConcurrentActive` refuses a missing member itself (C7, 21343d8f1; review row 73).** The `Deserialize` impl refuses a missing member (a `deserialize_any` visitor), and the two `deserialize_with` attributes are removed. An explicit `null` still means `Unlimited`, and a limit above `u32` is a decode error. Wire unchanged. The D-237 item 9 encoding ask stays open (item 71).
39. **Named indicative-tax lines (C7, dddcad9e7, 5e2abe600, 0074726b5; review row 78).** Orders' own proposed indicative-tax request and answer shape changes: `TaxQuery.lines` and `IndicativeTax.per_line` go from `[[uuid, amount]]` to `[{line_id, amount}]` (`LineAmount`, `deny_unknown_fields`), matching the SDK `LineTax`. `LineAmount` refuses the legacy `[uuid, amount]` sequence (map-only `Deserialize`). No provider exists. The dev-stub fixture format changes for the `tax.per_line` entries of `gears/bss/orders/config/gate-dev-stubs.example.json`, `infra/gate/tests/fixtures/alert-4-no-assessment-profile.json` and `infra/gate/tests/fixtures/alert-5-no-occupancy-entry.json`. These are dev-stub inputs only, not persisted or golden data. 0074726b5 corrects the fault row 5 comment: that fixture has no occupancy entry.
40. **CI green before the fixes (W0: 0a25a091e, 8570bc588, 4eee711e5; review rows 93, 94, 122–135, 174).** The settled-refusal byte comparisons in `orders/tests/rest_problems.rs` normalize with `sort_all_objects`, so they hold when `serde_json/preserve_order` is unified into the build. This restores b615af7c1's comparison, which 28473a77f had reverted. The gate test lint suppressions carry reasons, and the dead one in `gate_ports.rs` is removed. The subsumed `baseline_deadlines_are_f03_3_3` test is deleted. The unflagged in-crate test clippy errors are fixed at their sites (decision 4). Test-only.
41. **Added tests (D3 5d4e5f0fe, D4 2ba10ede0 and 00d9afe52, D5 56fff93ac and 213a26db1, D6 17a7dff1e; review rows 89, 90, 91, 97).** Test-only. `from_fraction` gains proptest properties and edge cases (proptest dev-dependency). A Docker-tier test pins Rust `decimal_valid` to `bss_orders__decimal_valid` over a 2096-pair corpus, with no mismatch found, and a boundary table covers 31 and 32 characters. A Docker/PG test shows `record_receipt` refusing the order member and an unfrozen line with `UnfrozenLine`, checked on committed state with no state change. The SDK Preview/submit undelivered → `Unavailable` path and the D-202 proof refusal are pinned through the real `LocalOrdersClient`, and the `preview_tests` module is removed. After this decision was first recorded, the post-D-244 test fixes are `c661ca830` (review row 136), `e4cebe465` (row 142), `4a6d40f04` (row 139), `7fa2df8d5` (row 144), `81e1bb4a2` (row 138, which adds the `opentelemetry_sdk` `testing` dev-dependency for the in-memory exporter), `9703c679e` (row 143), `d56c053f5` (the OTel test lock), then the final whole-branch review fixes `fb96208b6` (the frozen-attempt byte test normalizes with `sort_all_objects`, so it passes under `serde_json/preserve_order`), `43b77da0f` (the OTel exact set keeps only the D-237 instruments), `940da298d` (a misplaced test doc) and `16b64f53f` (the line-requests snapshot error names its order).
42. **The orders-pg lane runs the gated tests (D8, b7999a1d5, ffb358821; review rows 97, 143, 144; decision 5).** `lane_args_orders-pg` passes `--features gate-dev-stubs --run-ignored all`, so the in-crate `gate/tests/{faults,orchestrate,preview}` suites and the gated PG and `submit.rs` tests run where Docker already exists. The lane comment and the in-crate test headers no longer say the tests only type-check. This **amends D-237 items 6 and 8**, whose "type-check only" evidence lines carry an amendment note pointing here (D-241 item 5 had already recorded that the in-crate tests compile and execute).

**(c) Accepted without a code change**

43. **Hand-built scopes for the overlap readers and the Preview run (review rows 4, 5, 6).** `PrivateScope::for_payer_count` is the 08 §3.5 private-persistence exception that D-225 item 6 covers for the predicate 7 and 9 overlap readers. They return counts or requested held keys only, never a holder (D-180), and the PEP already authorized the arrangement, payer included. The Preview's standalone run write under the subject-tenant scope is D-235 item 8. The engine's `gate_outcome` write scope `AccessScope::for_tenant(subject_tenant_id)` is D-235 item 2: `gate_outcome` declares no PEP properties. ToolKit/AuthZ sign-off is still pending under D-225 item 6.
44. **Security headers on the gate routers (review rows 23, 24).** D-225 item 7 (#192) defers the secure-header set and `Cache-Control: no-store` to the api-gateway owners, so all responses change once. **D-225 item 7 now also covers the Preview and submit routers**, which are newer than D-225 but have the same shape. Both routes exist only in development delivery (accepted default (d), item 7).
45. **In-flight DB reads without a per-call timeout (review row 3).** D-222 item 2: the request-path DB bound is the deployment `database.params.statement_timeout`, as on every platform gear, and a per-call bound would be a toolkit-db change. D-231 item 5: the 2.25 s / 2.5 s ceilings are measured, not promised. The two reads are read-only, run outside a transaction, and are dropped with the request.
46. **Positional `Uuid` identities (review rows 55, 61).** D-220 item 9 and D-225 item 2 accept positional `Uuid`s on platform SDK precedent. For `gate_line` (review row 61), a swapped plan and revision fails closed at `verify_commit_line` and at Pricing's resolve. `command_key` (review row 55) has one caller, and it gains a literal golden test (item 32), so a swap no longer passes.
47. **The `catalog_tenant` metric attribute (review row 109).** D-237 item 4: the exception is bounded by `gate.commercial_identities[]`, one series per configured seller, and 03 §3.8 requires it.
48. **`Reason` stays closed in the SDK diagnostics (review row 28).** D-233 item 3: `reason` stays the closed `Reason`, and an unknown reason fails the whole diagnostics decode (D-219 item 4 `Integration`). Workflow #4787 takes new reasons in lockstep.
49. **Producer-built report and verbatim money views (review rows 30, 33).** D-233 item 1: consumers decode and never build the report. Verdict and applicability are open strings (D-233 item 3), so there is no SDK type to take, and `report_is_the_public_vector_and_failure_list_one_to_one` pins the one call site. D-216: the Preview money view holds verbatim parts with no SDK validation, and the producing gear validates them. After item 37 its only caller reads the validated `ExactMoney` accessors.
50. **`Unavailable(Missing)` for unasked ports (review row 50).** This is intentional and documented in the `orchestrate.rs` module doc ("Not asked", D-215, D-232 item 6), and evaluation prerequisites already use `PortAnswer::Skipped`. A new variant would add a metric label and wider matches for no change in behavior.
51. **Rating's `usage_flagged` (review row 77).** It is Rating's producer wire (`LineEvaluation`, the #5175/#5390 mirror), and `totals::total_rows` enforces the cross-field rule (`TotalsError::Usage`). Folding the fields together is a Rating-contract ask, not a local fix.
52. **Three AuthN exchanges per submit (review row 108; decision 2).** The phase-B re-exchange is deliberate: D-194 resolves the context again so that a rotated credential never rebinds. After item 14 every exchange is bounded. Reusing the catalog-read context for `attempt_inputs` would need new plumbing across the port trait and is not worth it while the exchange is bounded.
53. **Committed-total tokens and decimal text (review rows 34 and 35, the accepted halves).** The `CommittedTotal` tokens stay open strings with no `#[non_exhaustive]`. `OrderView` is a stored response replayed across builds, closed enums would break replay on a new token, and D-233 item 3 keeps tokens open for the same reason. Committed totals stay exact canonical decimal text, not fractions (D-216 item 1). The fixed halves are item 12.

**(d) Deferred**

54. **Line-requests and item-key shapes (review rows 56, 70).** `line_requests` is the frozen persisted format (SUBSCRIPTIONS_CONTRACT.md: exactly `{key, query, request_digest, item_keys_v1}`, plus the legacy 4/5-member form with a required-nullable `overlap_scope_key`). An untagged serde enum would lose the member-named `MalformedRequests` that item 27 logs. One enum for `overlap_item_keys` across `StoredLine`/`LineRequest`/`FrozenLine` is the right end state, but it changes the contract doc and the persisted adapter (D-242, DB columns, `activation-recheck-v2.json`). Trigger: the owner ratifies `item_keys_v1`.
55. **Pricing refusal codes (review row 69).** The codes live only in the Pricing impl crate, so exporting them is a `bss_pricing_sdk` change owned by Pricing. A rename fails closed today (classified as `Outage`). Trigger: a Pricing SDK ask. It is recorded here and is not yet in UPSTREAM_REQS.
56. **Item-key profile as an enum (review row 72).** This is not a pure typing change. Today a wrong profile is a per-line `valid()` failure (`overlap-key-unresolvable` for that line). A single-variant enum would make it a whole-answer decode error, which changes outcome granularity.
57. **The verdict carrying its reason (review row 81; decision 1).** `Outcome` is persisted in the frozen vector and mirrors DB columns with CHECK `ck_…gate_outcome__2`, and `Vector::build` runs on every decode, so the invalid pairs cannot persist. A `Failed(Reason)` fold needs a custom flat serde adapter and touches `replace_inflight`, `assessed_row` and the writer: churn on a frozen shape.
58. **Basket line-id mismatch kept defensive (review row 82).** The defensive behavior is intentional and pinned (`m_mismatched_line_id_is_reference_unresolvable_never_repaired`, `upstream_code: basket-line-id-mismatch`). It has 18 constructions and low value.
59. **Held keys as `OverlapScopeKey` (review row 84).** The values come back from the DB as a subset of keys that `OverlapScopeKey` already validated. Converting them would re-validate each row in infra for no new safety.
60. **A validated breaker policy (review row 88).** `OrdersConfig::validate` → `GateConfig::validate` runs at init, so no invalid policy reaches `budget.rs`. A `ValidatedPolicy` would mean config-plumbing churn plus 14 test constructions for a precondition that already holds.
61. **The re-check defer cause (review row 105).** `Recheck::Defer(Reason)` is the conformance shape Workflow #4787 adopts (`activation-recheck-v1/v2.json`, D-237 item 9). The domain is pure and does not log. After item 28 the typed cause is available to whichever executor logs, and adding it to `Recheck` is a Workflow mirror change.
62. **The nil-UUID roll-up line id (review row 52, the deferred half).** It is the stored roll-up key of `bss_orders__resolved_total`. Deriving it from the scope is churn across `usage_row` and `row` for no new safety, because `total_rows` is the only producer.

**(e) False positives**

63. **TEST-QUALITY-10, 27 findings (review rows 145–171).** Every flagged guard is a `parking_lot::Mutex`: `.lock()` has no `unwrap` and no poisoning. Every site takes exactly one lock inside one assertion, with no `.await` and no second lock of the same mutex in one expression (rows 164 and 171 are two statements on two mutexes). If the macro's temporary scope ends early, the guard is held for less time, which can neither deadlock nor hide a long hold. `cargo clippy -p cf-gears-bss-orders --features gate-dev-stubs --tests` on the pinned 1.98.1 gives no borrow-check error and no lock-related lint. Seven sites are `assert!`, outside the rule's own `assert_eq!`/`assert_ne!` scope. The same shape appears 167 times outside Orders, all CI-green. There is no sweep: rewrite a site only if it is edited for another reason.
64. **Explicit `null` on the committed total (review row 39).** The premise that every other optional member is omitted is wrong: stored nullable columns already answer explicit `null` in the same responses (`OrdersDraftLine` dates, `term_duration`, `billing_cycle`, `OrdersVersionSummary.supersedes_version`, the header `contract_id`). Only conditional members are skipped. `OrdersCommittedTotal` mirrors the nullable row, and changing it would be a wire change against the SDK `same_wire` goldens.
65. **An untagged enum for the test PDP predicate (review row 68).** `values` and `value` are two YAML keys. An untagged enum would need `#[serde(flatten)]`, which serde does not support together with `deny_unknown_fields`, and that would weaken unknown-key refusal on the test PDP (D-239). `PredicateSpec::validate` has to stay anyway, for the non-empty and no-blank-entry rules.
66. **Unbound line cursors (review row 44).** The Orders gear is not on `main`, so no line token without a version binding was ever issued. Every line token is encoded with `Some(current_version)`, and `current_version` is NOT NULL from creation (D-64). Rejecting `None` (`cursor-invalid`) is the intended binding. Only the `DecodedCursor` doc, which implied legacy unbound tokens, was reworded (f02bb93ed).

**(f) Known gaps on record**

67. **A line-requests `Unavailable` answers `Integration` (review row 17).** On the `begin_commercial_attempt` path a line-requests `PrepareError::Unavailable` is answered as `Integration` (500), while the engine's `prepare_error` maps `Unavailable` to `EngineError::Unavailable` elsewhere. The brief chose this, and it is deferred.
68. **The Pricing assessment adapter must map a decode failure to invalid coverage (review row 74).** After item 34, a producer assessment answer with `has_catalog_selection = false` and a key fails to decode as a whole, where before it set the malformed-coverage flag. When the real Pricing assessment adapter is built, it must map an answer-decode failure to invalid coverage (D-195).
69. **A transient failure near the deadline ends `Transient` (review row 25).** With under one pause of deadline left, a transient failure now ends `Transient` (`port_unavailable` kind `outage`), where the old immediate retry ended `Deadline` (item 16).
70. **Two named tenant tuples.** C6 asked for the B-T4 in-flight reader to take `TenantAxes` as well. It takes `ClaimParties` (`domain/overlap.rs`) instead, so the assessment (`AssessmentInput.occupancy_axes: Option<TenantAxes>`) and the D-180 reader name the same payer and resource axes with two types.
71. **Open owner items this decision does not close.** The 1..=60000 ms deadline caps are unratified (item 8, like D-231 item 5). Acceptance instruments (plan OWNER Q2) stay open until the D-237 alerts are ratified (item 16). The AuthN context cache returns only when the SDK exposes token expiry (item 15). The indicative-tax port has no provider (`cpt-cf-bss-orders-upreq-indicative-tax-read`): update that ask's text if it is ever given a shape (item 39). The `MaxConcurrentActive` explicit-unlimited encoding ask stays open (D-237 item 9, item 38).

**Propagation:** D-231 item 5, D-233 item 1, D-225 item 7 and D-240 are amended by items 14, 16, 35 and 44, as stated above. D-237 items 6 and 8 carry an amendment note pointing here. The Workflow #4787 mirror hand-off lists the public-type and decode changes of items 12, 34, 35 and 38 and the item 20 check. The per-row reconciliation of all 175 rows is the run's closure record, not this entry.

## D-245 — Orders reads Rating's exact money with main's fraction API

**Status:** selected 2026-10-29 by the user after main merged the bss-money exact-fraction API (#5527, `cc7473519`). Amends D-244 items 37 and 41 and the D-216 money note.

1. **No money-library change.** This branch no longer adds `ExactAmount::from_fraction`, `is_canonical_integer` or their tests to `gears/bss/libs/money`; the crate is exactly main's. Precedent: Rating writes and reloads exact amounts with the shared crate's exact-fraction API and never with a copy of it (Rating PRD §"exact amounts", money ADR rule 2, T-D-80).
2. **`ExactMoney` is the money ADR's canonical fraction.** `ExactMoney::new` and deserialization accept a pair only when `ExactAmount::parse_fraction` does: canonical integer text, a positive denominator and lowest terms (money ADR rule 6; Rating ADR-0005 "Form"; T-D-80 item 1). A fraction with a common factor (`2/4`, `0/2`) is refused as `ExactMoneyError::NotReduced`, a contract defect like a malformed part. Before, Orders accepted and reduced it.
3. **Totals.** `exact_decimal` renders through `ExactAmount::parse_fraction(…).canonical_at_scale(28)`; stored text is unchanged for every reduced fraction. Nothing persisted holds `ExactMoney`, so no stored row changes meaning.
4. **Tests.** The fixtures that used unreduced pairs (`…890/7`, `42/4`) now use reduced ones; `exact_money_rejects_noncanonical_parts` covers `2/4`, `-6/9` and `0/2`.

## D-246 — PR #5536 maintainer review (macrocheese1, 2026-10-29): fixes

**Status:** selected 2026-10-29 by the owner on macrocheese1's review of PR #5536 at `13b259fbd` (14 inline findings R1–R14 and four open questions OQ1–OQ4). The owner accepted every recommendation: Q-A1 (A), OQ-C1 (a), OQ-C2 (d), Q-D1 (a), Q-D2 (B) and Q-D3 (a). Pending design review. The fixes are the 24 commits `b3fe506fb`…`fa30f1bbb` on `bss/orders-stage3`, one task per item below (D2 and A2 have two commits each), and the final-review fixes `86df1dd54`…`cb9427165` (items 26 and 27). No new reason and no reason→status mapping change; inputs that answered 500 now settle registered refusals or 503. The visible changes are a stricter producer-answer decode (B1), a stricter authoring refusal (C7) and an additive refusal-body member (D1). Amendments to earlier decisions are recorded here only; their text is unchanged.

**(a) Compatibility with `main`**

1. **A1 — shipped migrations are corrected forward (R1; b3fe506fb).** Orders migrations 01–10 are on `main`, and the runner records migrations by name only, so a shipped migration is never edited again. Migration 01 is restored to main's bytes. `m20261027_000011_orders_item_keys` forward-adds `catalog_subscription_product_key`, `overlap_item_keys` and their CHECK, and drops and re-adds `ck_bss_orders__order_line__2` without the `overlap_scope_key` requirement. The re-added check also drops main's all-NULL (unpinned) branch, which is safe because main has no production order-line writer. `migrations_shipped_on_main_are_unchanged` pins main's SHA-256 per shipped migration, the Docker test `a_main_database_upgrades_through_item_keys_and_admits_keyless_lines` upgrades main's prefix through 11, and `build_inventory.py` applies ALTERs in source order. Amends D-206 item 4 (migrations "edited in place rather than corrected forward") and the D-208 note "amended in place under D-206 item 4's rationale"; withdraws the PLAN-M3C working rule "schema changes edit migrations in place" (item 23).
2. **A2 — stored exact money is reduced on decode (R13; e59261157, 30279c3d2).** `ExactMoney` is persisted, in `FrozenOrder.evaluation` inside `commercial_attempt.line_requests`, and attempts frozen before D-245 may hold a pair with a common factor. Before the strict decode, the stored decode (`decode_line_requests` → `reduce_stored_money`, over `bss_money` `parse_fraction`/`checked_div`/`canonical_fraction`) reduces the pairs at the typed `ExactMoney` positions of the frozen evaluation only: `Totals.{gross, net, discount}` on lines, line and order components, the TCV carrier and the order totals, plus `tcv.exact` and `promotion_status`. Recovery therefore commits the totals the old code rendered. The opaque producer evidence (`TotalsDeclaration` members, `Tcv.evidence`) is never visited and stays verbatim (D-197/D-216). A part that is not canonical integer text still refuses, and the wire and `ExactMoney::new` stay strict. Amends D-245 item 3 (item 23).
3. **A3 — main's line cursors keep paging their draft (R14, Q-A1 (A); 35901d775).** S2's reads are on `main`, and main issued line tokens without the version member. Main never committed a version, so a version-less line token is bound to the creation version (`MAIN_LINE_CURSOR_VERSION = 1`, `DecodedCursor::line_version`): it continues its draft page and is refused with `cursor-invalid` once the order commits. Precedent: `toolkit-odata` `CursorV1` refuses only another format version, and `CURSOR_VERSION` stays 1. Amends D-244 item 66 (item 23).
4. **A4 — the read service proves A3 (R14; fa30f1bbb).** A test pages a draft with a version-less token issued by main and shows the refusal after commit, at the read service. Test-only.

**(b) Gate answers and assessment**

5. **B1 — the overlap-key answer refuses unknown members (R12; b22777f59).** `answers::OverlapKey` is `#[serde(deny_unknown_fields)]`, as `docs/SUBSCRIPTIONS_CONTRACT.md` requires, so a misspelled `item_key` is never read as a legacy echo. `OverlapKey` is never persisted (frozen line requests keep scalars plus `ItemOverlapKey`, which already refused unknown members), so no stored row changes. No decision is amended.
6. **B2 — producer reasons map only to their predicate (R2; 300581834).** D-195's mapping registry is enforced per predicate. A failed producer subresult maps only to its predicate's `failure_reasons`, and an unevaluable one only to `catalog-predicate-unevaluable` or `catalog-predicates-unavailable`. Any other token, including a registered Orders reason such as `axis-invalid` or `request-invalid`, is unmapped, which means invalid coverage (`catalog-predicates-unavailable`, 503). It can no longer become a D-234 definitive refusal that hides an outage, or a 500 `GuardReason`. Refines D-195 and the F03 §3.1 recording rule without changing them.
7. **B3 — predicate 9 is never passed when keys were not read (R8; d6cab6cb9).** `item_keys::read_keys` is the single overlap-key validation. The orchestrator reads occupancy and the D-179/D-180 in-flight lookups only for its keys. When it rejects the answer (an omitted line, or a foreign or extra entry; `SUBSCRIPTIONS_CONTRACT.md` exact coverage), `assess` reports rows 7 and 9 of every keyed line unevaluable (`overlap-presence-unevaluable` / `overlap-key-unresolvable`, `upstream_code` `overlap-key-answer-rejected`). A line with no key keeps `failed`/`unevaluable` `overlap-key-unresolvable`. Settlement is unchanged. Amends D-179's advisory in-flight pre-check only in its reported row, and supersedes the earlier `assess` behaviour of ignoring foreign key entries.

**(c) Commercial inputs and acceptance**

8. **C1 — the billing-terms answer is verified first (R3; 464f21cfe).** An entry for a line the run did not ask, or a second entry for one line, makes the port `Unavailable(Malformed)`: the pin rows read `catalog-pin-composition-unavailable`, and nothing is frozen or sent to Rating. A line the answer omits keeps its per-line missing-terms meaning (D-189/D-195, TCV-only on Preview). The port call and its verification sit in a `resolve_terms` helper. Refines D-193 and D-231 (typed unavailability).
9. **C2 — the frozen Pricing requests match the evaluated Rating request (R3; 2ea939ec4).** D-197's exact acceptance-input equality is enforced twice. Before the reservation, each frozen `NewSaleQuery` must equal the run's frozen Rating request (line, market, axes, hold policy, revision, start, term, cycle, billing-terms digest, selections and quantity); a mismatch is `CommercialError::Mismatch` naming the member. At commit, the receipt's query and bindings are checked against `FrozenOrder.evaluation_request` (`verify_evaluated_query`, `verify_evaluated_bindings`). A mismatch is an integration failure, and nothing commits. Members a legacy request omits are not compared, and an attempt without a frozen request is not checked. Amends D-235 item 7: a receipt that does not verify against the frozen request is an integration failure, and that now covers the frozen evaluation request.
10. **C3 — one currency scale per evaluation (R7; 3ab4d21e9).** `verify_evaluation` requires the answer's `currency_scale` to equal every evaluated binding's `currency_scale`; otherwise the evaluation is `Malformed` (D-197 currency/rounding inconsistency, D-216). Refines D-197.
11. **C4 — unfreezable billing terms refuse at assessment (R5, OQ-C1 (a); c61fe9447).** Answered terms without a snapshot, or with a snapshot Pricing cannot express (schema other than 1, a zone other than UTC, a seller-policy source at version 0), refuse the pin-composition row `billing-terms-unsupported` at assessment on every operation, Preview included: `assemble` adds the line to `unsupported_terms` when `commercial::billing_terms` refuses the snapshot. They no longer reach submit's build probe. Preview withholds TCV as for the other unsupported-terms failures. Digest mismatches stay unrecorded integration failures (D-240 item 3). Amends D-240 item 2.
12. **C5 — the acceptance-policy version is verified (R5; 937fcd989).** A version that is not canonical positive-integer text makes the port `Unavailable(Malformed)`, so the pin row reads `catalog-pin-composition-unavailable` and no hold policy is frozen from it. Refines D-191.
13. **C6 — mixed item quantities fail at assessment (R5, OQ-C2 (d); 4fcbc5ea4).** Per D-231 item 4, a line whose items state different quantities fails `ReferenceResolution` `reference-unresolvable` (upstream code `mixed-item-quantities`) at assessment, and its pin carries that reason. This covers drafts already stored. Closes D-235 item 12 together with item 14. Q-03 stays open; if it admits per-item quantities, items 13 and 14 are lifted together.
14. **C7 — mixed item quantities are refused at authoring (R5, OQ-C2 (d); 933cc6d23).** `validate_items` (`AddLine`, `LinePatch` with items, Preview lines) refuses `request-invalid` (`quantity`) when two stated item quantities differ. Absent quantities stay the quantity floor's. No reason or wire shape changes. Closes D-235 item 12 with item 13.
15. **C8 — an oversized single pin is a capacity refusal (R6; c1b2b29aa).** A receipt whose schema-2 pin alone exceeds the 1 MiB version limit (`CodecError::TooLarge` from `OrderPin::new`, which also covers more than `MAX_BINDINGS` bindings or selections) fails `gate.binding-admission` with `purchase-capacity-exceeded`, as an oversized version does (`Blocked::Capacity`). The attempt settles refused, nothing commercial is written, and the same key replays the refusal. Before this, every retry answered 500 while abandonment was deferred (D-236). A single pin's size is judged when it is built, before that receipt's selection verification, so an oversized receipt settles `purchase-capacity-exceeded` even if it would also fail D-192/D-197 verification; nothing commits either way. Amends D-235 item 7.

**(d) Diagnostics, claims and authority**

16. **D1 — the collision refusal carries the gate report (R4; 3bfc7fa9c).** The step-17 `order-in-flight-for-key` refusal stores the rebuilt predicate-9 report in `context.data.assessment` as well as in the snapshot's top-level `assessment`, as D-240 item 4 already required. Before, only the snapshot copy was written, so REST and `LocalOrdersClient` never saw it. `GateReach::Reported` is merged into `Assessed`, and the SDK `RefusalDiagnostics::assessment` doc names the claim refusal. Refusals settled earlier replay their stored snapshot unchanged, with no body report. Regression: `submit.rs::a_concurrent_claim_refuses_order_in_flight_for_key_with_no_version`. Amends D-233's 2026-10-27 amendment and D-240 item 4 as implemented.
17. **D2 — retained claims count in the D-180 addend (R10; 2dc0bea4c, beca9af9e).** The cross-resource addend counts the holder's current-version lines for any live claim at or below that order's current version (`claim.version <= order.current_version`, lines joined on `order.current_version`), so a claim retained from an earlier version by an amendment counts. This agrees with `held_same_tuple` on retained claims; unlike that read, which has no version bound, a claim above the current version never counts. It was latent until amendment builds `ProposedClaims`. `beca9af9e` guards the bound with a test. Amends F03 §4.2 predicate 7's implementation note (D-180).
18. **D3 — the policy, terms and assessment ports take no caller context (R11; f17be8b27).** `BillingTermsReads::resolve`, `AcceptancePolicyReads::policy` and `AssessmentReads::assess` take no caller context. A delivered adapter resolves the seller's Orders service principal from the query's `seller_tenant_id`, as `PricingCatalogReads` does, with no buyer-context fallback. No runtime change while the adapters answer `Missing`. Implements D-194's R08 matrix at the port boundary.
19. **D4 — the commercial principal must be a seller-tenant service subject (OQ1, Q-D1 (a); f56710ce3).** `AuthnAuthority` refuses an exchanged principal whose subject tenant is not the seller, or whose type is not `gts.cf.core.security.subject_service.v1~` (`AuthorityError::Misprovisioned`, warned once per seller with the seller id, which check failed and the expected type id; `89ec35e5b` gave the catalog read's warning its own text). The catalog reads report `Missing`, and submit reports unavailable before any attempt or candidate. The seller's commercial principal is recorded as homed in the seller tenant, as the e2e provisioning already does (`testing/e2e/suites/bss/orders/config.yaml`). Refines D-194. See item 24.
20. **D5 — the re-check decodes stored keys from the item-key collection (R9, Q-D2 (B); 7b1e61517).** A line's stored re-check keys are the distinct `overlap_scope_key` values of `orders_order_line.overlap_item_keys`, or the legacy scalar `overlap_scope_key` when the collection is NULL, decoded by `StoredLine::from_stored`; mixed or malformed rows defer `overlap-presence-unevaluable`. The composed-read members remain S6-01, and the D-241 item 9 deferral is unchanged. Amends D-144 and 03 §3.6 steps 1 and 6.
21. **D6 — the authorization-fact fingerprint is recorded, not compared (OQ2; dcfe271fa).** The `authorization_fact_fingerprint` doc now says the fingerprint is recorded evidence that no path compares. The request fingerprint over the same three axes makes a moved axis an `idempotency-mismatch` first. No schema or behaviour change.
22. **D7 — Preview's `pin` view is the catalog composition outcome (OQ4, Q-D3 (a); e8c3254bd).** Preview's per-line `pin` view is the catalog pin-composition outcome, evaluated independently (03 §3). It is not the `orders.pin-composition` row's verdict, which also folds in the policy and terms. The SDK doc says so. No wire change.

**(e) Corrections, the open question and release notes**

23. **Corrections.** D-245 item 3 ("nothing persisted holds `ExactMoney`") is wrong: frozen attempts store it, and item 2 reduces it on decode. D-244 item 66's premise ("the Orders gear is not on `main`") is wrong: S2 is on `main`, and items 3 and 4 keep main's tokens paging. The PLAN-M3C working rule "schema changes edit migrations in place" is withdrawn: shipped migrations are corrected forward (item 1).
24. **OQ3 — an attempt stuck behind `idempotency-mismatch` after an axis edit (no change).** An axis edit on an order with an unresolved commercial attempt is an instance of D-236 item 7. The same-key retry becomes an `idempotency-mismatch` (the request fingerprint includes the axes), the attempt stays until retention purges its key, and the order is submittable under a new key. This is D-236's deferred abandonment; the D-236 worker body must cover this case when it is built.
25. **Release notes.** Item 19 refuses an off-tenant or non-service commercial principal: catalog reads answer `Missing`, submit answers `Unavailable`, and one warning is logged per seller. It also stops in-flight attempts frozen with such a principal at phase B, which re-exchanges the principal (D-194). Operators must home the seller's commercial principal in the seller tenant as a service subject before upgrading. With `oidc-authn-plugin`, the exchanged token's `subject_type` comes from the claim named by `s2s_oauth.claim_mapping.subject_type` (falling back to `jwt.claim_mapping`), or from `s2s_oauth.default_subject_type` when the token has no such claim. For the seller's commercial principal it must yield `gts.cf.core.security.subject_service.v1~`, or every seller answers `Misprovisioned`.

**(f) Final whole-run review**

26. **A refused line shape is never composed or asked for keys (C6 × B3; 86df1dd54).** Item 13 refused a mixed-quantity line only in `assess`, while the orchestrator's `line_bindings` filtered only the line-id mismatch. The orchestrator therefore composed the refused line and asked its key, the full key answer then held an entry `assess` called foreign, and item 7's rejection made every other keyed line's rows 7 and 9 `overlap-key-answer-rejected` although they had been read. One predicate, `assess::supported_basket_line` (the request names its line and its items state one quantity), now gates both `line_bindings` and `map_basket_line`, so a refused line is never composed, asked for keys, evaluated or read. Regression: `gate_orchestrate.rs::a_mixed_quantity_line_is_never_asked_a_key_and_leaves_other_lines_read`. Refines items 7 and 13.
27. **Diagnostics and doc corrections (cb9427165, 89ec35e5b).** The catalog read's `Misprovisioned` warning no longer says that no identity is configured, and the authority's warning says whether the tenant or the subject type failed (item 19). The submit commit's module doc states item 15's capacity-before-verification order. `OrderEvaluation`'s doc names `reduce_stored_money` (item 2), `PredicateSpec::failure_reasons` is documented as Failed reasons only (item 6), and `read_keys` as returning keys in line-id order (item 7). No behaviour change.

**Propagation:** item 1 amends D-206 item 4 and the D-208 note; item 2 amends D-245 item 3; items 3 and 4 amend D-244 item 66; item 6 refines D-195; item 7 amends D-179's reported row; item 8 refines D-193 and D-231; items 9 and 15 amend D-235 item 7; item 10 refines D-197; item 11 amends D-240 item 2; item 12 refines D-191; items 13 and 14 close D-235 item 12; item 16 amends D-233 (2026-10-27 amendment) and D-240 item 4 as implemented; item 17 amends D-180's implementation note; items 18 and 19 implement and refine D-194; item 20 amends D-144; item 26 refines items 7 and 13. Item 24 is recorded for D-236. The Workflow #4787 mirror hand-off (`artifacts/orders-stage3/WORKFLOW-4787-MIRROR.md`, section "Maintainer-review run (D-246, 2026-10-29)") lists what Workflow must change for items 1, 7, 11, 13–16, 19 and 26.

## D-247 — Main's D-227…D-230 are restored; this branch's decisions move to D-231…D-246

**Status:** selected 2026-10-30 by the owner ("restore main's four entries and renumber the branch's decisions"). Pending design review. Record and citation change only: no code behaviour, wire or stored data changes.

1. **The collision.** `main` holds D-227…D-230 from the S2 work (PR #5259). This branch had numbered its own Stage 3 decisions from D-227 as well, and every merge of `main` kept the branch's entries, so this PR deleted main's four. They are restored here word for word, in place, and index row L lists them again.
2. **The renumbering.** This branch's decisions move up by four. Every citation in this gear's documents, code comments, tests and the Orders e2e suite moves with them; a line that `main` also carries keeps main's meaning. Commit messages, and PR #5536 or Workflow #4787 text written before 2026-10-30, use the old numbers; read them through this table.

| Old | New | Title |
|-----|-----|-------|
| D-227 | D-231 | Stage 3 port inventory, typed unavailability and the S3-F01/F02/F08 reconciliation |
| D-228 | D-232 | The payer relationship refusal is a gate predicate result |
| D-229 | D-233 | The all-failures gate report is one optional member of the refusal diagnostics |
| D-230 | D-234 | Definitive local refusals outrank unavailability in the gate's primary |
| D-231 | D-235 | Milestone 2: development delivery, engine diagnostics hook, submit protocol bindings |
| D-232 | D-236 | Commercial attempt recovery: scope and the open abandonment question |
| D-233 | D-237 | Gate observability, the gear-local breaker, alert baselines, fault injection and the activation re-check vectors |
| D-234 | D-238 | The submit REST route in the development delivery and the end-to-end development build |
| D-235 | D-239 | A declarative test-host PDP for the Orders end-to-end suite |
| D-236 | D-240 | Milestone 3c gate behavior, wire additions and remaining prerequisites |
| D-237 | D-241 | Residual follow-through, proposed mirrors and runtime evidence |
| D-238 | D-242 | Adopt PR5516 protected semantics and publish proposed integration contract |
| D-239 | D-243 | A maintenance block must schedule the retention purge |
| D-240 | D-244 | PR #5536 toolkit-pr-review findings: fixes, accepts, deferrals |
| D-241 | D-245 | Orders reads Rating's exact money with main's fraction API |
| D-242 | D-246 | PR #5536 maintainer review (macrocheese1, 2026-10-29): fixes |

3. **The restored decisions on this branch.**
   - D-227 (gear-local fake PDP for in-crate tests): not carried yet. This branch's `authz/test_pdp.rs` keeps the recording double and the real rules provider behind the resolver API, not main's `FakePdp`. Reconciling the two is open; this decision changes neither.
   - D-228 (live E2E removed from PR #5259 until a follow-up): this branch is that follow-up. The Orders suite is `testing/e2e/suites/bss/orders` (D-238, D-239).
   - D-229 (#5381-dependent tests compiled out): its exit condition is met. #5381 is on `main`, and since the merge of `main` at `65a140992` this branch builds those tests with no `orders_event_broker_test_support` cfg and no `build.rs`, using `event_broker::test_support` directly.
   - D-230 (`\xHH` escapes in event schema patterns): unchanged. `infra/events/schema.rs` is main's file again and cites it.
   - Main's amendment notes that cite these four (the status lines of D-211, D-214, D-215, D-222, D-223 and D-226, and D-224 with its `make test-integration LANE=orders-pg` target) are restored as main wrote them; they record what happened on PR #5259. UPSTREAM_REQS §2.9 keeps this branch's text, which supersedes main's: the live E2E is back on the test-host plugin (D-239).
4. **Workflow mirror.** Workflow #4787 cites this gear's decisions as "Lifecycle D-…" and "Orders D-…"; those citations move by the same table in the Workflow change that accompanies this one.

## D-248 — The in-crate tests use main's gear-local `FakePdp`; the test-host plugin stays the e2e PDP

**Status:** selected 2026-10-30 by the owner. Pending design review. Test code, a dev-dependency and docs only: no production code, REST wire, stored data or migration changes. Carries D-227; amends D-239 item 4 and D-247 item 3.

1. **The problem.** `main` decided (D-227) that the in-crate tests answer PDP calls with the gear-local `authz::test_pdp::FakePdp`. This branch never carried it (D-247 item 3): its `test_pdp.rs` replaced `FakePdp` with `RulesProvider`, which compiled JSON policies with the Orders e2e test-host plugin crate `bss_orders_test_authz_plugin` (D-239 item 4), and a gear test parsed the e2e fragment into the plugin's config type. The in-crate tests therefore depended on an e2e fixture crate, and the branch's `test_pdp.rs` and four PostgreSQL-tier suites differed from main's for no recorded reason.
2. **What changed.** `authz/test_pdp.rs` is main's file again: `Grant` and `FakePdp` exactly as `main` has them (deny by default, `property IN values` paths, delegated grants, `delegation_proof_required` / `delegation_proof_invalid` codes), and `RulesProvider` is gone. The PostgreSQL-tier suites `infra/storage/tests/{authz,capture,engine,reads,submit}.rs` build `Grant`s instead of JSON policies: `authz.rs` and `capture.rs` are main's files again (one capture comment keeps this branch's wording), `engine.rs` and `reads.rs` take main's grant builders and keep this branch's added tests, and the branch-only `submit.rs` follows the same style (the buyer's grant with and without `OrderSubmit`). Each JSON rule became one `Grant` with the same subject, actions, paths and delegation; no rule needed anything `FakePdp` does not do, and no test asserted a plugin-only decision detail. Test names, subjects and assertions of the PostgreSQL-tier suites are unchanged. `gear/tests.rs`'s `e2e_fragment_grants_event_broker_produce_to_exactly_the_orders_producer` reads the parsed YAML of `config/e2e-orders.yaml` as D-227 item 4 has it: the `bss-orders-test-authz-plugin` block holds exactly one unconditional grant and one rule, both name exactly the configured producer principal (subject and tenant), the grant is the submitted event's `event_type` resource × `produce`, and the rule is `request` × `produce` with one `owner_tenant_id` predicate on one tenant; it no longer calls the plugin's `validate` and now also pins the block's sections, a single predicate and a single tenant value. The `bss-orders-test-authz-plugin` dev-dependency is dropped from `orders/Cargo.toml` and `Cargo.lock`; the plugin crate, the example server and the fragment are untouched.
3. **What the fake does not prove.** As D-227 item 6: the plugin's policy language, its rule matching beyond subject ID and exact resource/action, subject tenant, unconditional grants, priorities, deny precedence and per-rule proof lists. That evidence stays with the plugin's own unit tests and the live e2e suite (`make e2e-orders`), which still selects the plugin (D-239 item 5). The plugin's schema and `validate` of the fragment are the plugin's tests and the e2e host's start, not the gear test's.
4. **Amendments.** D-239 item 4's "Orders dev-depends on the plugin (452fadd71) and its in-crate rules PDP double runs on it" no longer holds; its known-error count is already history, since main's Event Broker `test_support` (#5381) is merged and the lib test target compiles and runs. D-247 item 3's "D-227 … not carried yet" is closed by this decision.
5. **Evidence.** `cargo nextest run -p cf-gears-bss-orders --features gate-dev-stubs --run-ignored all --no-fail-fast` (Docker): 866 passed, 0 skipped, the same count as before; with `--features gate-dev-stubs,serde_json/preserve_order`: 866 passed. `cargo nextest run -p cf-gears-bss-orders-sdk`: 53 passed. `cargo clippy -p cf-gears-bss-orders --all-targets --all-features -- -D warnings` and `cargo fmt --check -p cf-gears-bss-orders` are clean, and `grep -rn bss_orders_test_authz_plugin gears/bss/orders/orders` finds nothing.
