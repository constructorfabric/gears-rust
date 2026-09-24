<!-- CONFLUENCE_TITLE: [BSS]: Orders Lifecycle — Design Decisions Register -->
<!-- Related: ./DESIGN.md, ./features/, ./ADR/ | Owners: BSS Orders team -->

# Design Decisions — Orders Lifecycle

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
  - [D-12 (H) Rows disambiguated and the PRD's two approved edges restored *(autonomous, fixes R-08 and R-09)*](#d-12-h-rows-disambiguated-and-the-prds-two-approved-edges-restored-autonomous-fixes-r-08-and-r-09)
  - [D-13 (M) The spawn signal is permanent *(autonomous, fixes R-10)*](#d-13-m-the-spawn-signal-is-permanent-autonomous-fixes-r-10)
  - [D-14 (M) Draft auto-void targets expired and is called auto-void *(autonomous, fixes R-11; carries ADR-0004)*](#d-14-m-draft-auto-void-targets-expired-and-is-called-auto-void-autonomous-fixes-r-11-carries-adr-0004)
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
  - [D-82 (M) The version reason vocabulary is {create, submit, amendment}](#d-82-m-the-version-reason-vocabulary-is-create-submit-amendment)
  - [D-83 (H) The in-flight order cap stays at one; route (b) does not resolve Q-05 *(carries ADR/0007)*](#d-83-h-the-in-flight-order-cap-stays-at-one-route-b-does-not-resolve-q-05-carries-adr0007)
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
  - [D-139 (M) An invalid cursor is cursor-invalid, under one cursor contract for all five paged collections](#d-139-m-an-invalid-cursor-is-cursor-invalid-under-one-cursor-contract-for-all-five-paged-collections)
  - [D-140 (H) sales_path is partner_placed iff the allowed create carried a delegation proof reference](#d-140-h-sales_path-is-partner_placed-iff-the-allowed-create-carried-a-delegation-proof-reference)
  - [D-141 (M) A targeted request answers order-not-found on a delegation-proof denial; only untargeted requests disclose the reason](#d-141-m-a-targeted-request-answers-order-not-found-on-a-delegation-proof-denial-only-untargeted-requests-disclose-the-reason)
  - [D-142 (M) A boundary validation failure with no more specific reason is request-invalid; a no-op administrative edit is refused](#d-142-m-a-boundary-validation-failure-with-no-more-specific-reason-is-request-invalid-a-no-op-administrative-edit-is-refused)
  - [D-143 (M) Caller-supplied explanations go in a nullable audit caller_reason, covered by audit hash v2](#d-143-m-caller-supplied-explanations-go-in-a-nullable-audit-caller_reason-covered-by-audit-hash-v2)
  - [D-144 (M) The composed read carries the activation re-check's fulfillment inputs](#d-144-m-the-composed-read-carries-the-activation-re-checks-fulfillment-inputs)
  - [D-145 (M) A PATCH selects its trigger from field classes alone; a commercial edit outside draft is not-admissible](#d-145-m-a-patch-selects-its-trigger-from-field-classes-alone-a-commercial-edit-outside-draft-is-not-admissible)
  - [D-146 (H) The submit-time automatic acceptance keys on the submit request, and the recording-party bar also keys on stored actor tenants](#d-146-h-the-submit-time-automatic-acceptance-keys-on-the-submit-request-and-the-recording-party-bar-also-keys-on-stored-actor-tenants)
  - [D-147 (M) expected_draft_revision is optional at the boundary for draft-mutate and compared only after admissibility](#d-147-m-expected_draft_revision-is-optional-at-the-boundary-for-draft-mutate-and-compared-only-after-admissibility)
  - [D-148 (M) A committed audit entry's reason is one closed token per trigger](#d-148-m-a-committed-audit-entrys-reason-is-one-closed-token-per-trigger)
  - [D-149 (M) An administrative edit that changes nothing refuses administrative-edit-unchanged, keeping request-invalid boundary-only](#d-149-m-an-administrative-edit-that-changes-nothing-refuses-administrative-edit-unchanged-keeping-request-invalid-boundary-only)
- [High-register reconciliation (2026-09-23)](#high-register-reconciliation-2026-09-23)
- [Medium-register reconciliation (2026-09-23)](#medium-register-reconciliation-2026-09-23)
- [Open questions](#open-questions)
- [Traceability](#traceability)
- [Documentation review history](#documentation-review-history)

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
the 2026-09-09 to 2026-09-11 waves and from the review of PR #4775. **Thirty-one items are
routed as open questions** (`Q-01`…`Q-31`), of which twenty-six are routed and unanswered — Q-10
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
| Open questions | Q-01…Q-31 | — | 26 routed and unanswered; Q-10 out of scope for this gear; Q-02, Q-13, Q-14 and Q-19 closed without a Product decision (D-84; the PRD's own reason-phrase usage; a design correction; withdrawal of the Orders re-drive surface) |

## A. Foundational shape

### D-01 (H) The engine owns every state change *(autonomous)*

**Decision**: one Order Transition Engine owns the aggregate, the append-only version chain, the
state-machine table, guard evaluation, the idempotency registry, the version check, the audit
store, the typed event contract and the reason registry. Slices declare guards and supply document
contributions and never write order state.

**Rationale**: four `p1` NFRs are properties of how a state change commits, not of any
capability. Full alternatives analysis in [`ADR/0001`](./ADR/0001-cpt-cf-bss-orders-lifecycle-adr-transition-through-engine.md).

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
The alternatives remain in [ADR-0002](ADR/0002-cpt-cf-bss-orders-lifecycle-adr-slice-decomposition.md).
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
the system — [`ADR/0003`](./ADR/0003-cpt-cf-bss-orders-lifecycle-adr-fail-closed-gate.md)'s
fail-closed posture makes it so — unaudited and unsettled. Step 3.1 now **opens a refusal
transaction of its own**, taking the row lock, resolving or creating the idempotency record,
settling it with the guard's registered unevaluable reason, appending the refused-attempt audit
entry and committing. The open finding of review wave 4 (`F2-SEM-007` / `Rc2-025`) is resolved,
and "without exception" holds as written; the only remaining scoping caveat is authorization
denial, which audits and commits but deliberately does not settle a caller-supplied key
([01 §4.1](DESIGN.md#contract-01-4-1), [`ADR/0005`](./ADR/0005-cpt-cf-bss-orders-lifecycle-adr-refusals-commit.md)).

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
| D-26 | [H] | *(carries [`ADR/0007`](./ADR/0007-cpt-cf-bss-orders-lifecycle-adr-in-transaction-concurrency.md))* `overlap_scope_key` is persisted on the line and `orders_inflight_overlap_claim` enforces one open payer/key claim with a partial unique index inside the transition transaction; the gate predicate remains as the friendly pre-check | The rule was resolved outside the transaction with no constraint behind it, so two concurrent identical submits both passed (R-30); root state cannot be indexed from an append-only line, so the claim table owns the mutable lifecycle | [01 §3.7](DESIGN.md#contract-01-3-7); [03 §4.2](features/03-gate-and-pin.md#contract-03-4-2) predicate 9 |
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
| D-41 | [H] | *(carries [`ADR/0006`](./ADR/0006-cpt-cf-bss-orders-lifecycle-adr-outbox-publication.md))* Capacity baselines: 50 order transitions/second peak, 200 platform-produced events/second, 16 toolkit producer-queue partitions with the high-throughput profile, ~11 Orders rows per order at version 1 and ~5 per amendment, archival tier triggered at 24 months past terminal. **Delivery target reopened:** 30 seconds p95 is an unapproved proposal; the Lifecycle PRD's p95 < 1 s durable-write-plus-publish baseline governs until Product/Architecture approves a change under Q-16 | R-52/R-53 exposed missing capacity and delivery thresholds. The 30-second value was borrowed from Orders Workflow PRD §7.1's process-event class, not derived from Lifecycle measurements or approved as a relaxation. Asynchronous publication does not itself rule out sub-second delivery. | `DESIGN.md §4`; [01 §1.2](DESIGN.md#contract-01-1-2), `§3.8` |
| D-42 | [H] | Per-port deadlines (250 ms each for catalog predicates, frontier and pin composition, 250 ms identity, 500 ms evaluation, 250 ms overlap, 250 ms contracts, 250 ms Preview-only tax) inside a **2 s submit** budget over the seven submit-path operations and a **2.25 s Preview** budget over all eight operations; bounded retry with two attempts on transient failure only; a breaker per port opening on a rolling failure ratio and mapping to that port's existing fail-closed reason; a concurrency bulkhead per port; and rate limits declared in the operation specs. The platform producer queue uses **16 toolkit partitions and the high-throughput profile** | Only unavailable ports were handled and nothing specified a slow one, while five synchronous calls sat on the request path; and producer throughput needed an explicit scalable platform profile rather than an Orders-owned single worker (R-54, R-55) | [03 §2.1](DESIGN.md#contract-03-2-1), `§3.3`; [08 §3.5](DESIGN.md#contract-08-3-5); [01 §3.6](features/01-foundation.md#contract-01-3-6), `§3.8` |
| D-43 | [H] | Synchronous commit to a quorum with one standby in a second failure domain **inside** the residency boundary; nightly base backup with continuous WAL archiving for point-in-time recovery; RTO ≤ 60 min met by standby promotion, with a named DR drill each release | RPO zero and RTO ≤ 60 min were asserted with one mechanism that constrained nothing about surviving loss of the primary, and `§4` referred to "DR replicas" never specified (R-56) | `DESIGN.md §3.8`, `§4`; [01 §3.8](DESIGN.md#contract-01-3-8) |
| D-44 | [H] | **Erasure clause superseded by D-96; other provisions retained.** Data protection: encryption at rest by the platform's storage layer, TLS in transit on every hop, keys held in the platform KMS with the gear holding none, order content classified **commercial-confidential** and actor identifiers **personal-minimal**, no masking requirement since no surface returns another tenant's data, and erasure satisfied by pseudonymising actor identifiers in place — the one permitted mutation of the audit store, itself audited | The whole of data protection returned zero hits and the only explicit non-applicability in the set was PCI DSS, which the checklist's evidence standard treats as a violation rather than an exemption (R-57) | `DESIGN.md §4` |
| D-45 | [H] | A threat table is added covering the three tenancy axes, the partner-placed path, the Workflow-only operations, the outbound ports and the Preview surface, each with vector, boundary crossed, mitigation and residual risk | There was no threat model — one sentence naming one threat — while the threats the design named elsewhere were never mapped to mitigations (R-58) | `DESIGN.md §4` |
| D-46 | [H] | The audit store gains a predecessor-hash column forming a per-order chain, plus explicit revocation of UPDATE and DELETE on the audit role; the chain is verified by a periodic job | The PRD requires a tamper-**evident** record; `(order_id, sequence)` uniqueness detects nothing, and "append-only" was a property with no enforcement while the runtime holds database privilege (R-59) | [01 §3.7](DESIGN.md#contract-01-3-7), `§4.4`; `DESIGN.md §1.2` |
| D-47 | [H] | Each slice gains an observability subsection naming its own metrics, log fields and alerts; latency-SLO alerts are added for both budgets with a burn-rate policy; the tracing propagation contract across the seam and the ports is stated; readiness and liveness are distinguished | Observability was specified once, at gear level, entirely around engine-owned signals, so the risky work was unmonitored — and the alert list contained no alert on either latency SLO (R-60, R-65) | `DESIGN.md §4`; every slice `§3.8` |
| D-48 | [H] | An "Extension points and stability" section is added to `01` naming what a slice may add without an engine change — guards, reasons, contributions, policy rows — versus what requires one: a state, an edge, an event type or a schema column. `DESIGN.md §3.3` gains an API-evolution subsection with the stability ladder, the breaking-change definition and a deprecation window | The engine is deliberately closed and the design never said so, while all endpoints were uniformly "unstable" with no promotion criterion and the PRD delegates the major-version mechanism to Design (R-61) | [01 §4.6](DESIGN.md#contract-01-4-6); `DESIGN.md §3.3` |
| D-49 | [M] | Refusal audit rows carry a 90-day retention distinct from committed transitions, and repeated refusals against one order are rate-limited | Unbounded refusal auditing let any caller grow the audit store and slow every audit read on that order (R-62) | [01 §3.7](DESIGN.md#contract-01-3-7); [08 §4.5](DESIGN.md#contract-08-4-5) |
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
`POST /bss-orders-lifecycle/v1/outbox/dead-letters/{eventId}/re-drive`, is removed from the API
inventory, authorization matrix and foundation interface. Producer dead-letter operations use the
shared platform operator interface and SDK recovery mechanism required by
`cpt-cf-bss-orders-lifecycle-upreq-event-broker-dead-letter-recovery`
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
[`ADR/0004`](./ADR/0004-cpt-cf-bss-orders-lifecycle-adr-closed-enumerations.md), which
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

**Decision**: [01 §4.4](DESIGN.md#contract-01-4-4) declares one common summary block — `orderId`, `orderVersion`,
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
analysis in [`ADR/0003`](./ADR/0003-cpt-cf-bss-orders-lifecycle-adr-fail-closed-gate.md).

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

### D-83 (H) The in-flight order cap stays at one; route (b) does not resolve Q-05 *(carries [`ADR/0007`](./ADR/0007-cpt-cf-bss-orders-lifecycle-adr-in-transaction-concurrency.md))*

**Decision**: gate predicate 9 and `orders_inflight_overlap_claim` enforce **exactly one**
in-flight order per `(payer_tenant_id, overlap_scope_key)`, by a partial UNIQUE index on that pair.
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
an explicit `ContractError` code under `orders-lifecycle.v1` and one platform canonical category.
That category determines wire `type`, HTTP status and title; the GTS reason key remains registry
metadata. Canonical preconditions use 400, concurrency conflicts 409 and dependency outages 503.
`authorization-context-changed` retains 409 with canonical title "Aborted", its specific code
and sanitized detail. The complete gate failure report remains available; its primary error is
the first unavailable result, otherwise first failed result, in declared predicate order.
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

**Decision**: `orderId` is the GTS event partition key, so one order's events route to one Event
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

**Decision**: the order axis of the overlap rule is closed in-transaction by
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

**Decision**: **Layer 1** is the per-state TTL, measured from `state_entered_at`, which a resume
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
`cpt-cf-bss-orders-lifecycle-upreq-event-broker-root-tenancy`
([`UPSTREAM_REQS.md §2.7`](./UPSTREAM_REQS.md#27-event-broker)).

**Propagated**: [01 §4.4](DESIGN.md#contract-01-4-4), `§4.7`, `§3.8`; [08 §4.3](DESIGN.md#contract-08-4-3); `DESIGN.md §4.5`; `UPSTREAM_REQS.md §2.7`.

### D-96 (H) Audit actor references are immutable; identity lifecycle is separate

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
`cpt-cf-bss-orders-lifecycle-upreq-audit-identity-lifecycle`. **Amended by D-103:** this is a
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

**Verified precedent**: Pricing upstream `main` at
`8aca4d6df17c90f8ecb8b0195e4db4ec40921937` (checked 2026-09-21).
[`Pricing Governance`](../../pricing/docs/design/05-governance.md#audit-trail-and-retention)
records D-14/D-135. The implemented
[`audit writer`](../../pricing/pricing/src/infra/storage/repo/audit_repo.rs) receives the caller's
transaction; the [migration](../../pricing/pricing/src/infra/storage/migrations/m20260821_000007_create_pricing_audit_log.rs)
creates the segmented chain and UPDATE/DELETE rejection triggers;
[`audit_read`](../../pricing/pricing/src/infra/audit_read.rs) provides authorized local retrieval.
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

**Pricing alignment**: reuse the encoding discipline in
[`Pricing domain/audit.rs`](../../pricing/pricing/src/domain/audit.rs), not its exact bytes or
private helper code. Orders has its own tag, columns and version and starts at 1 rather than
Pricing's 0. There is no tenant-wide chain lock and no broker dependency in verification.

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

**Status amended by D-103**: `cpt-cf-bss-orders-lifecycle-upreq-audit-identity-lifecycle` remains
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
operation is raised upstream as `cpt-cf-bss-orders-lifecycle-upreq-catalog-subscription-product-key`,
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

**Decision**: [01 §4.3](features/01-foundation.md#contract-01-4-3) gains rows 26 and 27. Row 26 is `on_hold → fulfillment_failed` on
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

**Decision**: [01 §3.7](DESIGN.md#contract-01-3-7) defines one closed actor-class enumeration shared by
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
`DESIGN.md §1.1`, `§3.3`; [08 §4.3](DESIGN.md#contract-08-4-3); `ADR/0001-cpt-cf-bss-orders-lifecycle-adr-transition-through-engine.md`.

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
raised as `cpt-cf-bss-orders-lifecycle-upreq-pricing-catalog-tenant-reads` (p1). Preview and
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
The field is named in the Pricing ask `cpt-cf-bss-orders-lifecycle-upreq-pricing-catalog-tenant-reads`.

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
`cpt-cf-bss-orders-lifecycle-upreq-contract-acceptance-declaration` (p1), sits in
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

**Decision**: `/workflow-cancel` requires complete compensation evidence whether or not the spawn
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

**Decision**: [06 §3.6](features/06-workflow-seam.md#contract-06-3-6) *Acknowledge Fulfillment* takes `correlation_id`, as every seam call does,
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
(`libs/toolkit-odata/src/problem_mapping.rs`). It is not in the `orders-lifecycle.v1` error
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

**Decision**: `orders_transition_audit.reason` on a committed entry holds exactly one closed
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
  without demanding ownership of the seller. Slices contribute guards/results, never return a
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
| Q-04 | The `SUB-O*` register has forked between the Subscriptions seam map (`O1`–`O6`) and the Workflow PRD (`O1`, `O5`–`O9`), with `O6` carrying two meanings. Which numbering is canonical? | Architecture | This gear cites the seam-map numbering and adds `SUB-O10` (D-56); `UPSTREAM_REQS.md` records both readings |
| Q-05 | The default `overlapScopeKey` — `(payerTenantId, catalogSubscriptionProductKey)` at cardinality one — refuses a partner buying the same product for a second customer tenant. **Two upstream routes exist, and this register had recorded only the first**: (a) bind a resource dimension into the key, or (b) leave the key and raise `maxConcurrentActive`, which `subscriptions/docs/SEAMS.md` **SUB-G1** already says "may come from Catalog **or** Contract". Route (b) needs no key change at all. The key's shape is registry-owned by the `products` gear and SUB-G1 names the venue: **engage on PR #4177 before merge** | Architecture with Subscriptions | **Route (b) alone does not resolve this** (D-83): PRD §6.1(g) caps in-flight orders at one with no configurability clause, separately from §6.1(f)'s configurable subscription cardinality, so raising `maxConcurrentActive` lets the partner hold more concurrent subscriptions while still placing their orders **serially**. The choice is therefore route (a), accepting serialised ordering, or amending §6.1(g). The key is applied as adopted and this design must not fork the default locally. Subscriptions' **SUB-O5** already records the same collision with the same compensation-path reasoning, so the problem is understood on both sides of the seam — what is unagreed is only which of the two routes is taken |
| Q-06 | Per-state TTL defaults and whether seller scope may override platform scope | Product | No code default; an unconfigured state is not swept. The override mechanism is specified and ships disabled behind the gear-level `ttl_seller_override_enabled` flag, default off (D-137), so the override-scope answer becomes configuration: a yes turns the flag on, a no leaves it off, and neither needs a design change |
| Q-07 | **PRD §15 row 5 carries two values and this row tracks both.** (a) The program retention period for completed and cancelled orders. (b) The **`draft` auto-void TTL**, which [07 §4.5](DESIGN.md#contract-07-4-5) routes here and which is the more urgent half: while it is unset the draft sweep does no work, basket accumulation is **unbounded**, and there is **no fallback** — the absolute-lifetime backstop that previously supplied one was withdrawn by D-90. `draft` therefore has the largest exposure of any state to an unanswered value, and unlike the §15 row-7 TTLs it bounds storage rather than a commercial promise | Product | (a) Append-only with no destructive path; retention deferred to the policy. (b) No code default, per [07 §2.2](DESIGN.md#contract-07-2-2) — a default would become the platform answer. The condition is visible on the draft-age distribution ([02 §3.8](DESIGN.md#contract-02-3-8)) and the no-configured-TTL alert ([07 §3.8](DESIGN.md#contract-07-3-8)) |
| Q-08 | The minimum payment-outcome surface: a declined instrument's exit (currently expiry only) and refund-as-reversal after capture | Architecture with Product | Authorization-only, stated as a limitation in [05 §4.4](DESIGN.md#contract-05-4-4) |
| Q-09 | Is the Generic Approval service specified by its own PRD, or by a transitional module-local pattern? | Architecture | Stand-in behind the expectations contract; every verdict carries its deciding authority (D-73) |
| Q-10 | The durable-execution engine for the sibling Workflow gear | Architecture | Out of scope for this gear; nothing in the engine depends on it |
| Q-11 | **End-to-end submit latency needs boundary clarification and validation.** The PRD requires durable write plus event publish at p95 < 1 s (§7.1, §12), while [03 §2.2](DESIGN.md#contract-03-2-2) allows up to **2.25 s** for pre-transaction submit port resolution (2.5 s on Preview, which also calls the tax port; both raised by 250 ms for the catalog product-key operation, D-108). Those timeout budgets do not prove achieved p95 and do not exempt guard resolution from caller-visible latency | Product with Architecture | Measure request-to-commit and the complete request-to-broker path per `DESIGN.md §4.1`; attribute guard-resolution time as required by [03 §1.2](DESIGN.md#contract-03-1-2). Resolve the governing boundary jointly with Q-16. The design claims neither sub-second buyer response nor PRD compliance from commit-only measurements. |
| Q-12 | PRD §6.1 says amendments from `submitted` and `pending_approval` **do not change order state**, yet D-61 transitions a `pending_approval` amendment to `submitted`; its diagram also declares a direct `approved → pending_approval` edge while this design reaches that state in two steps because the direct edge's guard is unobtainable. Separately §10 UC-002 step 3 and §12 AC-5 require an amendment from `pending_approval` to return to its pre-approval state, while §5.1 and §6.2 scope that clause to `approved` only | Product with Architecture | The two-step shape and the `pending_approval → submitted` divergence are disclosed in [01 §4.3](features/01-foundation.md#contract-01-4-3) and [04 §4.3](features/04-versioning.md#contract-04-4-3); the PRD state rule, diagram and §12 AC-5 need reconciling, or a verdict port must be specified and AC-11a relaxed |
| Q-13 | **Closed — no Product decision required; the PRD's own usage settles it.** §12 requires rejecting a superseded version "with a machine-readable **stale-version** reason", and D-59 read that as a descriptor rather than a minted identifier, resolving it to the engine's `version-conflict`. That reading is not an interpretation among several: §12 uses the **identical construction** four more times — "a machine-readable **business-level** reason code", "a machine-readable **business** reason", "a machine-readable **business-level** reason indicating the mixed-currency basket", "a machine-readable **business-level** reason indicating fulfillment has already spawned a subscription" — and in none of those four is the adjective phrase a reason name. A reading that mints `stale-version` would have to mint `business-level` too. So the phrase names the *kind* of reason, `version-conflict` is a reason of that kind, and the `MUST` is met | Design | [01 §4.2](features/01-foundation.md#contract-01-4-2) holds the descriptor-to-identifier mapping so a later reader does not restore the descriptor as a name and reintroduce the duplication D-38 removed. Product is **notified, not asked**: if §12 is ever intended to mint identifiers it should say so for all five phrases, which would be a PRD change and not a design one |
| Q-14 | **Closed — no Product decision required.** PRD §6.1 gives the contract-effective date's default as **submit time**. The design now retains an explicitly authored date in draft and, where it remains absent, resolves it at submit from the commit instant before deriving dependent dates | Design | [02 §4.2](features/02-capture.md#contract-02-4-2) now implements the PRD rule; the resolved values are stored with the admitted version and no authoring-time/default divergence remains |
| Q-15 | ADR-0003's fail-closed posture means **no submit passes the gate** until `SUB-O5`, GA/prepaid and registry inputs, and the batched fixed-version Pricing SDK operations exist; bundle purchases additionally require frozen composition and component-conjunction integration, so operators will see submits refused for predicates not yet built upstream. That behaviour is designed, but it is product-visible and nobody outside this design has agreed it | Product with Architecture | Fail closed is specified in D-72 with ADR-0003; the phase map in `DECOMPOSITION.md` states the consequence |
| Q-16 | **Delivery target reopened.** Confirm the measurement boundary and feasibility of the PRD's p95 < 1 s durable-write-plus-publish baseline (§7.1, §12 AC-17), and decide whether a separate publication budget is justified. D-41's 30 s p95 was borrowed from Workflow and is unapproved for Lifecycle; asynchronous publication alone does not make sub-second delivery impossible | Product with Architecture | The PRD baseline remains governing. `DESIGN.md §4.1` separates request-to-commit, commit-to-broker acceptance and downstream processing, requires full-path measurement at expected load with backlog/retries, and keeps incomplete deliveries visible. Confirm population, observation window and tail criteria; approve and propagate any changed target only after reviewing the evidence. Delayed-delivery and dead-letter monitoring remain mandatory. |
| Q-17 | PRD §16's pin-staleness risk asks for an "acceptable staleness window" to be documented in the NFR workshop. The design carries the other two mitigations (re-pin on amendment, downstream seal) and no window; the de facto bound is the per-state TTL, itself unset under Q-06 | Architecture with Product (NFR workshop) | [03 §4.3](DESIGN.md#contract-03-4-3) now names the TTL as the bound rather than asserting staleness is "bounded" |
| Q-18 | PRD §11's Order Console step 4 lists **hold** among a Partner Admin's actions, while §5.1 and §6.3 name only the seller operator and Orders Workflow as hold actors. This design follows the stricter reading and denies Partner Admin hold | Product | Recorded in [08 §4.3](DESIGN.md#contract-08-4-3); either §11's step list or the §5.1/§6.3 actor set needs correcting |
| Q-19 | **Closed — no Product decision required.** The design-introduced Orders outbox re-drive endpoint had no PRD basis | Design | Removed by D-17/D-58 when the design adopted the platform producer outbox. Operations use toolkit-db dead-letter facilities; no Orders business API remains to acknowledge |
| Q-20 | The **audit read** has no FR basis (D-70): PRD §6.1 and the audit NFR oblige the system to *record*, not to *expose*, and §9.1 contains no retrieval operation. It exposes actor identities, delegation-proof references and correlation identifiers | Product | Specified in the [audit interface contract](DESIGN.md#contract-08-3-3); acknowledge it, or scope what the surface may return |
| Q-21 | **Preview persists a gate-outcome row per predicate per line** (D-52) against PRD §9.1's specification of Preview as creating and mutating **no state** | Product | Implemented with a 7-day retention and a rate limit ([03 §4.6](features/03-gate-and-pin.md#contract-03-4-6)); amend §9.1's wording, or drop the persistence and lose the Preview diagnostics |
| Q-22 | Row 6, **`draft → expired`** on the auto-void TTL, is an edge PRD §6.1's normative state diagram does not contain; §7.1 says only that abandoned drafts *SHOULD* be auto-voided (D-14). `OrderExpired` consequently carries two commercially different facts | Product with Architecture | Implemented and disclosed in [01 §4.3](features/01-foundation.md#contract-01-4-3); amend the §6.1 diagram, or introduce a twelfth state and a twelfth event, which ADR-0004 rejected |
| Q-23 | **Ten endpoints the PRD describes in §6 but omits from §9.1** mean §9.1 is no longer the normative operation set (`DESIGN.md §3.3`). Each has an FR basis, so this is a wording gap rather than a scope extension | Product | The endpoints are specified and inventoried; §9.1 needs the amendment `DESIGN.md §3.3` already calls for |
| Q-24 | The **version reason vocabulary** is `{create, submit, amendment}` (D-82) against PRD §6.2's MUST-level eight-value list; the other six name state-only transitions and live on `orders_transition_audit.reason` | Product | The split is implemented and stated in [04 §4.5](features/04-versioning.md#contract-04-4-5); amend §6.2 so its enumeration matches the version/audit split PRD §1.4 already draws, or a conformance run against §6.2 fails on six values |
| Q-25 | **Event-contract PRD reconciliation.** Freshness reads before consumer business effects deliberately depart from §9.2's "without a callback read" wording (D-67); read traffic, service grants and unavailable-read recovery require agreement. **`OrderAmended`'s PRD trigger also no longer holds.** PRD §6.5 emits it "on creation of a new order version", and D-64 makes creation and submit version-appending rows that publish no `OrderAmended` — creation is event-less, submit publishes `OrderSubmitted`. It fires only on the three amendment rows | Product with Architecture and consumer owners | Specified and disclosed in [01 §4.4](DESIGN.md#contract-01-4-4) and [04 §3.3](DESIGN.md#contract-04-3-3); retain authoritative freshness checks pending agreement; amend §6.5 and §9.2 to name amendment as the trigger, or a conformance run against §6.5 fails on two of the five version-appending rows |
| Q-26 | The **operational limits this design set as working baselines** need ratifying against real capacity: the refusal rate limit (20/min per caller-order, 200/min per caller), the per-port bulkhead (32 in-flight), the breaker ratio (0.5 over 30 s, open 10 s), submit and Preview rate limits (10/min and 60/min per caller), the line cap (200), the toolkit producer queue count (16), high-throughput profile and 64 KiB envelope bound. Each was previously named as a mechanism with no value, so five separate risk mitigations rested on thresholds nobody had set | Architecture | Values are set in [01 §3.7](DESIGN.md#contract-01-3-7), [02 §4.5](features/02-capture.md#contract-02-4-5), [03 §2.2](DESIGN.md#contract-03-2-2), `ADR/0006` and measured by the load tests those sections name; they are baselines to revise, not guesses to keep |
| Q-27 | **An order in a state with no configured TTL never expires.** PRD §6.3 requires bounded lifetime; PRD §15 row 7 leaves the TTL values open; and this design takes no code default, so the requirement is unmet for exactly the states Product has not yet valued — including `draft`, whose auto-void TTL is Q-07. D-90 records why the absolute-lifetime backstop that previously masked this was withdrawn. This is therefore a **requirement blocked on an unanswered question**, not a design gap | Product | Disclosed in [07 §4.2](features/07-hold-and-expiry.md#contract-07-4-2), `§4.4` and `§4.5`, alerted per [07 §3.8](DESIGN.md#contract-07-3-8), and bounded on the one axis this design can close — the resume cap of D-90 stops the dwell being restarted without limit. Answering §15 row 7 and Q-07 closes it; no mechanism changes when they are answered |
| Q-28 | **D-62 refuses a cross-seller payer rebinding that PRD §6.1 requires be honoured.** The PRD says a payer change crossing seller scope "**MUST** follow the paired payer/seller rebinding semantics (ownership-transfer alignment, manifest §4.11)", and §12's acceptance criterion repeats it — "paired with seller rebinding where the change crosses seller scope". D-62 freezes `sellerTenantId` as commercial-frozen, which makes the paired half unexpressible, and refuses the cross-seller payer change with `payer-rebinding-requires-seller`. The divergence is deliberate and was taken to close a real hole (an unguarded amendment could rebind the selling party), but it narrows a PRD MUST and the register recorded it as a decision rather than routing it | Product + Architecture | The freeze and the refusal are implemented as D-62 states ([04 §2.2](DESIGN.md#contract-04-2-2), `§3.3`, `§3.6`); a cross-seller payer change is therefore **not supported** and a caller must cancel and re-place. Closing it needs one of three: amend §6.1 to match, specify an ownership-transfer transition that rebinds both axes together under its own guard and event, or accept the refusal as the answer. Nothing changes in this design until it is answered |
| Q-29 | **PRD §1.1 claims the order does "double duty as quote and order" with "validity/expiry [as] the per-state TTL", and no state in this design is a quote.** A commercial quote is a *priced, non-binding, time-bounded offer*. A `draft` carries no price ([02 §3.2](DESIGN.md#contract-02-3-2)); submit is where the price appears and on the self-service path submit **is** the commitment ([05 §4.2](features/05-preconditions.md#contract-05-4-2)); Preview prices a basket but persists only its per-predicate verdicts, so the figure it quoted is unrecoverable and bound for no period ([03 §4.6](features/03-gate-and-pin.md#contract-03-4-6)). A partner-led sale needing "valid for thirty days" must hold that price outside this SoR with its validity unenforced — the outcome §1.1 gives as the reason no separate quote artifact is needed | Product + Architecture | Disclosed in [03 §4.6](features/03-gate-and-pin.md#contract-03-4-6). This design **MUST NOT** close it locally by storing Preview's total and calling it an offer: an offer needs a validity rule, an expiry actor, a re-price rule and a binding-on-acceptance rule, none of which any document in this set carries. Closing it means amending §1.1 to stop claiming quote coverage, or specifying a priced offer artifact — a scope decision, not a design one |
| Q-30 | **The partner path cannot complete without a `resourceTenantId` principal who can reach an acceptance surface.** [05 §4.2](features/05-preconditions.md#contract-05-4-2) bars the placing and selling parties from recording acceptance — the only technical control against manufactured consent, and kept. But where the end customer has no platform credential at the point of sale, nobody may record it: begin-fulfillment refuses, the order rests in `approved`, and with the TTL unset (Q-06, Q-27) it never leaves. A partner-placed order can be commercially agreed offline and still be unfulfillable | Product + whoever owns partner onboarding | Disclosed in [05 §4.2](features/05-preconditions.md#contract-05-4-2). The platform **MUST** be able to present an acceptance action to a `resourceTenantId` principal for any order the partner path produces — a capability this gear does not own. No delegated or operator-attested route is offered, deliberately, since an attested route is the authority artifact D-31 found unspecified. The available mitigation is a seller-scope election of acceptance **not** required ([05 §4.1](features/05-preconditions.md#contract-05-4-1)), which the seller requests through platform operations and which takes effect by deployment promotion, not a runtime call (D-133) — a decision about evidence, to be made knowingly |
| Q-31 | **PRD §6.3 says "A held order **MUST** be resumable", and the design caps resume.** [07 §4.1](features/07-hold-and-expiry.md#contract-07-4-1) refuses row 22 with `resume-cap-exhausted` once `resume_count` reaches the design-owned cap (baseline 5; [07 §4.5](DESIGN.md#contract-07-4-5) admits no "unlimited" value). The cap closes the hold/resume loop that restarts the dwell (D-90), and D-109 kept it unconditional. So PRD §6.3's resumability is qualified by a design-owned count cap: a held order at the cap exits only by cancel, expiry or, for an `in_fulfillment`-origin hold, Workflow's rows 26 and 27 (fail or mediated cancel), and never by completion | Product with Design | Cap kept for every resume (D-90, D-109) and disclosed in [07 §4.1](features/07-hold-and-expiry.md#contract-07-4-1) and `§4.5`. Product either accepts the qualification or amends PRD §6.3; if the design instead exempted some resumes from the cap, it would reopen the uncapped loop D-90 removed |

## Traceability

- **PRD**: [`./PRD.md`](./PRD.md)
- **DESIGN**: [`./DESIGN.md`](./DESIGN.md) and [Decomposition](DECOMPOSITION.md)
- **ADRs**: [`./ADR/`](./ADR/) — `cpt-cf-bss-orders-lifecycle-adr-transition-through-engine`, `cpt-cf-bss-orders-lifecycle-adr-slice-decomposition`, `cpt-cf-bss-orders-lifecycle-adr-fail-closed-gate`, `cpt-cf-bss-orders-lifecycle-adr-closed-enumerations`, `cpt-cf-bss-orders-lifecycle-adr-refusals-commit`
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
references and agreement with the transition table. This change adds `make design-check`
and a docs-workflow CI job for mechanical invariants, with positive/negative checker fixtures.
They do not establish semantic correctness or runtime integration; remote CI execution must
still be observed after publication.

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

- **ADRs**: [`ADR/0002`](ADR/0002-cpt-cf-bss-orders-lifecycle-adr-slice-decomposition.md) the foundation-plus-seven-slices decomposition
