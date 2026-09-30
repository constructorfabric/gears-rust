<!-- CONFLUENCE_TITLE: [BSS]: Orders Workflow — Design Decisions Register -->
<!-- Related: ./design/, ./PRD.md, ./UPSTREAM_REQS.md | Owners: BSS Orders team -->

# Design Decisions — Orders Workflow

<!-- toc -->

- [How to use this document](#how-to-use-this-document)
- [A. Process engine (slice 01)](#a-process-engine-slice-01)
  - [D-01: Engine execution history is not the process-audit source of record](#d-01-engine-execution-history-is-not-the-process-audit-source-of-record)
  - [D-02: Four distinct bounds govern execution, not three](#d-02-four-distinct-bounds-govern-execution-not-three)
  - [D-03: The idempotency registry's outcomes are exhaustive, and a still-processing conflict is never inferred as success](#d-03-the-idempotency-registrys-outcomes-are-exhaustive-and-a-still-processing-conflict-is-never-inferred-as-success)
  - [D-04: The dead-letter record and the manual-task record are two separate objects; a failed step never grows a second inspectable object](#d-04-the-dead-letter-record-and-the-manual-task-record-are-two-separate-objects-a-failed-step-never-grows-a-second-inspectable-object)
- [B. Triggers and start (slice 02)](#b-triggers-and-start-slice-02)
  - [D-05: Trigger intake is a closed, exhaustive vocabulary, admission-checked before any instance spawns](#d-05-trigger-intake-is-a-closed-exhaustive-vocabulary-admission-checked-before-any-instance-spawns)
  - [D-06: A trigger observed for a superseded orderVersion is ignored, and terminating on supersession follows the same path as terminating on a terminal order event](#d-06-a-trigger-observed-for-a-superseded-orderversion-is-ignored-and-terminating-on-supersession-follows-the-same-path-as-terminating-on-a-terminal-order-event)
  - [D-07: Un-activated wave-1 drafts are voided on every termination path, including on supersession](#d-07-un-activated-wave-1-drafts-are-voided-on-every-termination-path-including-on-supersession)
  - [D-08: Correlation is three distinct identifiers, never conflated](#d-08-correlation-is-three-distinct-identifiers-never-conflated)
- [C. Approval execution (slice 03)](#c-approval-execution-slice-03)
  - [D-09: Approval idempotency keys are composed from orderId, orderVersion, and gateId](#d-09-approval-idempotency-keys-are-composed-from-orderid-orderversion-and-gateid)
  - [D-10: Escalation-timer ownership sits in this gear, undivided across the service boundary](#d-10-escalation-timer-ownership-sits-in-this-gear-undivided-across-the-service-boundary)
  - [D-11: Escalation-timer pause preserves accrued elapsed time on hold and on a Generic Approval outage, and never resets the window](#d-11-escalation-timer-pause-preserves-accrued-elapsed-time-on-hold-and-on-a-generic-approval-outage-and-never-resets-the-window)
  - [D-12: Verdict-source unavailability parks the process fail-closed in `submitted`, and the park never suspends the Lifecycle `submitted` TTL](#d-12-verdict-source-unavailability-parks-the-process-fail-closed-in-submitted-and-the-park-never-suspends-the-lifecycle-submitted-ttl)
  - [D-13: The verdict cache is keyed on orderId plus orderVersion, and a stale-version verdict is discarded rather than reflected](#d-13-the-verdict-cache-is-keyed-on-orderid-plus-orderversion-and-a-stale-version-verdict-is-discarded-rather-than-reflected)
  - [D-14: Every stored verdict names its deciding authority, and the phase-1 stand-in is recorded as that authority by name](#d-14-every-stored-verdict-names-its-deciding-authority-and-the-phase-1-stand-in-is-recorded-as-that-authority-by-name)
- [D. Fulfillment plan (slice 04)](#d-fulfillment-plan-slice-04)
  - [D-15: A pre-activation abort is structurally distinct from a line-execution failure](#d-15-a-pre-activation-abort-is-structurally-distinct-from-a-line-execution-failure)
  - [D-16: Dependency resolution is Catalog-owned, graph-validated, and frozen per orderId plus orderVersion before any provisioning intent; a bundle is one line item](#d-16-dependency-resolution-is-catalog-owned-graph-validated-and-frozen-per-orderid-plus-orderversion-before-any-provisioning-intent-a-bundle-is-one-line-item)
  - [D-17: Pending and failed payment authorization are two distinct, never-collapsed process outcomes](#d-17-pending-and-failed-payment-authorization-are-two-distinct-never-collapsed-process-outcomes)
  - [D-18: The downstream Subscriptions transition-request id is a join key on the fulfillment task, never mirrored into order state](#d-18-the-downstream-subscriptions-transition-request-id-is-a-join-key-on-the-fulfillment-task-never-mirrored-into-order-state)
- [E. Commercial policy — owned elsewhere, no code default](#e-commercial-policy--owned-elsewhere-no-code-default)
  - [D-19: Partial-failure policy overrides per product line or commercial tier are commercial policy, not a design default](#d-19-partial-failure-policy-overrides-per-product-line-or-commercial-tier-are-commercial-policy-not-a-design-default)
  - [D-20: Approver absence and delegation reassignment is commercial/process policy owned by Product, not resolved by this design](#d-20-approver-absence-and-delegation-reassignment-is-commercialprocess-policy-owned-by-product-not-resolved-by-this-design)
- [F. Provisioning intents (slice 05)](#f-provisioning-intents-slice-05)
  - [D-21: The idempotency key composes orderId, orderVersion, orderLineId, wave, and intentKind, and is structurally distinct from correlationId](#d-21-the-idempotency-key-composes-orderid-orderversion-orderlineid-wave-and-intentkind-and-is-structurally-distinct-from-correlationid)
  - [D-22: The binding reference is opaque and caller-owned; Subscriptions and OSS never interpret it as order state](#d-22-the-binding-reference-is-opaque-and-caller-owned-subscriptions-and-oss-never-interpret-it-as-order-state)
  - [D-23: A pre-activation draft re-read immediately before every activation intent is the sole detection mechanism for an auto-voided wave-1 draft](#d-23-a-pre-activation-draft-re-read-immediately-before-every-activation-intent-is-the-sole-detection-mechanism-for-an-auto-voided-wave-1-draft)
  - [D-24: A voided wave-1 draft is rebuilt by re-running wave 1 against the same frozen orderId and orderVersion, never by resuming into wave 2](#d-24-a-voided-wave-1-draft-is-rebuilt-by-re-running-wave-1-against-the-same-frozen-orderid-and-orderversion-never-by-resuming-into-wave-2)
  - [D-25: The reconciliation sweep becomes read-only for an intent once its idempotency-key lifetime elapses, and never resubmits under an aged-out key](#d-25-the-reconciliation-sweep-becomes-read-only-for-an-intent-once-its-idempotency-key-lifetime-elapses-and-never-resubmits-under-an-aged-out-key)
  - [D-26: The first activation intent, not wave-1 draft-create acceptance, is reported as the spawn signal to Orders Lifecycle](#d-26-the-first-activation-intent-not-wave-1-draft-create-acceptance-is-reported-as-the-spawn-signal-to-orders-lifecycle)
- [G. Saga and compensation (slice 06)](#g-saga-and-compensation-slice-06)
  - [D-27: Both waves are compensable with no intra-saga pivot; compensation runs in reverse order across two structurally distinct legs](#d-27-both-waves-are-compensable-with-no-intra-saga-pivot-compensation-runs-in-reverse-order-across-two-structurally-distinct-legs)
  - [D-28: Operational compensation is a reporting boundary to Lifecycle, never a Billing gate](#d-28-operational-compensation-is-a-reporting-boundary-to-lifecycle-never-a-billing-gate)
  - [D-29: Draft-void failure and activated-cancel failure are two structurally distinct manual-task reasons, never collapsed into one generic compensation-failure reason](#d-29-draft-void-failure-and-activated-cancel-failure-are-two-structurally-distinct-manual-task-reasons-never-collapsed-into-one-generic-compensation-failure-reason)
  - [D-30: A dedicated five-step cancellation-fencing sequence must complete in full before any compensated outcome is reported, and a late success still counts as a created subscription requiring compensation](#d-30-a-dedicated-five-step-cancellation-fencing-sequence-must-complete-in-full-before-any-compensated-outcome-is-reported-and-a-late-success-still-counts-as-a-created-subscription-requiring-compensation)
  - [D-31: The FulfillmentTask unilateral-cancel window closes at activation-intent acceptance, not at draft-create acceptance](#d-31-the-fulfillmenttask-unilateral-cancel-window-closes-at-activation-intent-acceptance-not-at-draft-create-acceptance)
- [H. Manual tasks, override, queue, overdue escalation (slice 07)](#h-manual-tasks-override-queue-overdue-escalation-slice-07)
  - [D-32: Exactly one actionable manual task is created per permanently failed line under the default remediation policy, from any of the four failed-entrance routes; fail-fast substitutes a non-actionable tracked incident instead](#d-32-exactly-one-actionable-manual-task-is-created-per-permanently-failed-line-under-the-default-remediation-policy-from-any-of-the-four-failed-entrance-routes-fail-fast-substitutes-a-non-actionable-tracked-incident-instead)
  - [D-33: An override is rejected outright whenever it cannot be verified against a matching, active Subscriptions record; there is no provisional-acceptance path](#d-33-an-override-is-rejected-outright-whenever-it-cannot-be-verified-against-a-matching-active-subscriptions-record-there-is-no-provisional-acceptance-path)
  - [D-34: The overdue-fulfillment escalation window never auto-terminals the order and never by itself marks any line failed](#d-34-the-overdue-fulfillment-escalation-window-never-auto-terminals-the-order-and-never-by-itself-marks-any-line-failed)
- [I. Hold, resilience, and workflow-mediated cancel (slice 08)](#i-hold-resilience-and-workflow-mediated-cancel-slice-08)
  - [D-35: A hold suspends this gear's own dispatch and timers only; it never pauses, reads, or owns the Subscriptions draft auto-void TTL, and a paused timer resumes with its remaining window, never the full window](#d-35-a-hold-suspends-this-gears-own-dispatch-and-timers-only-it-never-pauses-reads-or-owns-the-subscriptions-draft-auto-void-ttl-and-a-paused-timer-resumes-with-its-remaining-window-never-the-full-window)
  - [D-36: Dependency-retry resilience and the Generic Approval fail-closed park are structurally distinct mechanisms, never one code path branching on dependency name](#d-36-dependency-retry-resilience-and-the-generic-approval-fail-closed-park-are-structurally-distinct-mechanisms-never-one-code-path-branching-on-dependency-name)
- [J. Read projection and authorization (slice 09)](#j-read-projection-and-authorization-slice-09)
  - [D-37: Every system-actor grant requires a verified service principal — `subject_type` plus `token_scopes` naming the calling gear on REST, the broker's produce grant on the event transport; actor class alone is never sufficient](#d-37-every-system-actor-grant-requires-a-verified-service-principal--subject_type-plus-token_scopes-naming-the-calling-gear-on-rest-the-brokers-produce-grant-on-the-event-transport-actor-class-alone-is-never-sufficient)
  - [D-38: Process-record retention defaults to at least 400 days at audit grade, configurable, independently of any durable-execution engine's own run-history retention, with the Orders Workflow platform-audit-policy owner as its named executor](#d-38-process-record-retention-defaults-to-at-least-400-days-at-audit-grade-configurable-independently-of-any-durable-execution-engines-own-run-history-retention-with-the-orders-workflow-platform-audit-policy-owner-as-its-named-executor)
- [K. Tuning-value working baselines (engine and intent path)](#k-tuning-value-working-baselines-engine-and-intent-path)
  - [D-39: The retry backoff curve is exponential with base 1 s, coefficient 2.0, a 30 s cap and full jitter, over a maximum of 5 submission attempts](#d-39-the-retry-backoff-curve-is-exponential-with-base-1-s-coefficient-20-a-30-s-cap-and-full-jitter-over-a-maximum-of-5-submission-attempts)
  - [D-40: The per-attempt timeout is 10 s, set from the downstream's service objective rather than from caller patience](#d-40-the-per-attempt-timeout-is-10-s-set-from-the-downstreams-service-objective-rather-than-from-caller-patience)
  - [D-41: The step deadline is 5 minutes per wave step, derived from the fulfillment service objective rather than chosen](#d-41-the-step-deadline-is-5-minutes-per-wave-step-derived-from-the-fulfillment-service-objective-rather-than-chosen)
  - [D-42: The nesting invariant over the four bounds is asserted at configuration load, and a violating configuration is refused at startup](#d-42-the-nesting-invariant-over-the-four-bounds-is-asserted-at-configuration-load-and-a-violating-configuration-is-refused-at-startup)
  - [D-43: A gear-wide retry budget caps retries at 10% of request volume with adaptive throttling, in addition to the per-request attempt cap](#d-43-a-gear-wide-retry-budget-caps-retries-at-10-of-request-volume-with-adaptive-throttling-in-addition-to-the-per-request-attempt-cap)
  - [D-44: Per-order line parallelism is 8; the cross-process aggregate limit is sized by Little's Law from measured capacity and preferably adaptive; the dead-letter delivery cap is 5](#d-44-per-order-line-parallelism-is-8-the-cross-process-aggregate-limit-is-sized-by-littles-law-from-measured-capacity-and-preferably-adaptive-the-dead-letter-delivery-cap-is-5)
  - [D-45: The reconciliation-sweep ladder is 30 s to 1 h with jitter and a bounded page size, and must resolve every intent inside the overdue window](#d-45-the-reconciliation-sweep-ladder-is-30-s-to-1-h-with-jitter-and-a-bounded-page-size-and-must-resolve-every-intent-inside-the-overdue-window)
  - [D-46: Generic Approval outage detection is fixed with a circuit breaker, while the escalation threshold remains deliberately unset](#d-46-generic-approval-outage-detection-is-fixed-with-a-circuit-breaker-while-the-escalation-threshold-remains-deliberately-unset)
- [L. Cross-cutting decisions (remediation)](#l-cross-cutting-decisions-remediation)
  - [D-47: Idempotency keys form four tenant-prefixed families, and `gateId` is derived rather than minted](#d-47-idempotency-keys-form-four-tenant-prefixed-families-and-gateid-is-derived-rather-than-minted)
  - [D-48: Three tenant axes, and every gear-owned table carries at least the resource axis](#d-48-three-tenant-axes-and-every-gear-owned-table-carries-at-least-the-resource-axis)
  - [D-49: Optimistic concurrency is a row version surfaced as an ETag, and one active instance per order is a partial unique index](#d-49-optimistic-concurrency-is-a-row-version-surfaced-as-an-etag-and-one-active-instance-per-order-is-a-partial-unique-index)
  - [D-50: The audit log is append-only and hash-chained, and human justification is a separate column from the reason code](#d-50-the-audit-log-is-append-only-and-hash-chained-and-human-justification-is-a-separate-column-from-the-reason-code)
  - [D-51: `parked` is a distinct process-phase value](#d-51-parked-is-a-distinct-process-phase-value)
  - [D-52: The reconciliation-sweep ladder splits by phase](#d-52-the-reconciliation-sweep-ladder-splits-by-phase)
  - [D-53: A non-pausable `max_process_lifetime` of 90 days bounds every process, independently of order state](#d-53-a-non-pausable-max_process_lifetime-of-90-days-bounds-every-process-independently-of-order-state)
  - [D-54: A partial wave-2 failure holds the order; already-activated lines are not rolled back](#d-54-a-partial-wave-2-failure-holds-the-order-already-activated-lines-are-not-rolled-back)
  - [D-55: "Remediation exhausted" is three failed attempts on the same task, or the manual-task SLA deadline elapsing](#d-55-remediation-exhausted-is-three-failed-attempts-on-the-same-task-or-the-manual-task-sla-deadline-elapsing)
  - [D-56: The submitting identity is refused at the approval-decision endpoint](#d-56-the-submitting-identity-is-refused-at-the-approval-decision-endpoint)
  - [D-57: `lifecycle_submitted_ttl` is mirrored as local configuration under a startup refusal, pending the upstream field](#d-57-lifecycle_submitted_ttl-is-mirrored-as-local-configuration-under-a-startup-refusal-pending-the-upstream-field)
  - [D-58 (H) Process events use the platform producer outbox; Workflow owns no outbox table, drain or re-drive](#d-58-h-process-events-use-the-platform-producer-outbox-workflow-owns-no-outbox-table-drain-or-re-drive)
  - [D-59 (H) Retain gear-owned transactional audit following Pricing and Orders Lifecycle](#d-59-h-retain-gear-owned-transactional-audit-following-pricing-and-orders-lifecycle)
  - [D-60 (H) Freeze the Workflow audit hash byte contract](#d-60-h-freeze-the-workflow-audit-hash-byte-contract)
  - [D-61 (M) Audit actor references are immutable; erasure is not an in-place rewrite](#d-61-m-audit-actor-references-are-immutable-erasure-is-not-an-in-place-rewrite)
  - [D-62 (M) Workflow-owned workers coordinate through toolkit-db session advisory locks under a named roster](#d-62-m-workflow-owned-workers-coordinate-through-toolkit-db-session-advisory-locks-under-a-named-roster)
  - [D-63 (H) Authorization is delegated to the platform PDP through the shared PolicyEnforcer adapter](#d-63-h-authorization-is-delegated-to-the-platform-pdp-through-the-shared-policyenforcer-adapter)
  - [D-64 (M) Refusal reasons follow the platform ContractError contract](#d-64-m-refusal-reasons-follow-the-platform-contracterror-contract)
- [M. The flow as a platform definition (ADR-0011…0013) and the slice operations](#m-the-flow-as-a-platform-definition-adr-00110013-and-the-slice-operations)
  - [D-65 (H) The order process flow is a versioned platform workflow definition executed by serverless-runtime](#d-65-h-the-order-process-flow-is-a-versioned-platform-workflow-definition-executed-by-serverless-runtime)
  - [D-66 (H) References, not payloads, cross the engine boundary](#d-66-h-references-not-payloads-cross-the-engine-boundary)
  - [D-67 (H) Protected steps are fenced by six validation rules and by run-time guards](#d-67-h-protected-steps-are-fenced-by-six-validation-rules-and-by-run-time-guards)
  - [D-68 (H) Instances are pinned to their definition version; the platform operator publishes](#d-68-h-instances-are-pinned-to-their-definition-version-the-platform-operator-publishes)
  - [D-69 (H) The step operation is the unit of work, behind one internal step surface](#d-69-h-the-step-operation-is-the-unit-of-work-behind-one-internal-step-surface)
  - [D-70 (H) Timers, waits and task retry policy are the platform's](#d-70-h-timers-waits-and-task-retry-policy-are-the-platforms)
  - [D-71 (M) The worker roster is three workers, and the sweep selects every due intent](#d-71-m-the-worker-roster-is-three-workers-and-the-sweep-selects-every-due-intent)
  - [D-72 (M) Inbound dead letters are the platform trigger path's; `owf_dead_letter_record` is retired](#d-72-m-inbound-dead-letters-are-the-platform-trigger-paths-owf_dead_letter_record-is-retired)
  - [D-73 (H) Start is the platform event trigger; the REST start route is removed](#d-73-h-start-is-the-platform-event-trigger-the-rest-start-route-is-removed)
  - [D-74 (M) The idempotency key families gain the trigger family and two round components](#d-74-m-the-idempotency-key-families-gain-the-trigger-family-and-two-round-components)
  - [D-75 (M) Supersession is unwind-then-start, and admission waits for the prior instance](#d-75-m-supersession-is-unwind-then-start-and-admission-waits-for-the-prior-instance)
  - [D-76 (M) The seller axis is resolved inside Orders and carried from admission to start](#d-76-m-the-seller-axis-is-resolved-inside-orders-and-carried-from-admission-to-start)
  - [D-77 (M) Twelve reasons are registered for the step operations; the catalogue holds forty-two](#d-77-m-twelve-reasons-are-registered-for-the-step-operations-the-catalogue-holds-forty-two)
  - [D-78 (M) The barrier and the park are definition patterns over Orders guards](#d-78-m-the-barrier-and-the-park-are-definition-patterns-over-orders-guards)
  - [D-79 (M) Each wave is one `call` carrying the line set as references](#d-79-m-each-wave-is-one-call-carrying-the-line-set-as-references)
  - [D-80 (M) Competing-fork arms only listen or wait; one shared return, and a hold pauses only the approval stage](#d-80-m-competing-fork-arms-only-listen-or-wait-one-shared-return-and-a-hold-pauses-only-the-approval-stage)
  - [D-81 (M) One shared unwind path: fence, compensate, report, terminate](#d-81-m-one-shared-unwind-path-fence-compensate-report-terminate)
  - [D-82 (M) A lifetime-ceiling park is allowed from `suspended`; a parked instance unwinds only through the fence](#d-82-m-a-lifetime-ceiling-park-is-allowed-from-suspended-a-parked-instance-unwinds-only-through-the-fence)
  - [D-83 (H) `compensate-order` is one operation over the Orders-owned ordinal, resumable by pass](#d-83-h-compensate-order-is-one-operation-over-the-orders-owned-ordinal-resumable-by-pass)
  - [D-84 (M) The apply-time cancel re-check runs once, and withdrawn authority becomes a task](#d-84-m-the-apply-time-cancel-re-check-runs-once-and-withdrawn-authority-becomes-a-task)
  - [D-85 (M) The cancel request is a slice-09 table; task requests are slice 07's](#d-85-m-the-cancel-request-is-a-slice-09-table-task-requests-are-slice-07s)
  - [D-86 (M) An operator re-drive keeps the invocation, or the instance is unwound and re-submitted](#d-86-m-an-operator-re-drive-keeps-the-invocation-or-the-instance-is-unwound-and-re-submitted)
  - [D-87 (M) One outage threshold governs the park clock, and the gate-open outage pause is a probe arm](#d-87-m-one-outage-threshold-governs-the-park-clock-and-the-gate-open-outage-pause-is-a-probe-arm)
  - [D-88 (L) The approval routing plan is saved at the first `open-gates`](#d-88-l-the-approval-routing-plan-is-saved-at-the-first-open-gates)
  - [D-89 (M) Lifecycle is the sole evaluator of tolerate-failure](#d-89-m-lifecycle-is-the-sole-evaluator-of-tolerate-failure)
  - [D-90 (M) Payments is read on request, polled by a definition wait](#d-90-m-payments-is-read-on-request-polled-by-a-definition-wait)
  - [D-91 (M) The overlap and market re-check is advisory at construction and authoritative before wave 2](#d-91-m-the-overlap-and-market-re-check-is-advisory-at-construction-and-authoritative-before-wave-2)
  - [D-92 (M) A plan-level failure never leaves `approved` by itself](#d-92-m-a-plan-level-failure-never-leaves-approved-by-itself)
  - [D-93 (L) Sizes: SLA population N ≤ 40 lines, a 200-line cap, 30-day authorization validity](#d-93-l-sizes-sla-population-n--40-lines-a-200-line-cap-30-day-authorization-validity)
  - [D-94 (M) Remediation exhaustion is the one automatic path that compensates activated lines](#d-94-m-remediation-exhaustion-is-the-one-automatic-path-that-compensates-activated-lines)
  - [D-95 (M) An operator retry returns a failed line to its pre-wave state](#d-95-m-an-operator-retry-returns-a-failed-line-to-its-pre-wave-state)
  - [D-96 (M) An admission deferral is a settled success, over a slice-05 admission table](#d-96-m-an-admission-deferral-is-a-settled-success-over-a-slice-05-admission-table)
  - [D-97 (M) Intent statuses, the confirmation arm and the Subscriptions tuple](#d-97-m-intent-statuses-the-confirmation-arm-and-the-subscriptions-tuple)
  - [D-98 (M) The manual-task reason enum is a catalogue subset, and SLA classes are 4 h and 24 h](#d-98-m-the-manual-task-reason-enum-is-a-catalogue-subset-and-sla-classes-are-4-h-and-24-h)
  - [D-99 (M) The remediation hold is a flag; cancelling the last open forward task exhausts remediation](#d-99-m-the-remediation-hold-is-a-flag-cancelling-the-last-open-forward-task-exhausts-remediation)
  - [D-100 (M) Task actions record a request and signal; every mutating route requires a key](#d-100-m-task-actions-record-a-request-and-signal-every-mutating-route-requires-a-key)
  - [D-101 (L) The task queue sorts on the immutable `(created_at, task_id)` key](#d-101-l-the-task-queue-sorts-on-the-immutable-created_at-task_id-key)
  - [D-102 (H) One rule for re-invokable operations: a round in, the next round out, a counter on the instance row, an attempt only from an operator retry](#d-102-h-one-rule-for-re-invokable-operations-a-round-in-the-next-round-out-a-counter-on-the-instance-row-an-attempt-only-from-an-operator-retry)
  - [D-103 (M) The registry lease is fenced by a holder token, sized below the retry horizon, and resolved per key family](#d-103-m-the-registry-lease-is-fenced-by-a-holder-token-sized-below-the-retry-horizon-and-resolved-per-key-family)
  - [D-104 (M) No Workflow-owned table is partitioned; registry rows are kept as tombstones until no replay can arrive](#d-104-m-no-workflow-owned-table-is-partitioned-registry-rows-are-kept-as-tombstones-until-no-replay-can-arrive)
  - [D-105 (H) A dead invocation is raised as one order-scope task; the platform re-drive is the recovery, an Orders-driven cancel the fallback](#d-105-h-a-dead-invocation-is-raised-as-one-order-scope-task-the-platform-re-drive-is-the-recovery-an-orders-driven-cancel-the-fallback)
  - [D-106 (H) Every step call is bound to the instance's invocation, and the fence needs a recorded cause](#d-106-h-every-step-call-is-bound-to-the-instances-invocation-and-the-fence-needs-a-recorded-cause)
  - [D-107 (M) The start is checked against the Lifecycle order, and the bindings are released with the definition](#d-107-m-the-start-is-checked-against-the-lifecycle-order-and-the-bindings-are-released-with-the-definition)
  - [D-108 (M) `retry-step` runs only in-process, with the actor from the request row](#d-108-m-retry-step-runs-only-in-process-with-the-actor-from-the-request-row)
  - [D-109 (H) Orders reports to Lifecycle only from fulfillment; a cancel before it is Lifecycle's own](#d-109-h-orders-reports-to-lifecycle-only-from-fulfillment-a-cancel-before-it-is-lifecycles-own)
  - [D-110 (H) The fence resolves the failure reason from the record and maps it to Lifecycle's closed enumeration](#d-110-h-the-fence-resolves-the-failure-reason-from-the-record-and-maps-it-to-lifecycles-closed-enumeration)
  - [D-111 (M) The report follows the fence row, carries the requester's reason, and names an answer for every Lifecycle refusal](#d-111-m-the-report-follows-the-fence-row-carries-the-requesters-reason-and-names-an-answer-for-every-lifecycle-refusal)
  - [D-112 (M) `reflect-verdict` sends Lifecycle's wire form, and a moved order is waited out, not failed](#d-112-m-reflect-verdict-sends-lifecycles-wire-form-and-a-moved-order-is-waited-out-not-failed)
  - [D-113 (M) Workflow does not pre-check buyer acceptance](#d-113-m-workflow-does-not-pre-check-buyer-acceptance)
  - [D-114 (H) One rule for a step call that fails: it faults the invocation unless the failure is a subject an operator acts on](#d-114-h-one-rule-for-a-step-call-that-fails-it-faults-the-invocation-unless-the-failure-is-a-subject-an-operator-acts-on)
  - [D-115 (M) An authority withdrawn before the fence refuses the cancel and raises no task](#d-115-m-an-authority-withdrawn-before-the-fence-refuses-the-cancel-and-raises-no-task)
  - [D-116 (M) Each task subject carries its reason and cause, and the task key ends in the failing call's tail](#d-116-m-each-task-subject-carries-its-reason-and-cause-and-the-task-key-ends-in-the-failing-calls-tail)
  - [D-117 (M) The remediation hold lasts until the order's last open task resolves](#d-117-m-the-remediation-hold-lasts-until-the-orders-last-open-task-resolves)
  - [D-118 (M) `escalate-gate` and `arm-park-escalation` have algorithms; every answer returns the next round, and a fire during an outage pauses](#d-118-m-escalate-gate-and-arm-park-escalation-have-algorithms-every-answer-returns-the-next-round-and-a-fire-during-an-outage-pauses)
  - [D-119 (H) A retried or never-written line is re-dispatched from Orders' record: a per-line wave attempt on the task row, one dispatch attempt per wave, and the barrier's undispatched set](#d-119-h-a-retried-or-never-written-line-is-re-dispatched-from-orders-record-a-per-line-wave-attempt-on-the-task-row-one-dispatch-attempt-per-wave-and-the-barriers-undispatched-set)
  - [D-120 (M) The read after a dispatch 409 is routed, and the call is re-issued only after a wait](#d-120-m-the-read-after-a-dispatch-409-is-routed-and-the-call-is-re-issued-only-after-a-wait)
  - [D-121 (H) The lifetime ceiling parks only a running, unparked instance, and each ceiling is its own round](#d-121-h-the-lifetime-ceiling-parks-only-a-running-unparked-instance-and-each-ceiling-is-its-own-round)
  - [D-122 (H) The ceiling wait consumes only its own task, and no unrecorded signal unparks](#d-122-h-the-ceiling-wait-consumes-only-its-own-task-and-no-unrecorded-signal-unparks)
  - [D-123 (M) The escalation re-check rides the 30-second probe tick](#d-123-m-the-escalation-re-check-rides-the-30-second-probe-tick)
  - [D-124 (M) An event delivered between listens needs platform retention; no poll covers it](#d-124-m-an-event-delivered-between-listens-needs-platform-retention-no-poll-covers-it)
  - [D-125 (M) A failure or floor trip the sweep worker records reaches the definition through its next reconcile round](#d-125-m-a-failure-or-floor-trip-the-sweep-worker-records-reaches-the-definition-through-its-next-reconcile-round)
  - [D-126 (H) The validation rules are stated over values the definition holds and a routing graph the check can enumerate](#d-126-h-the-validation-rules-are-stated-over-values-the-definition-holds-and-a-routing-graph-the-check-can-enumerate)
  - [D-127 (M) The Workflow callable declares every required trait: async-only, its limits, and no invocation retry](#d-127-m-the-workflow-callable-declares-every-required-trait-async-only-its-limits-and-no-invocation-retry)
  - [D-128 (M) Bounding one invocation's engine history is asked of the plugin; the ticks are not stretched to fit](#d-128-m-bounding-one-invocations-engine-history-is-asked-of-the-plugin-the-ticks-are-not-stretched-to-fit)
  - [D-129 (M) The ceiling task has an SLA tick, and a Seller Operator ends a ceiling park with the order cancel](#d-129-m-the-ceiling-task-has-an-sla-tick-and-a-seller-operator-ends-a-ceiling-park-with-the-order-cancel)
  - [D-130 (M) Until events are retained between listens, the resume wait polls whether Lifecycle still holds the order](#d-130-m-until-events-are-retained-between-listens-the-resume-wait-polls-whether-lifecycle-still-holds-the-order)
  - [D-131 (H) The references that cross the engine are a closed six-type vocabulary; cardinality, counters and the resource tenant are the stated residual](#d-131-h-the-references-that-cross-the-engine-are-a-closed-six-type-vocabulary-cardinality-counters-and-the-resource-tenant-are-the-stated-residual)
  - [D-132 (M) A step-route error answer carries fixed members only](#d-132-m-a-step-route-error-answer-carries-fixed-members-only)
  - [D-133 (M) Every wait that holds a recorded suspension polls the hold](#d-133-m-every-wait-that-holds-a-recorded-suspension-polls-the-hold)
  - [D-134 (H) The business windows are per-seller policy values pinned on the record; the definition owns only the tick](#d-134-h-the-business-windows-are-per-seller-policy-values-pinned-on-the-record-the-definition-owns-only-the-tick)
  - [D-135 (M) A version carries the `p1` composables and the shared arms on their paths; the check tracks the pinned enums](#d-135-m-a-version-carries-the-p1-composables-and-the-shared-arms-on-their-paths-the-check-tracks-the-pinned-enums)
  - [D-136 (M) One adjustability table; slice constraints are enforced items or canonical-definition guidance; no Function call](#d-136-m-one-adjustability-table-slice-constraints-are-enforced-items-or-canonical-definition-guidance-no-function-call)
  - [D-137 (H) The binding records the version the platform pinned; the publisher waits on the registry](#d-137-h-the-binding-records-the-version-the-platform-pinned-the-publisher-waits-on-the-registry)
  - [D-138 (M) A definition is published by a job of its own, behind a behavioural gate, and rolled back forward](#d-138-m-a-definition-is-published-by-a-job-of-its-own-behind-a-behavioural-gate-and-rolled-back-forward)
  - [D-139 (M) The build order follows in-process calls; 07 and 08 precede 06, and the back-edges are built against doubles](#d-139-m-the-build-order-follows-in-process-calls-07-and-08-precede-06-and-the-back-edges-are-built-against-doubles)
  - [D-140 (H) The per-seller policy is a foundation table on the policy channel, read through one port and pinned with its revision](#d-140-h-the-per-seller-policy-is-a-foundation-table-on-the-policy-channel-read-through-one-port-and-pinned-with-its-revision)
  - [D-141 (M) A resume closes only a suspension Lifecycle no longer holds](#d-141-m-a-resume-closes-only-a-suspension-lifecycle-no-longer-holds)
  - [D-142 (M) Every wait inside a stage is a competing fork carrying the shared arms](#d-142-m-every-wait-inside-a-stage-is-a-competing-fork-carrying-the-shared-arms)
  - [D-143 (M) No walk reaches a wave dispatch without `begin-fulfillment`; `beginResult` is a pinned member](#d-143-m-no-walk-reaches-a-wave-dispatch-without-begin-fulfillment-beginresult-is-a-pinned-member)
  - [D-144 (H) Rule 1 decides every route on members it tracks; the canonical definition passes it](#d-144-h-rule-1-decides-every-route-on-members-it-tracks-the-canonical-definition-passes-it)
  - [D-145 (M) A sent spawn signal leaves the held-spawn loop; a not-dispatchable re-check waits there](#d-145-m-a-sent-spawn-signal-leaves-the-held-spawn-loop-a-not-dispatchable-re-check-waits-there)
  - [D-146 (H) The failure stage never waits with no open task](#d-146-h-the-failure-stage-never-waits-with-no-open-task)
  - [D-147 (M) A call that consumes an arm's payload is its own checkpoint](#d-147-m-a-call-that-consumes-an-arms-payload-is-its-own-checkpoint)
  - [D-148 (M) The escalation bound counts every call between two fires](#d-148-m-the-escalation-bound-counts-every-call-between-two-fires)
  - [D-149 (H) Each ceiling's task is its own row, and the fence closes it](#d-149-h-each-ceilings-task-is-its-own-row-and-the-fence-closes-it)
  - [D-150 (L) The poll's task tail names its suspension; only `apply-hold` resets the poll round](#d-150-l-the-polls-task-tail-names-its-suspension-only-apply-hold-resets-the-poll-round)
  - [D-151 (M) The liveness pass observes the SLA of the tasks no definition waits on](#d-151-m-the-liveness-pass-observes-the-sla-of-the-tasks-no-definition-waits-on)
  - [D-152 (M) The quarantine counts only presented attempts](#d-152-m-the-quarantine-counts-only-presented-attempts)
  - [D-153 (M) Slice 05 re-arms an unresolved intent through its own port](#d-153-m-slice-05-re-arms-an-unresolved-intent-through-its-own-port)
  - [D-154 (M) `create-manual-task` verifies the route that names a subject, not only a failed task](#d-154-m-create-manual-task-verifies-the-route-that-names-a-subject-not-only-a-failed-task)
  - [D-155 (M) `supersededByOrderVersion` is admit-trigger's `currentOrderVersion`](#d-155-m-supersededbyorderversion-is-admit-triggers-currentorderversion)
  - [D-156 (L) The reference vocabulary names the formats of every identity member it lists](#d-156-l-the-reference-vocabulary-names-the-formats-of-every-identity-member-it-lists)
  - [D-157 (M) The callable's limits are sized from the residency of long waits and three ceilings](#d-157-m-the-callables-limits-are-sized-from-the-residency-of-long-waits-and-three-ceilings)
  - [D-158 (M) The rollback document carries its new version; a failed gate is rolled back; ticks are compared statically](#d-158-m-the-rollback-document-carries-its-new-version-a-failed-gate-is-rolled-back-ticks-are-compared-statically)
  - [D-159 (M) The overdue-window bound is checked on both sides over a defined version set](#d-159-m-the-overdue-window-bound-is-checked-on-both-sides-over-a-defined-version-set)
  - [D-160 (M) Configuration changes are recorded in an append-only revision table, not the audit chain](#d-160-m-configuration-changes-are-recorded-in-an-append-only-revision-table-not-the-audit-chain)
  - [D-161 (H) An admission answers only for its own event, and a ceiling restores the return stage it interrupted](#d-161-h-an-admission-answers-only-for-its-own-event-and-a-ceiling-restores-the-return-stage-it-interrupted)
  - [D-162 (M) The gate-loop calls retry under a policy whose worst case fits their timeout](#d-162-m-the-gate-loop-calls-retry-under-a-policy-whose-worst-case-fits-their-timeout)
  - [D-163 (M) A ceiling that fires while parked at a ceiling re-arms, and keeps the first ceiling's return state](#d-163-m-a-ceiling-that-fires-while-parked-at-a-ceiling-re-arms-and-keeps-the-first-ceilings-return-state)
  - [D-164 (H) The ceiling wait writes its own request reference, and a ceiling restores the payload of the stage it interrupted](#d-164-h-the-ceiling-wait-writes-its-own-request-reference-and-a-ceiling-restores-the-payload-of-the-stage-it-interrupted)
  - [D-165 (H) A wave answer routes deferred, then failed, then lapsed, and no entry drops what another has not folded](#d-165-h-a-wave-answer-routes-deferred-then-failed-then-lapsed-and-no-entry-drops-what-another-has-not-folded)
  - [D-166 (M) A completion report never answers terminal-event, and every terminate-instance names its terminal outcome](#d-166-m-a-completion-report-never-answers-terminal-event-and-every-terminate-instance-names-its-terminal-outcome)
  - [D-167 (H) A settled key's answer lives on its registry row; the step log is recovery history with its own purge grant](#d-167-h-a-settled-keys-answer-lives-on-its-registry-row-the-step-log-is-recovery-history-with-its-own-purge-grant)
  - [D-168 (H) A settlement that cannot commit aborts whole and answers the canonical infrastructure Problem](#d-168-h-a-settlement-that-cannot-commit-aborts-whole-and-answers-the-canonical-infrastructure-problem)
  - [D-169 (H) `start-instance` reads the platform before its one transaction](#d-169-h-start-instance-reads-the-platform-before-its-one-transaction)
  - [D-170 (M) One table says what each receipt records; `audit_kind` is the success kind](#d-170-m-one-table-says-what-each-receipt-records-audit_kind-is-the-success-kind)
  - [D-171 (H) An early evaluation settles success with `due: false`](#d-171-h-an-early-evaluation-settles-success-with-due-false)
  - [D-172 (H) A pre-admission `admit-trigger` audit entry may carry no seller](#d-172-h-a-pre-admission-admit-trigger-audit-entry-may-carry-no-seller)
  - [D-173 (H) READ COMMITTED; the pre-admission append allocates and checks in one statement, and `start-instance` collides with it symmetrically](#d-173-h-read-committed-the-pre-admission-append-allocates-and-checks-in-one-statement-and-start-instance-collides-with-it-symmetrically)
  - [D-174 (H) The quarantine counts from a per-family history of presented attempts](#d-174-h-the-quarantine-counts-from-a-per-family-history-of-presented-attempts)
  - [D-175 (M) `terminate-instance` takes only the two permitted edges; from `suspended` or `parked` it refuses `fence-not-claimed`](#d-175-m-terminate-instance-takes-only-the-two-permitted-edges-from-suspended-or-parked-it-refuses-fence-not-claimed)
  - [D-176 (L) The nesting invariant is enforced in four places, and §2.1 says so](#d-176-l-the-nesting-invariant-is-enforced-in-four-places-and-21-says-so)
  - [D-177 (H) The six events publish to this gear's own topic, named in the abstract type's `x-gts-traits`](#d-177-h-the-six-events-publish-to-this-gears-own-topic-named-in-the-abstract-types-x-gts-traits)
  - [D-178 (H) A producer-registration rotation rejects every message queued under the old id](#d-178-h-a-producer-registration-rotation-rejects-every-message-queued-under-the-old-id)
  - [D-179 (M) A settlement enqueues zero or more events of its declared type, at most one per subject](#d-179-m-a-settlement-enqueues-zero-or-more-events-of-its-declared-type-at-most-one-per-subject)
  - [D-180 (H) The suspension closure port also clears `owf_process_instance.suspended`](#d-180-h-the-suspension-closure-port-also-clears-owf_process_instancesuspended)
  - [D-181 (M) The step-retry route records its task resolution request through slice 07's intake port](#d-181-m-the-step-retry-route-records-its-task-resolution-request-through-slice-07s-intake-port)
  - [D-182 (M) The re-authorization mark is the cancellation fence's one rewritable stamp](#d-182-m-the-re-authorization-mark-is-the-cancellation-fences-one-rewritable-stamp)
  - [D-183 (H) A suspension a Lifecycle resume overtook is closed by the poll before `terminate-instance`](#d-183-h-a-suspension-a-lifecycle-resume-overtook-is-closed-by-the-poll-before-terminate-instance)
  - [D-184 (M) A consumed acceptance event and a consumed re-authorization signal are their own checkpoints](#d-184-m-a-consumed-acceptance-event-and-a-consumed-re-authorization-signal-are-their-own-checkpoints)
  - [D-185 (M) A key ages while its instance lives only if its operation submits downstream; an aged-out key is re-issued under an operator-minted successor](#d-185-m-a-key-ages-while-its-instance-lives-only-if-its-operation-submits-downstream-an-aged-out-key-is-re-issued-under-an-operator-minted-successor)
  - [D-186 (M) The Seller Operator reads an order's process audit trail through a paged, seller-scoped audit route](#d-186-m-the-seller-operator-reads-an-orders-process-audit-trail-through-a-paged-seller-scoped-audit-route)
  - [D-187 (L) A contended worker lock skips the pass; only a lock or database error is a coordination failure](#d-187-l-a-contended-worker-lock-skips-the-pass-only-a-lock-or-database-error-is-a-coordination-failure)
  - [D-188 (H) A Lifecycle re-run past Lifecycle's 24-hour key window reads the order back and settles `already-applied`](#d-188-h-a-lifecycle-re-run-past-lifecycles-24-hour-key-window-reads-the-order-back-and-settles-already-applied)
  - [D-189 (M) `open-gates` looks a gate's request up by its key before it submits, and adopts one it finds](#d-189-m-open-gates-looks-a-gates-request-up-by-its-key-before-it-submits-and-adopts-one-it-finds)
  - [D-190 (M) `reflect-verdict` answers a Lifecycle refusal as the settled answer `refused`, and the definition catches no 400 of it](#d-190-m-reflect-verdict-answers-a-lifecycle-refusal-as-the-settled-answer-refused-and-the-definition-catches-no-400-of-it)
  - [D-191 (M) The audit read is access-logged like Lifecycle's: one `owf_read_access_log` row per request, served or refused](#d-191-m-the-audit-read-is-access-logged-like-lifecycles-one-owf_read_access_log-row-per-request-served-or-refused)
  - [D-192 (M) The operator re-drive of a dead invocation is an explicit readiness item that gates the action, not readiness](#d-192-m-the-operator-re-drive-of-a-dead-invocation-is-an-explicit-readiness-item-that-gates-the-action-not-readiness)
  - [D-193 (H) PriceBook reaches this gear only through Lifecycle's Workflow SDK and its immutable-version read](#d-193-h-pricebook-reaches-this-gear-only-through-lifecycles-workflow-sdk-and-its-immutable-version-read)
  - [D-194 (H) The accepted binding's activation deadline is checked early at freeze, at the re-check and before every activation submit; expiry after an activation compensates first](#d-194-h-the-accepted-bindings-activation-deadline-is-checked-early-at-freeze-at-the-re-check-and-before-every-activation-submit-expiry-after-an-activation-compensates-first)
  - [D-195 (H) A provisioning intent carries the source version tuple as the accepted-binding reference, and `activated` means Subscriptions' `applied`](#d-195-h-a-provisioning-intent-carries-the-source-version-tuple-as-the-accepted-binding-reference-and-activated-means-subscriptions-applied)
  - [D-196 (H) Acquisition lines are independent: the dependency graph, its Catalog source and the `blocked-upstream` outcome are withdrawn](#d-196-h-acquisition-lines-are-independent-the-dependency-graph-its-catalog-source-and-the-blocked-upstream-outcome-are-withdrawn)
  - [D-197 (H) The approval policy owner is this gear's approval-policy adapter; the Generic Approval service is withdrawn](#d-197-h-the-approval-policy-owner-is-this-gears-approval-policy-adapter-the-generic-approval-service-is-withdrawn)
  - [D-198 (M) The approval request carries the stored TCV as an integer in minor units with its currency and scale](#d-198-m-the-approval-request-carries-the-stored-tcv-as-an-integer-in-minor-units-with-its-currency-and-scale)
  - [D-199 (L) Seam notes: the payment amount is separate from the TCV, no Products reservation exists, and change orders wait for their own PriceBook pass](#d-199-l-seam-notes-the-payment-amount-is-separate-from-the-tcv-no-products-reservation-exists-and-change-orders-wait-for-their-own-pricebook-pass)
- [Open Questions](#open-questions)
  - [Q-01: Which durable-execution substrate backs the process — the OSS Workflow Engine or a BSS-local mechanism?](#q-01-which-durable-execution-substrate-backs-the-process--the-oss-workflow-engine-or-a-bss-local-mechanism)
  - [Q-02: The Generic Approval escalation threshold — the one PRD-deferred numeric value this design deliberately leaves unset](#q-02-the-generic-approval-escalation-threshold--the-one-prd-deferred-numeric-value-this-design-deliberately-leaves-unset)
  - [Q-03: Does a bounded hold/resume cycle count or bound the total elapsed non-terminal lifetime of an order, beyond the single-cycle remaining-window guarantee this design already keeps?](#q-03-does-a-bounded-holdresume-cycle-count-or-bound-the-total-elapsed-non-terminal-lifetime-of-an-order-beyond-the-single-cycle-remaining-window-guarantee-this-design-already-keeps)
  - [Q-04: The SUB-O renumbering and the missing OrderAmended re-verdict requirement — two required PRD amendments](#q-04-the-sub-o-renumbering-and-the-missing-orderamended-re-verdict-requirement--two-required-prd-amendments)
  - [Q-05: The Generic Approval service has no specification anywhere in this repository, leaving the whole approval capability inert in phase 1](#q-05-the-generic-approval-service-has-no-specification-anywhere-in-this-repository-leaving-the-whole-approval-capability-inert-in-phase-1)
  - [Q-06: Payments has no specification and no register, so the payment-authorization ask has no owner; only "provision first, collect after" is expressible](#q-06-payments-has-no-specification-and-no-register-so-the-payment-authorization-ask-has-no-owner-only-provision-first-collect-after-is-expressible)
  - [Q-07: Tension between asynchronous outbox publication and the PRD's p95 < 30 s process-event delivery target](#q-07-tension-between-asynchronous-outbox-publication-and-the-prds-p95--30-s-process-event-delivery-target)
  - [Q-08: SUB-O5 (overlap-scope-key presence read) is unagreed, leaving the pre-activation overlap check unevaluable; SUB-O1 (compensation cancellation reason) is critical and unagreed, leaving the activated-cancel reason unspecifiable from this side](#q-08-sub-o5-overlap-scope-key-presence-read-is-unagreed-leaving-the-pre-activation-overlap-check-unevaluable-sub-o1-compensation-cancellation-reason-is-critical-and-unagreed-leaving-the-activated-cancel-reason-unspecifiable-from-this-side)
  - [Q-09: Do the Orders gears and Pricing converge on toolkit-db session advisory locks or on the `gears/bss/libs/coord` fenced lease for worker coordination?](#q-09-do-the-orders-gears-and-pricing-converge-on-toolkit-db-session-advisory-locks-or-on-the-gearsbsslibscoord-fenced-lease-for-worker-coordination)
  - [Q-10: May a seller-scoped role publish definition fragments, or tenants author Functions, in phase 1?](#q-10-may-a-seller-scoped-role-publish-definition-fragments-or-tenants-author-functions-in-phase-1)
  - [Q-11: Does the platform's DSL express the hold pattern and the other constructs the definition needs, or do they need Functions?](#q-11-does-the-platforms-dsl-express-the-hold-pattern-and-the-other-constructs-the-definition-needs-or-do-they-need-functions)
  - [Q-12: Engine-history isolation, retention and residency — the pending half of Q-01](#q-12-engine-history-isolation-retention-and-residency--the-pending-half-of-q-01)
  - [Q-13: Which caller-facing route or event sends `reauthorize-requested` and `unpark-requested`?](#q-13-which-caller-facing-route-or-event-sends-reauthorize-requested-and-unpark-requested)
  - [Q-14: How does an approval gate map onto a `cf-gears-bss-approval` unit once the library adapter replaces the stand-in?](#q-14-how-does-an-approval-gate-map-onto-a-cf-gears-bss-approval-unit-once-the-library-adapter-replaces-the-stand-in)
- [What This Design Set Does Not Claim](#what-this-design-set-does-not-claim)
- [Traceability](#traceability)

<!-- /toc -->

## How to use this document

Every call taken while authoring the design set lands here. An entry records the **decision**
(stated as a resolved fact, never as a debate or an option list), its **rationale** (including
what was rejected and why), and its **propagation** address — the document and section that must
agree with it. A decision this design may take is marked as such; a decision that is commercial
policy owned elsewhere is marked with no code default, so an unset value is visible as absent
behaviour rather than silently becoming the platform answer.

This is the whole register for the gear, in one file. It covers every slice — the process engine
(01), triggers and start (02), approval execution (03), the fulfillment plan (04), provisioning
intents (05), saga and compensation (06), manual tasks (07), hold and cancel (08), read and
authorization (09) and the process definition (10) — together with the commercial-policy decisions owned elsewhere, the
tuning-value working baselines, the cross-cutting decisions, and the open-questions register.
Numbering starts at `D-01`, runs in one continuous sequence, and is never split across parts.
Reopening a decision means flipping its status and recording why; no existing identifier is ever
renumbered.

Where a decision restates one already recorded as an ADR, the entry names that ADR. The two
registers are one system of record read from two angles: the ADR carries the options considered
and the reasoning; the entry here carries the resolved fact and its propagation address. Where they
would disagree, the ADR governs the rationale and this register governs the propagation.

**Note on validation coverage**: `DECISIONS.md` is not a registered `cfs` artifact kind for this
gear (`docs/DECISIONS.md` is excluded from `cfs` autodetect per `.cf-studio/config/artifacts.toml`), so
`cfs validate --artifact` reports it unmatched by design — this is expected, not a defect. This
document relies on `cfs toc`, `cfs validate-toc`, and `cfs check-language` instead.

## A. Process engine (slice 01)

### D-01: Engine execution history is not the process-audit source of record

**Amended by D-65 (2026-09-24).** The substrate is now selected (D-65, ADR-0011): the platform's Temporal plugin, not this gear, owns retries and timers, and its history is still never the audit trail. The ADR line below is superseded: ADR-0001 is rewritten and Q-01 is answered in two parts.

**Decision**: `owf_step_log` (and any durable-execution substrate run history) exists solely for
recovery and replay. `owf_audit_entry` is the sole audit source of record, append-only, with no
UPDATE or DELETE path. A durable-execution substrate is used for retries and durable timers, but
its own run history is never queried as, or substituted for, the process audit trail.

**Rationale**: substrate-side history is purge- and retention-policy-controlled by the substrate,
not by this gear, and the PRD's audit-retention NFR must survive independently of any engine
purge; treating substrate history as authoritative would make gear-owned audit retention hostage
to an infrastructure choice made via a separate ADR.

**ADR**: ADR-0001 (`cpt-cf-bss-orders-workflow-adr-durable-execution-substrate`) — the audit-source-of-record decision this entry restates. ADR-0001 selects no engine; the substrate choice is Q-01 below.

**Propagates to**: `gears/bss/orders-workflow/docs/design/01-foundation.md` § `4.1 Engine
execution history is not the audit source of record`

### D-02: Four distinct bounds govern execution, not three

**Amended by D-70 (2026-09-24).** The bounds are now **five, with two owners** (`01 §4.2`): the per-operation deadline is the operation's; the retry budget, task timeout, overdue window and lifetime ceiling are the definition's. `owf_retry_state` and `owf_durable_timer` are retired; the structural separation is kept by the definition's distinct `wait` arms and by validation rule 4.

**Decision**: retry budget (governs submission failures only), per-attempt timeout, and step
deadline are tracked together in `owf_retry_state`. The process deadline (the overdue window) is
a fourth, structurally separate bound, tracked as a `timer_kind` value in `owf_durable_timer`. The
process deadline MUST NOT by itself mark any line failed or auto-terminal the order.

**Rationale**: keeping the process deadline in a different table from the retry-budget/per-attempt
timeout/step-deadline trio makes the distinction structural rather than a column a handler could
accidentally conflate with a punitive bound; the PRD-driven overdue window exists to trigger
escalation, not to punish an order that is merely running long.

**Propagates to**: `gears/bss/orders-workflow/docs/design/01-foundation.md` § `4.2 Four distinct
bounds, not one`

### D-03: The idempotency registry's outcomes are exhaustive, and a still-processing conflict is never inferred as success

**Decision**: every idempotency-key lookup resolves to exactly one of: first call, absorbed
duplicate (settled, matching fingerprint, closure not re-invoked), still-processing conflict, or
aged-out (retention window elapsed, sweep becomes read-only). No fifth outcome exists. A handler
receiving a still-processing conflict MUST NOT advance any process-owned field, MUST NOT infer
success, and MUST NOT resubmit under a new key.

**Rationale**: an exhaustive, closed outcome set is what lets every handler slice use the same
caller-side duplicate protocol without inventing its own interpretation of an ambiguous
in-flight state; inferring success from silence or from a conflict is precisely the failure mode
that produces double-provisioning.

**Propagates to**: `gears/bss/orders-workflow/docs/design/01-foundation.md` § `4.3 The
idempotency registry's non-success outcomes are exhaustive`

**Amended (2026-09-26)**: the outcome set is unchanged; how a dead lease resolves now depends on
the key family — a lookup on an intent-submitting key, a re-run under a new holder on every other
key — and every settlement is fenced by the lease holder token (D-103). An aged-out key is
evaluated from its retained row, never from a missing one (D-104).

### D-04: The dead-letter record and the manual-task record are two separate objects; a failed step never grows a second inspectable object

**Amended by D-72 (2026-09-24).** `owf_dead_letter_record` is retired: an inbound delivery past its cap is the platform event-trigger path's dead letter. The separation this entry states stands — the manual task is still the one inspectable object for a step failure — and `dead-lettered` is no longer an outcome of the step surface.

**Decision**: `owf_dead_letter_record` records **delivery** exhaustion, never step exhaustion. It
is written only when an inbound Lifecycle trigger, or a Subscriptions / Payments /
Generic-Approval callback, exhausts its finite delivery-count cap; it is written by the step
executor in its delivery-intake role, and it is never an order state. A fulfillment step that
exhausts its retry budget does **not** reach this store by construction — its inspectable object
is the manual task (remediation policy) or the tracked incident (fail-fast), and that object is
the only one it gets. A failure therefore has exactly one inspectable object, but which store
holds it is decided by the failure's class — delivery-level or step-level — not by which code
path noticed first.

**Rationale**: giving one failure two inspectable objects would let an operator resolve one and
leave the other stale, and would double-count the same failure in two different operator queues.
The earlier formulation of this entry — calling the dead-letter record "the step executor's own
record of retry-budget exhaustion" — collapsed exactly the distinction it was written to preserve
and contradicted ADR-0009, which scopes the store to delivery exhaustion only. ADR-0009 governs;
this entry is corrected to it. The residue to watch for is `owf_step_log.outcome` still carrying
`dead-lettered`: that value denotes a delivery-level outcome observed by a step, never a step
outcome, and it does not license a dead-letter row for a failed step.

**ADR**: ADR-0009 (`cpt-cf-bss-orders-workflow-adr-manual-task-dead-letter-separation`) — governs this entry; the dead-letter store is delivery-scoped, never step-scoped.

**Propagates to**: `gears/bss/orders-workflow/docs/design/01-foundation.md` § `4.8 The
dead-letter record is never an order state`

## B. Triggers and start (slice 02)

### D-05: Trigger intake is a closed, exhaustive vocabulary, admission-checked before any instance spawns

**Amended by D-72 and D-73 (2026-09-24).** Trigger intake is now `admit-trigger`, called first on every trigger arm of the definition; the closed vocabulary is validation rule 3 of ADR-0012, the start triggers are the platform event triggers on `OrderSubmitted` and `OrderAmended`, and the delivery cap is the platform trigger path's.

**Decision**: Trigger Intake subscribes to the Orders Lifecycle state-event stream (event
subscription, not a command surface Lifecycle calls into) and recognizes a closed trigger
vocabulary. Exactly one active process instance per order id is enforced at admission — Trigger
Intake checks for an existing active instance before spawning a new one, not by later
reconciliation.

**Rationale**: Lifecycle already publishes these events to other consumers, so a command push
would invert the publisher/consumer ownership Lifecycle already holds; event subscription also
gives at-least-once-plus-dedup-by-event-id delivery for free, matching the duplicate-absorption
requirement. Enforcing the one-active-instance rule at admission (rather than by reconciling
after the fact) avoids a window in which two instances could race on the same order.

**Propagates to**: `gears/bss/orders-workflow/docs/design/02-triggers-and-start.md` § `2.1 The
trigger vocabulary is closed and exhaustive` and § `2.1 Exactly one active instance per order id`

### D-06: A trigger observed for a superseded orderVersion is ignored, and terminating on supersession follows the same path as terminating on a terminal order event

**Decision**: an `OrderAmended` trigger observed while a process instance is still active for the
pre-amendment version routes through the same Termination and Compensation component as a
terminal order event: cancel open approvals and escalation timers, cease pending provisioning
intents, run compensation, void un-activated wave-1 drafts, and record termination in the process
audit log. A trigger that names an orderVersion this gear has already superseded is ignored, not
acted on.

**Rationale**: treating "superseded by amendment" as a second, different termination path from
"terminal event" would create a drift channel where the two paths could silently diverge; folding
them into one component removes that channel by construction.

**ADR**: ADR-0003 (`cpt-cf-bss-orders-workflow-adr-process-state-non-authoritative`) — the rule that a
superseded-version trigger is judged by the live order read (R1), never by process state, which
this entry applies to termination.

**Propagates to**: `gears/bss/orders-workflow/docs/design/02-triggers-and-start.md` § `3.6
Terminal-event compensation, void, and audit`

### D-07: Un-activated wave-1 drafts are voided on every termination path, including on supersession

**Decision**: voiding un-activated wave-1 subscription drafts is a mandatory step of the shared
Termination and Compensation sequence (D-06), executed identically whether termination is caused
by a terminal Lifecycle event or by supersession via `OrderAmended`.

**Rationale**: a wave-1 draft left un-voided after the order it belongs to is superseded or
terminated would be an orphaned subscription artifact with no order to reconcile against.

**Propagates to**: `gears/bss/orders-workflow/docs/design/02-triggers-and-start.md` § `3.6
Terminal-event compensation, void, and audit`

### D-08: Correlation is three distinct identifiers, never conflated

**Decision**: the process `correlationId` (generated once at process start, whole-instance
scope), the per-call idempotency key (one call plus its retries), and the downstream
`TransitionRequest` id (one Subscriptions request) are three distinct identifiers. No component
derives one from another, and no idempotency key anywhere in this gear reuses `correlationId`.

**Rationale**: a single process instance can open multiple concurrent approval gates and
re-open gates across order versions, so an idempotency key derived from the whole-instance
`correlationId` would either collide across gates or fail to distinguish versions.

**Propagates to**: `gears/bss/orders-workflow/docs/design/02-triggers-and-start.md` § `2.1
Correlation is layered, not collapsed`

## C. Approval execution (slice 03)

### D-09: Approval idempotency keys are composed from orderId, orderVersion, and gateId

**Decision**: every approval-gate idempotency key is `orderId` + `orderVersion` + `gateId`. It
never reuses the process `correlationId` (per D-08).

**Rationale**: a `correlationId`-derived key would collide across a multi-party instance's
concurrent gates and would fail to distinguish a re-opened gate on an amended version from the
gate it superseded.

**ADR**: ADR-0006 (`cpt-cf-bss-orders-workflow-adr-idempotency-key-composition`) — the approval-request key family, including the deterministic `gateId` derivation.

**Propagates to**: `gears/bss/orders-workflow/docs/design/03-approval-execution.md` § `2.2
Idempotency keys must not reuse the process correlation id`

### D-10: Escalation-timer ownership sits in this gear, undivided across the service boundary

**Amended by D-70 (2026-09-24).** The escalation timer is a definition `wait` executed by the plugin's durable timers; the Escalation Timer Owner component is retired. What stays in this gear, undivided, is the record of the window (`owf_approval_gate.window_remaining_ms`) and the escalation operation `escalate-gate`; Generic Approval still stores no timer state.

**Decision**: the Escalation Timer Owner component is the sole scheduler, persister, and firer of
every approval gate's escalation timer. The Generic Approval service supplies configuration only
(window, escalation path) and never stores timer state.

**Rationale**: splitting timer ownership across the service boundary is the exact failure mode
that leaves neither side certain which one owns the clock during a partial outage; keeping
ownership undivided in this gear removes that ambiguity.

**Propagates to**: `gears/bss/orders-workflow/docs/design/03-approval-execution.md` § `2.1 Timer
ownership never splits across a service boundary`

### D-11: Escalation-timer pause preserves accrued elapsed time on hold and on a Generic Approval outage, and never resets the window

**Amended by D-70 and D-87 (2026-09-24).** The pause is the `pause_causes` of `owf_approval_gate` written through slice 03's gate-window port, and the remainder `window_remaining_ms` is the only remainder authority; `apply-hold`/`apply-resume` return it and the definition re-arms exactly that remainder. The Generic Approval outage pause is a probe arm over `escalate-gate` in `probe` mode (D-87). `owf_timer_pause` is retired.

**Decision**: hold and a Generic Approval service outage on an already-open gate use the same
pause mechanism owned by the Escalation Timer Owner (D-10). On resume, the timer's previously
accrued elapsed time is preserved rather than restarted from zero.

**Rationale**: the PRD forbids the escalation window burning into a dead dependency; resetting to
zero on resume would let repeated short outages extend the effective SLA indefinitely, which is
the opposite of the escalation window's purpose.

**ADR**: ADR-0007 (`cpt-cf-bss-orders-workflow-adr-fail-closed-verdict-park`) — the gate-pause-on-outage consequence this entry restates.

**Propagates to**: `gears/bss/orders-workflow/docs/design/03-approval-execution.md` § `3.6
Escalation timer fire and approval-service outage pause`

### D-12: Verdict-source unavailability parks the process fail-closed in `submitted`, and the park never suspends the Lifecycle `submitted` TTL

**Amended by D-78 (2026-09-24).** The park is a definition arm (`park`, a park loop armed by `arm-park-escalation`, exit by `unpark` or the fence); the fail-closed rule and the TTL rule are unchanged, and the park clock is governed by the one outage threshold of D-87.

**Decision**: every verdict-source or approval-service unavailability path parks or pauses the
process rather than assuming success; the order remains in Lifecycle's `submitted` state. This
gear never fails open to `approved`. The park does not suspend the Lifecycle `submitted` TTL, so
this gear escalates to the operator queue before that TTL elapses.

**Rationale**: fail-open on an unevaluable approval verdict would let an order proceed to
fulfillment with no policy check ever having run; the alternative of suspending the TTL was
rejected because Lifecycle's TTL is a Lifecycle-owned bound this gear cannot silently extend.

**ADR**: ADR-0007 (`cpt-cf-bss-orders-workflow-adr-fail-closed-verdict-park`) — the fail-closed park decision this entry restates, including the `parked` process phase.

**Propagates to**: `gears/bss/orders-workflow/docs/design/03-approval-execution.md` § `2.1
Fail-closed, never fail-open, never auto-reject`

### D-13: The verdict cache is keyed on orderId plus orderVersion, and a stale-version verdict is discarded rather than reflected

**Decision**: the approval-requirement verdict is cached per `orderId` + `orderVersion`. On
`OrderAmended`, the Verdict Gateway re-queries the verdict for the new version from scratch — it
never derives the new version's verdict from the version it superseded. A verdict result that
arrives for a version this gear has already moved past is discarded as stale, never reflected
onward.

**Rationale**: mirrors the sibling Orders Lifecycle design's normative rule that deriving an
amended version's verdict from the version it superseded is forbidden; a stale verdict reflected
onward would apply a policy decision made against commercial facts the order no longer has.

**Propagates to**: `gears/bss/orders-workflow/docs/design/03-approval-execution.md` § `3.2
Verdict Gateway` and § `3.6 Verdict retrieval and reflection on OrderSubmitted`

### D-14: Every stored verdict names its deciding authority, and the phase-1 stand-in is recorded as that authority by name

**Decision**: until the Generic Approval service exists, every verdict query resolves through a
named stand-in acting for `cpt-cf-bss-orders-workflow-actor-owf-generic-approval`, behind the
PRD §9.2 expectations contract — not a second policy author. It always returns "approval not required,"
and every such verdict is recorded with the stand-in named as the deciding authority. As a
consequence, multi-party gates, escalation timers, the approver inbox, and the
`OrderApprovalRequested` / `OrderApprovalEscalated` events never fire in phase 1 — the approval
capability is inert until the Generic Approval service exists.

**Rationale**: naming the stand-in as the deciding authority on every stored verdict is the only
field that lets a later audit distinguish an order the policy actually exempted from an order
nobody ever asked about, given the stand-in currently exempts everything.

**Propagates to**: `gears/bss/orders-workflow/docs/design/03-approval-execution.md` § `4
Disclosure 1 — the whole capability is inert in phase 1`

## D. Fulfillment plan (slice 04)

### D-15: A pre-activation abort is structurally distinct from a line-execution failure

**Amended by D-91 (2026-09-24).** The evaluation point moves: `construct-and-freeze-plan` records the overlap and market observation only, and `re-check-pre-activation` is authoritative before wave 2. An unevaluable re-check follows Lifecycle's `defer` ladder and then aborts with its own reason, `overlap-read-unevaluable`, never `overlap-collision`; the abort stays structurally distinct from a line-execution failure as stated below.

**Decision**: an overlap collision or market divergence detected immediately before the first
activation intent voids wave-1 drafts and acknowledges `fulfillment_failed` directly. It never
touches `FulfillmentTask.state` and never enters the remediate/fail-fast partial-failure policy
that governs a real line-execution failure.

**Rationale**: by construction nothing beyond a `draft` subscription has ever existed when a
pre-activation abort fires, so there is nothing to remediate and no partial outcome to reconcile;
conflating the two paths would let an operator retry a condition with no correctable root cause,
or let a real provisioning failure escape compensation.

**Propagates to**: `gears/bss/orders-workflow/docs/design/04-fulfillment-plan.md` § `2.2
Pre-Activation Abort Is Not a Line Failure` and § `3.6 Pre-Activation Abort`

### D-16: Dependency resolution is Catalog-owned, graph-validated, and frozen per orderId plus orderVersion before any provisioning intent; a bundle is one line item

**Superseded by D-196 (2026-09-30).** Under PriceBook there is no inter-line product topology: a line is one plan revision with its selected items, and nothing links one revision to another. The plan carries no dependency graph and reads no Catalog; the freeze per `orderId` + `orderVersion` and the one-task-per-line rule stand. Historical rationale follows.

**Decision**: the order document carries no dependency data. The Plan Constructor reads Catalog's
published topology, builds exactly one `FulfillmentTask` per order line item — bundle lines are
never expanded into multiple tasks — validates the resulting graph acyclic with every dependency
present among the order's own lines, and hands the result to the Plan Freeze Store, which is
keyed on `orderId` + `orderVersion` and rejects any post-freeze mutation. An invalid graph halts
fulfillment before any subscription is created and needs no compensation.

**Rationale**: this gear does not own product topology, so re-deriving or caching dependency data
locally would create a second, driftable copy of Catalog's own data; freezing the plan per
version before any provisioning intent prevents a mid-execution amendment from mutating a plan
that provisioning has already started acting on.

**Propagates to**: `gears/bss/orders-workflow/docs/design/04-fulfillment-plan.md` § `2.1
Dependency Ownership Stays With Catalog`, § `2.2 Bundle Lines Are Never Expanded`, and § `3.6 Plan
Construction and Freeze`

### D-17: Pending and failed payment authorization are two distinct, never-collapsed process outcomes

**Amended by D-90 (2026-09-24).** Payments is read on request inside `evaluate-payment-auth-eligibility`, polled by a definition `wait`; pending and failed stay distinct, and Lifecycle is the sole evaluator of tolerate-failure (D-89).

**Decision**: payment-authorization pending and payment-authorization failed are kept as two
distinct begin-fulfillment guard outcomes, never collapsed into one. Tolerate-failure and
recorded-buyer-acceptance are separate begin-fulfillment guards, and `OrderAcceptanceRecorded`
re-triggers begin-fulfillment eligibility evaluation. Payments' mechanism is consumed as an
opaque outcome only.

**Rationale**: collapsing "pending" (an asynchronous wait) into "failed" (a terminal negative
result) would force an order awaiting an SCA-style challenge down the same path as an order whose
charge was declined, which are not the same commercial situation and do not warrant the same
process response.

**Propagates to**: `gears/bss/orders-workflow/docs/design/04-fulfillment-plan.md` § `2.1 Pending
and Failed Are Distinct Outcomes`

### D-18: The downstream Subscriptions transition-request id is a join key on the fulfillment task, never mirrored into order state

**Decision**: the transition-request identifier returned by Subscriptions for a provisioning
intent is recorded on the corresponding `FulfillmentTask` row as a join key only. It is never
copied onto the order document or any Lifecycle-owned field.

**Rationale**: keeps process-owned correlation data separate from the commercial order aggregate
that Lifecycle owns, consistent with the dual-authority principle (this gear drives process,
Lifecycle owns commercial state).

**ADR**: ADR-0003 (`cpt-cf-bss-orders-workflow-adr-process-state-non-authoritative`) — process state is
never presented as order state (R5), which this entry applies to the downstream request id.

**Propagates to**: `gears/bss/orders-workflow/docs/design/04-fulfillment-plan.md` § `3.7 Table:
owf_fulfillment_task`

## E. Commercial policy — owned elsewhere, no code default

### D-19: Partial-failure policy overrides per product line or commercial tier are commercial policy, not a design default

**Decision**: whether the default partial-failure policy ("continue independent lines, halt
dependents") may be overridden per product line or commercial tier is commercial policy owned by
Product, not a call this design takes. This design implements only the default and provides no
per-tier override mechanism until Product resolves the question.

**Rationale**: defaulting an override behaviour in code before Product has decided whether one is
needed would make an unset commercial choice silently become the platform answer; leaving it
absent keeps that gap visible.

**Propagates to**: `gears/bss/orders-workflow/docs/PRD.md` § `15. Open Questions` (partial-failure
policy defaults per product line)

### D-20: Approver absence and delegation reassignment is commercial/process policy owned by Product, not resolved by this design

**Decision**: what happens when an approval gate's principal has left, changed role, or lost
scope is Product-owned policy. This design's only implemented behaviour for an open, unresolved
gate is escalation per D-10/D-11; reassignment of an open gate is out of scope and has no code
default.

**Rationale**: inventing a reassignment mechanism without a Product-confirmed rule would encode a
guess as platform behaviour; escalation is the one behaviour the PRD already authorizes as an
interim answer.

**Propagates to**: `gears/bss/orders-workflow/docs/PRD.md` § `15. Open Questions` (approver
absence and delegation)

## F. Provisioning intents (slice 05)

### D-21: The idempotency key composes orderId, orderVersion, orderLineId, wave, and intentKind, and is structurally distinct from correlationId

**Decision**: the idempotency key for every provisioning intent is five components —
`orderId` + `orderVersion` + `orderLineId` + `wave` + `intentKind` — tenant-prefixed with
`resource_tenant_id`. `intentKind` is the existing `owf_provisioning_intent.intent_kind` enum
(`draft_create | activation | draft_void | activated_cancel`). `orderVersion` is mandatory. The
key is never reused as `correlationId`, and `correlationId` is never substituted for it. For the
wave-1 rebuild path a sixth component, `attempt` (integer from 1, persisted on
`owf_provisioning_intent.wave_attempt`), is appended; it is the only component a rebuild changes.

**Rationale**: omitting `orderVersion` was rejected because a superseding amendment would then
collide with the prior version's key and either mask a legitimate resubmission as a duplicate or
let a stale-version retry pass as current. `intentKind` is required for a sharper reason: a
compensating intent shares its forward intent's order, version, line **and wave**, so a
four-component key makes `draft_void` byte-identical to the `draft_create` it reverses and
`activated_cancel` identical to the `activation` it reverses. `UNIQUE (idempotency_key)` then
rejects the compensating row locally, and Subscriptions returns the stored forward outcome as an
absorbed duplicate — so compensation records success against a subscription that is still live.
The key's purpose (submission de-duplication) and the correlationId's purpose (whole-process
tracing) diverge in lifetime and semantics, so conflating them would let an aged-out key be reused
to resubmit or lose traceability once the key expires. The `attempt` component exists because a
deterministic composition otherwise re-derives an identical key for the rebuild, which the design
forbids resubmitting.

**Amended (2026-09-26)** by D-119: the sixth component is per line and per wave, kept on the task
row, and appended to the `activation` key too. It is also minted by an operator's retry of an
intent recorded `failed`, so it is no longer only the component a rebuild changes.

**ADR**: ADR-0006 (`cpt-cf-bss-orders-workflow-adr-idempotency-key-composition`) — the five-component key composition this entry restates.

**Propagates to**: `gears/bss/orders-workflow/docs/design/05-provisioning-intents.md` § `2.2 The
Idempotency Key Is Not the Correlation Identifier`

### D-22: The binding reference is opaque and caller-owned; Subscriptions and OSS never interpret it as order state

**Decision**: every provisioning intent additionally carries an opaque, caller-owned binding
reference (`binding_reference`), which may be derived from the order's external reference where
one exists. Subscriptions and OSS consume it as an opaque token only and never interpret it as
order state.

**Rationale**: giving Subscriptions or OSS a field they could read as order state would create a
second, driftable channel for commercial order content outside Lifecycle's system of record;
keeping the reference opaque preserves the dual-authority boundary this gear's process state
already observes toward order state.

**Propagates to**: `gears/bss/orders-workflow/docs/design/05-provisioning-intents.md` §
`3.7 Database schemas & tables` (`owf_provisioning_intent`, `binding_reference`)

### D-23: A pre-activation draft re-read immediately before every activation intent is the sole detection mechanism for an auto-voided wave-1 draft

**Amended by D-78 (2026-09-24), as corrected there.** The re-read runs **inside** `dispatch-wave2-activate`, immediately before it submits any activation intent, and a lapsed draft is reported in its `lapsed[]`; `reread-draft-liveness` is a `composable`, advisory early read the definition may place after the expected-fulfillment wait, and the definition is not required to call it. The rule that the re-read immediately before every activation intent is the sole detection mechanism stands.

**Decision**: the Draft-Liveness Re-reader re-reads the wave-1 draft's liveness immediately before
every activation intent, and this re-read is the only detection mechanism this design relies on
for an auto-voided draft, regardless of cause (hold, platform TTL, or otherwise). No design path
depends on Subscriptions emitting a draft-void notification.

**Rationale**: Subscriptions is not obligated to emit a void notification for every voiding cause,
so a design depending on one would silently miss cases; re-reading immediately before the intent
that depends on liveness is the one point where staleness is guaranteed to be caught before it can
matter.

**Propagates to**: `gears/bss/orders-workflow/docs/design/05-provisioning-intents.md` §
`2.1 Detection by Re-read, Never by Absence of Notification`

### D-24: A voided wave-1 draft is rebuilt by re-running wave 1 against the same frozen orderId and orderVersion, never by resuming into wave 2

**Amended by D-78 and D-95 (2026-09-24).** The rebuild is the operation `rebuild-wave1`; a lapsed draft routes rebuild → wave 1 in the definition, and `draft_created → pending` (reason `draft-voided`) is driven only by it.

**Decision**: when the pre-activation re-read finds a voided draft, the Wave-1 Rebuilder issues a
fresh draft-create intent with a fresh idempotency key, against the same frozen `orderId` and
`orderVersion` already established for that line; it never attempts to proceed directly into wave
2 against the voided draft.

**Rationale**: an activation intent has no meaning without a live draft to activate, and rebuilding
against the same frozen version (rather than re-resolving the plan) keeps the rebuild consistent
with the plan-freeze decision already taken for dependency resolution.

**ADR**: ADR-0006 (`cpt-cf-bss-orders-workflow-adr-idempotency-key-composition`) — the rebuild `attempt` component is what makes the rebuild key fresh.

**Propagates to**: `gears/bss/orders-workflow/docs/design/05-provisioning-intents.md` §
`3.2 Component Model` (Wave-1 Rebuilder)

### D-25: The reconciliation sweep becomes read-only for an intent once its idempotency-key lifetime elapses, and never resubmits under an aged-out key

**Decision**: past the idempotency-key lifetime, the reconciliation sweep switches its lookup
strategy to `correlationId` plus `orderId`/`orderVersion`/line/wave, or the recorded
transition-request identifier (`SUB-O13`), and only re-reads status from that point forward. It
never invents a new idempotency key on a retry and never resubmits a submission under the aged-out
key.

**Rationale**: reusing or re-deriving a key past its lifetime would risk a second acceptance for a
submission Subscriptions may already have progressed past de-duplication for; read-only recovery
by correlation identifiers keeps reconciliation safe without depending on submission
de-duplication that is no longer guaranteed to apply.

**Propagates to**: `gears/bss/orders-workflow/docs/design/05-provisioning-intents.md` §
`3.2 Component Model` (Reconciliation Sweep)

### D-26: The first activation intent, not wave-1 draft-create acceptance, is reported as the spawn signal to Orders Lifecycle

**Decision**: this gear reports the spawn signal to Orders Lifecycle at the first activation
intent's confirmation, never at wave-1 draft-create acceptance.

**Rationale**: a `draft` subscription is not resource-affecting and does not commit the order to
provisioning; reporting spawn on draft-create acceptance would cause Lifecycle to treat the order
as spawned prematurely, before any activation has actually begun.

**Propagates to**: `gears/bss/orders-workflow/docs/design/05-provisioning-intents.md` §
`3.6 Interactions & Sequences` (Two-Wave Provisioning Dispatch)

## G. Saga and compensation (slice 06)

### D-27: Both waves are compensable with no intra-saga pivot; compensation runs in reverse order across two structurally distinct legs

**Decision**: wave-1 create is compensated by draft-void and wave-2 activation by
activated-cancel; there is no irreversible branch and no intra-saga pivot anywhere in this
design. Activation changes which leg applies, never whether a compensating leg exists.
Compensation for a multi-line order runs in reverse order of forward execution. OSS-side
irreversibility is absorbed inside Subscriptions' own cancel handling — this gear never reaches
past Subscriptions to undo OSS directly, in compensation any more than in forward execution.

**Rationale**: an irreversible branch would leave some failure combinations with no rollback path,
directly producing the stranded-subscription risk this design's own risk register names; keeping
both legs symmetric and running them in reverse order avoids compensating a line whose sibling
dependency has not yet been unwound.

**ADR**: ADR-0005 (`cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot`) — the compensable/no-pivot classification and the wave-grouped unwind order this entry restates.

**Propagates to**: `gears/bss/orders-workflow/docs/design/06-saga-and-compensation.md` §
`2.1 No Intra-Saga Pivot`

### D-28: Operational compensation is a reporting boundary to Lifecycle, never a Billing gate

**Decision**: operational compensation (draft-void, activated-cancel) completes or escalates on
its own timeline and is reported to Lifecycle the moment it reaches a known outcome. Reversal of
posted at-sale billable facts is tracked separately in the billing chain and is never awaited;
`CompensationRecord` records whether at-sale facts had been emitted as evidence only, never as a
gate.

**Rationale**: waiting on billing reversal before reporting an operational outcome would tie an
operational timeline to a financial-reconciliation timeline with different SLAs and a different
system of record, which this gear does not own.

**Propagates to**: `gears/bss/orders-workflow/docs/design/06-saga-and-compensation.md` §
`2.2 Operational Compensation Never Waits on Billing`

### D-29: Draft-void failure and activated-cancel failure are two structurally distinct manual-task reasons, never collapsed into one generic compensation-failure reason

**Decision**: `CompensationFailureReason` enumerates exactly `draft-void-failed` and
`activated-cancel-failed`. A compensating action that cannot complete never invents a third
compensating action; it escalates via the existing manual-task path with its own leg-scoped
reason and leaves the order non-terminal.

**Rationale**: collapsing the two into one generic "compensation failed" reason would erase, at
the operator queue, which leg (a still-draft subscription versus an active one) actually needs
remediation, which changes the operator's remediation action.

**ADR**: ADR-0005 (`cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot`) — the leg-specific manual-task reasons this entry restates.

**Propagates to**: `gears/bss/orders-workflow/docs/design/06-saga-and-compensation.md` §
`2.2 Distinct Manual-Task Reasons per Leg`

### D-30: A dedicated five-step cancellation-fencing sequence must complete in full before any compensated outcome is reported, and a late success still counts as a created subscription requiring compensation

**Decision**: the Cancellation-Fencing Component runs a strict, ordered five-step sequence — (1)
stop dispatching new provisioning intents, (2) identify all intents already accepted by or in
flight to Subscriptions, (3) wait for and reconcile terminal outcomes via the existing
reconciliation sweep, (4) compensate every created subscription including late successes, (5)
verify no active subscription remains — and all five steps must complete before any compensated
outcome is reported. A confirmation that arrives after cancellation was requested is still treated
as a created subscription and is compensated, never discarded as moot. Superseding an accepted
in-flight intent uses `SUB-O12` (cancel/void), never a second submit.

**Rationale**: reporting a compensated outcome before every in-flight intent has reached a terminal
outcome would risk declaring the order cancelled while a subscription that was actually accepted
sits unaccounted for; treating a late success as moot rather than as created would leave that
subscription active and billed with no rollback path, the same stranded-subscription failure mode
the design's own risk register names.

**ADR**: ADR-0005 (`cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot`) — cancellation fencing as a precondition of reporting any compensated outcome.

**Propagates to**: `gears/bss/orders-workflow/docs/design/06-saga-and-compensation.md` §
`3.6 Interactions & Sequences` (Five-Step Cancellation Fencing)

### D-31: The FulfillmentTask unilateral-cancel window closes at activation-intent acceptance, not at draft-create acceptance

**Decision**: a `FulfillmentTask` with only an accepted wave-1 draft-create is still unilaterally
cancellable without the fencing machinery, because a `draft` subscription is not
resource-affecting. From activation-intent acceptance onward, the task must be reconciled to a
terminal outcome through the fencing sequence before the order-level outcome is reported.

**Rationale**: fencing exists to guard against a race with an already resource-affecting
provisioning action; requiring the full fencing machinery for a still-draft, non-resource-affecting
subscription would add process overhead with no corresponding race to guard against.

**Propagates to**: `gears/bss/orders-workflow/docs/design/06-saga-and-compensation.md` §
`3.2 Component Model` (Cancellation-Fencing Component, Additional context)

## H. Manual tasks, override, queue, overdue escalation (slice 07)

### D-32: Exactly one actionable manual task is created per permanently failed line under the default remediation policy, from any of the four failed-entrance routes; fail-fast substitutes a non-actionable tracked incident instead

**Decision**: a single creation call site (Manual-Task Creator) is the only entrance to
`ManualTask` creation, invoked identically from all four `failed`-entrance routes — retry
exhaustion, failure confirmation, step-deadline expiry, and sweep-discovered terminal failure —
under the default remediation policy, with dedup against an already-existing task for the same
`FulfillmentTask`. Under fail-fast, a structurally distinct, non-actionable `Incident` is recorded
instead, carrying identifying and failure-reason fields but no SLA deadline or resolution actions.
Together the two paths guarantee 100% failure visibility: every permanent failure produces either
one manual task or one tracked incident, never neither.

**Rationale**: "by any route" is only honest if enforced structurally at one call site rather than
by convention at each entrance, since a task created on only some paths reproduces the
silent-failure bug the visibility requirement exists to prevent; an actionable retry/override/
escalate object is meaningless once the order is already terminal under fail-fast, so a degraded
version of the same object was rejected in favour of a structurally distinct, honestly
non-actionable record.

**ADR**: ADR-0009 (`cpt-cf-bss-orders-workflow-adr-manual-task-dead-letter-separation`) — the one-object-per-failure rule and the manual-task / tracked-incident split.

**Propagates to**: `gears/bss/orders-workflow/docs/design/07-manual-tasks.md` §
`2.2 Any-Route Manual-Task Creation`

### D-33: An override is rejected outright whenever it cannot be verified against a matching, active Subscriptions record; there is no provisional-acceptance path

**Decision**: override acceptance is gated on a synchronous Subscriptions verification call
(active, matching plan, quantity, and tenant), with no provisional-acceptance path. Failure to
verify is a hard rejection, not a soft warning. Operator identity and justification are recorded
in the audit log only on the verified-success branch.

**Rationale**: `OrderCompleted` carries the per-line `subscriptionId` as its audit backbone under
Lifecycle's atomic-completion invariant, so an unverified override would fabricate that linkage;
verification must precede trust rather than trust being extended provisionally and reconciled
later.

**Propagates to**: `gears/bss/orders-workflow/docs/design/07-manual-tasks.md` §
`3.6 Interactions & Sequences` (Override Rejected Without Verified Subscription)

### D-34: The overdue-fulfillment escalation window never auto-terminals the order and never by itself marks any line failed

**Decision**: the Overdue Escalation Monitor's firing is strictly non-terminalling — it raises an
operational escalation to the fulfillment operator queue only. The order remains `in_fulfillment`
(or the `on_hold` state taken from it) until operational compensation reaches a known outcome or an
operator explicitly cancels; the window itself never transitions any line to `failed` and never
auto-terminals the order.

**Rationale**: a subscription-spawn signal may already be in flight when the window elapses, and an
automatic terminal transition on top of that would be unsafe; this is also why Orders Lifecycle
does not auto-expire `in_fulfillment` or holds taken from it, making a non-terminalling escalation
the only safe deadline mechanism available to this gear.

**Propagates to**: `gears/bss/orders-workflow/docs/design/07-manual-tasks.md` §
`2.2 Overdue Window Is Non-Terminalling`

## I. Hold, resilience, and workflow-mediated cancel (slice 08)

### D-35: A hold suspends this gear's own dispatch and timers only; it never pauses, reads, or owns the Subscriptions draft auto-void TTL, and a paused timer resumes with its remaining window, never the full window

**Amended by D-80 (2026-09-24).** A hold is the Lifecycle `OrderHeld` event consumed by a `listen` arm that calls `apply-hold`; it pauses only the approval-escalation `wait`, in the approval stage, and dispatch operations refuse new intents while suspended. The Subscriptions draft TTL rule stands.

**Decision**: `OrderHeld` suspends this gear's own dispatch and timers only. It never pauses,
reads, or assumes the state of the Subscriptions draft auto-void TTL, which is a foreign
aggregate's clock and keeps running through a hold; already-accepted provisioning intents run to
terminal outcome against the frozen plan and are not reversed by a hold. Timer pause stores the
remaining window explicitly rather than re-deriving it, and resume rearms at `resume_time +
remaining_window`, never `resume_time + full_window`. Resume never performs its own wave-1
rebuild — it invokes slice 05's existing pre-activation draft re-read before any activation
intent and defers to slice 05's rebuild mechanism if a voided draft is found, regardless of
whether the hold, the draft TTL, or any other cause voided it.

**Rationale**: "suspend the process" is not the same guarantee as "pause every clock the order is
subject to"; treating the two as equivalent would let a hold silently extend a foreign TTL it does
not own, and re-deriving a remaining window instead of storing it would risk resetting a
partially-elapsed window to full on resume, defeating the guarantee a single hold/resume cycle
must preserve.

**Propagates to**: `gears/bss/orders-workflow/docs/design/08-hold-and-cancel.md` §
`2.1 A Hold Suspends Dispatch, Not Foreign Clocks` and §
`2.2 Timer Pause Preserves Remaining Window`

### D-36: Dependency-retry resilience and the Generic Approval fail-closed park are structurally distinct mechanisms, never one code path branching on dependency name

**Amended by D-70 (2026-09-24).** The retry half is now the definition's task retry policy over `retryable-failure` outcomes; the Dependency Retry Governor is retired. The two mechanisms stay structurally distinct: the park is a separate definition arm.

**Decision**: retry-then-manual-task applies only to Lifecycle/Subscriptions/Payments transient
unavailability, uses the calling step's existing retry budget, and escalates to a manual task on
exhaustion. Generic Approval unavailability (once that service exists) is exclusively slice 03's
fail-closed park: the order stays `submitted`, the Lifecycle `submitted` TTL is not paused, and
escalation happens before that TTL elapses. The two mechanisms are never unified into one code
path that branches on which dependency is unavailable.

**Rationale**: the two mechanisms answer different questions — resilience assumes the dependency
will recover and the process should keep trying, while the fail-closed park assumes no verdict can
be trusted yet and the process must not proceed; collapsing them would risk retrying a park
condition indefinitely or parking on a merely transient outage that resilience should absorb.

**ADR**: ADR-0007 (`cpt-cf-bss-orders-workflow-adr-fail-closed-verdict-park`) — the fail-closed park is the mechanism this entry keeps structurally distinct from dependency retry.

**Propagates to**: `gears/bss/orders-workflow/docs/design/08-hold-and-cancel.md` §
`2.1 Retry-Then-Escalate Is Not the Same Mechanism as Fail-Closed Park`

## J. Read projection and authorization (slice 09)

### D-37: Every system-actor grant requires a verified service principal — `subject_type` plus `token_scopes` naming the calling gear on REST, the broker's produce grant on the event transport; actor class alone is never sufficient

**Amended by D-69 (2026-09-24).** The event-transport half no longer has a consumer in this gear: the platform consumes the events and every event reaches Orders as a step call from the serverless-runtime service principal under `process_step × execute`. The REST-surface half stands and is the step route's principal check.

**Amended by D-63 (2026-09-24).** The original wording required "a gateway-asserted service
principal carrying a scope claim naming the calling gear" and, on the event transport, a signed
envelope. The platform `SecurityContext` carries no such claim and the broker signs no envelope,
so the requirement is restated in the platform's own terms; the requirement itself is unchanged.

**Decision**: every system-actor grant (Orders Lifecycle, Generic Approval, Subscriptions,
Payments) on the REST surface requires a platform `SecurityContext` whose `subject_type` is the
service-subject type and whose `token_scopes` names this gear, followed by the platform PDP's
allow on the route's registered `(resource, action)` pair for that subject; on the event
transport it requires delivery on a topic whose produce grant the broker issued to the publishing
gear alone, under platform-root tenancy (Lifecycle D-95), followed by the handler's
PDP-authorized Lifecycle `order × read` of the named order. Actor class alone is insufficient to
authorize a call. Generic Approval, Subscriptions and Payments are additionally restricted to
reporting an outcome and hold no catalogue pair that writes order state.

**Rationale**: an actor-class-only check cannot distinguish a legitimately calling gear from any
other caller asserting the same class, which is the unscoped-grant defect the sibling Orders
Lifecycle design's own review found and this design deliberately checked for and avoided by
following that precedent explicitly rather than re-deriving it. The amendment replaces the
mechanism with one the platform provides; a control written against a claim nobody issues
protects nothing.

**Propagates to**: `gears/bss/orders-workflow/docs/design/09-read-and-authz.md` §2.2
*System-actor grants require a verified service principal*, §3.6, §4.2; `DESIGN.md` §4.2
*Service identity*

### D-38: Process-record retention defaults to at least 400 days at audit grade, configurable, independently of any durable-execution engine's own run-history retention, with the Orders Workflow platform-audit-policy owner as its named executor

**Decision**: process records (saga log, process audit, dead-letter records, manual-task history)
are retained at audit grade for a default of at least 400 days, configurable, independently of any
durable-execution engine's own run-history retention; an engine purge of run history must not
erase this gear-owned audit record. The retention policy's named executor is the Orders Workflow
gear's platform-audit-policy owner, the same role already accountable for the gear's
dead-letter/manual-task audit trail.

**Rationale**: a retention rule with no named executor is a defect since no one is accountable for
enforcing or adjusting it; tying the floor to the gear's own audit-policy owner rather than to
whichever durable-execution engine is eventually selected keeps the audit record's survival
independent of an engine-side purge policy this gear does not control.

**ADR**: ADR-0001 (`cpt-cf-bss-orders-workflow-adr-durable-execution-substrate`) — gear-owned retention is a consequence of the audit-SoR decision.

**Propagates to**: `gears/bss/orders-workflow/docs/design/09-read-and-authz.md` §
`4.3 API latency and retention policy values`

## K. Tuning-value working baselines (engine and intent path)

Every decision in this section is a **working baseline** proposed into the program-wide
non-functional workshop the PRD defers to, not a settled platform value. Each is an engineering
tuning value with no commercial consequence beyond latency and throughput; the commercial-policy
values remain unset and routed, per Q-02.

### D-39: The retry backoff curve is exponential with base 1 s, coefficient 2.0, a 30 s cap and full jitter, over a maximum of 5 submission attempts

**Amended by D-70 (2026-09-24).** The curve is the definition's `use.retries.transient` (exponential from 1 s, jitter 0–30 s, 5 attempts), applied by the platform to operations registered `retryable-on: transient`; the value stands as a working baseline.

**Decision**: the delay before attempt *n* is drawn uniformly from `[0, min(30 s, 1 s * 2^n)]`,
with a maximum of 5 attempts against intent-submission failures only.

**Rationale**: full jitter is required rather than preferred — an unjittered or equal-jittered
retry train from many concurrent orders re-synchronises on the shared Subscriptions path and turns
a transient failure into a self-inflicted load spike. Plain exponential backoff and equal jitter
were both rejected on that ground. Five attempts spends roughly 15-30 s of cumulative backoff,
which keeps the budget provably nested inside the 5 min step deadline of D-41 rather than competing
with it.

**Propagates to**: `design/01-foundation.md` §4.5 (backoff curve and jitter working baseline)

**Amended (2026-09-26)**: a 1.0.0 retry policy has a delay, a backoff kind, a jitter range and
limits, and no cap on one delay and no full-jitter form (dsl-reference.md *Retry*, *Backoff*,
*Jitter*), so the curve above cannot be declared. `01 §4.5` now states exactly what
`use.retries.transient` declares — 1 s, exponential, jitter 0-30 s, 5 attempts — and leaves the
multiplier and how the draw composes to the plugin; the sizings that depend on the spread (the
registry lease, D-103) are stated as probable, and the bound on a whole train is the task timeout
on the same `try` (re-review R-3).

### D-40: The per-attempt timeout is 10 s, set from the downstream's service objective rather than from caller patience

**Amended by D-70 (2026-09-24).** The 10 s per-attempt timeout is the per-operation `deadline` of a dispatch operation inside the envelope (`01 §4.2`); record-only operations take 5 s.

**Decision**: a single attempt is cut at 10 s.

**Rationale**: the standard is 3-10x the downstream's p99, not a figure chosen for caller comfort.
The PRD puts control-operation acceptance at p95 < 1 s, so 10 s leaves headroom for a slow-but-live
dependency while still cutting a hung socket long before it consumes the step deadline. A timeout
chosen from caller patience instead would either abandon healthy slow calls or let one hang absorb
the whole step.

**Propagates to**: `design/01-foundation.md` §4.2 (working baselines for the four bounds)

### D-41: The step deadline is 5 minutes per wave step, derived from the fulfillment service objective rather than chosen

**Amended by D-70 (2026-09-24).** The step deadline is the definition's task timeout: 3 min for wave-2 tasks, 10 min for wave-1 tasks (`01 §4.2`).

**Decision**: each wave step is bounded at 5 minutes, inclusive of all attempts and backoff.

**Rationale**: this is a derivation, not a preference. The PRD's p95 <= 15 minutes for a standard
order spans two serial waves plus the barrier and the acknowledgement, which leaves roughly 5
minutes per wave step. Choosing a larger step deadline would make the service objective
unsatisfiable by construction; a smaller one would fail healthy slow provisioning.

**Propagates to**: `design/01-foundation.md` §4.2 (working baselines for the four bounds)

### D-42: The nesting invariant over the four bounds is asserted at configuration load, and a violating configuration is refused at startup

**Amended by D-67 and D-70 (2026-09-24).** The nesting invariant is validation rule 4 of ADR-0012, refused before publish and in CI; the gear additionally refuses readiness when an operation's `deadline_ms` breaks it against a bound version.

**Decision**: per-attempt timeout **<** step deadline **<** process deadline must hold, and a
configuration violating that ordering is refused at startup rather than accepted.

**Rationale**: a mis-ordered set does not fail loudly — it silently disables the inner bound, and
nothing else in the engine detects it. Treating the ordering as an asserted invariant rather than
as documentation is what makes the four-bound model in D-02 enforceable instead of aspirational.

**Propagates to**: `design/01-foundation.md` §4.2 (the nesting invariant is normative)

### D-43: A gear-wide retry budget caps retries at 10% of request volume with adaptive throttling, in addition to the per-request attempt cap

**Amended by D-70 (2026-09-24).** The gear-wide retry share is asked of the platform as a property of its task retry policy (`…-upreq-serverless-runtime-readiness-gate`); remaining-budget propagation is the ask `…-upreq-serverless-runtime-attempt-and-deadline-propagation`.

**Decision**: the retry/backoff controller enforces a gear-wide retry share, working baseline 10%
of request volume over a sliding window, with adaptive client-side throttling beyond it. The
remaining step budget is propagated on every outbound call rather than each hop timing out
independently.

**Rationale**: per-request attempt caps alone do not prevent amplification. Under a sustained
downstream failure every in-flight order retrying five times multiplies offered load at exactly
the moment the dependency is weakest, so the aggregate rate must degrade rather than compound.
Deadline propagation was adopted with it because without it Subscriptions keeps working on
requests this gear has abandoned, widening the late-success window slice 06's fencing must then
clean up.

**Propagates to**: `design/01-foundation.md` §4.5 (a retry budget bounds the gear, not just the request)

### D-44: Per-order line parallelism is 8; the cross-process aggregate limit is sized by Little's Law from measured capacity and preferably adaptive; the dead-letter delivery cap is 5

**Amended by D-72 and D-96 (2026-09-24).** The parallelism and aggregate caps are admission controls inside the dispatch operations over `owf_dispatch_admission` (slice 05); the dead-letter delivery cap is the platform trigger path's.

**Amended by D-70 (2026-09-24).** No Orders timer waits for an admission slot: a deferred dispatch answers `deferred` with `retryAfterMs`, and the definition waits it out in a fixed-granularity re-check loop (`waitDeferral1`/`waitDeferral2`) before calling again; the caps themselves stay Orders' admission controls.

**Decision**: 8 parallel lines per order; the aggregate in-flight-intent limit carries the working
baseline **256 concurrent intents gear-wide**, to be replaced by the `L = lambda * W` derivation
from measured downstream throughput and latency once measured capacity exists, with a gradient- or
delay-based adaptive controller preferred over a static ceiling thereafter; the admission queue in
front of it is capped at **10 x the aggregate limit (2,560 entries), reject-on-full**; per-tenant
token bucket beneath a global semaphore, keyed on `seller_tenant_id`; dead-letter delivery cap
of 5.

**Rationale**: a guessed aggregate constant is either wasteful or a bottleneck as Subscriptions'
capacity moves and cannot distinguish the two cases, so the limit is ultimately a derivation from
measurement rather than a number. A blank is worse than a baseline, however: with no value at all
there is nothing to configure, nothing to load-test against, and nothing for the queue-depth cap to
be ten times of. 256 is therefore stated as a working baseline with its derivation — 32 concurrent
orders at the per-order parallelism of 8 — explicitly placeholding for the Little's-Law figure and
carrying no claim to be the measured answer. A delivery cap of 5 absorbs ordinary at-least-once redelivery
without parking a payload a brief blip would have cleared.

**Propagates to**: `design/01-foundation.md` §4.12 (concurrency and back-pressure working baselines), §4.8 (delivery-count cap)

### D-45: The reconciliation-sweep ladder is 30 s to 1 h with jitter and a bounded page size, and must resolve every intent inside the overdue window

**Amended by D-71 (2026-09-24).** The ladder drives `owf_provisioning_intent.next_sweep_at`, and the `reconciliation-sweep` worker selects every due intent; the definition's poll arm is an early read.

**Amended by D-70 (2026-09-24).** No Orders durable timer wakes the ladder: each rung is the `next_sweep_at` value on the intent row, read by the `reconciliation-sweep` worker (D-71), and the definition's poll arm is a fixed-granularity `wait` followed by `reconcile-intent` (D-70's bounded re-check loop).

**Decision**: escalating re-reads at 30 s, 1 min, 2 min, 5 min, 15 min, 1 h, capped hourly, each
wake-up jittered, each pass bounded by page size; the ladder must reach a terminal outcome or the
dead-letter floor within the 24 h overdue window.

**Rationale**: the window bound is a correctness constraint, not tuning — a schedule still
re-reading past 24 h would let the overdue escalation fire on an intent the sweep had not resolved.
Jitter is required because the sweep runs across every non-terminal intent at once, so a fixed
interval re-reads them in lockstep and the sweep becomes the load spike it exists to recover from.

**Superseded in part by D-52**, which splits this ladder in two: wave-2 intents, which sit inside
the measured fulfillment window, need a faster in-window ladder, because the single ladder here
reaches its hourly cap 23.5 minutes in — slower than the 15-minute p95 it is offered as the
recovery for. The schedule stated above is retained for every intent outside that window.

**Propagates to**: `design/05-provisioning-intents.md` §4.2 (reconciliation-sweep schedule)

### D-46: Generic Approval outage detection is fixed with a circuit breaker, while the escalation threshold remains deliberately unset

**Amended by D-87 (2026-09-24).** The circuit breaker stays in the envelope (`circuit-breaker-open`); the relational threshold governs both the park clock and the gate-open outage probe arm.

**Amended by D-70 (2026-09-24).** The breaker stays in the step envelope, but no Orders timer measures the park or the outage: the park clock and the gate-open outage probe are definition ticks (the `PT30S` probe, `PT5M` park tick) over deadlines stored on Orders' record.

**Decision**: the verdict gateway detects unavailability through a circuit breaker opening at a
50% failure rate over a sliding window of 20 calls, held open 60 s, then probed half-open. The
threshold at which a parked process becomes an operator-visible incident is stated
**relationally**, not as an absolute: `min(30 min, 0.25 x lifecycle_submitted_ttl)`, with an
escalation lead time before the TTL of `max(4 h, 0.25 x lifecycle_submitted_ttl)`. Both are
**Accepted**. `lifecycle_submitted_ttl` is read from configuration and
asserted at startup; no absolute number is fixed on this side.

**Rationale**: these are two different quantities that have been conflated elsewhere. Detection
carries no commercial consequence — it determines only how quickly this gear notices. The
escalation threshold trades directly against the Orders Lifecycle `submitted` TTL, which this gear
does not own, and the binding requirement is relational rather than numeric: escalation must fire
before that TTL elapses. A number fixed on this side would either assume a TTL this gear cannot
see or become the platform's de facto commercial answer — so the threshold is expressed as a
function of that TTL instead, which is implementable today and still leaves the commercial call to
Product. Expressing it relationally is not the same as setting it: the multipliers and floors
above still require sign-off, and the TTL itself is an open upstream ask on Orders Lifecycle. The 20-call window is narrower than the
common 100-call default because per-gate verdict-query volume is low and a 100-call window would
not open until long after the dependency was plainly unavailable.

**Propagates to**: `design/03-approval-execution.md` §4.0 (detecting a Generic Approval outage, and what this design will not set)

## L. Cross-cutting decisions (remediation)

The decisions in this section were taken while remediating the design set against review. Each one
crosses more than one slice, which is why it is recorded here rather than under a slice letter.
Each entry carries a stated value with its derivation. Eight of them were raised as proposals
rather than decisions, because they are commercial judgements — what the business promises a
customer, or what an operator is permitted to do — rather than engineering ones. They were carried
through review under an explicit sign-off marker so that none could be mistaken for a settled
number. All eight have since been accepted and are marked **Accepted** below; the derivations are
retained so a later reader can argue with the value rather than guess at where it came from.

### D-47: Idempotency keys form four tenant-prefixed families, and `gateId` is derived rather than minted

**Amended by D-74 (2026-09-24).** A fifth family is added — the event-scoped trigger family `{tenant}:{eventId}:admit-trigger[:listen]` — plus an instance-scoped family for record-only operations; `begin-fulfillment`'s key ends in the eligibility round and `compensate-order`'s in the pass.

**Decision**: every idempotency key in this gear is prefixed with `resource_tenant_id`, so key
spaces are tenant-namespaced and two tenants can never collide. Four families exist and no fifth is
invented:

| Family | Composition |
|---|---|
| Provisioning intent | `orderId` + `orderVersion` + `orderLineId` + `wave` + `intentKind` |
| Provisioning intent, wave-1 rebuild | the five above plus `attempt` (integer from 1, persisted on `owf_provisioning_intent.wave_attempt`) |
| Approval request | `orderId` + `orderVersion` + `gateId` |
| Lifecycle transition call | `orderId` + `orderVersion` + `transitionName`, where `transitionName` is one of the five seam operations |

`gateId` is **derived deterministically** as a UUIDv5 over `(orderId, orderVersion, party)`, never
minted as a random uuid.

**Rationale**: a randomly minted `gateId` breaks approval idempotency at exactly the moment it
matters — a crash between submitting to Generic Approval and committing the gate row mints a
different id on replay, hence a different key, hence a second gate for the same party on the same
order version. Deriving it makes the replay converge. The Lifecycle transition-call family exists
because reflections across that seam are retried and must be absorbed rather than double-applied,
and no other family's shape fits an order-scoped call with no line, wave or kind. The tenant prefix
is the cheapest way to make cross-tenant collision structurally impossible rather than statistically
unlikely.

**ADR**: ADR-0006 (`cpt-cf-bss-orders-workflow-adr-idempotency-key-composition`) — the full key
composition, including this family set.

**Propagates to**: `gears/bss/orders-workflow/docs/design/03-approval-execution.md` §3.7
(approval-gate schema and key), `gears/bss/orders-workflow/docs/design/05-provisioning-intents.md`
§3.7 (intent schema and key), `gears/bss/orders-workflow/docs/design/06-saga-and-compensation.md`
§2.1 (compensating-intent keys)

### D-48: Three tenant axes, and every gear-owned table carries at least the resource axis

**Amended by D-69 (2026-09-24).** `owf_step_operation` is load-only configuration and carries no tenant column — the one stated exemption to this entry.

**Amended by D-140 (2026-09-26).** There are now two stated exemptions, both configuration: `owf_step_operation`, and `owf_seller_policy`, which is keyed by `seller_tenant_id` alone, NULL on its platform row (`design/01-foundation.md` §3.7). **Amended by D-160 (2026-09-26)**: a third, `owf_configuration_revision`, the append-only history of the configuration loads, which has no tenant column.

**Note (2026-09-28, re-review C-6)**: the register of exemptions was incomplete. Three non-configuration tables also carry no `resource_tenant_id`, each with another axis: `owf_dispatch_admission`, keyed by `seller_tenant_id` alone, NULL on its aggregate row (`design/05-provisioning-intents.md` §3.7), and `owf_audit_checkpoint` and `owf_audit_checkpoint_member`, keyed on the immutable audit namespace `audit_tenant_id` (`design/01-foundation.md` §3.7). `DESIGN.md` §3.7 now lists all six. No table changes.

**Amended by D-191 (2026-09-28)**: a seventh exemption, `owf_read_access_log`, the audit read's access log, keyed on the caller's `subject_tenant_id` NOT NULL, with the order's `resource_tenant_id` and `seller_tenant_id` nullable because a refused read of an order with no instance has none (`design/09-read-and-authz.md` §3.7).

**Decision**: this gear adopts the sibling Orders Lifecycle set's three tenant axes by name —
`resource_tenant_id` (resource recipient), `payer_tenant_id` (billing party), `seller_tenant_id`
(selling party). Every one of the gear's `owf_*` tables carries **at least** `resource_tenant_id`,
`NOT NULL`. A table backing an operator- or seller-scoped surface additionally carries
`seller_tenant_id`. Each table states its axis choice in its **Additional info** line. Per-tenant
fairness and back-pressure key on `seller_tenant_id`.

**Rationale**: "every table is tenant-scoped" was asserted in the foundation slice while no table
carried a tenant column, which leaves the platform's SecureORM `#[secure(tenant_col = ...)]`
isolation with nothing to attach to and the read slice's "every read is tenant-scoped" with no
enforcing predicate on the audit, step-log, dead-letter, timer or idempotency stores. One axis is
not enough either: the party who receives the resource, the party who pays, and the party who sells
are three different tenants on a reseller order, and collapsing them would silently mis-scope
either the operator surfaces or the billing joins. Adopting the sibling's names rather than
inventing new ones keeps cross-gear joins and operator tooling consistent.

**Propagates to**: `gears/bss/orders-workflow/docs/design/01-foundation.md` §3.7 (all engine
tables), and the §3.7 of every slice that declares a table; `DESIGN.md` §3.7 (table registry)

### D-49: Optimistic concurrency is a row version surfaced as an ETag, and one active instance per order is a partial unique index

**Amended by D-75 (2026-09-24).** The partial unique index now also arbitrates supersession: the new version's `admit-trigger` answers `prior-instance-active` until the prior instance is terminal, replacing the atomic terminate-then-start step.

**Amended by D-70 (2026-09-24).** D-70 lists this entry among those it amends only because the tables it retires (`owf_durable_timer`, `owf_retry_state`, `owf_timer_pause`) leave no timer or retry-state row to version; the `row_version`/`If-Match` rule covers `owf_process_instance` and `owf_manual_task` unchanged.

**Decision**: `owf_process_instance` and `owf_manual_task` each carry
`row_version bigint NOT NULL DEFAULT 0`, incremented on every write, surfaced to callers as an
ETag and **required** as `If-Match` on the mutating operations the PRD marks "Optimistic … version
check REQUIRED". A mismatch is a `409` in the RFC-9457 problem envelope. Separately,
`owf_process_instance` carries `UNIQUE (order_id) WHERE terminal_outcome IS NULL`.

**Rationale**: the PRD requires the version check and no column, parameter or status code existed
to implement it, so two operators resolving the same task both committed and a cancel racing an
approval advance both committed. The partial unique index closes the matching hole on the other
side: "exactly one active instance per order" was specified as a read-then-insert admission check,
which two workers consuming a redelivered `OrderSubmitted` both pass, producing two instances, two
verdict queries and two Lifecycle reflections. A uniqueness constraint the database enforces is the
only form of that rule that survives concurrency; the admission read stays as a fast path, not as
the guarantee.

**Propagates to**: `gears/bss/orders-workflow/docs/design/01-foundation.md` §3.7
(`owf_process_instance`), `gears/bss/orders-workflow/docs/design/07-manual-tasks.md` §3.7
(`owf_manual_task`), `gears/bss/orders-workflow/docs/design/09-read-and-authz.md` §3.3 (`If-Match`
on mutating operations)

### D-50: The audit log is append-only and hash-chained, and human justification is a separate column from the reason code

**Decision**: `owf_audit_entry` is append-only with **no UPDATE or DELETE grant** and is
**hash-chained**: each row carries `prev_hash` and `entry_hash`, where `entry_hash` covers the
row's own content together with `prev_hash`, so a removed or altered row breaks the chain at a
verifiable point. A free-text `justification text` column carries human-supplied reasons (override
justification, cancellation reason); it is distinct from `reason`, which stays a closed catalogue
value and is the only one of the two that rides event payloads.

**Rationale**: "append-only" enforced by convention is a discipline, not a property — a compliance
grade audit trail retained for 400 days needs tamper-evidence a reviewer can check without trusting
the writer, which is what the chain provides, and it matches the sibling Lifecycle set's own audit
posture rather than inventing a weaker one. Splitting `justification` from `reason` is what keeps
the reason catalogue closed: operators need to say *why* in prose, consumers need to key on a
stable enum, and one column cannot do both without either breaking consumers or censoring
operators.

**ADR**: ADR-0001 (`cpt-cf-bss-orders-workflow-adr-durable-execution-substrate`) — the audit log is
the gear-owned source of record this hardens.

**Propagates to**: `gears/bss/orders-workflow/docs/design/01-foundation.md` §3.7
(`owf_audit_entry`) and §4.6 (audit completeness)

### D-51: `parked` is a distinct process-phase value

**Decision**: the process phase enum is `started | suspended | parked | compensating |
terminated`, with declared transitions, defined in exactly one place. `parked` is the "verdict
unobtainable" state ADR-0007 requires: a *process* phase, not an order state — the order itself
remains `submitted` — and distinct from `suspended`, which records an operator- or
caller-initiated hold. A process is never in both.

**Rationale**: ADR-0007 records the park as a required consequence and nothing implemented it: the
verdict cache admitted only `required` / `not_required`, the phase enum had no park value, and no
table, sequence branch or timer kind existed for a parked order — which left the one acceptance
criterion that applies today unsatisfiable in all four of its bullets. Reusing `suspended` was
rejected because hold and park are entered by different actors, escalate on different clocks
(gate escalation timer vs. Lifecycle `submitted` TTL) and are left by different events; one value
for both would make every query that asks "why is this order not moving" unanswerable.

**ADR**: ADR-0007 (`cpt-cf-bss-orders-workflow-adr-fail-closed-verdict-park`) — the park this value
implements.

**Propagates to**: `gears/bss/orders-workflow/docs/design/01-foundation.md` §3.7
(`owf_process_instance.phase`), `gears/bss/orders-workflow/docs/design/03-approval-execution.md`
§3.6 (the park branch)

### D-52: The reconciliation-sweep ladder splits by phase

**Decision**: the sweep runs two ladders, selected by what it is reconciling. Wave-2 intents, which
sit inside the measured fulfillment window, use **5 s → 15 s → 30 s → 60 s → 2 min, terminal by
t+5 min**. Every other intent keeps the existing ladder, **30 s → 1 m → 2 m → 5 m → 15 m → 1 h**.
Both jitter every wake-up, both page at 500 rows under a 30 s transaction budget, and the sweep
floor is **30 reads or 23 h, whichever comes first** — a separate quantity from the inbound
delivery cap of 5.

**Rationale**: the single ladder reaches its hourly cap 23.5 minutes in, which is slower than the
15-minute p95 it was offered as the design response to. Because a post-accept hang is excluded from
the retry budget, the sweep is the *only* recovery for a lost wave-2 confirmation, so a ladder
slower than the SLA guarantees the SLA is missed whenever the sweep is what saves the order. The
in-window ladder is derived from that budget rather than chosen. The existing ladder is kept for
everything outside the window because re-reading every non-terminal intent every 5 seconds makes
the sweep the load spike it exists to recover from.

**Propagates to**: `gears/bss/orders-workflow/docs/design/05-provisioning-intents.md` §4.2
(reconciliation-sweep schedule); supersedes the single-ladder statement in D-45

### D-53: A non-pausable `max_process_lifetime` of 90 days bounds every process, independently of order state

**Amended by D-70 and D-82 (2026-09-24).** The lifetime ceiling is a top-level definition `wait` arm, never cancelled by a hold; on expiry it calls `raise-overdue-escalation` and `park`, and the park is allowed from `suspended`.

**Accepted.**

**Decision**: an unconditional, non-pausable `max_process_lifetime = 90 days` is armed at process
start and is independent of order state, hold, resume or approval. There is **no cap on hold/resume
cycles**; this timer is the bound instead. On expiry the process escalates; it never silently
terminates an order.

**Rationale**: the previously adopted backstop — the overdue-fulfillment escalation window — only
runs while the order is `in_fulfillment`, so it bounds nothing for an order cycling between hold
and resume before fulfillment starts, which is precisely the case Q-03 asks about. Capping cycles
was rejected because a legitimate commercial negotiation can involve many holds and the cap would
refuse the last one for no reason the customer can see. A wall-clock lifetime armed once at start
is immune to cycling by construction. 90 days is stated as a value with a derivation — an order
non-terminal for a full quarter is an operational fact somebody must look at. The figure was
carried as a proposal through review and has since been accepted as the settled value.

**Propagates to**: `gears/bss/orders-workflow/docs/design/08-hold-and-cancel.md` §3.6 (hold and
resume), `gears/bss/orders-workflow/docs/design/01-foundation.md` §4.2 (bounds); resolves the
interim backstop recorded under Q-03

### D-54: A partial wave-2 failure holds the order; already-activated lines are not rolled back

**Amended by D-94 (2026-09-24).** D-55 remediation exhaustion is the one automatic route to order-level compensation of activated lines; an operator-initiated cancel is the other route.

**Accepted.**

**Decision**: when some wave-2 activations succeed and others fail permanently, the order is
**held**. Already-activated lines are **not** rolled back. The order cannot be acknowledged
`completed` — atomic fulfillment forbids it — and it enters the remediate policy. The only path to
a terminal outcome from there is an **operator-initiated, workflow-mediated cancel**, which
compensates every created subscription under D-27's ordering.

**Rationale**: the alternatives are worse in ways a customer feels. Rolling back the successful
lines destroys live, working service to punish an unrelated line's failure. Acknowledging
`completed` lies about an order whose lines are not all activated. Leaving it silently in flight is
the current behaviour and produces four of five lines live and billable on an order no state
machine can ever finish. Holding it makes the partial commercial outcome explicit and puts a human
on it, which is the only honest resolution while the failed line is still remediable.

**Propagates to**: `gears/bss/orders-workflow/docs/design/04-fulfillment-plan.md` §3.6 (wave-2
partial failure), `gears/bss/orders-workflow/docs/design/07-manual-tasks.md` §3.6 (remediate
policy)

### D-55: "Remediation exhausted" is three failed attempts on the same task, or the manual-task SLA deadline elapsing

**Accepted.**

**Decision**: remediation is exhausted at **3 failed operator resolution attempts on the same
task**, or when the **manual-task SLA deadline elapses without resolution** — whichever comes
first. Exhaustion is what triggers order-level compensation under the remediate policy.

**Rationale**: the trigger for order-level compensation had no definition at all, which made the
entire remediate path terminate on an undefined condition — an order either compensated on a
condition nobody could state or never compensated. Two clauses rather than one are needed because
the failure modes are different: an operator who tries three times is facing something operator
action cannot fix, and a task nobody touches before its deadline is facing nobody at all. Either
way the order stops waiting.

**Propagates to**: `gears/bss/orders-workflow/docs/design/07-manual-tasks.md` §3.6 (resolution
paths), `gears/bss/orders-workflow/docs/design/06-saga-and-compensation.md` §3.6 (entry conditions
for order-level compensation)

### D-56: The submitting identity is refused at the approval-decision endpoint

**Accepted.**

**Decision**: the identity that submitted an order is **refused** at that order's
approval-decision endpoint. Separation of duties is added as clause (g) to the §9.2 expectations
contract this gear holds against the Generic Approval service, so routing-side enforcement is
expected upstream as well as refused locally.

**Rationale**: the PRD lets an Approver be a seller operator, and nothing anywhere in the set —
PRD, DESIGN, this register, any ADR or slice — mentions separation of duties, self-approval,
four-eyes or dual control. An actor can therefore submit an order and approve their own gate with
every stated control passing, which defeats the reason the financial/legal/partner gate exists.
Routing is legitimately the approval service's concern, but this gear owns the decision endpoint
and the expectations contract, so the local refusal is the part it can guarantee today and the
contract clause is how it asks for the rest.

**Propagates to**: `gears/bss/orders-workflow/docs/design/03-approval-execution.md` §3.3 (decision
endpoint), `gears/bss/orders-workflow/docs/design/09-read-and-authz.md` §4.1 (permission matrix);
recorded as a PRD amendment ask in `UPSTREAM_REQS.md` §4

### D-57: `lifecycle_submitted_ttl` is mirrored as local configuration under a startup refusal, pending the upstream field

**Accepted.**

**Decision**: this gear holds `lifecycle_submitted_ttl` as its own configuration value, deployed in
the same promotion that carries Orders Lifecycle's TTL policy, and **refuses startup** if the value
is absent, zero or negative, or if `outage_escalation_threshold < escalation_lead_time <
lifecycle_submitted_ttl` does not hold. When Orders Lifecycle exposes the per-order expiry instant
(`cpt-cf-bss-orders-workflow-upreq-submitted-ttl-visibility`), `submitted_expires_at` supersedes the
mirror and only the source of the deadline changes.

**Rationale**: the escalation obligation in `ADR/0007` is a strict inequality against a value this
gear does not own, so it was previously unconfigurable — and an unconfigurable escalation is not a
deferred feature, it is a silent one: a parked order reaches expiry with no human alerted, which is
the fail-closed park failing from the other direction. A mirrored constant carries a real drift
risk, since Lifecycle can retune `orders_state_ttl_policy` without this gear knowing. That risk was
accepted because it is **loud where the alternative was silent**: a drifted mirror is an
operational defect, whereas an absent value was an escalation path that existed in the design and
never ran. The presence-and-positivity check is what converts the remaining failure mode from
silent inertness into a refusal at configuration load, following the nesting-invariant pattern
`01-foundation.md` §4.2 already establishes.

**Consequence**: the upstream ask stays open — this interim does not close it — but it is no longer
a blocker. The park, its timer and its operator escalation are fully operable today.

**Propagates to**: `gears/bss/orders-workflow/docs/design/03-approval-execution.md` §4.2 (the
configuration values and the startup nesting check),
`gears/bss/orders-workflow/docs/ADR/0007-cpt-cf-bss-orders-workflow-adr-fail-closed-verdict-park.md`
(the escalation obligation the inequality serves); the upstream field stays registered as
`UPSTREAM_REQS.md` `…-upreq-submitted-ttl-visibility`

**Propagates to**: `design/03-approval-execution.md` §4.2; `UPSTREAM_REQS.md` §2.4.

**ADR**: `cpt-cf-bss-orders-workflow-adr-fail-closed-verdict-park`.

### D-58 (H) Process events use the platform producer outbox; Workflow owns no outbox table, drain or re-drive

**Accepted.** *(carries [`ADR/0008`](./ADR/0008-cpt-cf-bss-orders-workflow-adr-outbox-process-events.md); mirrors Lifecycle D-17, D-87 and D-95)*

**Decision**: the six named process events are published through
`event-broker-sdk::DbProducer` with feature `outbox`, backed by `toolkit_db::outbox`, in managed
`ProducerMode::Chained` — the same path Orders Lifecycle adopted in its
[`ADR-0006`](../../orders-lifecycle/docs/ADR/0006-cpt-cf-bss-orders-lifecycle-adr-outbox-publication.md)
and D-17. The producer queue is `bss-orders-workflow-events` with `Partitions::of(16)` and the
high-throughput profile; `orderId` is the GTS event partition key; envelope tenancy is
platform-root per the Lifecycle D-95 precedent, with the resource and seller axes as `data`
fields. Workflow owns typed event construction and the transactional enqueue with the step's
transaction runner, and nothing else: there is no `owf_event_outbox`, no per-correlation
`sequence` ordinal, no `schema_version` column, no Workflow drain, lease, retry cap,
`dead_lettered_at` marker, delivered-row purge or re-drive endpoint. The engine owned seven tables
at this decision; D-59's two audit checkpoint tables make it nine.
Ordering follows platform partition semantics and a permanent reject may create a gap (Lifecycle
D-87); consumers de-duplicate by event id and verify `orderVersion` and resulting state against an
authoritative Lifecycle read, and this gear carries the same obligation as a consumer of Lifecycle
triggers and Subscriptions confirmations. Event delivery observability, dead-letter recovery,
root tenancy and the Event Broker runtime are shared platform prerequisites co-signed in
`UPSTREAM_REQS.md` §2.7.

**Rationale**: the previous revision of `ADR/0008` designed a Workflow-owned outbox table and
drain "structurally parallel to" Lifecycle's; Lifecycle has since replaced its own with the
platform producer outbox now present in the repository. Keeping a bespoke table, ordinal, lease
and dead-letter column here would fork platform behaviour, leave the two Orders gears on different
publication paths, and keep a second dead-letter surface beside `ADR/0009`'s inbound store. The
per-correlation ordinal the old table minted is not what the broker orders by, so it bought no
guarantee a consumer could rely on. The p95 < 30 s target is unaffected in principle and unproven
by commit success: it is measured as producer-queue lag, which resolves Q-07. No earlier register
entry recorded the gear-owned outbox — it was carried by `ADR/0008` and Q-07 only — so no `D-`
entry is amended; Q-07 is resolved in place.

**Propagated**: `ADR/0008` (rewritten); `ADR/0009` (outbound dead letter is a platform dead
letter); `DESIGN.md` §1.1, §1.2, §1.3, §2.2, §3.2, §3.4, §3.5, §3.6, §3.7, §3.8, §4.1, §4.3,
§4.4, §4.5, §4.8; `design/01-foundation.md` §1.1, §1.2, §1.3, §3.1, §3.2 *Platform event producer
adapter*, §3.3, §3.4, §3.5, §3.6 *Platform producer-outbox publication*, §3.7 *Platform-managed
producer persistence* and the retention/immutability register, §3.8, §4.7, §4.8, §4.9, §4.10,
§4.11, §4.14; `design/02-triggers-and-start.md` §2.1, §4; `design/05-provisioning-intents.md`
§2.1; `design/06-saga-and-compensation.md` §3.2, §3.3, §3.6; `design/README.md`;
`UPSTREAM_REQS.md` §1.2, §2.7, §3; Q-07.

**ADR**: `cpt-cf-bss-orders-workflow-adr-outbox-process-events`.

**Amended by D-177, D-178 and D-179 (2026-09-28)**: the six events publish to this gear's own topic `gts.cf.core.events.topic.v1~cf.bss._.orders_workflow.v1`, declared in the abstract type's `x-gts-traits` with `/subject`, and share no partition with Lifecycle's events (D-177). A producer-registration rotation dead-letters every message queued under the old id (D-178). A settlement enqueues zero or more events of its declared type, at most one per subject (D-179).

### D-59 (H) Retain gear-owned transactional audit following Pricing and Orders Lifecycle

**Accepted.** *(mirrors Lifecycle D-97 and D-103; hardens D-50, which remains in force)*

**Amended by D-68, D-69, D-70 and D-72 (2026-09-24).** The engine owns **eight** tables today, not nine: `owf_definition_binding` (D-68) and `owf_step_operation` (D-69) were added, and `owf_durable_timer`, `owf_retry_state` (D-70) and `owf_dead_letter_record` (D-72) retired (`DESIGN.md` §3.7). **Amended by D-140 (2026-09-26)**: `owf_seller_policy` makes it **nine**. **Amended by D-160 (2026-09-26)**: `owf_configuration_revision` makes it **ten**.

**Decision**: `owf_audit_entry` stays the authoritative process-audit record of this gear, written
in the transaction of the transition it records on every path; a failed append aborts the step's
unit of work. Each process instance carries a committed hash chain keyed on `correlation_id`,
with `sequence` allocated from the new `owf_process_instance.audit_sequence` counter under the
instance row lock and `(correlation_id, sequence)` uniqueness rejecting a competing append. The
byte contract is frozen (D-60), actor references are immutable opaque subject UUIDs (D-61), the
store carries no UPDATE or DELETE grant to any role **and** database triggers rejecting both, and
a declared verifier — the fifth Workflow-owned worker, `audit/<audit-tenant>` — walks each chain
in a rolling pass with a 30-day full pass, alerts and never repairs. Per-namespace roll-ups
`owf_audit_checkpoint` / `owf_audit_checkpoint_member` follow Lifecycle D-100 by reference with
the process instance as the member unit. The audit row gains `hash_version`, `audit_tenant_id`,
`step_id`, `attempt_number`, `definition_version` and `phase_from`/`phase_to`; `event_kind` gains
`instance-start`, `phase-transition`, `termination` and `sweep-settlement`; `correlation_id` and
`prev_hash` become NOT NULL. The engine now owns nine tables (D-58's seven plus the two
checkpoint tables). A future platform Audit Gear may receive copies under a separately specified
integration; it is never this gear's authoritative store, and the platform producer outbox is
event transport, not audit evidence.

**Rationale**: Pricing Governance G4/D-14/D-135 and Orders Lifecycle D-97/D-103 are the verified
precedent — local durable append in the business transaction, aggregate-segmented chains,
append-only permissions plus triggers, a periodic verifier and per-tenant roll-ups. Lifecycle
D-97 records the Pricing source it checked; this decision reuses that check rather than repeating
it. The event-only replacement the sibling review proposed for this gear — enqueue the audit as
process events for a future Audit Gear — was considered and is **declined for the same reasons
Lifecycle declined it**: an enqueue is transport, not retained queryable evidence; at-least-once
delivery with a permitted permanent-reject gap (D-58) cannot carry a 100 %-completeness claim; a
gear whose evidence lives in a service that does not yet exist has no evidence; and it would
leave the two Orders gears on different audit architectures while sharing one review boundary.
Nothing here presumes reviewer or platform agreement; it records this gear's direction.

| Concern | Lifecycle / Pricing baseline | Workflow alignment or explicit difference |
|---------|------------------------------|--------------------------------------------|
| Chain unit | Per order (`order_id`), per aggregate in Pricing | **Per process instance**, keyed on `correlation_id`; several instances for one order over its versions are several chains |
| Row discriminator | Transition `trigger` with a closed reason token per trigger | **`event_kind`**, the closed process-event vocabulary of `01 §3.7`; `reason` remains the catalogue value and `justification` the human text (D-50) |
| Refusals | Refused attempts are unchained rows with NULL `sequence`, a bounded retention DELETE grant and a partial purge index | **No refusal rows**: authorization refusals are the read/authz slice's concern (`09`), so `prev_hash` is NOT NULL, no role holds any DELETE grant, and the trigger rejects DELETE unconditionally as Pricing's does |
| Evidence fields | State pair, version, delegation proof, administrative delta | **`step_id`, `attempt_number`, `definition_version`, `phase_from`/`phase_to`** as evidence; no delegation-proof or before/after columns |
| Namespace | `audit_tenant_id` captured at create, distinct from an editable resource tenant | `audit_tenant_id` = the instance's `resource_tenant_id` at start, which this gear never rewrites; same immutability, no separate rebinding rule needed |
| Subject tenant | Mandatory `subject_tenant_id` scoping unresolved refusals | **Not carried**: there are no unresolved rows to scope |
| Retention | Commercial retention is Lifecycle's Q-07; Pricing's ≥ 7 years is not imported | ≥ 400 days per `cpt-cf-bss-orders-workflow-nfr-owf-retention`, unchanged; the privacy ruling stays open (`UPSTREAM_REQS.md` §2.6) |
| Instance-less evidence | Unresolved refusals leave aggregate fields NULL | An instance-less inbound dead letter is audited under the derived `correlationId` (`02 §2.1`) so every entry has a chain; admission continues that chain rather than restarting it |

**Propagated**: `design/01-foundation.md` §1.2, §3.1, §3.2 *Audit writer*, §3.7 (`owf_process_instance`,
`owf_audit_entry`, the two checkpoint tables, the retention/immutability register), §3.8, §4.6,
§4.17; `DESIGN.md` §3.7, §4.2 threat model, §4.3.

### D-60 (H) Freeze the Workflow audit hash byte contract

**Accepted.** *(mirrors Lifecycle D-99; v1 only, no v2)*

**Decision**: `01 §4.17` defines audit encoding v1 for `owf_audit_entry`: SHA-256; Workflow's
own row tag `VHP-BSS-ORDERS-WORKFLOW-AUDIT-ROW-v1` and genesis tag
`VHP-BSS-ORDERS-WORKFLOW-AUDIT-GENESIS-v1`, each followed by `0x1f`; Lifecycle D-99's framing
rules by reference — NULL-safe length-prefixed fields, binary UUIDs, big-endian integers,
microsecond UTC instants, exact persisted UTF-8 text — with a fixed field order covering every
column except `entry_hash`; `hash_version` always 1; the first sequence is 1 and genesis binds
`(audit_tenant_id, correlation_id)`. `order_id` hashes as text because it is text in this gear.
Encoding failure aborts the append. An unsupported `hash_version` fails verification explicitly.
New evidence fields or changed encoding require a new version with old decoders retained, never a
rehash of persisted rows. There is no v2: no Workflow writer has shipped and no column has been
added after the freeze, so the contract has one version and its vectors are frozen against it.

**Rationale**: D-50's `SHA-256(canonical(entry fields) || prev_hash)` named no bytes, so two
implementers would produce two incompatible chains and a verifier could not exist. Lifecycle
D-99 settled the same gap with an explicit framing; reusing that discipline — not its tag, field
set or private helper code — keeps the two Orders verifiers reviewable side by side without
asserting wire-byte compatibility. Hash binding grants no access permission.

**Acceptance**: frozen preimage/digest vectors, every-field mutation tests, NULL/empty and
framing tests, timestamp round trips, genesis and namespace checks, and transactional concurrency
tests — all to implement; this decision specifies the contract, not a running verifier.

**Propagated**: `design/01-foundation.md` §3.7 (`hash_version`, `prev_hash`, *The chaining rule*),
§4.17; `DESIGN.md` §4.2 threat model.

### D-61 (M) Audit actor references are immutable; erasure is not an in-place rewrite

**Accepted.** *(mirrors Lifecycle D-96 and D-103; supersedes the in-place pseudonymisation
paragraph `DESIGN.md` §4.3 previously carried)*

**Decision**: `owf_audit_entry.actor` stores the platform `SecurityContext.subject_id()` as
lowercase hyphenated UUID text — an opaque, pseudonymous, immutable reference — never names,
emails, credentials or caller-supplied labels; `actor_class` keeps the configured worker and
service identities apart from users. Identifying attributes and any reference-to-person mapping
belong to the identity platform. No identity-erasure UPDATE grant, historical actor replacement,
chain recalculation or verifier exemption exists; the store's triggers reject the UPDATE an
in-place pseudonymisation would need. Identity stability, non-reuse and deletion lifecycle are the
shared p2 platform follow-up Lifecycle registered as
`cpt-cf-bss-orders-lifecycle-upreq-audit-identity-lifecycle`
([Lifecycle `UPSTREAM_REQS.md §2.8`](../../orders-lifecycle/docs/UPSTREAM_REQS.md#28-identity-platform));
this gear references it and does not copy it. The privacy ruling of `UPSTREAM_REQS.md` §2.6 stays
open and is not waived.

**Rationale**: the previous erasure text made pseudonymisation "the single permitted mutation of
the audit store", re-deriving the chain afterwards — which is a privileged chain-rewriting path,
the exact capability a tamper-evident store must not have, and one that makes routine erasure
indistinguishable from tampering. Separating identity data from evidence preserves the original
bytes; pseudonymous references do not by themselves make evidence anonymous, so Privacy/Legal
must still approve retained content, linkage risk and retention. Pricing and Lifecycle already
hold this position; a third answer in the same domain would be a defect.

**Propagated**: `DESIGN.md` §4.3 (authoritative identity and erasure contract), §4.2 threat
model; `design/01-foundation.md` §3.1, §3.7 (`actor`), §4.17 *Verifier*; `UPSTREAM_REQS.md` §2.6.

### D-62 (M) Workflow-owned workers coordinate through toolkit-db session advisory locks under a named roster

**Amended by D-71 (2026-09-24).** The roster is three workers — `reconciliation-sweep`, `retention-purge`, `audit/<audit-tenant UUID>`; `timer-wakeup` and `dead-lease-scan` are removed with the timers.

**Accepted.** *(mirrors Lifecycle D-92's coordination clarification)*

**Decision**: the previously unnamed "coordination lease library" is replaced by
`toolkit_db::Db::lock` / `Db::try_lock` session advisory locks. `01 §3.8` is the authoritative
roster for the five Workflow-owned workers in gear namespace `bss-orders-workflow` —
`timer-wakeup`, `reconciliation-sweep`, `dead-lease-scan`, `retention-purge` and
`audit/<audit-tenant UUID>` — each with the transactional recheck that keeps it correct when its
lock session is lost, because a session advisory lock is not a TTL lease and not a fence.
Lifecycle `01 §3.8`'s deployment constraint and *session loss is not fencing* rule apply by
reference. There is no idempotency-window sweep (registry retention is the retention purge's tombstone rule, D-104) and no
dead-letter delivery-count sweep (the delivery path parks inline). `cluster-sdk` is not selected
for the reason Lifecycle gives. `gears/bss/libs/coord` — Pricing's DB-backed TTL lease with an
in-transaction fence — is a candidate for the same roster and is registered as Q-09 for the
Lifecycle, Pricing and Workflow owners to decide jointly rather than chosen here. Toolkit owns
outbox coordination (D-58).

**Rationale**: a dependency that names no crate cannot be vetted, licensed or tested, and `01 §3.8`
listed three workers while the schema already required a dead-lease scan and the audit contract
requires a verifier. Adopting Lifecycle's primitive and table shape puts the two Orders gears on
one coordination contract, and stating the recheck per worker makes the primitive swappable if
Q-09 lands on the fenced lease.

**Propagated**: `design/01-foundation.md` §1.3, §3.4, §3.8, §4.15; `DESIGN.md` §1.3, §2.2,
§3.4, §3.8, §4.1, §4.2 *Supply chain*, §4.5; Q-09.

**Amended by D-187 (2026-09-28)**: a contended acquisition — `AlreadyHeld` from `Db::lock`, `Ok(None)` from `Db::try_lock` — skips the pass and is not a coordination failure; only an error from either call, or a database error during the pass, is.

### D-63 (H) Authorization is delegated to the platform PDP through the shared PolicyEnforcer adapter

**Accepted.** *(carries [`ADR/0010`](./ADR/0010-cpt-cf-bss-orders-workflow-adr-platform-pdp-authorization.md); mirrors Lifecycle D-34 as amended, D-111, D-114, D-141 and D-115; amends D-37)*

**Amended (2026-09-26)**: the "twenty-five tables" of the propagation line below was the count at this decision; the registry now holds twenty-seven (`DESIGN.md` §3.7; twenty-six until D-140 added `owf_seller_policy`). **Amended by D-160 (2026-09-26)**: twenty-eight, with `owf_configuration_revision`.

**Decision**: every authorization decision in this gear is made by the platform PDP
(`authz-resolver`), reached through **one shared `PolicyEnforcer`** from `authz-resolver-sdk`
constructed at initialisation from `dyn AuthZResolverApi` and shared by the Control Operation
Gateway, the read services and the event handlers' read-before-act gate. Workflow registers a
resource/action catalogue of six GTS labels `gts.cf.bss.orders_workflow.<noun>.v1~`
(`process_instance`, `fulfillment_task`, `manual_task`, `dead_letter`, `approval_gate`,
`progress`) and twelve actions, maps each of its seventeen REST routes to exactly one pair and
each of its twelve event handlers to a declared topic and read-before-act gate, and asserts the
mapping against the routing table at startup and in a CI conformance test with a recording PDP
double. Scopes are PDP constraints — seller scope on the row's `seller_tenant_id`, tenant
isolation on `resource_tenant_id`, approver assignment as an `Eq` on
`owf_approval_gate.assigned_principal` populated at gate-open — compiled to an `AccessScope`
with constraints required and applied by `SecureConn` inside the mutating statement, which is the
resource-ownership check. Service principals are `subject_type` plus `token_scopes` naming this
gear; event handlers trust the broker's produce grant and platform-root tenancy. The five
Workflow-owned workers run under configured system authority as the bounded exception Lifecycle
`08 §3.5` states. A targeted PDP denial answers `not-found` (404) unless a follow-up `read`
allows (`not-authorized`, 403); this and the 503 on PDP outage are the gear's two declared deviations from the platform's
403 default. PDP outage fails closed with a sanitized 503 and no key settlement; workers
continue. The apply-time re-check of a long-running command re-runs the same PDP decision and
routes a refusal to one `authority-withdrawn` manual task with the process phase unchanged, never
to `parked`. Deleted: the gear-local permission evaluator, `owf_permission_declaration`, the
`SecurityContext` and `PermissionDeclaration` entities, the invented claims (`actor_class`,
`seller_scope`, `approval_assignments`, `service_principal`, `delegation_proof`,
`principal_kind`), the signed-envelope mechanism and its `requires_signed_envelope` control, and
the `AUTH-O1` gateway ask, which the `assigned_principal` column makes unnecessary. Delegation
proof is forwarded to the PDP as request context and never validated locally (Lifecycle D-111).
The decision endpoint drops `+ver`: `owf_approval_gate` gains no `row_version`, because a gate has
one transition out of `open` and the `state = 'open'` predicate in the decision `UPDATE` is the
guard; its out-of-assignment refusal becomes 404 and its separation-of-duties refusal is the
distinct `submitter-barred` (403).

**Rationale**: the previous design evaluated claims the platform does not issue against a table
the platform policy owner cannot see, in violation of the unified-system rule that all
authorization decisions go through `PolicyEnforcer`; nothing in it could run. The platform
`SecurityContext` has five fields, the broker envelope has no producer principal, and the sibling
gear has already taken the shared-adapter path, so this is the only option that is implementable,
compliant and consistent across the two Orders gears. Rejected: the hybrid (PDP for tenancy, local
evaluator for assignment and service scope) — two evaluators are the drift the one-evaluator
principle forbids, and both scopes are expressible to the PDP as constraints on properties this
gear already holds. Closes review findings OW-44, OW-47, OW-50, OW-106, OW-108 (predicate half)
and OW-111.

**Propagated**: `ADR/0010` (new); `DESIGN.md` §1.2, §2.2 *Standard ToolKit authorization
posture*, §3.2, §3.3, §3.4, §3.5, §3.7 (registry row removed; twenty-five tables), §4.2, §4.8,
§5; `design/09-read-and-authz.md` §1, §2.1, §2.2, §3.1, §3.2, §3.3, §3.4, §3.5, §3.6, §3.7, §3.8,
§4.1, §4.2, §4.4, §4.5, §5; `design/03-approval-execution.md` §3.2, §3.3;
`design/07-manual-tasks.md` §1.2, §3.2, §3.3; `UPSTREAM_REQS.md` §1.1, §1.2, §2.8 (`AUTH-O1`
withdrawn), §2.8 (new), §3, §5; D-37 (amended).

**ADR**: `cpt-cf-bss-orders-workflow-adr-platform-pdp-authorization`.

### D-64 (M) Refusal reasons follow the platform ContractError contract

**Amended by D-77 (2026-09-24).** The catalogue now holds forty-two reasons — ten engine families (adding `definition-not-bound`) and thirty-two slice values.

**Amended by D-105 (2026-09-26).** The catalogue holds **43** reasons — the ten engine families and thirty-three slice values, `invocation-dead` added (`design/01-foundation.md` §4.9).

**Accepted.** *(mirrors Lifecycle `01 §4.7` *Refusal reasons are derived GTS error types*)*

**Decision**: every reason in the catalogue of `01 §4.9` is a derived GTS error type under the
abstract base `gts.cf.bss.orders_workflow.err.v1~`, keyed
`gts.cf.bss.orders_workflow.err.v1~cf.bss.orders_workflow.<name>.v1~` with hyphens as
underscores; the keys are registry names registered with `types-registry` at startup, never wire
`type` values. The wire contract is the platform `#[derive(ContractError)]` with
`#[error_domain("orders-workflow.v1")]` on the enum and a per-variant `#[error_code(...)]` and
`#[canonical(...)]`; `type`, `status` and `title` come from the canonical category, the business
reason is the `error_domain`/`error_code` pair, and there is no Problem extension member and no
gear-minted `type`. `01 §4.9` carries the per-reason table — nine engine families, thirteen slice
values, and the read-and-authz and approval refusals — each with its code, category and status,
stated once. Category rule: `FailedPrecondition` is 400 in the SDK, so a 409 conflict is
`Aborted` (retry may succeed) or `AlreadyExists` (it will not: `idempotency-key-conflict`); a
time bound exhausted is `DeadlineExceeded`, an attempt bound exhausted `FailedPrecondition`;
dependency unavailability is `ServiceUnavailable`. Newly registered: `submitter-barred` (403) and
`gate-not-open` (409) for the decision endpoint, and `authority-withdrawn` (400) for the apply-time
re-check of `09 §4.4`; retired: the read-and-authz slice's `key-conflict`, a second name for the
engine's `idempotency-key-conflict`. "An unregistered reason fails at configuration load" becomes
a compile-time property of the enum plus a contract test for duplicate keys and domain/code pairs.

**Rationale**: `DESIGN.md §3.3` placed the reason in an unnamed Problem extension member and made
concurrency, replay and validation failures "distinct Problem `type` values", which the canonical
SDK does not allow — `type` is the category URI and a noncanonical one is rejected as
`UnknownProblemType`. The registry also had no code, category or status per reason, so two slices
could have answered the same failure with different statuses. Lifecycle already fixed the same
gap in the same shape; adopting it keeps the two Orders gears' error contracts uniform for a
shared client. Closes review finding OW-108 (code half).

**Propagated**: `design/01-foundation.md` §3.2 *Reason catalogue*, §3.3 *Error surface*, §4.9;
`DESIGN.md` §3.3 *Error envelope*; `design/03-approval-execution.md` §3.2, §3.3;
`design/09-read-and-authz.md` §2.2, §3.3, §4.4.

## M. The flow as a platform definition (ADR-0011…0013) and the slice operations

The decisions in this section were taken when the order process flow moved from Rust code in this
gear to a versioned platform workflow definition (`ADR/0011`, `ADR/0012`, `ADR/0013`), and when
each slice was restructured into step operations and a definition fragment. D-65…D-72 carry the
three ADRs and their cross-cutting consequences; D-73…D-101 are the decisions the slice
restructurings recorded, D-102…D-163 the decisions taken on the second review of
2026-09-26, D-164…D-192 those taken on the re-review of 2026-09-28, and D-193…D-199 those taken on
2026-09-30 to mirror PriceBook and the Orders Lifecycle rewrite for it (Lifecycle `ADR/0008`,
D-150–D-168, uncommitted in its worktree at the time of writing). Each names the entries it amends; the amended entries carry a dated
**Amended by** note. D-65…D-101 were taken on 2026-09-24.

### D-65 (H) The order process flow is a versioned platform workflow definition executed by serverless-runtime

**Accepted.** *(carries `ADR/0011`; answers Q-01 part 1)*

**Decision**: the order process flow — step order, branches, waits, the two-wave barrier release,
event listening, the hold/resume/cancel signal arms and the structure of compensation — is a
Serverless Workflow v1.0.0 definition registered in the platform gear `serverless-runtime` as a
`gts.cf.core.sless.workflow.v1~` callable and executed by its Temporal plugin. The definition uses
`call`, `listen`, `wait`, `switch`, `fork`, `try`/`catch`/`raise` and `set`; it uses no `run`, no
`emit` and no `for`. This gear provides **step operations** and **the process record**. Adjusting
the flow is publishing a definition version; changing what a step does is an Orders release. The
durable-execution substrate is therefore **selected** — not the OSS Workflow Engine and not a
BSS-local mechanism — and ADR-0001 is rewritten to say so. The platform has no code today; the
canonical definitions are documentation until the readiness gate of `01 §3.8` passes, and if the
platform slips only sequencing falls back to code (`definition_source = code`) — the **fallback
property**, stated as a property of the decomposition and not as a plan.

**Rationale**: it is the only option under which the flow is adjustable without an Orders release
and no second orchestration engine is built; the code-defined flow, policy points and an
Orders-local interpreter each keep a timer service, a retry engine and scheduling workers this
gear would own alone. The record PRD §6.1 requires is unchanged in ownership, transaction shape and
audit grade.

**ADR**: ADR-0011 (`cpt-cf-bss-orders-workflow-adr-flow-as-platform-definition`); ADR-0001 as rewritten.

**Propagated**: `design/10-process-definition.md` (new); `design/01-foundation.md` §1.1, §3.2, §3.3;
`DESIGN.md` §1, §2.1, §3.2, §3.5, §4.9; `design/README.md`; Q-01, Q-12; `UPSTREAM_REQS.md` §4.

**Amended (2026-09-26)**: the PRD passages that still describe an unselected engine or Workflow-scheduled timers (§6.2 *Escalation Timer*, the §12 durable-timer acceptance criterion and the §17.1 diagram, §14, the §16 *Engine decision pending* risk) are registered for amendment as `UPSTREAM_REQS.md` §4 item 13, beside items 5 and 6 (second review OW2-99). **Amended (2026-09-26)**: the criterion is in PRD §12, not §17, and item 13 now also covers the §1.4 glossary *Escalation* (`PRD.md:107`) and the §6.3 provisioning requirement (`PRD.md:349`), which the finding had named (re-review R-25, R-35).

### D-66 (H) References, not payloads, cross the engine boundary

**Accepted.** *(carries `ADR/0013`)*

**Decision**: a task input or output carries only `correlationId`, `orderId`, `orderVersion`,
`resource_tenant_id`, the platform `invocation_id`/`attempt_id`, an opaque
`stepRef`/`taskRef`/`gateRef`/`lineRef`, small closed enums the operations return, and a duration
or instant a `wait` needs. It never carries a resolved total or price field, catalog or product
references, line items, approver identities or any `subject_id`, a tenant axis beyond
`resource_tenant_id`, payment, subscription or transition-request identifiers, the frozen plan,
approval context, the saga log, or free text. Every operation declares its `input` and `output` as
GTS reference schemas built from that list, and every operation reads commercial data inside this
gear under the PDP.

**Rationale**: whatever crosses is persisted in Temporal history this gear neither owns nor
retains; references answer PRD §15's "which commercial data would sit in engine history" with
"none" and keep D-61's erasure and ADR-0010's `assigned_principal` constraint enforceable in one
store. The residual — identifiers in engine history — is bounded by the residency ask and Q-12.

**Amended (2026-09-24).** The rule extends to **trigger inputs and consumed events**, which are
engine data as much as task inputs are: the start trigger's input is the raw `$workflow.input`
(Serverless Workflow DSL 1.0.0, dsl.md *Runtime expression arguments*) and a `listen` output is
the array of consumed events (dsl-reference.md *Listen*), while Lifecycle's events as published
carry tenant axes, per-line net components, the deciding authority and actor and reason fields
([Lifecycle `01 §4.4`](../../orders-lifecycle/docs/design/01-foundation.md#44-events-audit-and-the-outbox-normative),
lines 2396–2406). The start trigger and every Lifecycle, Generic Approval and Subscriptions
`listen` **MUST** keep only references. Two routes are registered and either closes the gap: the
platform persists only the members the definition selects
(`…-upreq-serverless-runtime-consumed-event-member-storage`), or Lifecycle publishes thin event
variants or confirms the full events may be stored (`…-upreq-lifecycle-thin-events`), with the
Generic Approval decision event reference-only by `…-upreq-generic-approval-expectations-contract`.
Until one lands, "no commercial data in engine history" holds for task inputs and outputs only, and
the consumed events as published are the stated residual of ADR-0013 and `DESIGN.md` §4.2.

**Amended (2026-09-26)**: the permitted list is the closed six-type vocabulary of D-131, which
names every reference and counter the canonical definition passes. "Line items" still never cross,
and "line counts" is struck, because array cardinality is an accepted residual. The residual
identifiers include `resource_tenant_id`, and the rationale's "none" holds for commercial content
in task data only (Q-01 as amended). Refused step calls are task data too, with a fixed-member
Problem shape (D-132).

**ADR**: ADR-0013 (`cpt-cf-bss-orders-workflow-adr-references-not-payloads`).

**Propagated**: `design/01-foundation.md` §2.1, §3.3, §4.14; `design/10-process-definition.md`
§2.1, §2.2 rule 5; every slice §3.3; `DESIGN.md` §2.1, §4.2, §4.3; `UPSTREAM_REQS.md` §2.3, §2.4,
§2.9.

### D-67 (H) Protected steps are fenced by six validation rules and by run-time guards

**Accepted.** *(carries `ADR/0012`, fence half)*

**Decision**: 35 step operations are registered: **22 `protected`** — `start-instance`,
`settle-from-lookup` (sweep-only), `terminate-instance`, `admit-trigger`,
`terminate-on-terminal-event`, `obtain-verdict`, `reflect-verdict`, `record-decision`,
`construct-and-freeze-plan`, `evaluate-payment-auth-eligibility`, `re-check-pre-activation`,
`begin-fulfillment`, `dispatch-wave1-create`, `dispatch-wave2-activate`, `report-spawn-signal`,
`run-cancellation-fence`, `compensate-order`, `report-outcome`, `create-manual-task`,
`apply-hold`, `apply-resume`, `authorize-cancel` — and **13 `composable`**. A protected operation
may be ordered by a definition, never omitted or replaced. A version is publishable only if it
passes six rules over the whole definition: protected operations present in their order
constraints; every `call` to a registered operation (a Function only for a composable one); every
`listen` inside the closed set; bounds nest (per-operation deadline < retry budget < task timeout
< overdue window < lifetime ceiling); no payload field crosses; no protected operation inside a
`catch` that continues the forward path. `design/10` §2.2 states the same rules as eight numbered
checks, adding the grammar subset and the fork-routing convention. The rules run in a pre-publish
validation hook (an upstream ask) and in a CI test over the canonical definitions; independently,
every protected operation re-checks its own precondition in Orders' record and refuses with a
catalogue reason, so a definition that bypasses the static fence fails closed at the first
protected operation it misorders.

**Rationale**: the PRD's `p1` invariants are properties of the path, not of one task, so only a
whole-definition check fences them; human review cannot check a nesting inequality or a schema;
and the run-time guard makes the fence hold even for a publish outside the pipeline.

**ADR**: ADR-0012 (`cpt-cf-bss-orders-workflow-adr-definition-versioning-and-protected-steps`).

**Propagated**: `design/10-process-definition.md` §2.2, §4.1, §4.2, §4.6; each slice §3.3
`protection`; `design/09-read-and-authz.md` §3.1; `DESIGN.md` §2.2, §4.2.

**Amended (2026-09-26)**: the run-time guard did not hold for the unwind path —
`run-cancellation-fence` with `trigger = failure` checked only that the instance was not terminal
(OW2-58). It now requires a recorded cause for every trigger, and every step call after
`start-instance` is bound to the instance's invocation (D-106). `retry-step` and
`settle-from-lookup` are never `call` targets (D-108); the counts are unchanged.

**Amended (2026-09-26)**: the bounds rule no longer compares with the overdue window or sums a
backoff: it checks each operation's deadline against the task timeout and every timeout and
`wait` against the literal lifetime ceiling, and the policy-held windows are bounded where the
policy is validated. The catch rule is one rule across `design/10` §2.2, §4.6 and ADR-0012 — a
retry-only `catch` or a named failure route — and the path rule walks a routing graph of literal
routing values (D-126). The counts are unchanged.

**Amended (2026-09-26)**: no `call` targets a Function any more, composable or not (D-136); the
fence also requires the `p1` composables and the shared arms on their paths, which stay
`composable`, so the counts are unchanged (D-135).

### D-68 (H) Instances are pinned to their definition version; the platform operator publishes

**Accepted.** *(carries `ADR/0012`, versioning half)*

**Decision**: `start-instance` writes `owf_definition_binding` (`correlation_id`, `definition_id`,
`definition_version`, `definition_source` ∈ `platform | code`, `pinned_at`, `published_by`,
`resource_tenant_id`) in the instance-creating transaction; `owf_process_instance.definition_version`
references it; the instance runs to termination on that version; there is no migration (PRD §5.2).
A version is never archived or deleted while a binding names it (an upstream ask on the platform
lifecycle). The publish role is the platform operator, authorized by the platform on its registry;
a seller-scoped fragment role is Q-10 and is not granted. Every publish must be attributable; until
the platform audits publishes, the CI run, the registry's version listing and
`owf_definition_binding.published_by` are the evidence.

**Rationale**: PRD §6.1 requires the version recorded on the instance and in the audit trail, and
§5.2 excludes migration; the platform pins an invocation to its callable version (serverless-runtime
DESIGN.md line 614), and Orders must hold the same pin on its side.

**ADR**: ADR-0012.

**Propagated**: `design/01-foundation.md` §3.3 *start-instance*, §3.7; `design/10-process-definition.md`
§2.2 *Versioning, pinning, publish*, §4.3; `DESIGN.md` §3.6 *Publish a definition version and pin
an instance*; `UPSTREAM_REQS.md` §2.9; Q-10.

**Amended (2026-09-26)**: the binding records `definition_id` and `definition_version` from the
platform's invocation record, never from the document, and `published_by` is nullable until the
registry reports a publisher, since no registry read or publisher field existed to copy it from
(D-137). The platform-operator publish role is held by one publisher, the definition publish job,
which builds and deploys nothing, runs a behavioural gate, deprecates the version it replaces and
rolls back forward (D-138).

### D-69 (H) The step operation is the unit of work, behind one internal step surface

**Accepted.**

**Amended by D-108 (2026-09-26).** Thirty-three values are granted: `retry-step` joins `settle-from-lookup` as in-process only, granted to no principal (`design/09-read-and-authz.md` §3.1).

**Amended by D-140 (2026-09-26).** "The one stated exemption to D-48" below is now one of two: `owf_seller_policy` is the other. **Amended by D-160 (2026-09-26)**: one of three, with `owf_configuration_revision`; and the registry load's audit is an `owf_configuration_revision` row, not an `owf_audit_entry`.

**Decision**: every step the definition can order is an Orders operation on
`POST /bss-orders-workflow/v1/steps/{operation}`, one route per registered operation, callable only
by the serverless-runtime service principal (`subject_type` service, `token_scopes` naming this
gear) and authorized as `gts.cf.bss.orders_workflow.process_step.v1~` × `execute` with the
operation name as a resource property; 34 values are granted, `settle-from-lookup` to none. Each
operation is declared once in its slice §3.3 with `name`, `protection`, `input`, `output`,
`idempotency_key`, `declared_event`, `compensation`, `reasons`, `audit_kind`, `retry_class`,
`deadline`, mirrored into `owf_step_operation`, which is load-only, audited on load and carries
**no tenant column** — the one stated exemption to D-48, because it is configuration, not a
process record. Every operation runs inside the envelope: registry resolution before the effect,
record, audit, declared event and settlement in one transaction after it, answer only after
commit. Orders subscribes to no topic: the event-handler rows of `09` are removed and every event
reaches Orders as a step call.

**Rationale**: an HTTP route with a declared reference schema is the only boundary the platform's
`call` task can reach and the only one this gear can authorize, audit and replay-proof uniformly;
declaring each operation once keeps the definition, the PDP catalogue and the registry in lockstep
by a startup assertion in both directions.

**ADR**: ADR-0011; ADR-0010 as amended.

**Propagated**: `design/01-foundation.md` §3.2, §3.3, §3.7; `design/09-read-and-authz.md` §3.1,
§3.3, §4.1; `DESIGN.md` §3.3, §4.2; D-37, D-48; `ADR/0010`.

### D-70 (H) Timers, waits and task retry policy are the platform's

**Accepted.** *(amends D-02, D-10, D-11, D-39…D-46, D-49, D-53)*

**Decision**: every timer — escalation window, park escalation, expected-fulfillment wait,
barrier poll, overdue window, lifetime ceiling, SLA clock — is a definition `wait` executed by the
plugin's durable timers; the task retry policy (`use.retries.transient`: exponential from 1 s,
jitter to 30 s, 5 attempts, on `$error.status` ∈ {429, 503, 504, 409}) re-issues only operations
registered `retryable-on: transient`, under the same idempotency key. `owf_durable_timer`,
`owf_retry_state` and slice 08's `owf_timer_pause` are **retired**; Orders records the platform
`attempt_id` and its own attempt number on every `owf_step_log` row. There are **five bounds, two
owners** (`01 §4.2`): the per-operation deadline is the operation's; the retry budget, task
timeout, overdue window and lifetime ceiling are the definition's, and their nesting is validation
rule 4. The per-dependency retry budgets of the former Dependency Retry Governor are superseded by
the definition's per-task retry policy, which **MAY** be tighter per task and **MUST** nest inside
the task timeout. The only remainder authority for a paused escalation window is
`owf_approval_gate.window_remaining_ms` with `pause_causes`, written through slice 03's gate-window
port.

**Amended (2026-09-24).** Two corrections against the platform schemas and the 1.0.0 DSL.
(1) *Task retry is the DSL's.* The per-task policy is the definition's `use.retries` policy
referenced from a `try`'s `catch.retry`, with `catch.errors`/`catch.when` selecting the statuses
(Serverless Workflow DSL 1.0.0, dsl-reference.md *Try*, *Retry*); a status no `catch` matches,
such as 400, is not retried. The platform's `RetryPolicy` is a different, **invocation-level**
policy keyed by SDK error category ([serverless-runtime DESIGN.md](../../../serverless-runtime/docs/DESIGN.md)
lines 354–370) and is not what re-issues a step operation. (2) *Computed waits are fixed-duration
loops.* A 1.0.0 `wait` accepts only an inline duration object or an ISO 8601 string, never a
runtime expression (dsl-reference.md *Wait*, *Duration*). A wait whose length Orders computes — the
remaining escalation window, the expected-fulfillment instant, a deferral's `retryAfterMs`, the SLA
remainder — is therefore a **bounded re-check loop**: a `wait` of fixed granularity declared per
wait in `design/10-process-definition.md` §3 (for example PT1M for a deferral, PT5M for approval
escalation and the park, PT1H for the barrier and the overdue window), then a `call` to the
Orders operation that owns the deadline, which compares database time with the stored deadline and
returns `due: true | false`, then a `switch` that loops while not due. The lifetime ceiling needs no
loop: it is a literal `P90D` wait. No Function sleeps: a Function is bounded by platform timeout
limits and durable waits belong to Workflows (serverless-runtime DESIGN.md lines 579, 582).
Whether the plugin accepts a runtime-expression duration as an extension is Q-11 (i).

**Rationale**: a durable timer service and a retry controller in a business gear are the
duplication ADR-0005 of serverless-runtime refuses; the plugin already owns them. What PRD §6.1
requires Orders to hold of them is evidence — the attempt per step record and the remaining window
— not the scheduling.

**ADR**: ADR-0011; ADR-0006 and ADR-0007 as amended.

**Propagated**: `design/01-foundation.md` §3.7 *Retired tables*, §4.2, §4.4; `design/03-approval-execution.md`
§3.7; `design/08-hold-and-cancel.md` §3.2, §3.7; `design/10-process-definition.md` §2.2;
`DESIGN.md` §3.7, §4.8.

**Amended (2026-09-26)**: the escalation window, the overdue window and the SLA classes are not
timers of the definition. They are values of the seller's policy, pinned on the order's record by
the operation that needs them, and the definition owns only the tick of the re-check loop over
each stored deadline (D-134). "Five bounds, two owners" keeps its five bounds; the overdue
window's value moves to the seller's policy, bounded where the policy is written.

### D-71 (M) The worker roster is three workers, and the sweep selects every due intent

**Accepted.** *(amends D-62)*

**Decision**: the advisory-locked roster of `01 §3.8` is `reconciliation-sweep`,
`retention-purge` and `audit/<audit-tenant UUID>`. `timer-wakeup` and `dead-lease-scan` are
removed: timers are the platform's, and a dead lease on a dispatching step key is found by
`reconcile-intent`'s read on the sweep's schedule and settled by `settle-from-lookup`. The sweep's
candidate set is `next_sweep_at <= now()` over **every non-terminal intent**, one bounded page per
pass, whether or not the owning instance has a live invocation; it reads invocation status only to
report intents whose instance has no live invocation. The definition's poll and confirmation arms
are early reads of the same rows, never a reason to skip one. `retention-purge` additionally purges
`owf_compensation_record`, `owf_cancellation_fence` and `owf_task_resolution_request` at ≥ 400 days
and stale `owf_dispatch_admission` seller rows, and holds no grant on `owf_definition_binding`.

**Rationale**: a sweep that selected only dead-invocation rows would leave a live instance's lost
confirmation to the definition alone; selecting by `next_sweep_at` makes the sweep correct
whether or not the platform is healthy.

**Propagated**: `design/01-foundation.md` §3.8; `design/05-provisioning-intents.md` §3.8;
`DESIGN.md` §3.8; Q-09.

**Amended (2026-09-26)**: the settlement by `settle-from-lookup` applies to a dead lease on an
intent-submitting key (`dispatch-wave1-create`, `dispatch-wave2-activate`, `compensate-order`);
a dead lease on any other key is re-run under a new holder by its next same-key call, and a
record-only operation leaves none (D-103). The roster is unchanged: still no dead-lease scan.
The retention purge deletes row-wise; no table is partitioned (D-104).

**Amended (2026-09-26), second**: reading invocation status "only to report" is withdrawn. The
sweep's pass gains an **instance liveness pass** over non-terminal instances by
`next_liveness_at`, which raises an instance whose invocation is not live as an `invocation-dead`
task and drives the dead-instance unwind after a cancel (D-105). Still three workers.

### D-72 (M) Inbound dead letters are the platform trigger path's; `owf_dead_letter_record` is retired

**Accepted.** *(amends D-04, D-05, D-44)*

**Decision**: an inbound delivery that exhausts its cap — a Lifecycle trigger, the approval
decision or a Subscriptions outcome the platform could not deliver to an invocation — is the
platform event-trigger path's dead letter (serverless-runtime DESIGN.md line 976). Orders writes no
dead-letter row: `owf_dead_letter_record` is retired, and the registry's `delivery_count` with it.
`owf_dead_letter_triage` and the three dead-letter routes of `09 §4.1` are **pending** the
platform's answer on operator visibility (`…-upreq-serverless-runtime-dead-letter-operator-visibility`);
if it is declined they are retired rather than rebuilt on an Orders copy. The manual task remains
the one inspectable object for a step-level failure, and `dead-lettered` is not an outcome of the
step surface.

**Rationale**: once the platform consumes the events, the delivery cap is the platform's; an
Orders copy of the platform's dead letter would be the second inspectable object ADR-0009 exists to
prevent.

**ADR**: ADR-0009 as amended.

**Propagated**: `design/01-foundation.md` §3.3, §3.7 *Retired tables*, §4.8; `design/07-manual-tasks.md`
§3.7; `design/09-read-and-authz.md` §3.1, §4.1; `DESIGN.md` §3.7, §4.4, §4.5; `UPSTREAM_REQS.md` §2.9, §4.

**Amended (2026-09-26)**: the PRD clauses that still require an Orders dead-letter record, alert, queue entry, progress entry and audit row (`fr-owf-dead-letter`, `fr-owf-task-queue`, §9.1 progress, `nfr-owf-audit`, acceptance criteria 7c, 7d and 8a) are restated for platform-owned dead letters by the PRD amendment registered as `UPSTREAM_REQS.md` §4 item 12 (second review OW2-85).

### D-73 (H) Start is the platform event trigger; the REST start route is removed

**Accepted.**

**Decision**: PRD §9.1 *Start workflow* is realised by two serverless-runtime event triggers on the
`order_process` callable — `OrderSubmitted` (filtered to `category = new_sale`) and `OrderAmended`
— whose invocation's first calls are `admit-trigger` (`role: start`) and `start-instance`. The
start-trigger set is `{OrderSubmitted, OrderAmended}`, because Lifecycle publishes no
`OrderSubmitted` after an amendment. `POST /bss-orders-workflow/v1/workflows` is removed, the
`process_instance × start` action is retired, and no Orders route calls
`POST /api/serverless-runtime/v1/invocations`.

**Rationale**: a REST start would be a second entry into the process the definition does not see;
the trigger is the platform's own mechanism for event-driven invocation, and the admission guard is
the same protected operation on every path.

**Propagated**: `design/02-triggers-and-start.md` §2.2, §3.3; `design/10-process-definition.md`
§2.2 rule 7, §3.3; `design/09-read-and-authz.md` §3.1, §3.3, §4.1; `DESIGN.md` §3.3;
`UPSTREAM_REQS.md` §2.8 item 4, §4 item 8; `ADR/0010`.

**Amended (2026-09-26)**: the `new_sale` filter is a routing filter, not the guard. The bindings
are released with the definition, and `admit-trigger` re-checks the tenant and the category from
the Lifecycle read (D-107).

### D-74 (M) The idempotency key families gain the trigger family and two round components

**Accepted.** *(amends D-47; ADR-0006 as amended)*

**Decision**: a fifth family is added for event-scoped admission, `trigger`:
`{tenant}:{eventId}:admit-trigger[:listen]`, carried in `owf_step_operation.key_family`; an
instance-scoped family `{tenant}:{correlationId}:{name}[:{subject}][:{attempt}]` covers the
record-only operations. Two instance-bound keys carry a round component: `begin-fulfillment`'s
lifecycle-transition key ends in the eligibility round (`…:begin-fulfillment:{eligibilitySeq}`), so
a re-evaluated eligibility is a new transition attempt and a replay of the committed round is
absorbed by both gears; and `compensate-order`'s step key ends in the pass
(`…:compensate-order:{pass}`). The platform's same-key re-invocation is absorbed by every family.

**Rationale**: an admission happens before an instance exists, so no instance-bound key can name
it; and a begin-fulfillment or a compensation pass that legitimately repeats must not be absorbed
as a duplicate of the previous round.

**ADR**: ADR-0006 as amended.

**Propagated**: `design/01-foundation.md` §3.3 *The step-operation contract*;
`design/02-triggers-and-start.md` §2.1; `design/04-fulfillment-plan.md` §3.3, §4.1;
`design/06-saga-and-compensation.md` §3.3.

**Amended (2026-09-25)**: `run-cancellation-fence`'s step key ends in the triggering request's
reference, `…:run-cancellation-fence:{trigger}[:{triggerRef}]`: `cancelRequestRef` on `cancel`,
`triggerEventId` on `supersede` and `terminal-event`, and nothing on `failure`. The body carries
that reference and the registry fingerprints the body, so a key naming only the trigger kind made
a second authorized cancel an uncaught `idempotency-key-conflict`. That was the only exit from a
withdrawn-authority stall (`design/06-saga-and-compensation.md` §4.3), and the conflict faulted the
invocation partway through compensation. A supersede or terminal event arrives at most once per
version, so its reference only keeps the rule uniform. With the reference in the key, a replay of one request is still absorbed, and a
different request reaches the fence's absorption table. Propagated: `design/06-saga-and-compensation.md`
§3.3, §4.3; `design/10-process-definition.md` §3.6 fragment (c); ADR-0006 as amended.

**Amended (2026-09-26)**: generalised by D-102. "Two round components" understated the surface:
every re-invokable operation carries a round, every Lifecycle-transition key ends in
`{round}[:{attempt}]`, and the two keys named above are two rows of the register in
`design/01-foundation.md` §3.3 *Rounds and attempts*, where each round is validated against the
instance's `key_rounds` counter.

### D-75 (M) Supersession is unwind-then-start, and admission waits for the prior instance

**Accepted.** *(replaces the atomic terminate-then-start step; D-06 and D-07 stand; amends D-49's use of the one-active-instance index)*

**Decision**: when an `OrderAmended` supersedes a version whose instance is still running, the
prior invocation's `listen` arm runs the shared unwind path (fence, compensate, report
`superseded`, terminate) while the new version's invocation, started by the trigger, holds its
admission `open`: `admit-trigger` answers `prior-instance-active` (409) until the prior instance is
terminal, under a dedicated `supersession` retry policy (constant 5 min, up to 24 h). The partial
unique index `UNIQUE (order_id) WHERE terminal_outcome IS NULL` arbitrates the race; it is never
bypassed.

**Rationale**: two invocations cannot share one atomic transaction; the ordering the atomic step
gave is kept by the index and the held admission, and a longer, constant retry fits an unwind that
may wait on Subscriptions for hours.

**Propagated**: `design/02-triggers-and-start.md` §4.3, §4.7; `design/10-process-definition.md`
§3.6 (a), (f).

### D-76 (M) The seller axis is resolved inside Orders and carried from admission to start

**Accepted.**

**Decision**: `start-instance` reads `seller_tenant_id` (and `payer_tenant_id`) from the settled
result of `admit-trigger`, which resolved them from Lifecycle inside Orders, never from the task
input; the definition never carries a seller or payer axis.

**Rationale**: ADR-0013 keeps every tenant axis beyond `resource_tenant_id` out of engine history,
and `start-instance` needs the seller axis for every seller-scoped row it writes.

**Propagated**: `design/01-foundation.md` §3.3 *start-instance*; `design/02-triggers-and-start.md` §3.3.

**Amended by D-172 (2026-09-28)**: the seller is resolved from `admit-trigger`'s Lifecycle read, so a pre-admission audit entry written before that read returns an order — its `step-start`, and the settlement of a failed or foreign-tenant read — carries `seller_tenant_id` NULL; `owf_audit_entry` admits NULL only there.

### D-77 (M) Twelve reasons are registered for the step operations; the catalogue holds forty-two

**Accepted.** *(amends D-64's counts)*

**Decision**: `01 §4.9` registers `trigger-applicability-unverified` (503) and
`prior-instance-active` (409) for slice 02; `identity-party-unavailable` for slice 04;
`activation-precondition-unmet` (409) and `intent-unresolved` (400) for slice 05;
`fence-not-claimed` and `outcome-not-reportable` for slice 06; `order-fenced`,
`action-not-offered`, `override-unverified` and `lifetime-ceiling-reached` for slice 07; and
`approval-reflection-refused` (400) for slice 03, which raises it from `reflect-verdict` and
whose manual task slice 07 creates (`01 §4.9`). The engine contributes ten families (adding
`definition-not-bound`); the catalogue is **42** reasons — ten engine, thirty-two slice.

**Rationale**: a reason a slice raises but the catalogue does not register does not compile
(D-64); registering them once with their categories stops two slices choosing different statuses.

**Propagated**: `design/01-foundation.md` §3.3 *Error surface*, §4.9; slices 02, 03, 04, 05, 06, 07 §3.3;
`DESIGN.md` §3.3 *Error envelope*.

**Amended (2026-09-26)**: `invocation-dead` is registered for slice 07 (D-105); the catalogue is
**43** reasons — ten engine, thirty-three slice.

**Amended by D-194 and D-196 (2026-09-30)**: `order-binding-expired` is registered for slices 04 and
05; `invalid-dependency-graph` and `catalog-topology-unavailable` (04) and `blocked-upstream` (06)
are withdrawn; the catalogue is **41** reasons — ten engine, thirty-one slice (04 seven, 06 four).

### D-78 (M) The barrier and the park are definition patterns over Orders guards

**Accepted.** *(amends D-12, D-23, D-24; ADR-0004 and ADR-0007 as amended)*

**Decision**: the two-wave barrier is a definition pattern — the expected-fulfillment `wait` over
the instant `construct-and-freeze-plan` returns, and the eligibility re-evaluation — whose
all-creates half and conjunction are `evaluate-activation-eligibility`, and whose run-time guard is
`dispatch-wave2-activate`'s refusal (`activation-precondition-unmet`) unless every task of the
frozen plan is `draft_created` and the instant has passed by database time. The pre-activation
draft re-read (D-23) is `reread-draft-liveness`, and the wave-1 rebuild (D-24) is `rebuild-wave1`,
both ordered by the definition before wave 2; a lapsed draft routes rebuild → wave 1. The
fail-closed park (D-12) is a definition arm — `park`, then a park loop whose escalation `wait` is
armed by `arm-park-escalation` — and exits only through `unpark` or the fence; it never suspends
the Lifecycle `submitted` TTL.

**Amended (2026-09-24).** The pre-activation draft re-read of D-23 is not a separate step the
definition orders before wave 2: it is inside `dispatch-wave2-activate`, which re-reads draft
liveness immediately before submitting any activation intent and reports a lapsed draft in
`lapsed[]` (`design/05-provisioning-intents.md` §3.3; ADR-0004 as amended).
`reread-draft-liveness` is `composable` and advisory — an optional early read after the
expected-fulfillment wait — and a definition that never calls it is valid. A lapsed draft still
routes `rebuild-wave1` → wave 1.

**Rationale**: timing and ordering are the definition's under D-65; the invariants those
mechanisms protect stay Orders' guards so a publish cannot weaken them.

**ADR**: ADR-0004, ADR-0007 as amended.

**Propagated**: `design/03-approval-execution.md` §3.3, §4.2; `design/04-fulfillment-plan.md`
§3.3; `design/05-provisioning-intents.md` §3.3; `design/10-process-definition.md` §3.6 (a), (b).

**Amended (2026-09-26)**: the park loop carries a hold and a resume arm that only record
(`holdPauses` false); a hold during a park records the suspension and leaves the phase `parked`,
so the reflection after the `unpark` answers `held` and waits for the resume instead of replaying a
Lifecycle refusal (D-102). The park clock is still never paused and the TTL rule is unchanged.

### D-79 (M) Each wave is one `call` carrying the line set as references

**Accepted.**

**Decision**: `dispatch-wave1-create` and `dispatch-wave2-activate` are each **one** `call` per wave
carrying the wave's line set as references; the per-order parallel-line cap and every other
admission control run inside the operation. There is no per-line `fork`.

**Rationale**: the DSL's `for` iterates sequentially and `fork` takes a static branch list, so it
has no dynamic per-line fan-out (Q-11 (iii)); putting the fan-out inside the operation also keeps
the concurrency bound where the admission state is.

**Propagated**: `design/05-provisioning-intents.md` §3.3, §4.3; `design/10-process-definition.md`
§2.2; ADR-0004 and ADR-0012 rule 1.

### D-80 (M) Competing-fork arms only listen or wait; one shared return, and a hold pauses only the approval stage

**Accepted.** *(amends D-35)*

**Decision**: every branch of a competing `fork` only `listen`s or `wait`s and then `set`s `arm`;
no step operation runs inside a competing branch, so a losing branch is never cancelled halfway
through an operation, and the sibling `switch` routes the winner to the path that calls the
operation. Every stage loop records its name in `$context.stageLoop`; the shared paths that return
to the stage an arm left end in `returnToStage`. A hold enters the resume wait (pausing the
escalation `wait` with the remainder Orders returns) **only in the approval stage**; elsewhere the
hold is recorded by `apply-hold` and the stage loop continues, because the barrier, the
expected-fulfillment and overdue `wait`s and the lifetime ceiling do not pause.

**Rationale**: an operation cancelled mid-flight by a competing arm would leave an `open` key and a
half-recorded effect; routing after the switch keeps every operation whole. Pausing only where PRD
§6.3 requires a pause keeps the other clocks honest.

**Amended (2026-09-24).** In Serverless Workflow DSL 1.0.0 a flow directive (`then`) may target
only a sibling task in the same `do` list, never a task at a different depth (dsl.md *Task
Flow*). The shared return is therefore a **stage dispatcher**, not a jump: the top-level `do` list
holds one composite task per stage (approval, fulfillment, unwind, park, hold and the others of
`design/10-process-definition.md` §3.6) and a `dispatch` task, a `switch` on `$context.nextStage`
whose `then:` names sibling stage tasks. A stage ends by `set`-ting `$context.nextStage` and
`then: exit`, which returns to the top-level list, whose next task is `dispatch`. Inside a stage a
loop is flat: one `do` list whose tasks jump only to siblings. `returnToStage` is this dispatcher:
a shared path returns to the stage an arm left by `set`-ting `$context.nextStage` to that stage and
exiting to the top level.

**Amended (2026-09-24, dispatcher placement).** The dispatcher does not sit in the document's own
`do` list but one level below it, in the `process` branch of the top-level `lifetime` fork
(`compete: true`, beside the `lifetimeCeiling` and `overdueMonitor` branches), because the lifetime
ceiling must compete with every stage and a `fork` branch is the only construct that races a
`wait` against a `do` list. The `process` branch holds the stage tasks and `dispatch`; a stage's
`then: exit` returns to that branch's list, and every stage task carries `then: dispatch`. When the
ceiling wins, the document-level tasks after the fork (`afterLifetime`, `ceilingEntry`) record the
interrupted stage and checkpoint and re-enter the fork at the ceiling stage, under a fresh `P90D`
ceiling; an unwind interrupted by the ceiling re-enters the fork at the unwind without parking
(no `compensating → parked` edge, `design/01-foundation.md` §3.7). Where the text above says "the
top-level list", read "the `process` branch's list". The hold carries no remainder either: the
escalation window is re-checked on a fixed `PT5M` tick and `apply-resume` answers the first `due`
(D-70 as amended).

**Amended (2026-09-26)** by D-121, D-123 and D-124. (1) "The remainder Orders returns" in the
decision above is superseded: `apply-hold` and `apply-resume` return no remainder; the window
stays on the gate row and the re-check continues against the re-based deadline. (2) The
escalation window is re-checked on the gate loop's `PT30S` probe tick, not a `PT5M` tick (D-123).
(3) The ceiling's exemption reads `$context.unwind` and the verdict park loop, not `nextStage`
(D-121). (4) The re-entry of a stage loop does not recover an event delivered between listens;
that is the platform retention of D-124.

**Propagated**: `design/10-process-definition.md` §3.6, §4.5; `design/08-hold-and-cancel.md` §3.3,
§3.6, §4.7; `design/01-foundation.md` §3.7; `design/03-approval-execution.md` §4.5;
`design/07-manual-tasks.md` §4.8; `ADR/0011`.

### D-81 (M) One shared unwind path: fence, compensate, report, terminate

**Accepted.**

**Decision**: every failure, cancel, supersession and terminal-event arm routes to the same unwind
path — `run-cancellation-fence` → `compensate-order` (repeated by pass until `complete`) →
`report-outcome` → `terminate-instance`. No path reaches `report-outcome` with `failed`,
`cancelled` or `superseded` except through `compensate-order`, and none reaches it with `completed`
except through `dispatch-wave2-activate`.

**Rationale**: one path is one fence rule to validate and one set of run-time guards; four paths
would be four chances to report before compensating.

**ADR**: ADR-0005, ADR-0012.

**Propagated**: `design/10-process-definition.md` §3.6 (c), (d), (f), §4.1; `design/06-saga-and-compensation.md` §3.3.

### D-82 (M) A lifetime-ceiling park is allowed from `suspended`; a parked instance unwinds only through the fence

**Accepted.**

**Decision**: `01 §3.7` gains `suspended → parked` for `parkReason = lifetime-ceiling`, leaving
the open suspension open because the hold is still Lifecycle's fact; a parked instance —
including one parked at the lifetime ceiling — reaches an unwind only by passing the cancellation
fence (`parked → compensating`). No `suspended → terminated` edge is added: every unwind from hold
passes `compensating`.

**Rationale**: the lifetime ceiling is non-pausable (D-53) and must bound a held order; parking it
without closing the suspension keeps the two facts separate.

**Propagated**: `design/01-foundation.md` §3.7 *phase*; `design/08-hold-and-cancel.md` §2.1, §4.8.

**Amended (2026-09-26)** by D-121: the lifetime-ceiling park is taken only from `started` or
`suspended`. A ceiling inside an unwind (`$context.unwind` set) or in the verdict park loop parks
nothing, because the table has neither a `compensating → parked` nor a `parked → parked` edge.
Each ceiling parks under its own subject `ceiling:{round}`, and its `unpark` requires that
ceiling's task resolved `retry` (D-122).

**Amended by D-175 (2026-09-28)**: `terminate-instance` called from `suspended` or `parked` refuses `fence-not-claimed` rather than moving the phase, so the missing `suspended → terminated` edge is enforced at the operation.

### D-83 (H) `compensate-order` is one operation over the Orders-owned ordinal, resumable by pass

**Accepted.** *(ADR-0005 as amended)*

**Decision**: the whole reverse compensation walk is one protected operation, `compensate-order`,
because the ordinal (`owf_compensation_record.compensation_sequence`) is Orders' record and a
definition cannot see it. Each call is one bounded **pass** under the key
`…:compensate-order:{pass}` with a per-pass deadline, and returns `complete`, `in-progress` or
`pending-escalation`; the definition repeats it while `in-progress`. Each leg carries a
`submission_failure_count` with a bound of **5**; a leg reaching the bound or the sweep floor
becomes `failed-pending-escalation` and a manual task is created in that unit of work, under
either partial-failure policy — compensation failure always escalates to a manual task. The walk
continues past an escalated leg.

**Rationale**: a definition that expressed the walk as per-line tasks could only order by wave,
the formulation ADR-0005 refuses; a pass-scoped key makes a long walk resumable without absorbing
the next pass as a duplicate.

**ADR**: ADR-0005 as amended.

**Propagated**: `design/06-saga-and-compensation.md` §3.2, §3.3, §3.7; `design/10-process-definition.md`
§3.6 (c).

### D-84 (M) The apply-time cancel re-check runs once, and withdrawn authority becomes a task

**Accepted.**

**Amended by D-182 (2026-09-28)**: `reauthorization_required_at` is the fence's one rewritable stamp; `run-cancellation-fence`'s absorption of a newly authorized cancel clears it and replaces `cancel_request_ref`, and `06 §3.7` names that writer.

**Decision**: the apply-time re-check of an authorized cancel runs once, in `authorize-cancel`,
before `run-cancellation-fence`; `report-outcome` does not repeat it. `compensate-order`
(`pre-compensation`) and `report-outcome` (`pre-submission`) call slice 08's cancel-authority port
on a cancel run, and on `withdrawn` set `owf_cancellation_fence.reauthorization_required_at`: no
further leg is submitted and no Lifecycle submission is made until a newly authorized cancel is
absorbed against the run. Withdrawn authority — and `authorize-cancel` exhausting its retry budget
— creates an `authority-withdrawn` manual task; the recorded phase does not change.

**Rationale**: repeating the re-check at every step would let a later PDP outage strand a fence
half-way; marking the fence rather than failing it keeps the record honest about why it paused.

**Propagated**: `design/06-saga-and-compensation.md` §3.2, §3.7; `design/08-hold-and-cancel.md`
§3.3, §4.3; `design/10-process-definition.md` §3.6 (d).

**Amended (2026-09-26)**: the entry contradicted itself: it said the re-check runs once, then
named a `pre-submission` re-check in `report-outcome`, and 06, 08 and 09 carried three points
(OW2-11). It now runs at two points. `authorize-cancel` decides at `pre-fence`, and
`compensate-order` re-checks at `pre-compensation` and marks the fence on `withdrawn`.
`report-outcome` does not re-check, and `pre-submission` is removed from the port, from
`owf_cancel_request.last_recheck_point` and from `09 §4.4`. After the walk has verified that no
active subscription remains, the submission records a fact and performs no destructive act. A
refusal there would fault the invocation with every subscription already removed, and would leave
Lifecycle asserting subscriptions that no longer exist. That is the rationale 06 §3.2 already gave.
`authorize-cancel` also answers `preFulfillment` (D-109).

**Amended (2026-09-26)**: a withdrawn authority at `pre-fence` refuses the request and raises no
task; the task is raised only at `pre-compensation` (D-115). A spent retry budget of
`authorize-cancel` no longer creates a task: it faults the invocation, and the `invocation-dead`
task's re-drive resumes at `authorize-cancel` (D-114).

### D-85 (M) The cancel request is a slice-09 table; task requests are slice 07's

**Accepted.**

**Amended by D-181 (2026-09-28)**: the step-retry route no longer inserts `owf_task_resolution_request` itself; slice 09's gateway records it through slice 07's intake port `record_resolution_request`, in the gateway's unit of work, so slice 07 stays the table's sole writer.

**Decision**: an accepted Seller Operator cancel is recorded in `owf_cancel_request` (slice 09),
carrying the authorization snapshot, before its `cancel-requested` signal is delivered; the
signal carries only the reference tuple and `requestRef`. A `retry`, `override` or task `cancel`
is slice 07's `owf_task_resolution_request`, which the step-retry route also writes.

**Rationale**: every signal must be recorded in Orders before delivery (`10 §4.4`), and the
operator's identity must stay out of engine history (D-66); the record that names the request is
the one the consuming operation reads.

**Propagated**: `design/09-read-and-authz.md` §3.1, §3.7; `design/08-hold-and-cancel.md` §3.3;
`design/07-manual-tasks.md` §3.7; `DESIGN.md` §3.7.

### D-86 (M) An operator re-drive keeps the invocation, or the instance is unwound and re-submitted

**Accepted.**

**Decision**: an operator re-drive of an invocation the platform reports `failed` or
`dead_lettered` is the platform's `…:control` `retry` **keeping `invocation_id`**, once the
platform confirms that property (`…-upreq-serverless-runtime-signals`); `owf_process_instance.invocation_id`
is never re-bound to a second invocation, and `start-instance` answers an existing binding to a
second invocation, which then ends itself. Until the platform confirms it, a dead invocation's
instance is unwound through the fence and the order re-submitted.

**Amended (2026-09-24).** The platform accepts `retry` only from `failed`
([serverless-runtime DESIGN.md](../../../serverless-runtime/docs/DESIGN.md) line 888), and a failed
invocation with no `on_failure` handler — this definition declares none — moves `failed →
dead_lettered` (line 458). The re-drive is therefore `:control` `retry` **from `failed`**, keeping
`invocation_id`; the signals ask additionally asks that `retry` be valid **from `dead_lettered`**,
keeping `invocation_id`, and that the platform name one path for the verb, since `:control`
actions "never reach the plugin" (line 873) while the plugin-control passthrough routes `retry` to
the plugin (line 893). Until both properties are confirmed, a dead invocation's instance is unwound
through the fence and the order re-submitted.

**Rationale**: re-binding would let two invocations believe they drive one instance; the platform's
own retry of the same invocation preserves the one-to-one binding the record depends on.

**Propagated**: `design/09-read-and-authz.md` §3.3; `design/01-foundation.md` §3.3;
`UPSTREAM_REQS.md` §2.9; `DESIGN.md` §4.5.

**Amended (2026-09-26)**: "unwound and re-submitted" is replaced by D-105. The platform re-drive is
the recovery, and it must also resume at the faulted task. The fallback is a Seller Operator's
cancel, carried out in-process by the sweep. Re-acquiring the customer takes a new order, and the
PRD amendment for that loss is registered.

**Amended by D-192 (2026-09-28)**: the confirmation this entry waits for is now an explicit item of the readiness gate of `design/01-foundation.md` §3.8 that gates the `invocation-dead` task's `retry`, not readiness: the gear reports ready without it, and until the platform confirms `retry` keeps `invocation_id`, resumes at the faulted task and is valid from `dead_lettered`, the unwind is the only remedy.

### D-87 (M) One outage threshold governs the park clock, and the gate-open outage pause is a probe arm

**Accepted.** *(amends D-11, D-46)*

**Decision**: the Generic-Approval outage escalation threshold `min(30 min, 0.25 ×
lifecycle_submitted_ttl)` governs both clocks: the park escalates at `escalation_due_at = min(parked_at + threshold,
submitted_at + lifecycle_submitted_ttl − escalation_lead_time)`, settling the ADR-0007 text against the old slice text. A gate-open outage is a
definition probe arm calling `escalate-gate` in `probe` mode, which answers `available` with the
remainder or the outage state; it replaces the Orders probe loop and the `owf_timer_pause`
`approval-outage` row. The routing plan's remainder is `owf_approval_gate.window_remaining_ms`
through the gate-window port (D-70), and resume-ahead rows of `owf_process_suspension` reconcile
out-of-order hold/resume delivery.

**Rationale**: one threshold, measured from first-observed unavailability, is one number to sign
off (Q-02); a probe arm keeps the pause decision in the definition and the evidence in Orders.

**Propagated**: `design/03-approval-execution.md` §4.1, §4.2; `design/08-hold-and-cancel.md` §3.7;
`design/10-process-definition.md` §3.6 (a).

**Amended (2026-09-26)**: "first-observed unavailability" is now a column,
`owf_approval_gate.outage_since`, set with the `approval-outage` cause and cleared with it. A
fire that finds the breaker open pauses the due gates instead of refusing (D-118).

**Amended by D-141 (2026-09-26)**: the resume-ahead reconciliation now reads the order. An
`OrderResumed` closes a suspension, or leaves a `resume_ahead` row, only when Lifecycle no longer
holds the order; otherwise it answers `still-held` and closes nothing. A hold that consumes a
`resume_ahead` row still records its suspension when Lifecycle holds the order
(`design/08-hold-and-cancel.md` §2.2).

### D-88 (L) The approval routing plan is saved at the first `open-gates`

**Accepted.**

**Decision**: the full routing plan is persisted at the first `open-gates` of an order version, every
gate beyond the first in a new `planned` state; the definition opens the next gate by calling
`open-gates` with the `nextPosition` `record-decision` returns.

**Rationale**: the definition carries positions, not the plan (D-66); the plan must be fixed once
so a replay cannot re-route.

**Propagated**: `design/03-approval-execution.md` §3.7, §4.3.

### D-89 (M) Lifecycle is the sole evaluator of tolerate-failure

**Accepted.**

**Decision**: `evaluate-payment-auth-eligibility` reports a conclusive `failed` rather than
deciding it; `begin-fulfillment` carries it to Lifecycle, which applies the seller's
tolerate-failure policy, and a Lifecycle refusal is recorded as `withheld`. PRD §6.3 *Payment
Authorization Precondition* is amended to say begin-fulfillment is not **committed** rather than
not called.

**Rationale**: evaluating the policy on both sides of the seam is a second author of one rule (R2).

**Propagated**: `design/04-fulfillment-plan.md` §3.2, §3.3; `UPSTREAM_REQS.md` §4 item 9.

### D-90 (M) Payments is read on request, polled by a definition wait

**Accepted.** *(amends D-17's mechanism)*

**Decision**: the Payments coupling is an outbound read-by-request inside
`evaluate-payment-auth-eligibility`, re-evaluated on `OrderAcceptanceRecorded`, on
`reauthorize-requested` and on a definition poll `wait`; the inbound Payments reporting arm and the
`payment-auth-wait` timer are withdrawn. Pending and failed stay distinct.

**Rationale**: Payments has no specification (Q-06) and no event this gear could `listen` for; a
read keeps the coupling one-directional.

**Propagated**: `design/04-fulfillment-plan.md` §3.4, §3.6; `design/10-process-definition.md` §3.6 (b);
`design/09-read-and-authz.md` §4.1.

### D-91 (M) The overlap and market re-check is advisory at construction and authoritative before wave 2

**Accepted.** *(amends D-15's evaluation point)*

**Decision**: `construct-and-freeze-plan` records the overlap/market observation only;
`re-check-pre-activation` is authoritative before wave 2. An unevaluable re-check follows
Lifecycle's `defer` ladder, 3 attempts within 60 s, then aborts with `overlap-read-unevaluable`; an
unavailable identity port is `identity-party-unavailable`.

**Rationale**: a construction-time answer can be stale by activation; an abort must rest on the read
nearest the resource-affecting step, and an honest reason distinguishes an outage from a collision.

**Propagated**: `design/04-fulfillment-plan.md` §4.2; `design/01-foundation.md` §4.9; Q-08; `UPSTREAM_REQS.md` §2.1.

### D-92 (M) A plan-level failure never leaves `approved` by itself

**Accepted.**

**Amended by D-194 and D-196 (2026-09-30).** `topology-unavailable` no longer exists; the plan-level manual task under either policy is now `binding-expired` — the expected fulfillment time at or after the accepted-binding activation deadline (`design/04-fulfillment-plan.md` §4.3). The rule itself — no Lifecycle transition from `approved` by a plan-level failure alone; every route passes `begin-fulfillment` — is unchanged.

**Decision**: plan-level failures take no Lifecycle transition from `approved`; `topology-unavailable`
is a plan-level manual task under either policy; a plan-level failure that must report
`fulfillment_failed` passes `begin-fulfillment` first.

**Rationale**: Lifecycle's `fulfillment_failed` is reachable only from `in_fulfillment`; a plan that
never began cannot fail fulfillment.

**Propagated**: `design/04-fulfillment-plan.md` §4.3; `design/07-manual-tasks.md` §3.7.

**Amended (2026-09-26)**: "passes `begin-fulfillment` first" held only on the fail-fast route. The
exhaustion routes (`onCreate`, `onResolution`, `afterOverride`) went straight to the unwind, and
`begin-fulfillment` refused the unfrozen plan `version-mismatch` (OW2-43). Every route now passes
it: fragment (c)'s `failFastUnwind` sends a failure with no committed begin back to `planFailFast`.
`begin-fulfillment` admits an unfrozen plan with a plan `abort_record`. A `withheld`, `held` or
`version-conflict` answer waits in the eligibility fork and retries on the next `eligible` round
(D-109).

### D-93 (L) Sizes: SLA population N ≤ 40 lines, a 200-line cap, 30-day authorization validity

**Accepted.**

**Decision**: the 15-minute SLA population is orders of at most 40 lines; the plan-size admission
bound is 200 lines; `payment_auth_validity` is 30 days. These replace the non-existent "D-5" the
previous revision of slice 04 cited.

**Rationale**: the numbers were stated without a register entry; a citation to a decision that
does not exist is a defect.

**Propagated**: `design/04-fulfillment-plan.md` §4.6; `DESIGN.md` §4.1.

### D-94 (M) Remediation exhaustion is the one automatic path that compensates activated lines

**Accepted.** *(amends D-54)*

**Decision**: D-54's "only an operator-initiated cancel" rolls back activated lines is amended: D-55
remediation exhaustion is the one **automatic** route to order-level compensation of activated
lines, as `PRD.md:339` states.

**Rationale**: without it, an order whose remediation is exhausted would stay held with live
subscriptions and no path out but an operator.

**Propagated**: `design/04-fulfillment-plan.md` §3.6; `design/07-manual-tasks.md` §4.3; D-54, D-55.

### D-95 (M) An operator retry returns a failed line to its pre-wave state

**Accepted.**

**Decision**: a retry returns a failed line to the state before the failed wave — `pending` for wave 1,
`draft_created` for wave 2 — and slice 07's `retry` is defined per wave. `draft_created → pending`
with reason `draft-voided` is a machine transition driven only by `rebuild-wave1`.

**Rationale**: re-entering at the failed wave keeps the barrier's all-creates half true for a
wave-2 retry and forces a new draft for a wave-1 retry.

**Propagated**: `design/04-fulfillment-plan.md` §3.7; `design/05-provisioning-intents.md` §3.3;
`design/07-manual-tasks.md` §3.3.

**Amended (2026-09-26)** by D-119: the transition out of `failed` increments the line's attempt
for that wave, so the line is sent again under a new intent key. `evaluate-activation-eligibility`
names a retried wave-1 line in `undispatchedLineRefs`, the route that sends it.

### D-96 (M) An admission deferral is a settled success, over a slice-05 admission table

**Accepted.** *(amends `01 §4.12`'s first rule; D-44)*

**Decision**: a line the admission controls cannot admit is deferred, and the dispatch operation
settles as success carrying `deferred[]` and `retryAfterMs`, never as `retryable-failure`, so the
definition's deferral arm waits and calls again without consuming the task retry budget. The
admission state is `owf_dispatch_admission` (slice 05), serialized by row locks, not an in-memory
controller; in-flight counts are computed.

**Rationale**: PRD §6.3 requires a throttle not to consume the retry budget; a settled deferral is
the only outcome the platform's retry policy will not count.

**Propagated**: `design/01-foundation.md` §4.12; `design/05-provisioning-intents.md` §2.1, §3.7, §4.3;
`DESIGN.md` §3.7.

### D-97 (M) Intent statuses, the confirmation arm and the Subscriptions tuple

**Accepted.**

**Decision**: the intent status `dead_lettered` is renamed `unresolved` and a status `lapsed` is
added; until the platform can store only a consumed event's exported members, the confirmation arm
targets a reference-only Subscriptions notification (a further Subscriptions ask) or is dropped in
favour of the poll arm; the confirmation arm calls `reconcile-intent` first. `SUB-O13` asks for the
full lookup tuple (with `intentKind` and `wave_attempt`), and `SUB-O16` for the union of field
lists.

**Rationale**: `dead_lettered` now names a platform status; the published Subscriptions outcome
carries `subscriptionId`, which D-66 keeps out of engine history.

**Propagated**: `design/05-provisioning-intents.md` §3.7, §4.1; `UPSTREAM_REQS.md` §2.1, §2.9; D-131.

**Amended (2026-09-26)**: no "further Subscriptions ask" exists and none is raised. The
confirmation arm is built as the canonical definition has it: it listens to the published
`ProvisioningIntentConfirmed` / `ProvisioningIntentFailed` events and keeps only the line
reference and wave, and the `subscriptionId` those events carry is part of ADR-0013's stated
residual (D-131) until `…-upreq-serverless-runtime-consumed-event-member-storage` lands. If that
ask is declined, the confirmation arm is dropped and the poll arm is the only path; nothing else
changes, because the arm only shortens the wait before `reconcile-intent` (second review OW2-88).

### D-98 (M) The manual-task reason enum is a catalogue subset, and SLA classes are 4 h and 24 h

**Accepted.**

**Decision**: `owf_manual_task.reason` is the catalogue subset covering line, plan, order-level and
lifetime-ceiling subjects, including `trigger-applicability-unverified`; `overlap-collision` and
`market-divergence` are dropped as task reasons. The SLA classes are **4 h** for a resource-affecting
subject and **24 h** otherwise, replacing a stale "D-6" citation; stale "D-3" citations mean D-55.

**Rationale**: a task reason outside the catalogue is free text in disguise; the 4 h window sits
inside the 24 h overdue window so an SLA breach is visible before the order-level escalation.

**Propagated**: `design/07-manual-tasks.md` §3.7, §4.1, §4.7.

**Amended (2026-09-26)**: `trigger-applicability-unverified` is no longer a task reason. A spent
listen-arm admission faults the invocation and reaches the operator as `invocation-dead` (D-114).
It stays a catalogue reason, the retryable refusal of `admit-trigger`, so the catalogue count is
unchanged.

**Amended (2026-09-26)**: 4 h and 24 h are the defaults of the seller's policy, not fixed values.
`create-manual-task` resolves the class window from the seller's policy and pins it into
`sla_deadline`, and a policy write that puts a class above the overdue window is refused (D-134).

### D-99 (M) The remediation hold is a flag; cancelling the last open forward task exhausts remediation

**Accepted.**

**Decision**: the remediation hold is an open forward task under `remediate`, projected as a derived
flag, not a sixth `assignment_state`. A Seller Operator's `cancel` of the last open forward task of a
line or plan subject that is still failed under a live order returns `exhausted`, which declares
remediation exhausted for that subject.

**Rationale**: an assignment state would conflate the operator's progress with the order's dispatch
state; cancelling the last task is the Seller Operator's judgement that remediation is moot.

**Propagated**: `design/07-manual-tasks.md` §3.3, §4.3; `design/04-fulfillment-plan.md` §4.4, §4.7.

**Amended (2026-09-26)**: the hold lasts until the order's last open task resolves. A line retry or
a verified override while other tasks are open returns the definition to `awaitResolution`, and
the last resolution takes every retried line back to the barrier (D-117).

### D-100 (M) Task actions record a request and signal; every mutating route requires a key

**Accepted.**

**Decision**: the task routes are per action; `retry`, `override` and task `cancel` record
`owf_task_resolution_request` and signal `task-resolution-requested`, consumed by
`resolve-manual-task` (and `verify-override`); `assign` and `escalate` apply in-process and signal
nothing; `escalate` is not a closing action. Every mutating task route requires `Idempotency-Key`
`{tenant}:{taskId}:{action}:{rowVersion}`, the decision route
`{tenant}:{gateId}:decision:{subject_id}`. The generic `…/tasks/{taskId}/resolve` route is retired
and the step-retry route is an alias of the task's `retry`.

**Rationale**: the definition must apply what changes the process path; actions that change only the
task record need no round trip through the platform.

**Propagated**: `design/07-manual-tasks.md` §3.3; `design/09-read-and-authz.md` §3.3, §4.1;
`DESIGN.md` §3.3; `ADR/0010`.

### D-101 (L) The task queue sorts on the immutable `(created_at, task_id)` key

**Accepted.**

**Decision**: the operator task queue pages on the immutable key `(created_at, task_id)`, as slice
09's paging rule requires; `sla_deadline` is mutable on reopen and is therefore not a sort key.

**Rationale**: a keyset cursor over a mutable column skips or repeats rows when the column changes
between pages.

**Propagated**: `design/07-manual-tasks.md` §3.3; `design/09-read-and-authz.md` §2.2.

### D-102 (H) One rule for re-invokable operations: a round in, the next round out, a counter on the instance row, an attempt only from an operator retry

**Accepted (2026-09-26).** *(generalises D-74; amends D-78; ADR-0006 as amended)*

**Decision**: every **re-invokable** operation — every re-check of a fixed-wait loop, the
dispatch, re-read, rebuild and sweep operations, the evaluations, and every operation that calls a
Lifecycle transition — keys on a round the definition passes back from the previous settled answer
of the same operation (`0` on first entry) and returns the next one. Every settled success returns
the next round, including `due: false`, `unobtainable`, `none`, `withheld` and `held`, and none
leaves its record `open`. The envelope validates the round against a per-instance, per-family
counter, `owf_process_instance.key_rounds`, advanced in the settlement transaction, never against a
count of `owf_step_log` rows. An operator retry mints the next `attempt` of the failed step's family
through `retry-step`; the re-entered operation appends it after the round, and `reflect-verdict`
gains it. The Lifecycle-transition family is
`{tenant}:{orderId}:{orderVersion}:{transitionName}:{round}[:{attempt}]`. On a Lifecycle
`not-admissible` the operation reads the order, and if it is `on_hold` answers `held`, a settled
success; the definition waits for the resume (`awaitHeldReflect`, `heldWait` or the eligibility
wait, each with a `PT5M` tick) and calls again under the next round. The park loop gains a hold and
a resume arm that only record, and `apply-hold` on a parked instance records the suspension and
leaves the phase `parked`. `rebuild-wave1` and `reread-draft-liveness` get rounds of their own.
`begin-fulfillment` short-circuits to `in-fulfillment` once committed and answers `held` and
`version-conflict` as settled successes, so its call sits under a plain retry-only `catch`. The
definition's `attemptId` reads `$task.reference`, not `$task.name`.

**Rationale**: Lifecycle settles a `not-admissible` refusal under its key and replays it
"regardless of the version the retry carries" (Lifecycle `01-foundation.md:2097`, `:2163`), so a
fixed per-trigger key let one hold-time refusal block the spawn signal, the reflection or the
completion acknowledgement for good (second review OW2-41). Lifecycle's own answer to a held order,
`not-dispatchable` — re-read the order, wait for the resume, re-run (`06-workflow-seam.md:743`) —
is the precedent the `held` answer follows. The same rule closes six more findings: a same-key
`due: false` loop kept one key open past its 30-day lifetime (OW2-34); counting settled step-log
rows broke once the 90-day step-log purge ran (OW2-38); `rebuild-wave1` was given wave 1's round
(OW2-22); an operator retry of a refused reflection replayed the stored refusal (OW2-25);
`begin-fulfillment`'s 409 catch read still-processing as a version conflict (OW2-24); and the
register named two round components where many exist (OW2-26). `$task` in the call envelope is the
inner `call` task every step shares, so `$task.name` was `call` everywhere (OW2-5). Rejected:
exempting re-check keys from the key lifetime, which leaves `01 §3.7`'s "`open` only after a
retryable failure" contradicted; and mapping `not-admissible` to a retryable answer, because a
retry reuses the key and Lifecycle answers it with the stored refusal.

**Propagated**: `design/01-foundation.md` §3.3 (*The step-operation contract*, *Rounds and
attempts*, *Attempt identity*, `retry-step`), §3.7 (`owf_process_instance`, the phase table),
§4.3, §4.14; `design/03-approval-execution.md` §3.3, §3.6, §4.1, §4.4, §4.5;
`design/04-fulfillment-plan.md` §3.3, §3.6, §4.1, §4.8; `design/05-provisioning-intents.md` §3.3,
§3.6, §4.5; `design/06-saga-and-compensation.md` §3.3, §3.6; `design/07-manual-tasks.md` §3.3,
§3.6, §4.2; `design/08-hold-and-cancel.md` §3.3, §3.6; `design/10-process-definition.md` §3.1,
§3.6 (a), (b), (e); `UPSTREAM_REQS.md` §2.9; ADR-0006 as amended; `ADR/0011` (*Attempt identity* amendment); D-74, D-78.

**Amended (2026-09-26)** by D-119: for a line task the attempt belongs to the dispatch family of
the line's wave, and the definition keeps one attempt per wave (`wave1AttemptKey`,
`wave2AttemptKey`). The dispatch step keys end `{dispatchRound}[:{attempt}]`. The intent key's
`wave_attempt` is a separate, per-line counter on the task row.

**Amended by D-171 (2026-09-28)**: `01 §4.15` now applies rule 1 to an early wake-up — `due: false` and the next round, a settled success — where it had required `retryable-failure`.

**Amended by D-188 (2026-09-28)**: rule 4's read of the order on a Lifecycle `not-admissible` first recognises the transition already applied — the order at `orderVersion` in the target state of the transition submitted — and settles success `already-applied`, because a key re-issued after Lifecycle's 24-hour window is a new operation there (Lifecycle `01-foundation.md:238-241`).

### D-103 (M) The registry lease is fenced by a holder token, sized below the retry horizon, and resolved per key family

**Accepted (2026-09-26).** *(amends D-03 and D-71)*

**Decision**: `owf_idempotency_registry` gains `lease_holder`, a token minted at every lease
acquisition, and `receipt_count`. Every heartbeat and settlement is a conditional update on
`status = in_flight`, the caller's `lease_holder` and a live `lease_expires_at`, run last in its
transaction; zero rows rolls the settlement back whole and answers `retryable-failure`. A
record-only operation resolves, runs and settles in **one** transaction, so a crash leaves no
`in_flight` row. A dead lease is settled by lookup (`settle-from-lookup`) only on the
intent-submitting operations — `dispatch-wave1-create`, `dispatch-wave2-activate`,
`compensate-order`; every other operation re-runs it under a new holder, because its outbound call
is a read or a submission the downstream de-duplicates under the key the step derives. The lease is
**15 s** with a **5 s** heartbeat (was 60 s and 20 s). `settle-from-lookup` keys on the stuck
record's `lease_holder`. `owf_step_log` rows carry a per-key `receipt_ordinal`, with
`UNIQUE (operation, idempotency_key, receipt_ordinal)`; `attempt_number` is a derived, non-unique
count.

**Rationale**: the precedent is the BSS `coord` lease, whose ack transaction ends in a conditional
self-update on `locked_by` and the live deadline and rolls back on zero rows
(`gears/bss/libs/coord/src/lease/guard.rs:160-264`); the single-transaction record-only shape is
Lifecycle's (`01 §4.2`, "crash recovery normally relies on transaction rollback or settled-outcome
replay"). Without a holder a late holder could commit over a re-run or a lookup settlement
(OW2-36); a crash during any non-dispatch step left a key no one could settle, and a 60 s lease
outlived the ~15-30 s retry budget so every retry met a live lease (OW2-30); a second crash behind
an `absent` reopen was absorbed as a replay of the first lookup (OW2-31); and a UNIQUE over the
settled-attempt count collided with the rule that every receipt writes a row (OW2-37).

**Propagated**: `design/01-foundation.md` §3.3 (`settle-from-lookup`, the outcome table), §3.6,
§3.7 (`owf_step_log`, `owf_idempotency_registry`), §3.8, §4.3; `design/05-provisioning-intents.md`
§3.2, §3.6, §4.4; `design/06-saga-and-compensation.md` §3.6; `DESIGN.md` §3.7, §3.8; D-03, D-71.

**Amended (2026-09-26)**: "sized below the retry horizon" holds for most retry trains, not all
of them. The lease dies 10-15 s after a crash, and a train whose every attempt meets the live
lease exhausts and faults the invocation; that fault is the stated fallback (`invocation-dead` and
the re-drive, D-105), and a longer lease would make every train meet it. `01 §3.7` states the
lease rationale as probabilistic (re-review R-3, D-39 as amended).

**Amended by D-167, D-168, D-169 and D-170 (2026-09-28)**: a replay answers from the registry's `settled_output`, not the step-log `result`; the zero-rows fence is one case of a general abort rule, which also states the answer (503 or 500) and what each operation shape leaves; `start-instance` joins the exceptions to the one-transaction shape, reading the platform before its transaction; and "every receipt writes a row" is "every receipt that reached registry resolution", with `absorbed` added to the outcome enum.

### D-104 (M) No Workflow-owned table is partitioned; registry rows are kept as tombstones until no replay can arrive

**Accepted (2026-09-26).** *(mirrors Lifecycle D-91)*

**Decision**: no Workflow-owned table is range-partitioned, in any slice; every purge is a bounded
row-wise `DELETE … WHERE` through the table's retention index, run by `retention-purge`. A future
partitioning **MUST** put the partition column in every PK and UNIQUE and **MUST NOT** be applied
to a table whose uniqueness is a deduplication guard. `owf_idempotency_registry` rows are deleted
only once `expires_at` has passed and the owning instance has been terminal for 30 days (a
`trigger`-family row that started no instance: 30 days past `expires_at`); until then an expired row
stays as a tombstone, and aged-out is evaluated from it.

**Rationale**: PostgreSQL requires every unique constraint of a partitioned table to include the
partition columns, so the registry PK, `UNIQUE (idempotency_key)` on intents and compensation
records and the one-open-row partial indexes could not be built as written, or held only within a
month (OW2-33). Lifecycle withdrew partitioning for this class of reason (Lifecycle D-91);
Ledger's composite keys add the partition column (`01-repository-foundation.md:453`), which keeps a
constraint buildable but gives up the global uniqueness deduplication needs. Once a registry
partition was dropped, an expired key had no row and resolved as a first call, so a late replay
could repeat its effect (OW2-39); Lifecycle's rule that "expiry is logical, not dependent on sweep
timing" (`01 §4.2`) is the one kept here.

**Propagated**: `design/01-foundation.md` §3.7 (every table, *Partitioning, retention and
immutability*), §3.8; `design/03-approval-execution.md` §3.7; `design/04-fulfillment-plan.md`
§3.7; `design/05-provisioning-intents.md` §3.7; `design/06-saga-and-compensation.md` §3.7;
`design/07-manual-tasks.md` §3.7; `design/08-hold-and-cancel.md` §3.7; `DESIGN.md` §3.7, §4.1; `design/09-read-and-authz.md` §3.7, §4.3.

**Amended (2026-09-26)**: the retention register of `DESIGN.md` §3.7 now has a row for every one of
the twenty-six tables (twenty-seven since D-140, twenty-eight since D-160), with its window and what the `retention-purge` worker does with it, and the
worker's roster in `design/01-foundation.md` §3.8 names every store with a window; the
never-purged stores (the audit store and its checkpoints, the instance, the binding, the progress
view) are listed as such. `design/09-read-and-authz.md` §4.3 cites the register instead of
keeping a partial copy. The worker never deletes a row of a non-terminal instance, which can
outlive a 400-day window only behind a ceiling park (D-129). Second review OW2-87: the register
said every window had a worker, while the roster named six stores.

**Amended by D-167 (2026-09-28)**: the tombstone rule keeps a replayable answer too — each settled row carries its immutable `settled_output` — and the step log's 90-day window runs whatever the instance's state, under a DELETE grant only the retention purge holds.

**Amended by D-185 (2026-09-28)**: "aged-out is evaluated from it" holds for a key whose operation submits downstream; any other key is evaluated for aging only once its instance is terminal, so a live instance's record-only or read-only key is never answered `aged-out`.

**Amended by D-191 (2026-09-28)**: the roster gains `owf_read_access_log`, purged at 90 days from `accessed_at` whatever the instance's state (`design/09-read-and-authz.md` §3.7); the register now covers twenty-nine tables.

### D-105 (H) A dead invocation is raised as one order-scope task; the platform re-drive is the recovery, an Orders-driven cancel the fallback

**Accepted (2026-09-26).** *(amends D-71 and D-86)*

**Decision**: the `reconciliation-sweep` pass gains an **instance liveness pass**. It pages over
`owf_process_instance` rows that are non-terminal and bound to an invocation, with
`next_liveness_at <= now()` (a new column, set 15 min ahead at start and after every read). For
each row it reads `GET …/invocations/{invocation_id}`. `queued`, `running` and `suspended` are
live. For any other status, or a 404, it creates one order-scope manual task with reason
`invocation-dead` (new catalogue reason, owner slice 07, 4 h SLA) and cause `invocation-ended`,
under the instance row lock, with a `sweep` audit entry. The task's uniqueness absorbs the task on
later passes, and a re-drive that dies again reopens it. An unreadable status writes nothing.
The task offers two resolutions:

- **`retry` is the recovery** (owner ruling R3): the platform's `:control` `retry`, keeping
  `invocation_id`. The definition is written for a re-drive that **resumes at the faulted task**.
  A restart from the top would replay settled rounds, but it would wait for events that were
  consumed before the fault. So `retry` is offered only once the platform confirms that it keeps
  the invocation, resumes at the faulted task, and is valid from the status the invocation is in
  (today `failed` only).
- **`cancel` (Seller Operator) is the fallback**: the order cancel of `09 §3.3`. With no
  invocation to signal, the sweep runs the cancel path in-process, one operation per pass, under
  the keys the definition would present: `authorize-cancel`, `run-cancellation-fence`,
  `compensate-order` until `complete`, `report-outcome`, then `terminate-instance`. Task
  resolutions raised on the way are consumed in-process. The customer is re-acquired by a **new
  order**, and the PRD amendment for this loss is registered (`UPSTREAM_REQS.md` §4 item 11).

A `canceled` invocation raises the task the same way. A generic `suspend` by another caller
cannot be told apart from a wait. Denying generic control on `order_process` is a new upstream ask.

**Rationale**: the sweep's candidate set was non-terminal intents. An instance whose intents were
all terminal — a Lifecycle outage past the retry budget at `report-outcome` — was not even
metered. Its order stayed in `in_fulfillment` with live subscriptions, no task and no escalation,
which breaks `fr-owf-dependency-resilience` and `nfr-owf-manual-task-sla` (OW2-27). "Unwound and
re-submitted" named no actor, no executor and no Lifecycle operation. A `fulfillment_failed` or
`cancelled` order cannot be amended (Lifecycle `01 §3.7` transitions 13–27), so the only
re-submission is a new order, and the PRD must accept that loss explicitly (OW2-28). The platform
states only that `retry` runs "with same parameters" (serverless-runtime `DESIGN.md:888`), so the
resume point has to be asked for and not assumed (OW2-35). Orders cannot stop generic control, so
it must at least detect it (OW2-67). **No precedent exists** in the platform or in the BSS gears
for detecting a dead platform invocation, because no sibling runs a platform workflow. The
per-row transactional recheck is the roster rule this gear adopts from Lifecycle `01 §3.8`, and
the status read is the platform's own `GET …/invocations/{id}` (`DESIGN.md:867`). This is the
smallest rule: one read per bound instance per interval, and one task per dead invocation.

**Propagated**: `design/01-foundation.md` §3.3 (`start-instance`), §3.7 (`owf_process_instance`),
§3.8, §4.9, §4.13, §4.16; `design/05-provisioning-intents.md` §3.8;
`design/06-saga-and-compensation.md` §3.2, §3.6, §3.8; `design/07-manual-tasks.md` §3.3, §3.7,
§4.1, §4.4, §4.7, §4.8; `design/09-read-and-authz.md` §3.3; `design/10-process-definition.md`
§3.3, §3.8, §4.4; `DESIGN.md` §1.2, §3.3, §3.8, §4.2, §4.4, §4.5, §4.9;
`UPSTREAM_REQS.md` §2.9, §3, §4; `ADR/0001`, `ADR/0005`, `ADR/0012`; D-64, D-71, D-86.

**Amended (2026-09-26)**: the fallback's cancel reported `cancel-workflow-mediated` whatever the
order's state. Lifecycle admits that trigger only from fulfillment. For an order whose fulfillment
never began, the task's `cancel` is `action-not-offered` while Lifecycle holds the order live. The
Seller Operator cancels it through Lifecycle first, and the unwind then makes no Lifecycle call
(D-109).

**Amended (2026-09-26)**: the liveness pass is the failure arm of every step call the definition
does not catch. Retry exhaustion of a listen-arm admission, `apply-hold`, `apply-resume`,
`authorize-cancel`, the fence or `report-outcome` reaches the operator only as `invocation-dead`
(D-114).

**Amended by D-151 (2026-09-26)**: while the platform reports the invocation not live, the
pass runs every 5 min and calls `resolve-manual-task` (`sla-check`) in-process, scoped to the
`invocation-dead` task and to the escalate-only tasks the dead-instance unwind raised, so their
SLA breaches. The control-surface algorithm (`09 §3.6` `inst-cs-deliver`) issues `:control retry`
only as the `invocation-dead` task's `retry` and only under D-86's confirmation condition
(re-review R-2).

**Amended (2026-09-26, third re-review S-9)**: the control-surface algorithm decides liveness
before it records anything (`09 §3.6` new step `inst-cs-live`): on a not-live invocation a step
retry is the `invocation-dead` task's `retry`, refused `action-not-offered` with no row written
unless D-86's condition holds, and a cancel is that task's `cancel`, recorded for the dead-instance
unwind; the request row names the task it resolves.

**Amended by D-185 (2026-09-28)**: the `invocation-dead` task's `retry` also mints, in the transaction that records it, the next `attempt` of each downstream-submitting family whose key resolves `aged-out`, and the dead-instance unwind does the same after a recorded cancel, so the re-driven or in-process call runs under its successor key instead of faulting again.

**Amended by D-192 (2026-09-28)**: "`retry` is offered only once the platform confirms" is stated as an explicit item of the readiness gate (`design/01-foundation.md` §3.8) that gates the action, not readiness; `design/07-manual-tasks.md` §4.4 names the unwind as the only remedy until then, and aged-out downstream keys (D-185) and deterministic 500s (D-168) as faults that land there.

**Amended by D-194 and D-196 (2026-09-30)**: the catalogue is **41** reasons (D-77 as amended); `invocation-dead` is unchanged.

### D-106 (H) Every step call is bound to the instance's invocation, and the fence needs a recorded cause

**Accepted (2026-09-26).** *(amends D-67)*

**Decision**: `start-instance` returns `invocationId`, the invocation bound to the instance, and
the definition's `onBinding` compares it with `$workflow.id`. Every later step-route call must
carry that invocation. The exceptions are `admit-trigger` in the start role and `start-instance`
itself. The envelope compares the body's `invocationId` with `owf_process_instance.invocation_id`
under the instance row lock. A mismatch, or no instance, is `not-found` and runs no effect.
In-process calls are not route calls and are exempt. `run-cancellation-fence` requires a recorded
cause per trigger:

- `cancel`: a settled `authorize-cancel` that answered `true`;
- `supersede`: a settled `supersede` admission;
- `terminal-event`: a settled `terminate-on-terminal-event`;
- `failure`: an exhausted task, a fail-fast failure or a settled pre-activation abort.

Otherwise it answers `not-found`. The attempt-identity ask gains a clause asking the platform to
assert the invocation on the call.

**Rationale**: `start-instance`'s output schema had no `invocationId`. Built to the table, the
definition's inequality check read null and ended every invocation (OW2-29). The `Gr` grant
restricted a call to the `correlationId` the caller named, which is a derivable UUIDv5. Nothing
compared the call with the bound invocation, and a `failure` fence claimed on nothing. So any
caller holding the platform identity could fence and compensate a healthy order, and
`DESIGN.md`'s claim that the run-time guards bound a rogue definition was false for the unwind
(OW2-58). Precedent: the BSS `coord` lease re-checks its holder inside every write's transaction
(`gears/bss/libs/coord/src/lease/guard.rs:160-264`, D-103), and here the bound invocation is that
holder. The per-trigger precondition follows `terminate-on-terminal-event`, which already refuses
`not-found` without a settled `terminate` admission (`02 §3.3`).

**Propagated**: `design/01-foundation.md` §3.3 step 3, `start-instance`, §3.6, §3.7;
`design/02-triggers-and-start.md` §3.6; `design/06-saga-and-compensation.md` §3.3, §3.6;
`design/09-read-and-authz.md` §2.2, §4.1, §4.2; `design/10-process-definition.md` §3.6 (a);
`DESIGN.md` §3.3, §4.2; `UPSTREAM_REQS.md` §2.9; D-67; `ADR/0012`.

### D-107 (M) The start is checked against the Lifecycle order, and the bindings are released with the definition

**Accepted (2026-09-26).** *(amends D-73)*

**Decision**:

- `admit-trigger` refuses `not-found` when the order it reads from Lifecycle has a
  `resource_tenant_id` different from the body's.
- On the start role it admits `start` only for a `submitted` order whose `category` is
  `new_sale`; any other category is `no-active-instance`.
- The definition derives `triggerKind` from the exact event type. Any other type is null, which
  the schema refuses.
- `order_process` is started only by its two bindings. They are repository artefacts in
  `definitions/`, CI-checked and applied by the release pipeline under the publish role, and the
  readiness check reports drift.
- The platform is asked to refuse other starts and to deny generic control on `order_process`
  (`…-upreq-serverless-runtime-invocation-control-restriction`).

**Rationale**: the instance's tenant axis came from the event's `data.resourceTenantId`. Any
type not ending in `amended.v1~` became `OrderSubmitted`, and no step compared the read order's
tenant with the body's (OW2-59). The `new_sale` filter sat only on a tenant-scoped platform object
that anyone with binding rights can edit, outside the fence (OW2-66). Precedent: this gear already
resolves the seller axis from the Lifecycle read, never from the task input (D-76). Lifecycle
states that identifier equality alone never confers cross-tenant access (`08 §4`). Lifecycle
refuses a category other than `new_sale` at creation (`category-not-admitted`, Lifecycle
`01-foundation.md:812-814`), so the check here is defence in depth.

**Propagated**: `design/02-triggers-and-start.md` §2.1, §2.2, §3.1, §3.3, §3.6, §3.8, §4.7;
`design/10-process-definition.md` §3.3, §3.6 (a), §3.8; `DESIGN.md` §3.5, §4.2;
`UPSTREAM_REQS.md` §2.9; D-73.

**Amended (2026-09-26)**: the bindings are applied by the definition publish job, which builds
and deploys nothing and re-points both bindings on a major bump (D-138).

### D-108 (M) `retry-step` runs only in-process, with the actor from the request row

**Accepted (2026-09-26).**

**Decision**: `retry-step` stays a registered `composable` operation, and it is **in-process
only**, like `settle-from-lookup`. It runs inside `resolve-manual-task`'s `retry` resolution. Its
`process_step × execute` value is denied to every principal, and the validation hook rejects a
`call` to it. Its input gains `requestRef`, and it records the actor from the request row's
`requested_by`. Thirty-three values are granted, down from thirty-four. The protected and
composable counts are unchanged at 22 and 13.

**Rationale**: the operation was granted to the platform principal for the Failure stage, but no
definition calls it. It recorded its actor from the `SecurityContext`, which on that route is the
serverless-runtime principal, against `07 §4.6` rule 3. A direct call would mint an attempt with
no operator request and audit the platform as the operator (OW2-63). Precedent: the in-process,
denied-to-every-caller shape of `settle-from-lookup` (`01 §3.3`, `09 §3.1`), and slice 07's
actor-from-the-request-row rule.

**Propagated**: `design/01-foundation.md` §3.3; `design/07-manual-tasks.md` §4.6;
`design/09-read-and-authz.md` §3.1, §4.2, §4.5; `design/10-process-definition.md` §2.2, §4.1;
`DESIGN.md` §3.3, §3.5, §4.2; `UPSTREAM_REQS.md` §2.8; D-67, D-69.

**Amended (2026-09-26)**: "exactly as `settle-from-lookup` runs only inside the sweep" was
wrong since `compensate-order` settles its own previous pass through it (`06 §3.6`
`inst-co-settle-prior`). The comparison is with `settle-from-lookup` running only in-process inside
its three named callers — `reconcile-intent`, `compensate-order`, the sweep worker (re-review
R-24).

**Amended by D-185 (2026-09-28)**: `retry-step`'s minting effect also runs in-process inside the control gateway's recording of an `invocation-dead` `retry` and inside the dead-instance unwind, for an aged-out successor, with the actor from the recorded request; it is still never a definition `call` and no principal is granted it.

### D-109 (H) Orders reports to Lifecycle only from fulfillment; a cancel before it is Lifecycle's own

**Accepted (2026-09-26).** *(amends D-84, D-92, D-105)*

**Decision**:

- `failed` and `cancelled` reach Lifecycle only for a version whose `begin-fulfillment`
  committed (`owf_fulfillment_plan.begin_fulfillment_committed_at`).
- The Workflow cancel route refuses `action-not-offered` while fulfillment has not begun and the
  Lifecycle order is live. `authorize-cancel` checks the same at apply time and answers
  `preFulfillment`, and the definition returns through `back`. The order is cancelled through
  Lifecycle's `POST /cancel`, and its `OrderCancelled` ends the process on the terminal-event path.
  The dead-invocation task's `cancel` follows the same rule (D-105).
- Every failure unwind passes `begin-fulfillment` first (D-92 as amended). Only a plan-scope
  failure is reached before it, because order-scope tasks never exhaust (`07 §4.2`).
- `report-outcome` makes no Lifecycle call for a run whose begin never committed. It settles
  `terminal-event` where Lifecycle holds the order terminal. It settles the same where Lifecycle
  refuses `not-admissible` on an order it holds terminal.
- `report-spawn-signal` answers `not-dispatchable`, a settled success, when Lifecycle refuses
  `not-admissible` on a terminal order. The definition returns to the barrier loop, whose
  lifecycle arm consumes `OrderCancelled`.

**Rationale**: the cancel arm in the approval and eligibility forks always reported
`cancel-workflow-mediated`, and an exhausted plan task reported `acknowledge-failed`. Lifecycle
admits both only from `in_fulfillment`, or from `on_hold` with pre-hold `in_fulfillment`, so
either faulted the invocation (OW2-43). A seller's direct cancel racing a failure unwind met an
undefined refusal at the report (OW2-10). A direct cancel committing before the spawn signal,
which Lifecycle declares the normal outcome of that race, went to a retrying catch and faulted
(OW2-46). B2 left open what the dead-instance cancel reports for such an order. Precedent:
Lifecycle `01 §4.3` rows 14, 16, 26, 27 and 17; its permission matrix, "via workflow-cancel only"
(`08 §4.3`); `06 §4.3`, "if cancellation commits first, the signal refuses and Workflow
dispatches nothing". Within the set, `re-check-pre-activation` already answers `not-dispatchable`
for a terminal order (`04 §3.6`), `begin-fulfillment`'s `version-conflict` is a settled success
(D-102), and 06 §3.2 already reports a terminal-event unwind with no call.

**Propagated**: `design/01-foundation.md` §4.16; `design/04-fulfillment-plan.md` §3.6, §4.3,
§4.8; `design/05-provisioning-intents.md` §3.3, §4.5; `design/06-saga-and-compensation.md` §3.2,
§3.6, §4.1, §4.9; `design/07-manual-tasks.md` §4.4; `design/08-hold-and-cancel.md` §3.3, §3.6,
§4.7; `design/09-read-and-authz.md` §3.3, §3.7; `design/10-process-definition.md` §3.6 (b), (c),
(d); `DESIGN.md` §3.3, §3.6; D-84, D-92, D-105.

### D-110 (H) The fence resolves the failure reason from the record and maps it to Lifecycle's closed enumeration

**Amended by D-194 and D-196 (2026-09-30).** Plan failures no longer map to `dependency-graph-invalid`: the two graph causes are withdrawn, and the three accepted-binding causes — construction `binding-expired`, the re-check abort and the wave-2 guard or receiver refusal — map to Lifecycle's `order-binding-expired` (`06 §4.8`; Lifecycle feature 06 §4.4). `payment-authorization-stale` remains the one interim value.

**Accepted (2026-09-26).**

**Decision**: `run-cancellation-fence` takes no failure reason as input. On `failure`, the cause
its precondition found (D-106) names the Orders catalogue reason. The fence records it as
`orders_failure_reason`, together with the Lifecycle `failure_reason` that the table of
`06 §4.8` maps it to:

- line failures map to `line-execution-failed`;
- plan failures map to `dependency-graph-invalid`;
- re-check aborts map to `overlap-collision`, `market-divergence`, `overlap-presence-unevaluable`
  or `identity-party-unavailable`.

`payment-authorization-stale` and `catalog-topology-unavailable` have no Lifecycle value. They are
sent as interim values and registered as
`…-upreq-lifecycle-failure-reason-coverage` (`p2`). `re-check-pre-activation` records the
exhausted port's own reason, identity or occupancy.

**Rationale**: the fence input and column were typed as Lifecycle's enumeration, but the
definition passed Orders catalogue codes, or nothing at all on pre-activation abort and on the
line branches. Lifecycle refuses any value outside its six with `request-invalid`, so every
failed order would have faulted at the report (OW2-42). Deriving the reason from the recorded
cause, rather than taking it from the definition, follows ADR-0013. It also follows the D-106
precondition, which already reads that cause. Precedent: Lifecycle `06 §4.4` (D-136) and its
refusal of out-of-enumeration values at the boundary (`01 §4.7`, D-142). Lifecycle carries each
unavailable port's reason separately (its D-127).

**Propagated**: `design/04-fulfillment-plan.md` §3.6, §4.3; `design/06-saga-and-compensation.md`
§3.3, §3.6, §3.7, §4.3, §4.8; `design/10-process-definition.md` §3.6 (b), (c);
`UPSTREAM_REQS.md` §1.1, §1.2, §2.4, §3, §5.

### D-111 (M) The report follows the fence row, carries the requester's reason, and names an answer for every Lifecycle refusal

**Accepted (2026-09-26).**

**Decision**:

- The definition takes `reportAs` and `terminationKind` from the fence's `effectiveTrigger`,
  never from the path it entered by. A cancel absorbed against a supersede or terminal-event run
  keeps that run's report.
- The cancel route requires a free-text `reason` of 1–500 characters. A missing one is canonical
  `InvalidArgument` with a field violation. It is stored as `owf_cancel_request.cancel_reason` and
  carried as Lifecycle's `cancel_reason`. On a promoted failure run it is the promoting request's.
- `report-outcome` answers every Lifecycle outcome of its two endpoints by `06 §4.9`.
- The completion predicate requires the subscription identifiers to be distinct across the plan's
  tasks.

**Rationale**: `toCancelUnwind` overwrote `reportAs`, so a superseded version's unwind faulted at
the report with `version-mismatch`, and the amended version never started (OW2-9). Lifecycle's
`workflow-cancel` requires `cancel_reason`, but neither the route nor the request record captured
one (OW2-45). Only three refusals had an answer (OW2-47). Precedent: `06 §4.1` item 3, which takes
the mode from the fence row, never from the input. The task requests' required `justification`
(`07 §3.7`) and the canonical `InvalidArgumentV1::FieldViolations`
(`libs/toolkit-canonical-errors/src/context.rs:128`). Lifecycle's distinctness guard
(`06 §4.4`).

**Propagated**: `design/04-fulfillment-plan.md` §2.1 (via `06 §3.2`); `design/06-saga-and-compensation.md`
§3.2, §3.3, §3.6, §3.7, §4.3, §4.7, §4.9; `design/09-read-and-authz.md` §3.3, §3.7;
`design/10-process-definition.md` §3.6 (c), (d), (f); `DESIGN.md` §3.3, §3.6.

### D-112 (M) `reflect-verdict` sends Lifecycle's wire form, and a moved order is waited out, not failed

**Accepted (2026-09-26).**

**Decision**: `reflect-verdict` sends:

- `verdict` ∈ `required` · `not_required` · `granted` · `denied`;
- one `deciding_authority`: for `granted`, the authority of the last-decided gate, by
  `sequence_index`, then `decided_at`, then `gate_id`; for `denied`, the rejecting gate's;
- `denial_reason` on `denied` only.

Lifecycle `version-conflict`, and `not-admissible` on an order Lifecycle holds terminal, answer
`moved`. It is a settled success, which the definition waits out in `awaitHeldReflect`, where the
lifecycle arm consumes the event. Every other refusal is `approval-reflection-refused` (400),
which fragment (a)'s catch routes to the order-scope task. `version-mismatch` is kept for a
missing or terminal instance.

**Rationale**: the contract named `approval-reflection-refused`, but the algorithm and §4.4
answered `version-mismatch` (409). The inner catch retried 409 and then faulted, so the task arm
never fired (OW2-44). The call sent a "trigger" and plural authorities and dropped the denial
reason, which Lifecycle would refuse `denial-reason-missing` (OW2-48). Precedent: Lifecycle
`06 §3.6` *Reflect Verdict* and `04 §4.4`, where a stale result "MUST NOT be treated as a failure
of the operation it reports". Also `begin-fulfillment`'s settled `version-conflict` (D-102).

**Propagated**: `design/03-approval-execution.md` §3.3, §3.6, §4.4;
`design/10-process-definition.md` §3.6 (a).

**Amended by D-190 (2026-09-28)**: every other refusal answers `reflected = refused`, a settled success carrying `refusalReason`, which fragment (a) routes on the output to the order-scope task; the 400 `catch` is removed. D-188 adds that a `rejected` order at this version after `reflect-approval-denied` is the reflection applied, not `moved`.

### D-113 (M) Workflow does not pre-check buyer acceptance

**Accepted (2026-09-26).**

**Decision**: `evaluate-payment-auth-eligibility` evaluates the payment authorization only and
answers `eligible` or `pending`. Buyer acceptance is Lifecycle's begin-fulfillment guard.
`begin-fulfillment` surfaces it as `withheld`, and the eligibility fork's acceptance `listen`
wakes the next round.

**Rationale**: the operation read "whether acceptance is required" from Lifecycle's order read,
which exposes no guard state. Lifecycle resolves the requirement live at each guard and never
snapshots it (OW2-49). Precedent: Lifecycle `05 §3.6`, whose requirement is resolved live and
never snapshotted, and `08 §4.2`, which says the read "MUST NOT expose guard state". Also
`begin-fulfillment`'s existing `withheld` mapping (`04 §3.6`).

**Propagated**: `design/04-fulfillment-plan.md` §1.2, §3.2, §3.3, §3.6.

### D-114 (H) One rule for a step call that fails: it faults the invocation unless the failure is a subject an operator acts on

**Accepted (2026-09-26).** *(amends D-84 and D-105)*

**Decision**: a spent retry budget, a spent timeout and a permanent refusal of a step call fault
the invocation. The instance liveness pass raises the `invocation-dead` task within one pass
interval. The task's platform re-drive resumes at the faulted call under its still-open key,
because `retryable-failure` leaves the key `open`. The definition catches a failure only where it
is the failure of a subject an operator can act on:

- a wave call's exhaustion and a wave's `failed[]` (line tasks);
- `compensate-order`'s exhaustion (`awaitCompensationResolution`, then the next pass);
- `reflect-verdict`'s refusal (`approval-reflection-refused`, an order-scope task);
- the start path's `prior-instance-active` (the `supersession` retry).

Every other call faults: listen-arm admissions, `apply-hold`, `apply-resume`, `authorize-cancel`,
slice 04's evaluations and `begin-fulfillment`, `report-spawn-signal`, the fence,
`report-outcome`, `create-manual-task` and `terminate-instance`. `trigger-applicability-unverified`
stays a retryable refusal of `admit-trigger` but is no longer a manual-task reason, and
`authorize-cancel`'s exhaustion catch and the definition's `authorityTask` are removed.

**Rationale**: four slices promised that an exhausted admission, hold, resume, fence or report
would reach `create-manual-task`, while the canonical definition carried a bare retry, and
`10 §4.6` endorsed letting exhaustion propagate (OW2-13, 06 §4.7 item 5). Routing each of them
into the failure stage would need a return point into another stage's checkpoint. The one such
route that existed, a spent `authorize-cancel` taken from the hold's resume wait, stranded the
order once the failure stage consumed the resume (OW2-19). A fault of these calls is a failure
of Orders or of a dependency the order cannot proceed without, not of a subject, so no stage
remedy fits it. The permanent refusals of the fence and the report are defects no retry of the
same step resolves. **Precedent**: the platform's own status machine, where a failed invocation
with no `on_failure` handler moves to `dead_lettered`
([serverless-runtime `DESIGN.md:458`](../../../serverless-runtime/docs/DESIGN.md#invocation-status-state-machine)).
The liveness pass that observes it is D-105. `DESIGN.md` §1.2 already stated this split for
`report-outcome` and `create-manual-task`.

**Propagated**: `design/10-process-definition.md` §2.2, §3.6 (c), (d), §4.6;
`design/01-foundation.md` §3.3, §4.5, §4.8, §4.13; `design/02-triggers-and-start.md` §4.2, §4.7;
`design/04-fulfillment-plan.md` §4.8; `design/06-saga-and-compensation.md` §4.7;
`design/07-manual-tasks.md` §3.3, §3.7, §4.1, §4.4, §4.8; `design/08-hold-and-cancel.md` §1.1,
§1.2, §2.1, §3.2, §3.3, §3.6, §4.7; `design/09-read-and-authz.md` §4.4, §4.6; `DESIGN.md` §1.2, §4.4.

### D-115 (M) An authority withdrawn before the fence refuses the cancel and raises no task

**Accepted (2026-09-26).** *(amends D-84)*

**Decision**: at `pre-fence`, `authorize-cancel` marks a withdrawn authority's request `refused`
with `authority-withdrawn`, audits it, creates no manual task and answers `authorized = false`. The
definition returns to where the cancel was taken. A Seller Operator who still means the cancel
submits a new one under a fresh authorization. The `authority-withdrawn` task is raised only at
`pre-compensation`, inside `compensate-order`, where the claimed fence waits on it in
`awaitCompensationResolution`. `authorize-cancel` returns no `taskRef`.

**Rationale**: the deny created a task and the definition went `back` to a stage fork with no
resolution arm and no SLA branch. That broke "every task has a waiter" (`07 §4.8` item 3): the
operator's action had no consumer and the SLA never breached (OW2-12). Nothing waits on the task
at `pre-fence`, because the order continues, and its only meaningful resolution was always a new
cancel. **Precedent**: Lifecycle's late authorization conflict is a refusal with an audit entry,
"with no automatic reauthorization"; "a subsequent attempt must … obtain fresh authorization"
([Lifecycle `08-read-and-authz.md:1286-1293`](../../orders-lifecycle/docs/design/08-read-and-authz.md)).

**Propagated**: `design/08-hold-and-cancel.md` §1.2, §3.2, §3.3, §3.6, §4.3;
`design/09-read-and-authz.md` §4.4; `design/07-manual-tasks.md` §3.2, §3.3, §4.4;
`design/10-process-definition.md` §3.6 (d); `ADR/0010`; `UPSTREAM_REQS.md` §2.8.

### D-116 (M) Each task subject carries its reason and cause, and the task key ends in the failing call's tail

**Accepted (2026-09-26).**

**Decision**: every branch that enters the failure stage sets the whole `create-manual-task` body:

- `failureScope`;
- `failureSubjects`, one `{ subjectRef, reason, cause }` per subject, from the `failure_reason`
  and `failure_cause` enums of `07 §3.7`;
- `sourceStep`;
- `sourceAttempt`, the failing call's key tail `{round}[:{attempt}]`, exported with that call's
  answer: the event reference for `apply-resume` and the `attempt` alone for
  `construct-and-freeze-plan`.

The call-level `failureCause` is removed. `owf_manual_task.source_attempt` becomes text.
`apply-resume`'s `failedTaskRefs[]` carries `lineRef` and `reason`, the shape of a wave's
`failed[]`.

**Rationale**: no branch set `failureCause` or `sourceAttempt`, and the subjects came in three
shapes. Two failures from one step therefore presented one key with different bodies, which is
`idempotency-key-conflict`, and the reopen branch was unreachable (OW2-14). A sweep answer mixes
terminal failures and unresolved intents in one call, so the cause belongs to the subject, as it
does on the `owf_manual_task` row. **No precedent exists** in the sibling gears for a
definition-built task body. The rule reuses this gear's round tail (D-102) and the table's own
per-row reason and cause.

**Propagated**: `design/07-manual-tasks.md` §3.2, §3.3, §3.6, §3.7;
`design/08-hold-and-cancel.md` §3.3, §3.6; `design/10-process-definition.md` §3.6 (a), (b), (c),
(e); `design/06-saga-and-compensation.md` §4.8.

**Amended by D-150 (2026-09-26)**: the apply-resume poll's tail is `poll:{suspensionRef}:{round}`, not `poll:{round}`, because its round family restarts for every suspension; with the shorter tail two holds' deferred failures shared one key.

### D-117 (M) The remediation hold lasts until the order's last open task resolves

**Accepted (2026-09-26).** *(amends D-99)*

**Decision**: `resolve-manual-task` and `verify-override` return `openTaskCount`. A line `retry`,
or a verified override, while `openTaskCount > 0` returns the definition to `awaitResolution`. The
resolution that leaves no open task takes every retried line back to the barrier together.
`reconcile-intent` lists a line in `failed[]` or `unresolved[]` only in the round that records the
failure.

**Rationale**: the first retry left the failure stage, and the barrier fork had no resolution arm.
The sibling tasks lost their waiter, and their SLA ticks stopped (OW2-15). Staying in the stage is
the remediation hold that `07 §4.3` and `04 §4.4` already define: dispatch is stopped by the
definition's position in `awaitResolution`. Listing a failure once removes the ordering hazard in
`onSweep`. **Precedent**: D-99 and `04 §4.4` in this gear. No sibling gear runs a multi-task
remediation.

**Propagated**: `design/07-manual-tasks.md` §3.3, §3.6, §4.3, §4.8;
`design/05-provisioning-intents.md` §4.5; `design/10-process-definition.md` §3.6 (b), (c).

**Amended by D-146 (2026-09-26)**: the last resolution that takes the held lines back to the barrier may also be a Seller Operator's `closed`, when it leaves no open task on a line entry; the failure stage never waits with no open task.

### D-118 (M) `escalate-gate` and `arm-park-escalation` have algorithms; every answer returns the next round, and a fire during an outage pauses

**Accepted (2026-09-26).** *(amends D-87)*

**Decision**: slice 03 gives CDSL algorithms for `escalate-gate` `fire`, `escalate-gate` `probe`
and `arm-park-escalation`:

- **Rounds**: every answer, `due: false`, `available` and `outage` included, settles its key and
  returns the next round. The `escalate-gate` family is the position and mode, so each position
  starts both rounds at 0.
- **`fire`**: escalates the open, unpaused gates whose window has elapsed. It stamps
  `escalated_at`, re-arms the window, enqueues `OrderApprovalEscalated` and delivers the
  escalation command under the step key.
- **`fire` on an open breaker**: sends no command. It pauses the due gates for `approval-outage`
  with no remainder and answers `due: false`, and is no longer refused `circuit-breaker-open`.
- **`probe`**: records the outage pause or its end.
- **Outage threshold**: measured from a new `owf_approval_gate.outage_since` column.
- **`arm-park-escalation`**: answers `due` against `escalation_due_at`, and `false` once the park
  has escalated. It records nothing.

**Rationale**: the sequence had a diagram but no steps. It never said when a round advances or
what a fire sends. A probe that replayed its first `available` would never detect an outage
(OW2-20). The diagram still said "key open" for `due: false`, contradicting D-102. A fire refused
`circuit-breaker-open` would, under D-114, fault the invocation exactly when the approval service
is down. **Precedent**: Lifecycle's expiry scheduler answers `expiry-not-due` against the stored
effective TTL and records the outcome
([Lifecycle `07-hold-and-expiry.md:358`](../../orders-lifecycle/docs/design/07-hold-and-expiry.md)).
The round rule is D-102.

**Propagated**: `design/03-approval-execution.md` §3.2, §3.3, §3.6, §3.7.

### D-119 (H) A retried or never-written line is re-dispatched from Orders' record: a per-line wave attempt on the task row, one dispatch attempt per wave, and the barrier's undispatched set

**Accepted (2026-09-26).** *(amends D-21, D-95, D-102; ADR-0006 as amended)*

**Decision**:

- **Per-line attempts on the task row.** `owf_fulfillment_task` gains `wave1_attempt` and
  `wave2_attempt` (from 1). The intent key of either wave appends the line's attempt once it is
  above 1. `rebuild-wave1` increments `wave1_attempt` for a lapsed draft. An operator's line
  `retry` increments the attempt of the line's wave in the transition out of `failed`, and only
  when the intent is recorded `failed` (a synchronous refusal, a confirmed failure, or
  `never-dispatched`).
- **No new key for a live intent.** A retried line whose intent is `submitted` or `unresolved`
  keeps its key. A `submitted` row stays the sweep's. An `unresolved` row is set back to
  `submitted` with `next_sweep_at` now for one on-demand read. A line with no row is sent under
  its unchanged key.
- **One dispatch attempt per wave.** For a line task, `retry-step` mints the `attempt` of the
  dispatch family of the line's wave, whatever step raised the task. `resolve-manual-task` answers
  `retryWave`, and the definition files the attempt as `wave1AttemptKey` or `wave2AttemptKey`.
  Both dispatch step keys end `{dispatchRound}[:{attempt}]`. A wave call passes only its own
  attempt and spends it once the call answers. Wave calls no longer pass the generic `attemptKey`.
- **The barrier's undispatched set.** `evaluate-activation-eligibility` answers
  `undispatchedLineRefs`: every `pending` line with no wave-1 intent row under its current
  attempt, or only a never-sent row. `onEvaluate` sends them to `dispatch-wave1-create` before
  acting on anything else. A retried wave-2 line is `draft_created` again and returns through
  `eligibleLineRefs`.
- **Lookup-settled dispatch output.** A dispatch key that `settle-from-lookup` settles `success`
  stores the operation's full output built from Orders' record. That output lists every row
  written under the key by its recorded state, an empty `deferred[]`, and the next round. A line
  the crashed attempt never wrote is recovered through the undispatched set (wave 1) or through
  `eligibleLineRefs` (wave 2).

**Rationale**: a line retry moved the line back to `pending` or `draft_created`, but nothing sent
it again. `toBarrier` only re-entered the barrier loop. Wave 1 is reached from there only through
`reconcile-intent`'s `redispatch[]`, which lists never-sent rows. `evaluate-activation-eligibility`
never releases while a line is `pending`. A re-dispatch would have skipped the failed row anyway,
because `wave_attempt` was minted only by a rebuild. Subscriptions returns the original outcome
for a seen key before any guard runs
([Subscriptions `01-foundation-lifecycle.md:314`](../../subscriptions/docs/design/01-foundation-lifecycle.md#42-transitionrequest-envelope-idempotency-ordering-normative)),
so a re-send under the refused key would get the refusal back (second review OW2-21). Keeping the
attempts on the task rows lets several retries resolved together under the remediation hold keep
their own attempts (D-117); the definition holds only the latest attempt per wave. The call
attempt is still needed: the round of an exhausted call is still `open`, and its fingerprint named
other lines, so re-presenting that key with a different `lineRefs[]` is `idempotency-key-conflict`
(`01 §4.3`). Wave calls passed the generic `attemptKey`, so a leftover attempt minted for another
family could exceed the dispatch family's counter and be refused `idempotency-key-mismatch`. A
lookup that settled a dispatch key left the stored lists undefined, and a line with no row was in
no list and had no route (OW2-32). The undispatched set covers it from the record, so no list of
named lines has to be stored. **Precedent**: `rebuild-wave1`'s own minting rule
(`05 §3.6` *Wave-1 Rebuild*) and D-102 rule 3. The caller-side duplicate protocol of `01 §4.5`
forbids a second submit under a new key while an intent may be live. Subscriptions' replay rule is
cited above. No sibling gear re-dispatches a multi-line submit after an operator's retry.

**Propagated**: `design/04-fulfillment-plan.md` §3.3, §3.6, §3.7, §4.8;
`design/05-provisioning-intents.md` §1.1, §1.2, §2.2, §3.2, §3.3, §3.6, §3.7, §4.2, §4.4, §4.5,
§4.6; `design/07-manual-tasks.md` §3.3, §3.6; `design/01-foundation.md` §3.3, §4.3;
`design/06-saga-and-compensation.md` §2.1; `design/10-process-definition.md` §3.6 (b), (c);
`DESIGN.md` §1.2; ADR-0006 as amended.

**Amended by D-152, D-153 and D-154 (2026-09-26)**: the quarantine skips an attempt no call
presented, so the attempts several line retries mint into the wave's one family are never counted
(D-152); the re-arm of an `unresolved` row is slice 05's port `rearm_unresolved`, not a write by
slice 07 (D-153); and `create-manual-task` accepts the subjects this entry's retry branch serves —
an `unresolved` intent and every line an exhausted call named — rather than only `failed` tasks
(D-154).

### D-120 (M) The read after a dispatch 409 is routed, and the call is re-issued only after a wait

**Accepted (2026-09-26).**

**Decision**: `wave1Reread` and `wave2Reread` export `reconcile-intent`'s `failed[]`,
`unresolved[]`, `redispatch[]` and `nextSweepRound` as the poll arm does. Failures and unresolved
intents go to fragment (c). Otherwise wave 1 waits the fixed `PT30S` `waitReread1` and re-issues
the same key, and wave 2 waits in the barrier loop. A never-sent row keeps `not_found_at`, so the
same-key re-run or the next evaluation sends it, and `redispatch[]` stays a routing hint.

**Rationale**: both reads discarded their output and passed no round back. The next read under
the same round was an absorbed replay, and failures the read found never reached fragment (c).
Wave 1 looped `wave1 → 409 → wave1Reread → wave1` with no wait against a key held by a live lease
(second review OW2-23). **Precedent**: the poll arm's export and `onSweep` in the same fragment,
and D-102's round rule.

**Propagated**: `design/10-process-definition.md` §3.6 (*Fixed waits and re-check loops*, (b));
`design/05-provisioning-intents.md` §3.3, §4.5.

**Amended by D-142 (2026-09-26)**: the `PT30S` wait before the re-issue is the tick branch of
`awaitReread1`, which also carries the hold, resume, lifecycle and cancel arms; an arm re-enters
at `wave1` under the unchanged key.

**Amended (2026-09-26)**: `05 §4.5` item 6 now carves out this read: after a dispatch 409, `redispatch[]` is covered by wave 1's same-key re-issue after `waitReread1` and by the barrier's next evaluation for wave 2, so the reread routes only `failed[]` and `unresolved[]` (R-16).

### D-121 (H) The lifetime ceiling parks only a running, unparked instance, and each ceiling is its own round

**Accepted (2026-09-26).**

**Decision**: `afterLifetime` decides from the definition's own state. A ceiling that fires while
`$context.unwind` is set — on every entry to the unwind, never cleared, so it covers a cancel or a
task taken from inside the unwind — re-arms and parks nothing (no `compensating → parked` edge).
A ceiling that fires while `stageLoop` is `parkLoop` — the verdict park, or a hold, resume,
lifecycle or cancel stage taken from it — re-arms and parks nothing (no `parked → parked` edge);
the verdict park keeps its own escalation and its three routes out. `enterParkLoop` is recorded
before `park`, whose re-entry re-issues the call under its unchanged key, and `leftPark` after
`unpark`. Every other ceiling is a new round: `raise-overdue-escalation` `lifetime-ceiling`
carries `round` (the definition's `ceilingRound`, 0 first) and returns `nextRound`; its escalation
row's subject is `ceiling:{round}`; `park` and `unpark` take that subject and are keyed by it, with
no `{attempt}`, because the subject is unique per park and no manual-task retry re-enters them.

**Rationale**: the exemption read only `nextStage == unwind`, so a ceiling during a cancel or task
taken from an unwind, or during the verdict park, called a `park` the phase table refuses; the
refusal exhausted `*transient` and faulted the invocation, and a successful park inside the
verdict park would later have been undone by the ceiling's `unpark` while the verdict park stayed
open. A second ceiling after an unpark presented the first ceiling's escalation, park and unpark
keys, so nothing was raised or parked and the process waited on a closed task (second review
OW2-50, OW2-51). **Precedent**: D-102's round rule (the overdue monitor's `raise-overdue-escalation`
round is the same shape); `01 §3.7`'s transition table as the authority for which park is legal.

**Propagated**: `design/10-process-definition.md` §3.6 (a), (d); `design/01-foundation.md` §3.3,
§3.7; `design/07-manual-tasks.md` §3.2, §3.3, §3.6, §3.7, §4.8; `design/03-approval-execution.md`
§2.1; `design/08-hold-and-cancel.md` §4.5; D-53, D-82.

**Amended by D-147 and D-149 (2026-09-26)**: a call that consumes an arm's payload is recorded as the stage's checkpoint before it runs, so the re-entry after a ceiling re-issues it (D-147); each ceiling's task is its own row (`scope_ref = ceiling:{round}`), which the creation port's uniqueness had denied (D-149).

**Amended by D-161 (2026-09-26)**: `ceilingEntry` also records the interrupted stage's
`returnStage` as `ceilingReturnBack`, and `backToProcess` restores it, so a lifecycle, hold, resume
or cancel stage the ceiling interrupted returns where it would have, not to the ceiling stage.

**Amended by D-163 (2026-09-28)**: a ceiling that fires while a lifecycle or cancel stage taken
from the ceiling wait is running re-arms without parking and without running `ceilingEntry`.

**Amended by D-164 (2026-09-28)**: `ceilingEntry` also saves the interrupted stage's payload
(`lifecycleEventId`, `triggerKind`, `cancelRequestRef`, `requestRef`, `taskRef`) and
`backToProcess` restores it with `preAdmitted: false`; the ceiling wait's resolution arm writes
its own `ceilingRequestRef`.

### D-122 (H) The ceiling wait consumes only its own task, and no unrecorded signal unparks

**Accepted (2026-09-26).**

**Decision**: the ceiling wait's task-resolution `listen` correlates on `ceilingTaskRef` as well
as the order, and `resolveCeilingTask` resolves that task. While a `lifetime-ceiling-reached`
task is open, every other task of the instance offers `escalate` only (`invocation-dead` exempt),
so no other resolution is signalled into a wait that would consume it. `unpark` of a
`ceiling:{round}` subject refuses `not-found` unless that ceiling's task is resolved `retry` by
`resolve-manual-task`, which applies only a request row the task route wrote under the
operator's authorization. The canonical definition has no `unpark-requested` arm; the signal type
stays reserved, and no version may `listen` for it until Q-13 gives it an origin route and a
request row.

**Rationale**: the ceiling wait reused the generic resolution arm, correlated only on the order,
so an operator's retry of a line task was applied by `resolve-manual-task` and then taken as the
ceiling's retry, which unparked the order and lost the line's re-dispatch; an override landed on
`wait` and was never verified (OW2-52). The `unpark-requested` arm unparked on a signal no Orders
route records, while `:plugin-control` is authorized platform-side, so anyone the platform lets
act on the invocation could release a ceiling park with no actor on the audit trail (OW2-60).
**Precedent**: `run-cancellation-fence`'s recorded cause (D-106, `06 §3.6` `inst-fence-cause`);
the task route's request row and `action-not-offered` refusal (`07 §3.3`, D-100).

**Propagated**: `design/10-process-definition.md` §2.2, §3.1, §3.3, §3.6 (d);
`design/01-foundation.md` §3.3; `design/07-manual-tasks.md` §4.4, §4.8; `DESIGN.md` §3.1; Q-13.

**Amended (2026-09-26)**: the ceiling wait also carries an SLA tick scoped to the ceiling task,
and the ceiling task offers no task `cancel`; the Seller Operator's way out besides `retry` is the
order cancel, which the wait's cancel arm consumes (D-129).

**Amended by D-149 (2026-09-26)**: the fence closes the ceiling task in step 1, so its exclusivity lasts only while the instance is parked at that ceiling and never freezes the unwind's compensation tasks.

### D-123 (M) The escalation re-check rides the 30-second probe tick

**Accepted (2026-09-26).**

**Decision**: the gate loop carries one tick, the `PT30S` `waitProbe`; each tick calls
`escalate-gate` `mode: fire` and then `mode: probe`. There is no separate escalation `wait`. The
worst-case escalation lateness is one tick plus one call: 30 s plus the 3-minute `step` timeout,
inside the PRD's ± 5 min.

**Rationale**: the canonical `PT5M` escalation tick admitted lateness of one tick plus one call,
beyond ± 5 min once the call's retries are counted (OW2-91). Shortening it was not enough: the
`PT5M` escalation `wait` and the `PT30S` probe `wait` were branches of one competing fork
re-armed on every pass, so the probe won every race and the escalation `wait` never fired.
**Precedent**: the park loop, whose one `PT5M` tick carries both the verdict retry and the park
re-check (`03 §4.5` item 6); none in the platform, whose timers are the plugin's.

**Propagated**: `design/10-process-definition.md` §1.2, §3.6 (a), (e); `design/03-approval-execution.md`
§1.2, §3.6, §4.5; `design/08-hold-and-cancel.md` §1.2, §3.3, §3.6, §4.7; `DESIGN.md` §1.2, §4.1;
D-70, D-80; `ADR/0011`.

**Amended by D-148 (2026-09-26)**: "one tick plus one call" omitted the probe and every other call between two fires. The gate loop's calls carry a 60-second `gate` timeout, a pending decision and every re-entry fire first, and the worst case is 30 s + 4 × 60 s = 4 min 30 s.

### D-124 (M) An event delivered between listens needs platform retention; no poll covers it

**Accepted (2026-09-26).**

**Decision**: the canonical definition runs only on a platform that retains a broker event
correlated to an invocation while no matching `listen` is armed — during a step call, or between
a competing fork's teardown and re-arm — and delivers it, in order, to the next matching
`listen`. That is a new upstream ask,
`cpt-cf-bss-orders-workflow-upreq-serverless-runtime-event-retention-between-listens`. Q-11 (iv)'s
fallback "covered by the poll arms and the re-entry of every stage loop" is withdrawn.

**Rationale**: no poll re-reads a hold, a resume, a decision or an amendment; the only polls are
eligibility and the barrier. A resume delivered while the hold stage runs `admitHold` and
`applyHold` left the instance in `awaitResume` until the ceiling, and a decision delivered during
a probe was lost, so the gate escalated on its window (OW2-53). The signals ask already requires
the plugin to hold a signal until an arm consumes it; broker events had no such rule.
**Precedent**: the signal-retention clause of `…-upreq-serverless-runtime-signals`; none in the
DSL, which states no buffering between `listen` tasks (dsl-reference.md *Listen*).

**Propagated**: `design/10-process-definition.md` §4.4, §4.5; `design/08-hold-and-cancel.md`
§4.7; `DESIGN.md` §1.2; `UPSTREAM_REQS.md` §2.9, §3; Q-11.

**Amended (2026-09-26)**: "no poll covers it" now has one exception, a stopgap until the ask
lands: the approval stage's resume wait reads through `apply-resume` whether Lifecycle still holds
the order, so a resume lost there is applied at most one `PT15M` poll late (D-130). A lost resume
outside the approval stage, a lost hold, decision or amendment stay uncovered.

**Amended (2026-09-26, second)**: a lost resume outside the approval stage is now covered by the
same poll in every other wait that holds a recorded suspension (D-133). A lost hold, decision or
amendment stays uncovered.

### D-125 (M) A failure or floor trip the sweep worker records reaches the definition through its next reconcile round

**Accepted (2026-09-26).**

**Decision**: a forward intent recorded `failed` or `unresolved` carries
`owf_provisioning_intent.handed_off_at` null until the definition is told. A definition-called
`reconcile-intent` round lists, besides what it records itself, every such intent of the
instance with `handed_off_at` null — the worker's in-process failures and floor trips, and a
re-armed `unresolved` row the worker read to the floor again — and stamps `handed_off_at` in its
settlement. The dispatch operations stamp the synchronous refusals they list, `apply-resume`'s
deferral port stamps the deferred failures it hands off, and an operator's retry of an
`unresolved` row clears it. The definition's existing `sweepFailure` route creates the task.

**Rationale**: the worker runs `reconcile-intent` in-process with no caller, so a floor trip or a
failure it recorded never reached `unresolved[]` or `failed[]`, and the barrier waited on a line
no task named (open item from the B5 fix of OW2-32). The dead-invocation pattern of D-105 — the
worker raising the task in-process — was rejected here: the invocation is alive and waiting in
the barrier loop, and a task raised behind its back would have no waiter
(`07 §4.8` item 3) and would not count in the remediation hold's `openTaskCount`. The poll arm
already reaches the definition every `PT30S`, so the hand-off rides it and the definition keeps
creating every forward task.
**Precedent**: the poll arm and `sweepFailure` (`10 §3.6` (b), D-120); D-105 considered and
not followed.

**Propagated**: `design/05-provisioning-intents.md` §3.2, §3.3, §3.6, §3.7, §4.5;
`design/07-manual-tasks.md` §3.6; `design/08-hold-and-cancel.md` §3.2, §3.6;
`design/10-process-definition.md` §3.6 (b).

**Amended by D-153 (2026-09-26)**: the operator's re-arm of an `unresolved` row, which clears
`handed_off_at`, is made through slice 05's re-arm port, not by slice 07 writing the row.

### D-126 (H) The validation rules are stated over values the definition holds and a routing graph the check can enumerate

**Accepted (2026-09-26).**

**Decision**: the eight rules of `design/10` §2.2 are restated so a validator can be written
without inventing a rule. **Rule 1** walks a routing graph: its states are a task position plus
the routing members (`nextStage`, `stageLoop`, the return members, `heldStage`, `heldLoop`,
`arm`), which may be written only as string literals, copies of one another or an
`if … then … else` over them, so they range over a finite set of literals; a `switch` over any
other member is taken both ways; every reachable walk to `end` must satisfy the §4.1 fence, and a
walk the check cannot decide is refused. The §4.1 Waves row carries the ADR-0004 conjunction —
the barrier's `waitExpected` loop and an `evaluate-activation-eligibility` answer `released`
before `dispatch-wave2-activate` — and `start-instance` is the first operation after
`admit-trigger` (`role: start`). **Rule 2** requires every endpoint to be
`$context.stepsBase + "/<operation>"` with `stepsBase` written only by `input.from` and compared
with the environment's step-surface base. **Rule 3** exempts the `OrderAmended` filter from the
`orderVersion` correlation. **Rule 4** checks the operation's `deadline_ms` < the task timeout and
every task timeout and `wait` < the literal `P90D` ceiling, and computes no cumulative backoff;
the overdue window and the SLA classes are per-order policy values, bounded where the policy is
validated (`design/07` §4.8 item 8). **Rule 6**, `design/10` §4.6 and ADR-0012 rule 6 now say the
same thing: a `catch` around a protected operation only retries, so exhaustion faults the
invocation, or routes to a named failure route of §4.6. That a retry-only `catch` re-raises the
last error once its limit is spent is added to Q-11 as (vi). A `catch` that must see a spent
timeout names 408 and carries no `errors.with.type` filter, because the DSL's timeout error has
its own type; `compensate-order`'s catch loses its communication-type filter accordingly.

**Rationale**: rule 6, §4.6 and ADR-0012 named three different catch rules, and rule 6 as written
refused every retry-only catch in the canonical file (OW2-1). Rule 4 compared the 25 h admission
timeout with a `PT1H` tick, or, in the ADR's wording, with a stored per-order window no definition
holds, and summed an exponential backoff the DSL gives no multiplier for (OW2-2). Rule 1 promised
a whole-path check over routing that runs on jq values in `$context`, with no method stated
(OW2-4); the conjunction lived only in the run-time guard (OW2-82). Endpoints were expressions over
a member any `set` could overwrite (OW2-62). The compensate catch filtered on the communication
type, so its 408 case never matched (OW2-3). **Precedent**: the wave catches, which carry no type
filter (`design/10` §3.6 (b)); the start path's exact-type lookup (D-107), reused for the
lifecycle arm's `triggerKind` (OW2-6); none in the platform for a routing check, whose
registration-validation hook states no rule of this kind (serverless-runtime `DESIGN.md:762`).

**Propagated**: `design/10-process-definition.md` §1.2, §2.2, §3.2, §3.6, §4.1, §4.5, §4.6;
`design/07-manual-tasks.md` §4.8; `design/01-foundation.md` §4.2 (amended note); `ADR/0012`;
`ADR/0011` (amended note); D-67; Q-11.

**Amended (2026-09-26)**: rule 1 also tracks four pinned members (`verdict`, `reflected`,
`policy`, `forceTask`), and §4.1 requires the `p1` composables on their paths; rule 8 requires the
shared arms (D-135). Rule 2 no longer allows a `call` to a Function (D-136). The windows rule 4
leaves out are seller-policy values pinned on the record, bounded at the policy write (D-134).

**Amended by D-142 and D-143 (2026-09-26)**: rule 1 tracks a fifth pinned member, `beginResult`,
and the §4.1 Waves row requires `beginResult = in-fulfillment` on every walk to a wave dispatch
(D-143). Rule 8 forbids a plain `wait` inside a stage (D-142).

**Amended by D-144 (2026-09-26)**: rule 1 tracks ten pinned members, and the canonical definition routes every `switch` towards a protected operation on tracked members only; the ADR-0004 conjunction is a walk that passes the pinned `released = true`.

**Amended (2026-09-26)**: the nesting invariant is qualified where it was still stated whole.
`design/01-foundation.md` §4.2 now orders each operation's deadline below the timeout of the task
that calls it and below the ceiling, and the longest fulfillment-stage timeout below the overdue
window below the ceiling; the 25 h admission timeout is outside the overdue window's span. ADR-0011's
*Bounds nest* bullet carries a dated block that drops the retry-budget term this decision removed
(re-review R-23).

### D-127 (M) The Workflow callable declares every required trait: async-only, its limits, and no invocation retry

**Accepted (2026-09-26).**

**Decision**: the callable's `traits` declare the four members the Workflow base type requires.
`invocation: { supported: [async], default: async }` is the async-only declaration.
`limits: { timeout_seconds: 15552000, max_concurrent: 5000 }`: 180 days covers the `P90D`
ceiling, the ceiling park and one fresh ceiling after an operator's unpark, and 5,000 is the sized
in-flight process count of `DESIGN.md` §4.1. `retry: { max_attempts: 0 }`: no invocation-level
retry, because a faulted order process is re-driven only by an operator through the
`invocation-dead` task. `workflow` keeps its three members. An invocation that outlives the
timeout is ended by the platform guardrail and raised as `invocation-dead` (D-105). The readiness
gate checks that the tenant quotas `max_execution_duration_seconds`,
`max_concurrent_executions` and `max_execution_history_mb` admit these values, and asks whether
`timeout_seconds` counts suspended time and whether `max_concurrent` counts suspended
invocations. The async-only ask is narrowed to confirming that `traits.invocation` is what
serverless-runtime `DESIGN.md:653` means.

**Rationale**: only `workflow_traits` was declared, so the schema defaults applied — a 30-second
timeout, a cap of 100 concurrent invocations and three automatic whole-invocation retries — and
the async-only ask waited on a field the base type already has (OW2-68, OW2-70). **Precedent**:
the platform's own schema (serverless-runtime `DESIGN_GTS_SCHEMAS.md` lines 142–174, 250–292,
1136–1230, 1799–1855) and its duration guardrail (serverless-runtime PRD BR-028).

**Propagated**: `design/10-process-definition.md` §3.1; `UPSTREAM_REQS.md` §2.9; `ADR/0011`.

**Amended by D-157 (2026-09-26)**: the limits are now `timeout_seconds: 23328000` (three `P90D`
ceilings, because the fresh ceiling fires by 25 h + 180 d) and `max_concurrent: 50000`, sized from
the residency of long waits rather than the ~360 s mean; the answers on what each limit counts gate
readiness.

### D-128 (M) Bounding one invocation's engine history is asked of the plugin; the ticks are not stretched to fit

**Accepted (2026-09-26).**

**Decision**: the re-check ticks stay as the NFRs set them — the gate loop and the barrier poll at
`PT30S`, the SLA, park and eligibility ticks at `PT5M`, the resume poll at `PT15M`, the overdue
monitor at `PT1H` — and `design/10` §3.6 states their growth per waiting instance and day. The
plugin is asked to bound one invocation's history over a life of 90 days and more, by truncating
it inside its DSL interpreter while keeping the invocation, its `$context`, its position and its
pending events, or by stating a per-invocation budget and how `max_execution_history_mb` applies.
The ask is `cpt-cf-bss-orders-workflow-upreq-serverless-runtime-history-growth`, `p1` on the
platform path.

**Rationale**: a `PT30S` tick adds 2,880 fork-and-call iterations a day, and a 72-hour approval
or a long manual-task wait is ordinary; the DSL has no construct that truncates history, and
neither document set said how long an invocation's history may grow (OW2-69). Lengthening the
ticks would break the ± 5 min escalation accuracy (D-123) and the fulfillment SLA without knowing
the budget they would have to fit. **Precedent**: the tenant history quota of the platform's own
policy schema (serverless-runtime `DESIGN_GTS_SCHEMAS.md` line 1840); none for a per-invocation
bound.

**Propagated**: `design/10-process-definition.md` §3.1, §3.6; `UPSTREAM_REQS.md` §2.9, §3;
`DESIGN.md` §4.1.

### D-129 (M) The ceiling task has an SLA tick, and a Seller Operator ends a ceiling park with the order cancel

**Accepted (2026-09-26).**

**Decision**: the ceiling wait (`awaitOperatorAfterPark`) carries the `PT5M` SLA branch every
task waiter carries. It calls `resolve-manual-task` `sla-check` scoped to `ceilingTaskRef`, under
that task's own `slaRound` family (`…:resolve-manual-task:sla:{taskRef}:{slaRound}`, from round
0), so it escalates the order-scope ceiling task once its 24 h deadline has passed and answers
`escalated` or `none`, never `exhausted`; other open tasks are re-checked by the waiter of their
own stage after the unpark. The `lifetime-ceiling-reached` task offers no task `cancel`: the
route refuses `action-not-offered`, and the Seller Operator ends the park with the order cancel of
`design/09` §3.3, which the ceiling wait's cancel arm consumes and the fence unwinds
(`parked → compensating`), or, before fulfillment has begun, with Lifecycle's own cancel (D-109).

**Rationale**: with no SLA tick the ceiling task's 24 h SLA never breached, and an unscoped
`sla-check` could answer `exhausted` for another open forward task, which the ceiling stage has
no route for. A task cancel resolved the ceiling task `closed` and left the order parked with
nothing to wait on (open items of the B6 fix of OW2-50…57). **Precedent**: the `invocation-dead`
task, whose `cancel` is the order cancel (`design/07` §4.4, D-105); the SLA waiter rule
(`design/07` §4.8 item 3); an order-scope breach escalates without resolving (`design/07` §4.2).

**Propagated**: `design/10-process-definition.md` §3.6 (d); `design/07-manual-tasks.md` §3.3,
§3.6, §4.4, §4.8; `design/01-foundation.md` §3.3; D-122.

**Amended by D-146 and D-149 (2026-09-26)**: the `approval-reflection-refused` task, like the ceiling task, offers no task `cancel` (D-146); the ceiling task's SLA family starts at round 0 on a row of its own (D-149).

### D-130 (M) Until events are retained between listens, the resume wait polls whether Lifecycle still holds the order

**Accepted (2026-09-26).**

**Decision**: the resume wait of the approval stage carries a `PT15M` `waitResumePoll` branch
that calls `apply-resume` with `trigger: poll`, keyed
`{tenant}:{correlationId}:apply-resume:poll:{suspensionRef}:{round}`. The operation reads the
order through the Lifecycle PDP-authorized order read; while Lifecycle holds it the call records
nothing and answers `still-held` with the next round; once Lifecycle no longer holds it at the
instance's version it applies the resume exactly as for an `OrderResumed`, closing the suspension
with `closed_reason = resumed-by-read`, and answers `resumed-by-read`. An `OrderResumed` that
arrives later, with no open suspension, is attached to that suspension as an absorbed duplicate,
not recorded as a resume ahead of the next hold. The poll follows no `admit-trigger`, because it
consumes no event; the read is its guard.

**Rationale**: a resume delivered while the hold stage ran `admit-trigger` and `apply-hold` was
lost with no fallback, so the order waited in the resume wait until an operator cancelled it or
the lifetime ceiling parked it (D-124; open item of the B6 fix). The stopgap covers that case
only: a lost resume outside the approval stage, which has no resume wait, still leaves the hold
predicate set until the event-retention ask lands. **Precedent**: the poll arms of the barrier and
eligibility waits (`design/10` §3.6 (b)); the Lifecycle order read inside `reflect-verdict`,
`begin-fulfillment` and `report-outcome` for a `not-admissible` answer (`design/01` §3.3
*Rounds and attempts* rule 4); Lifecycle's own "re-read the order, wait for the resume"
(Lifecycle `06 §4.3`).

**Propagated**: `design/10-process-definition.md` §3.6 (e), §4.1, §4.4, §4.5;
`design/08-hold-and-cancel.md` §3.3, §3.6, §3.7, §4.7; `design/01-foundation.md` §3.3;
`design/07-manual-tasks.md` §3.3; `UPSTREAM_REQS.md` §2.9; D-124, Q-11.

**Amended (2026-09-26)**: "the stopgap covers that case only" no longer holds. D-133 extends the
same poll to every other wait that holds a recorded suspension. The round family stays per
suspension, and `apply-hold` resets it on each new `suspensionRef`. The absorption of a late
`OrderResumed` into a `resumed-by-read` row is unchanged and now applies wherever the poll ran.

**Amended by D-141 and D-143 (2026-09-26)**: the order read now guards the event path too, so a
resume Lifecycle has overtaken closes nothing (D-141). The resume wait's poll has no failure
route, because the wait precedes `begin-fulfillment` and nothing is deferred there
(`pollResumeFailure` is removed, D-143).

**Amended by D-183 (2026-09-28)**: the poll no longer answers `still-held` on every terminal order.
A `rejected` or `completed` order at the instance's version is not held, and the poll closes its
open suspension. The definition also calls the poll once before `terminate-instance` after a
rejection or a completion while it holds a `suspensionRef`.

**Amended by D-150 (2026-09-26)**: entering the resume wait no longer resets `resumePollRound`; only `applyHold` resets it, for a new `suspensionRef`.

### D-131 (H) The references that cross the engine are a closed six-type vocabulary; cardinality, counters and the resource tenant are the stated residual

**Accepted (2026-09-26).**

**Decision**: every member of a task input, output, `export`, `body` or header, and of `$context`,
is of one of six types, each a registered GTS schema with a format. The types are: **identity**
(`correlationId`, `orderId`, `orderVersion`, `resourceTenantId`, `invocationId`, `attemptId`, the
binding members, consumed-event ids); **opaque record reference** (a UUID per kind: `stepRef`,
`taskRef`, `gateRef`, `lineRef`, `planRef`, `parkRef`, `requestRef`, `cancelRequestRef`,
`suspensionRef`; a `subjectRef` of one of these, `correlationId` or `ceiling:{round}`; arrays,
bare or paired with a reason code); **counter** (a non-negative integer, or a key tail of counters
and a literal prefix); **closed enumeration or boolean**; **instant or duration**; and
`stepsBase`. A string member of no such type is refused by `design/10` §2.2 rule 5 and at the
envelope. The residual in engine history is stated in full: the identifiers, **including
`resource_tenant_id`**, which is in every task input and `Idempotency-Key` header, so the timeline
shows which resource tenant owns each order; the record references; and the counters and array
cardinalities, which show line, gate, task and retry counts but no line content. "Line counts" is
struck from ADR-0013's never-cross list, and "line items" stays.

**Rationale**: ADR-0013's list omitted `lineRef`, `planRef`, `parkRef`, `requestRef`,
`suspensionRef`, the event ids and every counter, all of which the canonical definition passes.
ADR-0012 rule 1 requires `lineRefs[]` on each wave. The confirmation test rejected every string
without a closed enum, so a validator written from the ADR would refuse the canonical definition,
and one written from the definition had no closed list (OW2-61). The residual named only three
identifiers and claimed the timeline does not show "for whom", while `resourceTenantId` is in
every body and key (OW2-64). The smallest consistent rule is to type what already crosses, and to
state its disclosure, rather than redesign the wave calls to carry no line set. **Precedent**:
Lifecycle's actor and resource references as lowercase UUID text, "an opaque, pseudonymous,
immutable reference" (D-61 in this register, mirroring Lifecycle D-96 and D-103); 01 §3.3's per-operation
GTS `input`/`output` types, which this vocabulary types member by member. No platform precedent
exists: the serverless-runtime has no data-classification model for history (`NEXT_ADR_SCOPE.md`
line 23, BR-017).

**Propagated**: `ADR/0013` (amendment blocks, *Confirmation*); `design/01-foundation.md` §1.1,
§2.1; `design/10-process-definition.md` §1.1, §2.1, §2.2 rule 5; `DESIGN.md` §4.2, §4.3;
`UPSTREAM_REQS.md` §2.9, §4 item 7; D-66, Q-01, Q-12.

**Amended by D-156 (2026-09-26)**: the closed format list gains the platform invocation
identifier, the attempt identifier and the GTS callable identifier, the formats of identity
members the list already named.

### D-132 (M) A step-route error answer carries fixed members only

**Accepted (2026-09-26).**

**Decision**: every answer the step route `/bss-orders-workflow/v1/steps/{operation}` produces
carries only `type`, `status`, `title`, `error_domain`, `error_code`, a `detail` equal to the fixed
text registered for the `error_code`, and an empty `context` (`context.data` `{}`), with no
`instance` and no `trace_id`. That covers a registered reason, the envelope's validation refusal
and a PDP denial. The definition reads only `$error.status` (and `error_code` once Q-11 (ii)
answers) and exports no `$error` member. The operator-facing routes keep the full RFC 9457 shape.
ADR-0013's golden-response test is extended to every refusal the step route can answer.

**Rationale**: the DSL raises a non-2xx answer as the communication error the `catch` sees as
`$error`, and the engine records it. 01 let the Problem carry a free-text `detail` and variant
`context.data`, which ADR-0013 forbids ("free text of any kind — error messages"). The golden test
checked only success bodies (OW2-65). **Precedent**: the canonical-errors SDK's
`Problem::contract_error` places variant data at `context.data`, so an empty `data` is the SDK's own
empty variant ([`problem.rs`](../../../../libs/toolkit-canonical-errors/src/problem.rs)
`contract_error`); 01 §4.11's rule that every `detail` is a bounded, sanitized diagnostic. No BSS
gear restricts a machine surface's Problem further, so this is the smallest rule that closes the
gap.

**Propagated**: `ADR/0013` (*Error answers*, *Confirmation*); `design/01-foundation.md` §4.9,
§4.11; `design/10-process-definition.md` §2.1, §2.2 rule 5; `DESIGN.md` §4.2; `UPSTREAM_REQS.md`
§2.9; D-66.

### D-133 (M) Every wait that holds a recorded suspension polls the hold

**Accepted (2026-09-26).**

**Decision**: while the definition holds a `suspensionRef`, which `apply-hold` answered and nothing
has closed since, every wait other than the approval resume wait calls `apply-resume`
`trigger: poll` (`pollHeld`) on its own tick, counted in `heldTicks`, about every `PT15M`. That
covers the park loop, the held-reflection wait, the eligibility, expected-time, barrier and held
waits, and a wave deferral answered `deferReason = held`. The cadence is every third `PT5M` tick,
every thirtieth `PT30S` barrier poll, every fifteenth `PT1M` deferral tick, and every `PT1H`
expected-time tick. The way out of the park loop polls once as well. The poll uses the resume
wait's key family `…:apply-resume:poll:{suspensionRef}:{round}`, whose `resumePollRound`
`apply-hold` resets whenever it answers a new `suspensionRef`. A `resumed-by-read` answer, the
resume wait's `backFromPoll` and the stage-level resume arm clear `suspensionRef`. After the poll
the definition decides on `stageLoop` first, then runs the tick's own re-check. In the eligibility
wait, which precedes `begin-fulfillment`, there is no failure route. Elsewhere in fulfillment a
non-empty `failedTaskRefs[]` goes to fragment (c) before any dispatch. `gateLoop` and `outageArm`
do not poll: a hold there enters the resume wait, and their `PT30S` tick carries D-123's
lateness bound. The failure stage's resolution wait does not poll, because every way out of it
reaches a polled wait or the unwind. A late `OrderResumed` for a suspension the poll closed is
still absorbed by `inst-ar-ahead` into that row, wherever it arrives.

**Rationale**: outside the approval stage a hold is only recorded, and D-130 polled only the
resume wait. A resume lost between listens therefore left `owf_process_instance.suspended` set.
`evaluate-activation-eligibility` withheld release, and the dispatch operations deferred every
line with `deferReason = held` (`design/05` `inst-pi-held`). A held wave deferral loops
`dispatch` ↔ `PT1M` with no `listen` at all, so it could not consume even a retained
`OrderResumed`, and it looped until an operator cancel or the lifetime ceiling (B7 open item 1). A
suspension carried from the park loop into the gates also made the next hold `absorbed-duplicate`,
so the gate windows went unpaused. The poll also heals the misattribution B7 noted: an
out-of-order resume absorbed into an earlier `resumed-by-read` row leaves the next suspension open,
and the next polled wait closes it by read. Counting ticks keeps the waits' literal durations, and
it keeps the key count near the resume wait's 96 a day rather than one key per `PT30S` tick. No
routing member is written from an operation output (`design/10` §2.2 rule 1): `afterHeldPoll`
switches on `stageLoop`, and the counter and `suspensionRef` are data. **Precedent**: D-130's
resume-wait poll, of which this is the same call in more places; the fixed-tick re-check loops of
`design/10` §3.6 *Fixed waits and re-check loops*; Lifecycle's "re-read the order, wait for the
resume" (Lifecycle `06 §4.3`).

**Propagated**: `design/10-process-definition.md` §3.6 (a), (b), (e), *Fixed waits and re-check
loops*, §4.1, §4.4, §4.5; `design/08-hold-and-cancel.md` §3.2, §3.3, §4.7 items 3, 9, 10;
`design/01-foundation.md` §3.3; `DESIGN.md` §1.2; `UPSTREAM_REQS.md` §2.9; D-124, D-130, Q-11.

**Amended by D-142 (2026-09-26)**: `deferralTick1`/`deferralTick2` are gone. The held count
rides the tick arms of the new `awaitDeferral1`/`awaitDeferral2` forks, which carry the four
shared arms, so a held deferral now listens for a hold, a resume, a lifecycle event and a cancel.

**Amended by D-150 (2026-09-26)**: the held poll's failure tail is `poll:{suspensionRef}:{round}`, and the resume wait continues the per-suspension round rather than restarting it.

### D-134 (H) The business windows are per-seller policy values pinned on the record; the definition owns only the tick

**Accepted (2026-09-26).** *(amends D-70, D-98, D-126)*

**Decision**: the approval escalation window, the overdue window and the manual-task SLA classes
are values of the **seller's policy** — the per-seller policy rows of `DESIGN.md` §4.8, the store
the partial-failure policy election already lives in. The operation that needs a window resolves
it from that policy and pins it on the order's record, as `construct-and-freeze-plan` pins the
partial-failure policy (`design/04` §2.2 `inst-pc-resolve-policy`): `open-gates` pins each gate's
`escalation_window_ms` (the party's window, else the seller default, 72 h);
`construct-and-freeze-plan` pins the new `owf_fulfillment_plan.overdue_window_ms` (default 24 h),
which `raise-overdue-escalation` reads in place of its hard-coded 24 h; `create-manual-task` and
the reopen pin the class window into `sla_deadline` (defaults 4 h and 24 h). The definition owns
only the tick of each re-check loop. Changing a window is an audited policy write: no Orders
release and no new definition version, and it reaches only records pinned after the write. That
write **MUST** refuse, and keep the prior value, a set in which an SLA class exceeds the overdue
window, the overdue window is not between the active version's longest fulfillment-stage task
timeout (`wave1`, 10 min) and the `P90D` ceiling, or the escalation window is not below the
ceiling (`design/07` §4.8 item 8).

**Rationale**: the windows had five homes — definition values in `DESIGN.md` §4.8, 01's bounds
table and ADR-0011; routing configuration in 03; a hard-coded 24 h in 07's algorithm; per-seller
policy rows in `DESIGN.md` §4.8 again; and "a definition input" in 07 §4.8 — and since the
fixed-waits restructure (D-70 as amended) no definition could move any of them (OW2-75). The owner
ruled that the values live in Orders' per-seller policy and the definition owns the tick (R1).
**Precedent**: the partial-failure policy pinned at freeze (`design/04` §2.2), whose "never the
live configuration" rule is the same guard against a mid-flight change; and Lifecycle's snapshot
of the tenant's effective `orders_date_policy` row, stored with the admitted order version and
never re-read from a later policy row (Lifecycle `design/03-gate-and-pin.md` §4.2 item 8). No
platform precedent applies: the platform has no per-tenant business-window store.

**Propagated**: `design/01-foundation.md` §2.1, §4.2, §4.4; `design/03-approval-execution.md`
§3.2, §3.6, §3.7; `design/04-fulfillment-plan.md` §2.2, §3.6, §3.7, §4.7, §4.8;
`design/07-manual-tasks.md` §1.2, §3.2, §3.6, §3.7, §4.1, §4.8; `design/10-process-definition.md`
§1.1, §1.2, §2.2, §3.6, §4.7; `DESIGN.md` §1.2, §4.7, §4.8; `UPSTREAM_REQS.md` §4 item 14;
`ADR/0011`; D-70, D-98, D-126.

**Amended by D-140 (2026-09-26)**: "the per-seller policy rows of `DESIGN.md` §4.8" now have a
store and an owner. They are the foundation's `owf_seller_policy` (`design/01-foundation.md`
§3.7), written only by the policy load when a policy-channel promotion is applied. "An audited
policy write" is that promotion, validated against the bounds before it commits. The operations
read the effective policy through the seller-policy port and record the revision they pinned.

### D-135 (M) A version carries the `p1` composables and the shared arms on their paths; the check tracks the pinned enums

**Accepted (2026-09-26).** *(amends D-126)*

**Decision**: the operations stay `composable` (22 `protected`, 13 `composable` unchanged), but
`design/10` §4.1 requires the `p1` ones on their paths, as the Waves row already requires
`evaluate-activation-eligibility`. On an `unobtainable` verdict, `park` and a park loop whose tick
calls `arm-park-escalation`, with a route to `raise-overdue-escalation` (`park`), and no
reflection, gate or fulfillment until a later verdict is obtained. On a pending-approval
reflection, `open-gates` before `record-decision`, with a gate wait carrying the decision `listen`
and the `escalate-gate` fire and probe, and a route to `raise-overdue-escalation`
(`approval-outage`). After every `create-manual-task`, and after the ceiling's escalation, a wait
carrying the task's resolution `listen` and SLA check through `resolve-manual-task`.
`verify-override` comes only after `resolve-manual-task`. Rule 1 now tracks four **pinned
members** — `verdict`, `reflected`, `policy`, `forceTask` — each written only as a copy of its
operation's output or as a literal, and forks the walk on their values. A `switch` over the
seller's pinned partial-failure policy is therefore decided: `remediate` must reach
`create-manual-task` before any terminal outcome, and `fail-fast` must reach the unwind without
it, so a version that tests a literal instead of `policy` is refused. Rule 8 requires the four
shared arms (hold, stage-level resume, lifecycle, cancel) in every stage `fork`, with the unwind,
ceiling and resume waits' stated exceptions, and the top-level `lifetime` fork's `P90D` branch.

**Rationale**: a version could drop `open-gates`, `escalate-gate`, `raise-overdue-escalation`
or the resolution wait and pass validation, publishing a process with no approval requests, no
escalations or no task resolution, or one that ignored the seller's pinned policy (OW2-80). It
could also drop the hold or amendment arm that 08 and 02 require, since no rule checked arm
presence (OW2-73). The owner ruled that the operations stay composable and a rule requires them on
their paths (R2). No routing member is written from an operation output: the pinned members are
tracked beside the routing members and never route a stage (`design/10` §2.2 rule 1).
**Precedent**: the §4.1 Waves row of D-126, which requires the composable
`evaluate-activation-eligibility` answering `released` before wave 2; `design/04` §2.2 for the
pinned policy the switch must read.

**Propagated**: `design/10-process-definition.md` §2.1, §2.2, §4.1, §4.7;
`design/01-foundation.md` §3.1, §3.3; `design/03-approval-execution.md` §3.3;
`design/07-manual-tasks.md` §3.3; `ADR/0012`; `ADR/0011`; `DESIGN.md` §2.2; D-126.

**Amended by D-142 and D-143 (2026-09-26)**: the pinned members are five; `beginResult`, a copy
of `begin-fulfillment`'s `result`, joins them (D-143). Rule 8's shared arms now reach every wait
inside a stage, because a stage may hold no plain `wait` (D-142).

**Amended by D-144 (2026-09-26)**: the pinned members are ten: `released`, `spawned`, `planFailed`, `preAdmitted` and `failureScope` join the five.

### D-136 (M) One adjustability table; slice constraints are enforced items or canonical-definition guidance; no Function call

**Accepted (2026-09-26).**

**Decision**: `design/10` §4.7 is the one table of which change needs which vehicle: a definition
version, a seller-policy write, an Orders release, a PRD change, or nothing. ADR-0011's
adjustability contract, `DESIGN.md` §4.7 and `design/10` §1.1 and §2.2 point to it. The two
examples the vision gave that the fence forbids — re-authorisation after plan freeze, parking a
partial failure — are removed from §1.1 and §2.2 and listed as not permitted. Each slice's
*Constraints this slice places on the definition* is mapped item by item in §4.7. An **enforced**
item restates a rule of §2.2 or a §4.1 row, and the validator refuses a version through that rule.
Every other item is **canonical-definition guidance**: the canonical version carries it, the
publish job's behavioural gate (D-138) asserts it for every candidate, and a version departs from
it only by amending the slice item in the same change. **No `call` targets a registered
Function**, `composable` operation or not.

**Rationale**: the slices said "a version that violates any of them MUST be refused" over about 70
items, while 10 §4.7 permitted changing any wait, any listen arm and any composable, and the hook
implemented only the eight rules (OW2-73). The vision's two headline examples were forbidden by
the fence (OW2-76). Four lists of what may change disagreed (OW2-83). A Function in place of a
composable would act outside the record and outside the step surface's authorization,
contradicting "the definition never acts", with no identity, schema or record rule for it
(OW2-81). **Precedent**: dropping the Function follows the principle
`cpt-cf-bss-orders-workflow-principle-definition-orders-never-acts` and Q-10, which already
declines tenant Functions. No platform precedent exists for splitting validator rules from
guidance.

**Propagated**: `design/10-process-definition.md` §1.1, §2.1, §2.2, §4.7;
`design/02-triggers-and-start.md` §4.7; `design/03-approval-execution.md` §4.5;
`design/04-fulfillment-plan.md` §4.8; `design/05-provisioning-intents.md` §4.5;
`design/06-saga-and-compensation.md` §4.7; `design/07-manual-tasks.md` §4.8;
`design/08-hold-and-cancel.md` §4.7; `design/09-read-and-authz.md` §4.6; `DESIGN.md` §4.7;
`ADR/0011`; `ADR/0012`; Q-10.

### D-137 (H) The binding records the version the platform pinned; the publisher waits on the registry

**Accepted (2026-09-26).** *(amends D-68)*

**Decision**: `start-instance` reads the platform's invocation record for the bound invocation
(`GET /api/serverless-runtime/v1/invocations/{invocation_id}`, the read the instance liveness pass
already makes) and records its `function_id` and `function_version` as `definition_id` and
`definition_version`. The document's self-declared `version` is only compared with them.
`definition-not-bound` now means that the invocation runs a callable other than a major of
`order_process`, or a `function_version` other than the document's `version`; once the hook
lands it also means a version the hook has not validated. `owf_definition_binding.published_by`
becomes nullable and stays null until the registry reports a publisher per version, because a
registered callable carries none and the definition must not name its own. Until the hook lands,
the evidence of a publish is the publish job's run and the registry's version listing. The
hook ask gains two parts: the registry reports each version's publisher, and until the hook exists,
publishing and lifecycle transitions of `order_process` are restricted to the publish job's
identity. The claim that the registry is the system of record for publishes is dropped from
`design/10` §2.2.

**Rationale**: `published_by` was "copied from the registry's record" with no registry read
defined and no publisher field to read; `definitionVersion` was the document's own string; and
`definition-not-bound` checked a hook that does not exist (OW2-74). A mistaken or malicious
version could claim to be the canonical one. **Precedent**: the invocation record's required
`function_version` (serverless-runtime DESIGN_GTS_SCHEMAS.md *InvocationRecord*) and the
liveness pass's read of it (D-105). An Orders-side allow-list was considered and rejected: a
compiled list needs a deploy, which D-138 rules out for a definition change, and a list any
operator can write proves nothing. The publish restriction is an ask, not a platform fact.

**Propagated**: `design/01-foundation.md` §3.3 *start-instance*, §3.6, §3.7;
`design/10-process-definition.md` §2.2, §3.1, §3.3, §3.6, §4.2, §4.3; `DESIGN.md` §3.1, §3.6,
§4.2; `UPSTREAM_REQS.md` §2.9; `ADR/0012`; D-68, D-105.

**Amended (2026-09-26)**: the start sequence of `02 §3.6` now shows the invocation-record read
and the `definition-not-bound` branch, as `01 §3.6` does (re-review R-31).

### D-138 (M) A definition is published by a job of its own, behind a behavioural gate, and rolled back forward

**Accepted (2026-09-26).** *(amends D-68, D-107)*

**Decision**: definition publishing is a pipeline job of its own that **MUST NOT** build or deploy
the gear. A merge to `definitions/` needs its code owners, who are the Orders definition owners,
and a second reviewer; a change that touches an operation is an Orders release first. In each
environment the job runs the §2.2 rules against that environment's deployed operation registry.
In the first non-production environment it runs the **behavioural gate**: it publishes the
candidate there and drives one order down each path (a)–(f) against the real step surface, with
test doubles for Lifecycle, Subscriptions and the approval service, asserting the record and the
guidance items of `design/10` §4.7. A failing scenario stops the job. The job then publishes,
deprecates the version it replaces in the same run so one version per major is `active`, applies
the trigger bindings, and never archives or deletes. A rollback publishes the last good document as
a new minor version, because the registry has no `deprecated → active` transition. A major bump
re-points both bindings in the same run. The bound-version check runs in the gear's readiness
check in each environment, not in the repository's CI. Which version a trigger starts, and a
canary or tenant-scoped activation, are the new ask
`cpt-cf-bss-orders-workflow-upreq-serverless-runtime-trigger-version-selection`.

**Rationale**: "the release pipeline publishes it" did not say whether a definition change builds
or deploys the gear, which would defeat changing the process without a redeploy (OW2-79). No rule
said how a version becomes the one triggers start, how a bad version is rolled back, or what a
major bump does to the bindings, and CI was to read every environment's bindings with no access
to them (OW2-77). The only gate was static, so a version that misroutes would reach every new
order in the environment (OW2-78). **Precedent**: the platform function lifecycle, whose state
machine has no way back from `deprecated` (serverless-runtime DESIGN.md lines 588–607), and its
`dry_run`, which validates only and so is not a behavioural test (DESIGN.md, *Invocation API*);
D-107 already applies the trigger bindings from `definitions/` through one pipeline. No BSS gear
publishes a platform definition, so the job and the gate are the smallest rule consistent with
the set.

**Propagated**: `design/10-process-definition.md` §2.2, §3.2, §3.3, §3.7, §3.8, §4.2, §4.3;
`design/01-foundation.md` §3.7; `DESIGN.md` §2.2, §3.6, §3.8, §4.7; `UPSTREAM_REQS.md` §2.9, §3;
`ADR/0012`; `ADR/0011`; D-68, D-107.

**Amended by D-158 and D-159 (2026-09-26)**: the rollback's document carries the new version in
its own `document.version`; the premise "no `deprecated → active` transition" is corrected — a
two-step return through `disabled` exists and is not used. A failing gate rolls its environment
back forward, the first environment is published once, the job compares literal ticks with their
guidance values, and it refuses a candidate whose `wave1` timeout is not below every effective
overdue window.

### D-139 (M) The build order follows in-process calls; 07 and 08 precede 06, and the back-edges are built against doubles

**Accepted (2026-09-26).** *(carries `ADR/0002` as amended)*

**Amended by D-181 (2026-09-28)**: slice 09's step-retry route calls slice 07's resolution-request intake port; 09 is built after 07, so this is a forward edge and the back-edges are unchanged.

**Decision**: the build order is **01, 10, 02, 03, 04, 05, 07, 08, 06, 09**. A slice depends on
another when it calls that slice's operation or port in-process, or reads or writes its table; the
definition's `call` to an operation is not an edge. Slice 06 is built after 07 and 08, because it
calls slice 07's creation port and Incident Recorder and slice 08's suspension-closure and
cancel-authority ports; slice 08 is built after 07, whose manual-task creator it calls in the same
unit of work. The edges that still point later are stated as **back-edges**: slice 07's retry
re-check reads slice 06's fence row; slices 06, 07 and 08 declare their routes against slice 09's
catalogue and scope predicate, and slices 06 and 08 reach slice 09's cancel request record, by a
read of its `cancel_reason` and through its request-delivery port. The earlier slice is built and unit-tested against a double of the
later slice's port, and its integration test runs once the owner lands; 09's conformance test is
the check on the catalogue half.

**Rationale**: the table named the build-order authority ordered 06 before the 07 and 08 ports it
calls, left 07 out of 08's dependencies, and hid the 06↔07, 06↔09, 07↔09 and 08↔09 cycles, so a builder
following it could not compile 06 (OW2-84). ADR-0002's own table also ordered `10` before `01`,
while `design/README.md` and `design/10` put the foundation first. **Precedent**: Orders
Lifecycle builds capture and local transition tests against an `EventBrokerApi` double before the
broker exists (`orders-lifecycle/docs/design/README.md`, *Phase 0/1*); ADR-0002's rule "a slice
depends only on a slice earlier in the build order" is kept, with the stated back-edges as its
only exceptions. Rejected: splitting every port trait into a separate build step ahead of all
slices, which adds a build stage for a handful of edges.

**Propagated**: `design/README.md` (the build-order table, the edge rationale, the cycles);
`ADR/0002`; `DESIGN.md` §3.2.

**Amended (2026-09-26)**: the foundation has back-edges too. Its reconciliation sweep runs slice
05's `reconcile-intent`, its liveness pass drives slices 06 and 08 in the dead-instance unwind and
calls slice 07's creation port and `resolve-manual-task`, and `terminate-instance` calls slice 07's
closure port; each is built against a double. The stated cycles are eight: `01` with each of
`05`, `06`, `07` and `08`, and the four above. ADR-0002's block said three cycles and its
*Confirmation* forbade every later dependency; both are amended (re-review R-27, R-34).
**Propagated**: `design/README.md` (row 1, *What an edge is*, the cycles); `ADR/0002`.

**Amended (2026-09-26, second)**: the count of eight omitted the foundation's `retention-purge`
worker, whose bounded deletes reach the tables of slices 03 through 09 (`01 §3.8`); by the
README's own rule a table write is an edge. Row 1 now lists 03, 04, 05, 06, 07, 08 and 09 for the
purge, and the stated cycles are **eleven**: `01` with each of `03`–`09` (seven), plus `06`↔`07`,
`06`↔`09`, `07`↔`09` and `08`↔`09`. These edges are table deletes, not ports: each slice's purge
predicate joins the worker's roster in that slice's own build step, where its test runs (third
re-review S-7). **Propagated**: `design/README.md`
(row 1, *What an edge is*, the cycles); `ADR/0002`.

### D-140 (H) The per-seller policy is a foundation table on the policy channel, read through one port and pinned with its revision

**Accepted (2026-09-26).** *(amends D-48, D-59, D-63, D-69, D-104, D-134)*

**Decision**: the per-seller policy of D-134 lives in `owf_seller_policy`
(`design/01-foundation.md` §3.7), a table the foundation owns. It holds one platform row, seeded
by migration with the defaults (`remediate`, 72 h, 24 h, 4 h, 24 h) and never deleted. It holds
one row per seller that overrides any value; a NULL value there inherits the platform row. The
table carries the partial-failure policy, the default and per-party escalation windows, the
overdue window and the two SLA class windows, each row with a positive monotonic
`policy_revision` and the promotion's `updated_by`/`updated_at`. **Who writes it**: no Orders
endpoint and no PDP action. Rows change only by promotion on the policy channel Lifecycle uses
for its own policy rows, and a seller asks for a change through platform operations. A promotion
carries rows only, builds no slice and publishes no definition version. The foundation's
**policy load** applies it in one transaction. It locks the platform row, then each changed
seller row, bumps the revisions and writes one audit entry under the deployment marker. Before it
commits, it validates the effective policy of every affected seller against the bounds of
`design/07` §4.8 item 8. The overdue window is checked against the `wave1` timeout of every
definition version that active bindings name. A violating promotion commits nothing, so the
prior rows stay in force. **How an operation reads and pins it**: `open-gates`,
`construct-and-freeze-plan`, `create-manual-task` and the reopen read the effective policy once,
inside their settlement transaction, through the foundation's seller-policy port. The effective
policy is the seller's value, else the platform's; a gate takes its party's window, else the
default. The operation pins the value on its record and writes the revision it read into its
settled step-log `result` as `sellerPolicyRevision`. A replay answers from the record and never
reads again. The table is keyed by `seller_tenant_id` alone, so it is the second configuration
exemption from D-48's resource-axis rule. It is the engine's ninth table and the store count
becomes 27.

**Rationale**: D-134 moved the three windows into "the seller's policy", the per-seller policy
rows of `DESIGN.md` §4.8, and B9 found that no slice specified that store, its writer, its audit
or its validation point. An implementer would have invented all four. The foundation owns it
because the three readers are slices 03, 04 and 07, and a table in any one of them would give
the others a back-edge in the build order of D-139. The foundation precedes them all.
**Precedent**: Orders Lifecycle keeps each tenant or seller policy as Orders-owned rows on its
policy channel. `orders_state_ttl_policy` has platform and seller scope, `NULLS NOT DISTINCT`
uniqueness, a positive revision and the platform-row-first lock order
(Lifecycle `design/07-hold-and-expiry.md` §3.7). `orders_date_policy` has a revisioned platform
default whose absence is a deployment failure (Lifecycle `design/02-capture.md` §3.7, D-121).
`orders_policy_election` changes only by promotion, and a seller asks through platform
operations (Lifecycle `design/05-preconditions.md` §3.7, D-133). The snapshot of the effective
row with its revision, never re-read, is Lifecycle's `design/03-gate-and-pin.md` §4.2 item 8.
Within this gear, the load follows `owf_step_operation`: one transaction, audited on change.
**Rejected**: Pricing's runtime tenant configuration plane, `GET/PUT` routes under their own PDP
resource (Pricing `design/05-governance.md`). It fits a seller-edited commercial surface, but here
it would add routes, a catalogue resource and grants for rarely changed values, the trade-off
Lifecycle D-133 rejected for its elections. Also rejected: the platform Settings Service
(`gears/settings-service`), which has no implementation and would need an upstream ask, as
Lifecycle D-121 found; and `get_gear_config`, which is static per gear and carries neither a
tenant key nor a revision.

**Propagated**: `design/01-foundation.md` §2.1, §3.4, §3.7 (*Table: owf_seller_policy*,
tenancy, retention and immutability), §4.2, §4.11; `design/03-approval-execution.md` §3.6
`inst-og-plan`; `design/04-fulfillment-plan.md` §3.6 `inst-pc-resolve-policy`;
`design/07-manual-tasks.md` §4.1, §4.8 item 8; `design/10-process-definition.md` §4.7;
`DESIGN.md` §3.7, §4.7, §4.8; `UPSTREAM_REQS.md` §4 item 14; D-48, D-59, D-63, D-69, D-104,
D-134.

**Amended by D-159 and D-160 (2026-09-26)**: "active bindings" named no set, since no binding is
ever inactive; the check uses the **live version set** of `design/01-foundation.md` §4.2 (the
registry's `active` versions and the `deprecated` versions a non-terminal instance is bound to),
and the publish job checks the other direction. The "one audit entry under the deployment marker"
could not be written: `owf_audit_entry` is one instance's chain. Each changed row is instead
appended to `owf_configuration_revision`, which resolves every recorded `sellerPolicyRevision`.

**Amended (2026-09-28, residuals of the re-review)**: the pin's provenance was recorded only in the settled `owf_step_log.result`, which is purged at 90 days while the gate, plan and task rows it provenances are kept ≥ 400 days (V-A-7). Each pinned record now carries it in a `seller_policy_revision` column — `owf_approval_gate` (`design/03` §3.7), `owf_fulfillment_plan` (`design/04` §3.7), `owf_manual_task` (`design/07` §3.7) — written with the pinned value, and the registry's `settled_output` (D-167) carries it for replays; neither log nor registry is the provenance. **Precedent**: Lifecycle's expiry record captures the effective `(policy_id, policy_revision)` beside the value it applies (Lifecycle `07-hold-and-expiry.md:461`). A column realizing this decision's "pinned with its revision", not a behaviour change, so no new decision.

### D-141 (M) A resume closes only a suspension Lifecycle no longer holds

**Accepted (2026-09-26).** *(amends D-87, D-130)*

**Decision**: `apply-resume` reads the order through the Lifecycle order read on an
`OrderResumed` too, not only on the poll. If Lifecycle holds the order `on_hold`, the resume has
been overtaken by a later hold. The operation closes nothing, records no `resume_ahead` row and
answers `still-held`, with no `nextRound`. The definition returns to where the arm was taken:
the resume wait with `suspensionRef` kept, or the stage. The open suspension now stands for the
later hold and closes on its resume or by the poll. `apply-hold` that finds a `resume_ahead` row
reads the order too. It consumes the row, and it still records the suspension when Lifecycle
holds the order. The resume arm no longer clears `suspensionRef`; the resume stage clears it only
after a suspension is closed. A transient read failure is `retryable-failure` under the open key.

**Rationale**: a resume could close the wrong suspension (B8 open item 1). Take Lifecycle's order
H1, R1, H2, delivered to this gear as H1, H2, R1. H2 was absorbed against S1 by the partial
index, and R1 then closed S1, leaving Orders unsuspended while Lifecycle held the order. A
resume must be matched to its own hold, but nothing on the wire makes that match. `OrderHeld`
carries the outgoing state and `OrderResumed` the restored state, with event ids and neither a
hold identity, a hold ordinal nor `resume_count` (Lifecycle `design/01-foundation.md` §4.4, *The
event set*). The broker's per-partition sequence is not a per-order barrier, and a rejected
message lets later events pass (Lifecycle `01 §2.2`, *Delivery is at-least-once; ordering is
partition-scoped*). The match is therefore made against the one authority that knows which
hold is in force. **Precedent**: Lifecycle's consumer rule in the same constraint: "Consumers
**MUST** … validate the event's `orderVersion` and resulting state against authoritative Orders
state before acting". Within this gear, the poll's read of the hold (D-130, `design/08`
`inst-ar-poll`) is the read the event path now shares. **Rejected**: an upstream ask for a hold
ordinal on both events. It would give an identity match but adds a Lifecycle schema change, and
the read already decides every interleaving without it.

**Propagated**: `design/08-hold-and-cancel.md` §2.2 *One Open Suspension Per Order*, §3.3,
§3.6 `inst-ah-consume-ahead` and `inst-ar-poll`, §4.2, §4.7 item 3;
`design/10-process-definition.md` §3.6 (e); D-87, D-130.

**Amended (2026-09-26)**: the resume stage's `apply-resume` body passes `if $context.returnStage == "hold" then $context.suspensionRef else null end`, so the stage-level arm still passes the null `suspensionRef` that `08 §4.7` item 3 requires now that the arm keeps the member (R-36).

### D-142 (M) Every wait inside a stage is a competing fork carrying the shared arms

**Accepted (2026-09-26).** *(amends D-120, D-133, D-135)*

**Decision**: `design/10` §2.2 rule 8 forbids a plain `wait` task inside a stage. The wave-1 and
wave-2 deferral waits (`waitDeferral1`, `waitDeferral2`) become the tick branches of
`awaitDeferral1` and `awaitDeferral2`, and the wave-1 re-issue wait (`waitReread1`) the tick
branch of `awaitReread1`. Each carries the hold, stage-level resume, lifecycle and cancel arms.
The unwind's re-poll (`waitRepoll`) becomes the tick branch of `awaitRepoll`, which carries the
cancel arm only, as every unwind fork does. The held count of D-133 rides the deferral tick
arms. An arm returns through the stage's `enter`. Wave 1 re-enters at `wave1`, which
re-dispatches the deferred lines that `enterDeferral1` filed before the wait, or re-issues the
unchanged key after a read. Wave 2 re-enters at its stage loop, which re-evaluates before any
dispatch. `enterWave1` sets `holdPauses: false`, so a hold in a wave wait is recorded and never
waited on. The waits of the top-level `lifetimeCeiling` and `overdueMonitor` branches are outside
every stage and are not covered.

**Rationale**: rule 8 required the shared arms in every stage fork, but these four waits were
plain `wait` tasks and escaped it (B8 open item 2, B9 note). A cancel or a terminal order event
sent during a long held deferral was left to signal retention for the whole wait. A cancel
during the unwind's re-poll was not consumed before `report-outcome`, so a failure unwind could
report `failed` although a cancel had been accepted. Wrapping each wait costs one fork and one
switch and needs no exception in the rule. **Precedent**: the barrier's own timer half,
`awaitExpected`, a tick branch beside the four arms (`design/10` §3.6 (b)), and the unwind's
`awaitCompensationResolution`, which carries the cancel arm only.

**Propagated**: `design/10-process-definition.md` §2.2 rule 8, §3.6 (b) and (c), *Fixed waits
and re-check loops*; `ADR/0012`; D-120, D-133, D-135.

**Amended (2026-09-26)**: `08 §4.7` item 2 now names the resume wait `awaitResume` among the forks that carry no hold arm, as rule 8 already did (R-30).

### D-143 (M) No walk reaches a wave dispatch without `begin-fulfillment`; `beginResult` is a pinned member

**Accepted (2026-09-26).** *(amends D-126, D-130, D-133, D-135)*

**Decision**: `design/10` §2.2 rule 1 tracks a fifth pinned member, `beginResult`, written only
as a copy of `begin-fulfillment`'s `result`. The §4.1 Waves row requires every walk that reaches
`dispatch-wave1-create` or `dispatch-wave2-activate` to carry `beginResult = in-fulfillment`. The
failure stage returns to the barrier (`routeRetry` `barrier`, `afterOverride` `verified`) only on
`beginResult = in-fulfillment`; any other barrier retry, which no line task produces, stays in
`awaitResolution`. The resume stage decides on the interrupted stage before it looks at
`failedTaskRefs`. From the approval stage or the eligibility wait it returns without a failure
route. The resume wait's poll has no failure route at all, so `pollResumeFailure` is removed:
the resume wait is entered only from the approval stage, before any dispatch, where nothing is
deferred.

**Rationale**: rule 1 takes a `switch` case over a non-routing member both ways. B7's
`pollResumeFailure` therefore gave the walk a route from the approval resume wait into the
failure stage, and from there `toBarrier` into wave 2, with no `begin-fulfillment` on the walk
(B8 open item 3). The event-driven resume and every pre-fulfillment failure entry had the same
route through `toBarrier`. The walk could not prove the fence it is meant to prove, and a CI
check written to rule 1 would have refused the canonical definition. Deciding on a member the
walk tracks closes every such route, and no routing member is written from an operation output
(B7's rule holds). **Precedent**: B8's `afterHeldPoll`, which decides on `stageLoop` first so
that the eligibility wait has no failure route (D-133), and the pinned `policy` of D-135.

**Propagated**: `design/10-process-definition.md` §2.2 rule 1, §3.6 (c) and (e), §4.1;
`design/08-hold-and-cancel.md` §4.7 items 8 and 10; `ADR/0012`; D-126, D-130, D-133, D-135.

**Amended by D-144 (2026-09-26)**: the failure stage's routes and the Waves row are also decided on `failureScope`, `released` and `spawned`; the pinned members are ten.

### D-144 (H) Rule 1 decides every route on members it tracks; the canonical definition passes it

**Accepted (2026-09-26).** *(amends D-126, D-135, D-143)*

**Decision**: `design/10` §2.2 rule 1 tracks ten pinned members. Beside `verdict`, `reflected`,
`policy`, `forceTask` and `beginResult` it tracks `released`, a copy of
`evaluate-activation-eligibility`'s `released`, and four members written only as literals in the
task one case of a `switch` over an operation's closed enum routes to: `spawned` (`true` only in
`spawnSent`, after a sent spawn signal), `planFailed` (`true` only in `enterPlanFailedWait`),
`preAdmitted` (`true` only in a `toLifecycle` after a hold, resume or acceptance admission) and
`failureScope` (`line`, `plan` or `order`, in the task that enters the failure stage). The
canonical definition is restructured so that every `switch` leading towards a protected
operation reads only routing and pinned members, or a member both of whose ways satisfy §4.1:

1. `heldCall` is removed. The held wait's operation is the stage loop, `heldSpawn` or
   `heldReport`, literals of a routing member; `retryHeld` and the fulfillment `enter` switch on it.
2. `onEvaluate` decides `releasedFirst` and `releasedAgain` on the pinned `released` and
   `spawned`; `onSweep`'s wave-2 redispatch is taken only once `spawned` is true.
3. `routeRetry` routes by `failureScope` (line → barrier, plan → plan, order → stage), which
   `07` maps one-to-one onto `resumeAt`; the compensation case is removed from the failure stage,
   because a compensation task's retry is the unwind's. `failFastUnwind` routes `begun` on
   `beginResult`, a plan-scope failure through `begin-fulfillment` first, and faults the
   invocation (`orderTaskExhausted`, as `unknownStage`) for an order-scope task, which
   `07 §4.2` never exhausts.
4. The §4.1 Waves row states the ADR-0004 conjunction as a walk to `dispatch-wave2-activate`
   that passes the pinned `released = true` and `spawned = true`: `released` implies `due`
   (`04 §3.6` `inst-ae-if-conjunction-false`), so the `waitExpected` loop is where the walk waits,
   not a separate constraint.
5. ADR-0012's D-126 block named the routing members incompletely and allowed only literals and
   copies; rule 1 and the canonical `ceilingEntry` also use an `if … then … else` over routing
   members. The ADR now carries the full list and the third form.

**Hand-walk of the canonical version** (every walk the second re-review named, R-38, R-18): the
held spawn reaches `report-outcome` `completed` only through `dispatch-wave2-activate`
(`retryHeld` is decided by `heldSpawn`); the first release reaches `re-check-pre-activation`
and `report-spawn-signal` before wave 2 (`spawned` is false until `spawnSent`); the reflection
task's retry reaches only `reflectVerdict` and its exhaustion only a fault (`failureScope =
order`), never `construct-and-freeze-plan`, `compensate-order` or `begin-fulfillment`; the first
eligible round reaches `construct-and-freeze-plan` (`planFailed` false); a lifecycle event is
admitted before `run-cancellation-fence` or `terminate-on-terminal-event` unless `preAdmitted`
is true, which only an admission sets; a wave-2 redispatch follows the spawn signal. Every other
`switch` over an untracked member — `planState`, `eligibility`, the wave lists, `preActivation`,
`spawnAnswer`, `lifecycleCall`, `compensationState`, `resolution`, `admission`, `terminate`,
`cancelAuthorized`, `holdOutcome`, `unwind` — was taken both ways, and each way satisfies §4.1.

**Rationale**: rule 1 takes a case over an untracked member both ways and refuses a version on
which any walk breaks §4.1. The canonical definition routed on `heldCall`, `spawned`, `resumeAt`,
`planFailed`, `exhaustedTaskRefs`, `released` and `preAdmitted` (R-38, R-18), so its own CI check
refused it, and relaxing the check to pass it would have stopped it catching real violations.
Tracking literals written under a decided case keeps B7's rule that no routing member is copied
from an output, and the members stay within the six-type vocabulary of D-131 (booleans, a closed
enum). **Precedent**: `forceTask`, the literal pinned member of D-135, and `beginResult` (D-143);
none in the platform, whose registry validates no definition.

**Propagated**: `design/10-process-definition.md` §2.2 rule 1, §3.6 (b), (c), §4.1, §4.7;
`design/07-manual-tasks.md` §3.6, §4.7, §4.8 item 4; `design/05-provisioning-intents.md` §4.5
item 6; `ADR/0012`; D-126, D-135, D-143.

**Amended by D-161 (2026-09-26)**: the hand-walk's "unless `preAdmitted` is true, which only
an admission sets" held only for the event that was admitted: a ceiling during the lifecycle stage
followed by the ceiling wait's lifecycle arm routed a new event on the earlier event's admission
(third re-review S-1). Every arm that writes `lifecycleEventId` now sets `preAdmitted: false`, and
D-161 carries the re-walk of every entry into the lifecycle stage. The Decision's wording of the
four literal members was broader than rule 1 and the canonical allow (S-6); it reads as rule 1
does: `spawned`, `planFailed` and `preAdmitted` are boolean literals, each `false` where it is
initialised or consumed (`preAdmitted` also in every arm that writes `lifecycleEventId`) and `true`
only in the task one case of a `switch` over an operation's closed enum routes to; `failureScope`
is one of the literals `line`, `plan` and `order` in the task that enters the failure stage,
whatever case routes there (`lineFailure1`, `sweepFailure`, `lineFailure2`, `heldPollFailure` and
`resumeFailure` are reached by cases over list lengths).

### D-145 (M) A sent spawn signal leaves the held-spawn loop; a not-dispatchable re-check waits there

**Accepted (2026-09-26).**

**Decision**: `spawnSent`, the `sent` case of `onSpawn`, sets `spawned` and the stage loop
`barrierLoop` before `dispatch-wave2-activate`, so an arm taken from a wave-2 deferral, the held
poll or a ceiling re-enters at the barrier's `evaluate`, never at `re-check-pre-activation`. A
`not-dispatchable` answer of `re-check-pre-activation` (`observed` `on-hold`, `superseded` or
`terminal`) goes to the held-spawn wait, `heldWait` with stage loop `heldSpawn`: its `PT5M`
tick re-runs the re-check, and its resume, lifecycle and cancel arms consume what the operation
observed. `report-spawn-signal`'s own `not-dispatchable` still returns to the barrier loop
(D-109).

**Rationale**: the held spawn's stage loop was never reset, so after wave 2 had dispatched, any
arm re-entered through `retryHeld`, ran a protected re-check after activations were accepted
(an `abort` would then have voided only wave-1 drafts with wave-2 lines live) and re-sent the
stale `eligibleLineRefs` (R-39). A `not-dispatchable` re-check went to the barrier, whose
`PT30S` poll re-ran the protected Lifecycle read some 2,880 times a day for the whole hold (R-42).
**Precedent**: `onSpawn`'s `held` route to the same wait (`05 §3.3`, D-109), and the barrier's
re-evaluation before any dispatch (`05 §4.5` item 3).

**Propagated**: `design/10-process-definition.md` §3.6 *Fixed waits* and (b);
`design/04-fulfillment-plan.md` §4.8 item 4; D-109.

### D-146 (H) The failure stage never waits with no open task

**Accepted (2026-09-26).** *(amends D-117, D-129)*

**Decision**: a `resolve-manual-task` answer `closed` that leaves `openTaskCount` at 0 on a line
entry, with `beginResult = in-fulfillment`, returns to the barrier exactly as the last retry
does (`onResolution` `lastClosed` → `toBarrier`), taking with it every line an earlier retry held
behind the remediation hold. The one order-scope task the failure stage creates,
`approval-reflection-refused`, offers no task `cancel` (`07 §4.4`, and the task cancel route
refuses `action-not-offered`): the order is not yet in fulfillment, so it is ended by Lifecycle's
own cancel, whose `OrderCancelled` the stage's lifecycle arm consumes (D-109).

**Rationale**: `closed` returned to `awaitResolution` whatever the count. A Seller Operator's
cancel of the last open task — the sibling that held a retried line, or a lone `intent-unresolved`
task — left the stage waiting with nothing but an SLA tick that answers `none`, and the retried
lines were never re-dispatched until an order cancel or the ceiling (R-40). Cancelling the
reflection task did the same before fulfillment (R-45). **Precedent**: D-129, which withdrew the
task `cancel` from the ceiling task because closing it left the order parked with nothing to wait
on; D-117's `toBarrier` on the last resolution.

**Propagated**: `design/10-process-definition.md` §3.6 (c); `design/07-manual-tasks.md` §3.3,
§4.4, §4.8 item 4; D-117, D-129.

### D-147 (M) A call that consumes an arm's payload is its own checkpoint

**Accepted (2026-09-26).** *(amends D-121)*

**Decision**: `record-decision` after the decision arm, and `resolve-manual-task` after the
failure stage's, the unwind's and the ceiling wait's task-resolution arms, are each preceded by a
task that records the call in `stageLoop` (`recordDecision`, `resolveTask`,
`resolveCompensationTask`, `resolveCeilingTask`), and the stage's `enter` switch re-issues the call
under its unchanged key. The routes back to the waiting fork go through the stage's `enter…` task,
so the checkpoint is replaced before the stage waits again.

**Rationale**: the lifetime ceiling cancels `process` wherever it is, and the stage re-enters at
its recorded `stageLoop`. The checkpoint was the fork, whose arm had already consumed the event,
so a ceiling during the call either left the gate decided or the task resolved while the
definition waited for a signal that would not come again, or dropped the operator's action
(R-41). The shared stages did not have the defect because hold, resume and lifecycle re-admit the
event id they stored. **Precedent**: `enterParkLoop` before `park` and `leftPark` after `unpark`
(D-121), which record the checkpoint before the call for the same reason.

**Propagated**: `design/10-process-definition.md` §3.6 intro, (a), (c), (d); D-121.

**Amended by D-164 (2026-09-28)**: the re-issue is under the interrupted stage's own payload — the
ceiling wait's resolution arm writes `ceilingRequestRef`, not `requestRef`, and `backToProcess`
restores the event and request members `ceilingEntry` saved — so a ceiling arm no longer changes
the key a checkpoint call re-issues under.

**Amended by D-184 (2026-09-28)**: the fulfillment stage's two payload-consuming calls follow the
same rule — the acceptance `admit-trigger` (`admitAcceptance`) and the evaluation a
`reauthorize-requested` signal asks for (`eligibility`) are recorded in `stageLoop` before they run.

### D-148 (M) The escalation bound counts every call between two fires

**Accepted (2026-09-26).** *(amends D-123)*

**Decision**: the calls that can run between two escalation fires — `escalate-gate` `mode: fire`
and `mode: probe`, and `record-decision` — carry a 60-second `gate` timeout (above their 10 s
`deadline_ms`, rule 4); a decision recorded `pending` goes straight to `escalateGate`
(`reenterGateLoop`), and every re-entry of the approval stage at `gateLoop` fires before it
waits. The worst case, with every call's retries running to its timeout, is the rest of the fire
whose read the deadline just missed, the probe, the tick or a decision's `record-decision`, and
the next fire: 30 s + 4 × 60 s = 4 min 30 s, inside ± 5 min. An arm that leaves the stage and
returns adds only the calls of the stage it visited, normally inside their 5–10 s
`deadline_ms`; only with those calls' retries running to the 3-minute `step` timeout can the bound
be exceeded, which is stated as the residual of a degraded Orders. A hold pauses the window.

**Rationale**: D-123's "one tick plus one call" omitted `probeGate`, which sits between one fire
and the next, and every other call on that path; with 3-minute timeouts the worst case was about
6.5–9.5 min (R-10). Moving the probe elsewhere does not help, because any order of fire, probe and
tick puts one probe between two fires. **Precedent**: D-70's derivation of the wave timeouts from
the SLA they must fit, and rule 4's `deadline_ms` < timeout.

**Propagated**: `design/10-process-definition.md` §1.2, §3.6 (a) and *Fixed waits*;
`design/03-approval-execution.md` §1.2, §4.5 item 4; `design/08-hold-and-cancel.md` §1.2;
`DESIGN.md` §1.2; D-123.

**Amended by D-162 (2026-09-26)**: the 60 s `gate` timeout was shorter than the `transient`
curve it bounded (worst case about 185 s), so it, not the retry limit, ended a spent call and
faulted the invocation with an uncaught 408. The four gate-loop calls — `recordDecision`,
`escalateGate`, `probeGate` and now `probeOutage` — retry under their own `gate` policy (4
attempts, constant 2 s, jitter 0–3 s; worst case 55 s), and the outage arm's `available` answer
fires before it waits. The 30 s + 4 × 60 s bound is unchanged.

### D-149 (H) Each ceiling's task is its own row, and the fence closes it

**Accepted (2026-09-26).** *(amends D-121, D-122, D-129)*

**Decision**: `raise-overdue-escalation` `lifetime-ceiling` creates the `lifetime-ceiling-reached`
task with `scope_ref = ceiling:{round}`, the escalation's subject, so the creation port's
uniqueness tuple gives each ceiling its own row. `run-cancellation-fence`, in step 1, calls slice
07's closure port scoped to `lifetime-ceiling-reached` (`outcome = closed-by-unwind`, a new
`resolution_outcome`), which closes an open ceiling task and its `requested` request. Every way
from the ceiling wait into an unwind — the order cancel, a supersession, a terminal order event —
passes the fence, so the ceiling task is open only while the instance is parked at that ceiling,
and 07 §4.4's exclusivity rule says so.

**Rationale**: under the creation port's `(order_id, order_version, task_scope, scope_ref,
failure_reason)` uniqueness, a second ceiling reopened the first ceiling's resolved row, returned
the same `taskRef`, and replayed that task's stored SLA rounds from round 0, so its 24 h
escalation was suppressed for as long as the first park had lasted (R-12). Nothing closed the
ceiling task on an unwind, so the exclusivity rule froze every compensation task at `escalate`,
and the unwind's resolution wait could take the ceiling task's `retry` for a compensation retry
(R-11). **Precedent**: fence step 1 already calls slice 03's closure port for gates and parks and
slice 08's for the suspension (`06 §3.6` `inst-fence-step1`); `terminate-instance` calls this
same closure port (`07 §3.2`).

**Propagated**: `design/07-manual-tasks.md` §3.2, §3.6, §3.7, §4.3, §4.4, §4.7;
`design/06-saga-and-compensation.md` §3.4, §3.6; `design/10-process-definition.md` §3.6 (d);
D-121, D-122, D-129.

### D-150 (L) The poll's task tail names its suspension; only `apply-hold` resets the poll round

**Accepted (2026-09-26).** *(amends D-116, D-130, D-133)*

**Decision**: the `sourceAttempt` a held poll's failure presents to `create-manual-task` is
`poll:{suspensionRef}:{round}`, and `enterResumeWait` no longer sets `resumePollRound` to 0:
`applyHold` alone resets it, when it answers a new `suspensionRef`.

**Rationale**: the round family is per suspension and restarts at 0, so two holds whose polls
each found deferred failures presented the same tail `poll:0` with different subjects, and the
second `create-manual-task` was refused `idempotency-key-conflict`, faulting the invocation
(R-7). Resetting the round in `enterResumeWait` replayed the stored `still-held` answers of a
suspension already polled in another wait, so a lost resume was found up to k × `PT15M` late
(R-44). **Precedent**: apply-resume's own poll key, which already carries the suspension
(`08 §3.3`); D-116's rule that every failing call has its own tail.

**Propagated**: `design/10-process-definition.md` §3.6 (b), (c), (e);
`design/07-manual-tasks.md` §3.3; `design/08-hold-and-cancel.md` §4.7 item 10; `ADR/0013`;
D-116, D-130, D-133.

### D-151 (M) The liveness pass observes the SLA of the tasks no definition waits on

**Accepted (2026-09-26).** *(amends D-105)*

**Decision**: for an instance the liveness pass reads not live, `next_liveness_at` advances by
5 min instead of 15, and each pass calls `resolve-manual-task` with `trigger: sla-check`
in-process through the envelope, scoped by `taskRef` to each open escalate-only task whose only
waiter was the definition: the `invocation-dead` task, and the compensation-reason or order-scope
tasks the dead-instance unwind raised. Each scoped check has its own family
`…:resolve-manual-task:sla:{taskRef}:{slaRound}` from round 0, and the pass takes its round from
the previous settled answer, as the dead-instance unwind takes its keys. A scoped check never
exhausts, and one naming a forward task is refused `action-not-offered`. Forward tasks left open
by the dead invocation are not checked: exhausting them would enter compensation, which only a
running definition or the operator's `cancel` drives.

**Rationale**: the `invocation-dead` task carries the 4 h window its own row calls the most
urgent, but the only breach detector was the definition's `PT5M` branch, and for this task no
definition runs; the pass only created the task and advanced `next_liveness_at`. The breach and
the `seller-operator` escalation never happened (re-review R-1). **Precedent**: the ceiling
wait's scoped check (D-129, `07 §3.3`), which already scopes `sla-check` to one escalate-only task
under its own family; the dead-instance unwind (`01 §4.16` item 2), which already drives
operations in-process from the pass under the keys the definition would present.

**Propagated**: `design/01-foundation.md` §3.7 (`next_liveness_at`), §3.8
(`inst-owf-live-dead`, `inst-owf-live-sla`); `design/07-manual-tasks.md` §3.3, §3.6
(`inst-rmt-sla`), §4.2, §4.8 item 3; D-105.

**Amended (2026-09-26)**: the pass's scope named only the tasks the dead-instance unwind raised,
while its lead-in described every task whose only waiter was the definition. It is every open
escalate-only task of the instance — compensation reason or order scope, whenever raised,
including a compensation task in `awaitCompensationResolution` and an open
`lifetime-ceiling-reached` task opened before the invocation died; forward tasks stay excluded
and are observed on re-drive (`01 §3.8` `inst-owf-live-sla`, `07 §4.2`, §4.8 item 3; third
re-review S-8).

### D-152 (M) The quarantine counts only presented attempts

**Accepted (2026-09-26).** *(amends D-119; refines `01 §4.13`)*

**Decision**: `retry-step`'s quarantine counts an attempt toward the three only when a call of the
family arrived under that attempt's key (the registry holds a record for it) and left it
`in_flight` with a dead lease or produced no step record. An attempt no call presented is skipped:
it neither counts nor breaks the consecutive run.

**Rationale**: under D-119 every line task of a wave mints into the wave's one dispatch family and
the definition carries only the latest attempt, so the attempts of several line retries resolved
together are overwritten unused, and a retry of a `submitted` or `unresolved` line mints one that
dispatches nothing. None produced a step record, so the fourth line retry of one wave was refused
`poison-step` and its task waited for SLA exhaustion (re-review R-13). The guard exists for crash
loops, and an unpresented attempt is not a crash. **Precedent**: none in the platform or the BSS
gears for an operator-retry quarantine; the rule keeps `01 §4.13`'s own intent that an ordinary
retry train must not exhaust the quarantine allowance.

**Propagated**: `design/01-foundation.md` §3.3 (`retry-step`), §4.13; D-119.

**Amended by D-174 (2026-09-28)**: "presented" is recorded, not inferred: each family keeps a `presented` history of `{attempt, key}` in `key_rounds`, and an attempt counts when its key's registry record is `in_flight` with a dead lease; a settled or `open` record resets the run.

### D-153 (M) Slice 05 re-arms an unresolved intent through its own port

**Accepted (2026-09-26).** *(amends D-119, D-125)*

**Decision**: slice 05 owns the in-process re-arm port `rearm_unresolved(lineRef, wave)`. In the
caller's unit of work, under the intent row lock, it sets an `unresolved` forward intent back to
`submitted` with `next_sweep_at` now and `handed_off_at` null, on the same row and key, and
answers every other status as it stands. Slice 07's line `retry` calls it and writes no
`owf_provisioning_intent` column.

**Rationale**: 05 §3.7 had named slice 07's retry as "the one other writer" of the intent table,
against `DESIGN.md` §3.7's sole-writer rule, which OW2-56 had restored elsewhere, and gave the
builder no port for the write (re-review R-14). **Precedent**: slice 05's own deferral port
`take_deferred_failures(correlationId)` (`05 §3.2`), the door through which slice 08's
`apply-resume` takes and clears deferred failures in its own unit of work.

**Propagated**: `design/05-provisioning-intents.md` §3.2, §3.7, §4.4;
`design/07-manual-tasks.md` §3.4, §3.6 (`inst-rmt-retry`); D-119, D-125.

### D-154 (M) `create-manual-task` verifies the route that names a subject, not only a failed task

**Accepted (2026-09-26).** *(amends D-116, D-119)*

**Decision**: `inst-cmt-verify` accepts a `line` subject of the instance's frozen plan when the
record shows the route that names it: a `failed-pending-escalation` compensation record for a
compensation reason; the line's current forward intent recorded `unresolved` for
`intent-unresolved`; any task of the plan for a wave reason with cause `retry-budget-exhausted` or
`step-deadline-exceeded` (the exhausted call, which names every line of a call that never
answered); and the task in `failed` for every other forward reason. A line an exhausted call did
land gets a task whose `retry` moves nothing and returns it to the barrier, and whose `cancel`
closes it (D-146's `lastClosed`).

**Rationale**: the verify step refused every line subject that was not a `failed` task, but
`sweepFailure`'s `intent-unresolved` subjects and `wave1Exhausted` / `wave2Exhausted` name lines
that are never `failed` — an intent that may be live is never moved to `failed` (`04 §2.1`). The
400 faulted the invocation under the bare `*transient` catch, so D-119's retry branch for a line
not in `failed` served tasks that could not exist (re-review R-15, and the older finding that a
wave-exhaustion task lists subjects not recorded failed). Moving such lines to `failed` first was
rejected: it would allow a second submit for an intent that may be live (`01 §4.5`). **Precedent**:
none for the exhausted-call case; the verification keeps its purpose — the definition cannot name
a subject that is not this instance's or a route the record does not show.

**Propagated**: `design/07-manual-tasks.md` §3.6 (`inst-cmt-verify`, `inst-rmt-retry`); D-116,
D-119.

**Amended (2026-09-26)**: the exhausted-call branch checked nothing in the record, against the
rationale's "a route the record does not show" (third re-review S-10). It now also requires that
the idempotency registry hold no `settled` record under the wave dispatch key `sourceStep` and
`sourceAttempt` name: a call that never answered leaves it absent, `open` or `in_flight`, while a
settled key means the call answered and its stored outcome is the route (`07 §3.6`
`inst-cmt-verify`).

### D-155 (M) `supersededByOrderVersion` is admit-trigger's `currentOrderVersion`

**Accepted (2026-09-26).** *(realises `02 §4.3` in the definition)*

**Decision**: every listen admission of the definition (the lifecycle, hold, resume and acceptance
arms) exports `supersededByOrderVersion` = the answer's `currentOrderVersion` on `supersede`, and
null otherwise; `terminateAborted` passes it to `terminate-instance`. The lifecycle arm no longer
copies the event's `orderVersion` (`newOrderVersion` is removed). `supersededByOrderVersion` is an
identity member of ADR-0013's vocabulary.

**Rationale**: 02 §4.3 fixes the value as admit-trigger's `currentOrderVersion`, but no admission
exported it; the only version the definition wrote was the lifecycle arm's copy of any event's
own version — the instance's own for an `OrderCancelled` correlated on the pinned version — and a
supersede reached through the hold, resume or acceptance admission set nothing. The terminal record
named the wrong superseding version or none (re-review R-43). **Precedent**: `02 §4.3` and the
supersession sequence of `02 §3.6`, which already carry `currentOrderVersion = N+1` into
`terminate-instance`.

**Propagated**: `design/10-process-definition.md` §3.6 (b), (e), (f); `design/02-triggers-and-start.md`
§4.3; `ADR/0013`.

**Amended (2026-09-26)**: the vocabulary restatements in `design/01-foundation.md` §1.2
*References, not payloads* and `design/10-process-definition.md` §1.1 now list
`supersededByOrderVersion` and point to ADR-0013 item 1 for the full identity list (third
re-review S-11).

### D-156 (L) The reference vocabulary names the formats of every identity member it lists

**Accepted (2026-09-26).** *(amends D-131)*

**Decision**: ADR-0013's closed format list gains three formats: the platform invocation
identifier (the opaque `invocation_id` string of the platform's invocation record, referenced and
compared, never parsed); the attempt identifier `{invocationId}:{taskReference}`, where the task
reference is the JSON pointer of a `call` task of the bound document; and the GTS callable
identifier under `gts.cf.core.sless.workflow.v1~*`. Rule 5 refuses a string member of no listed
type or format.

**Rationale**: the D-131 list named `attemptId` and `definitionId` as identity members, but their
values fit none of its formats, and neither does the platform's opaque `invocationId`; a validator
built from the ADR refused every step body and `start-instance`'s input (re-review R-19).
**Precedent**: the platform's own schemas — the invocation record's `invocation_id`, "Opaque unique
identifier", and the `x-gts-ref` pattern `gts.cf.core.sless.workflow.v1~*`
(serverless-runtime `DESIGN_GTS_SCHEMAS.md` lines 628, 1269–1272) — and the DSL's task
`reference` (dsl.md *Task Descriptor*).

**Propagated**: `ADR/0013` (amendment block after the format list); `design/10-process-definition.md`
§2.2 rule 5; D-131.

### D-157 (M) The callable's limits are sized from the residency of long waits and three ceilings

**Accepted (2026-09-26).** *(amends D-127)*

**Decision**: `traits.limits` is `{ timeout_seconds: 23328000, max_concurrent: 50000 }`. The
timeout is three `P90D` ceilings, 270 days: the first ceiling fires by 25 h + 90 d (it is armed
after admission), the fresh ceiling by 25 h + 180 d, and the rest carries the second ceiling's
park and an unwind; a third ceiling is not reachable, and an invocation live at 270 days is ended
by the guardrail and raised as `invocation-dead`. `max_concurrent` is sized for the reading that
counts suspended invocations: 1,800 on the standard path (5 / s × ~360 s) plus ≈ 12,830 in
manual-task waits (≤ 1 % of lines, about 3 % of orders at p50 3 lines, held ~24 h) gives ≈ 14,630
in phase 1; each 1 % of orders in a 72 h approval window adds 12,960, and each 1 % dated *d* days
ahead adds 4,320 × *d*. 50,000 leaves about 35,000 for those. If only running invocations count,
the population is about 7,500 (750 calls / s at peak × the 10 s dispatch deadline) and a later
version **MAY** lower the cap to 10,000. What each limit counts, and the platform's capacity above
its stated regional target of 10,000 concurrent executions, gate readiness.

**Rationale**: 180 days equalled two ceilings exactly, so the fresh ceiling D-121 arms could never
fire before the guardrail ended the invocation (re-review R-20); 5,000 came from the ~360 s mean
residency, while the design keeps invocations alive through multi-day waits, so under the
counts-suspended reading the cap would refuse order starts once long waits accumulate (R-21).
**Precedent**: the platform's limit semantics — "max execution duration in seconds" and "max
concurrent invocations", with no maximum (serverless-runtime `DESIGN_GTS_SCHEMAS.md` lines
142–174), the duration guardrail (PRD BR-028) and the regional target (PRD line 843, BR-208); the
residency figures are this design's own (`DESIGN.md` §4.1).

**Propagated**: `design/10-process-definition.md` §3.1; `DESIGN.md` §4.1 (*Concurrent in-flight
processes*, *Cost*); `UPSTREAM_REQS.md` §2.9 (readiness (a′), history growth); D-127.

**Amended (2026-09-26)**: the unit of the sizing is the `P90D` window, not the ceiling park: an
expiry `afterLifetime` absorbs through its `unwinding` or `verdictPark` case uses up one of the
three windows. An order whose first window expires in the verdict park and which then proceeds
reaches its only ceiling park at 25 h + 180 d; a second would fall after the guardrail. That path
is the accepted limit, stated in `design/10-process-definition.md` §3.1, rather than a fourth
window (third re-review S-12).

### D-158 (M) The rollback document carries its new version; a failed gate is rolled back; ticks are compared statically

**Accepted (2026-09-26).** *(amends D-138)*

**Decision**: the forward rollback republishes the last good document with its `document.version`
set to the new minor. The design keeps the forward version although the registry allows
`deprecated → disabled → active`, because that path disables a version while instances pinned to
it are live, and the platform does not say what disabling does to them. In the first
non-production environment the behavioural gate's publish is the environment's publish (step 3
only applies the bindings there), and a failing scenario rolls that environment back forward and
ends the suite's own orders with the order cancel. Step 1 compares every literal tick of the
candidate with the value its §4.7 guidance item fixes and refuses a difference unless the change
amends that slice item.

**Rationale**: republished unchanged, the last good document's old `version` would fail D-137's
check at every `start-instance`, stopping every new order, and the stated premise misread the
platform state machine (re-review R-26). The gate published the candidate before judging it and
had no way to withdraw a failure, and published twice in the first environment (R-33). Tick
guidance the gate cannot exercise was checked by nothing (R-32). **Precedent**: the platform
function state machine (serverless-runtime `DESIGN.md` lines 588–607); D-137's own version check;
no BSS gear publishes a platform definition.

**Propagated**: `design/10-process-definition.md` §3.8, §4.2, §4.7; `DESIGN.md` §3.6; `ADR/0012`;
D-138.

**Amended (2026-09-26)**: (1) a rollback minor, the gate's included, is the next minor above the
highest version the first non-production environment lists; the job commits the re-versioned
document to `definitions/` in the same run (its only commit) and publishes it through the normal
job in promotion order without the behavioural gate, so every environment holds the same content
under each semver, and CI refuses a candidate not versioned above every document in
`definitions/` (third re-review S-13). (2) In the first environment step 2 also applies the
trigger bindings, so a major bump's gate drives the new callable id, and the failure rollback
restores them (S-14). **Propagated**: `design/10-process-definition.md` §4.2.

### D-159 (M) The overdue-window bound is checked on both sides over a defined version set

**Accepted (2026-09-26).** *(amends D-134, D-140)*

**Decision**: the **live version set** of an environment is every version of `order_process` the
registry lists as `active`, every `deprecated` version that a non-terminal instance's binding
names, and, during a publish, the candidate. The policy load checks the overdue window against the
`wave1` timeout of every version in the set, reading them through the registry's version listing
before its transaction; the publish job refuses a candidate whose `wave1` timeout is not below
the smallest overdue window of the environment's policy rows; and the readiness check alerts when
a publish and a promotion that raced leave a live version at or above an effective window.

**Rationale**: "every definition version that active bindings name" named no set — a binding is
never inactive, and a published version with no instance yet is named by none — and 07 said "the
active definition version"; the reverse direction was unchecked, so a version raising `wave1`
above a seller's overdue window would publish (re-review R-28). **Precedent**: the gear's own
readiness check of bound versions against the registry listing (D-138) and the registry's
*list versions* and *get* (serverless-runtime `DESIGN.md` line 857); none in the BSS gears for a
cross-store bound.

**Propagated**: `design/01-foundation.md` §3.7 (*owf_seller_policy*), §4.2;
`design/07-manual-tasks.md` §4.8 item 8; `design/10-process-definition.md` §2.2 rule 4, §4.2,
§4.7; `ADR/0012`; D-134, D-140.

### D-160 (M) Configuration changes are recorded in an append-only revision table, not the audit chain

**Accepted (2026-09-26).** *(amends D-140, D-69)*

**Decision**: a new foundation table, `owf_configuration_revision`, holds one append-only row per
content change of a configuration load — kind `seller-policy` (keyed by `policy_id` and the new
`policy_revision`, carrying the whole promoted row and the promotion's change identity),
`step-operation-set` (the operation registry's load) or `authorization-catalogue` (slice 09's
catalogue check) — with a content hash and the Orders release that ran the load, which is what
"the gear's deployment marker" meant. Rows are inserted only in the load transaction that makes
the change, have no UPDATE or DELETE grant and are trigger-protected, carry no tenant column (the
third exemption to D-48) and are never purged. Every recorded `sellerPolicyRevision` resolves there
to the values in force.

**Rationale**: the loads were to write "one `owf_audit_entry` under the gear's deployment marker",
but `owf_audit_entry` is one instance's hash chain with NOT NULL instance and tenant columns and a
closed set of process kinds, and no marker was defined; the policy table keeps current rows only,
so a pinned revision could not be resolved to its values (re-review R-29). **Precedent**: Pricing's
price history, retained superseded rows under append-only protection
(`pricing/docs/design/01-foundation.md:562`), and the audit tables' grant-plus-trigger protection
(D-59). Lifecycle keeps no policy history, and needs none, because the record that uses a policy
captures the value beside its revision (Lifecycle `07 §3.6`).

**Counts**: tables 28 — ten engine, eighteen slice; mutable tables stay 19; three configuration
tables are exempt from D-48.

**Propagated**: `design/01-foundation.md` §3.2, §3.7 (*owf_step_operation*, *owf_seller_policy*,
*Table: owf_configuration_revision*, tenancy, retention, immutability), §3.8, §4.11;
`design/09-read-and-authz.md` §3.7; `DESIGN.md` §3.7, §4.7, §4.8; D-48, D-63, D-69, D-104, D-140.

**Amended (2026-09-26)**: the table has one writer, the foundation port
`record_configuration(kind, subject_id, content)`, called by the three loads, the seeding migration
and slice 09's startup catalogue check (a forward `09 → 01` edge); 01 §3.7's engine-schema rule
names the configuration loads, not the envelope, as the writers of the three configuration tables
(third re-review S-17). The change test compares with the latest row of the same `(kind,
subject_id)`; for `seller-policy` a row is appended for each row whose `policy_revision` the load
bumps (S-16). The two sentences that still said nine engine tables and the tenancy sentence that
named one exemption now say ten and three (S-18, S-19). **Propagated**: `design/01-foundation.md`
§1.2, §3.7, §3.8; `design/09-read-and-authz.md` §3.7; `DESIGN.md` §3.7, §3.8.

### D-161 (H) An admission answers only for its own event, and a ceiling restores the return stage it interrupted

**Accepted (2026-09-26).** *(amends D-121, D-144)*

**Decision**: (1) every arm that writes `lifecycleEventId` — the acceptance, hold, resume and
lifecycle arms — sets `preAdmitted: false`, so `preAdmitted` is true only while `lifecycleEventId`
is the event whose admission set it, and the lifecycle stage's `enter` routes a new event through
`admitLifecycle`. (2) `ceilingEntry` records the interrupted stage's `returnStage` in a new routing
member, `ceilingReturnBack` (with the same nested-ceiling `if` as its two siblings), and
`backToProcess` restores it. (3) Rule 1's routing members add `ceilingReturnBack`; its pinned-member
wording is the one the ADR-0012 and D-144 texts now copy.

**Re-walk of rule 1 over every entry into the lifecycle stage** (canonical, by hand; no tool runs
rule 1 yet):
- *A lifecycle arm's `leave`*, from any stage fork or the ceiling wait: the arm wrote
  `lifecycleEventId` and the literal `preAdmitted: false`, so `enter` takes `fresh` →
  `admitLifecycle` → `onLifecycleAdmission`; `supersedePath` and `terminalEvent` follow
  `admit-trigger` on both ways of the untracked `admission`. PASS.
- *A `toLifecycle`* in the fulfillment (acceptance), hold and resume stages: each follows
  `admitAcceptance`, `admitHold` or `admitResume` of the event its arm wrote (the resume arm writes
  `resumeEventId` and `lifecycleEventId` together), on the `onward` case of the same untracked
  `admission`; `enter` takes `preAdmitted`, and the admission already on the walk is the one for
  `lifecycleEventId`. PASS.
- *`backToProcess` with `ceilingReturnStage = lifecycle`* — a ceiling during `admitLifecycle` or
  `terminalEvent`: between `ceilingEntry` and `backToProcess` only the ceiling wait's lifecycle arm
  writes `lifecycleEventId`, and it writes `preAdmitted: false`; its cancel arm writes neither. So
  the stage re-enters either with `preAdmitted` true and the event its admission answered, or at
  `admitLifecycle` (a replay under the unchanged key, or the new event's first admission). The new
  event's admission loses nothing of the earlier one: `supersede` and `terminate` are read from
  Lifecycle's order state, which never leaves superseded or terminal (`02 §3.3`
  *Trigger-to-outcome table*), so it answers one of them whenever the earlier admission did. With
  `returnStage` restored, the stage's `back` reaches the stage the lifecycle stage was entered
  from, not the ceiling stage. PASS — this is the walk S-1 found failing.
- *A re-entry of the fork through `afterLifetime`'s `unwinding` or `verdictPark` case* while
  `nextStage = lifecycle`: no member is written, so the dispatcher re-enters the stage with the
  state it had. PASS.
- *No other route*: `returnStage`, `heldStage` and `ceilingReturnBack` are never `lifecycle`,
  because the lifecycle stage has no `leave` and each of them copies `returnStage` or a stage's
  `leave` literal.

**The same fault outside the lifecycle stage.** Before (2), any stage whose `back` reads
`returnStage` — lifecycle, hold (and `enterResumeWait`'s copy into `heldStage`), resume, cancel —
went back to the ceiling stage if a ceiling interrupted it and an arm was then taken from the
ceiling wait, since that arm's `leave` sets `returnStage: ceiling`; the ceiling stage then read a
foreign `stageLoop` and escalated a new ceiling round. The restore closes it for all four.

**Rationale**: rule 1 tracks `preAdmitted` as a literal, and a literal set by one admission and
read after another event was consumed decided the route of an unadmitted event: at run time the
fence's `inst-fence-cause` finds no supersede admission for it and refuses `not-found`, faulting
the invocation, or `terminate-on-terminal-event` consumes an unadmitted event (third re-review
S-1). Clearing it in `ceilingEntry` alone would have left the ceiling wait's arm the only writer,
so the arm is where the literal belongs. **Precedent**: the `false` literals rule 1 already
requires where a pinned member is consumed (D-144), and `ceilingReturnLoop`, the sibling member
`ceilingEntry` already records.

**Propagated**: `design/10-process-definition.md` §2.2 rule 1, §3.6 (a), (d), (e), (f) and their
descriptions; `ADR/0012` (amendment block after the D-144 block); D-121, D-144.

**Amended by D-164 (2026-09-28)**: the re-walk's `backToProcess` case no longer relies on the new
event's admission matching the earlier one: `backToProcess` restores `lifecycleEventId` and
`triggerKind` with `preAdmitted: false`, so the interrupted lifecycle stage re-admits its own event.

### D-162 (M) The gate-loop calls retry under a policy whose worst case fits their timeout

**Accepted (2026-09-26).** *(amends D-148)*

**Decision**: `use.retries.gate` is 4 attempts, `constant` backoff with a 2 s delay and jitter 0–3
s. `recordDecision`, `escalateGate`, `probeGate` and `probeOutage` carry `catch: *gateTransient`
(the `*transient` status set under that policy) and the 60 s `gate` timeout; `probeOutage` moves
from `step` to `gate`, and the outage arm's `available` answer goes through `outageOver` to
`escalateGate`, so every return into the gate loop fires first. Rule 4 requires a task under the
`gate` timeout to carry the `gate` policy and computes its worst case: 4 × 10 s (the largest
`deadline_ms`) + 3 × (2 s + 3 s) = 55 s < 60 s. The escalation bound is unchanged: 30 s tick +
4 × 60 s = 270 s = 4 min 30 s, 30 s inside the ± 5 min of `nfr-owf-escalation-timer`.

**Rationale**: the bound is a sum of timeouts only if a timeout is the most a call can take and the
retry limit, not the timeout, ends its retries. Under `transient` — 5 attempts × 10 s plus four
gaps of 1, 2, 4 and 8 s backoff and up to 30 s jitter each, about 185 s — the 60 s timeout fired
after two or three attempts with a 408 that `*transient` does not catch, so a short blip faulted
every gated instance (third re-review S-2). A larger timeout breaks the bound (4 × 185 s > 5 min);
a catch for 408 would add a failure route to §4.6's table. The price is a shorter absorption
window, about 6–15 s of spacing for fast failures instead of about a minute, stated in
`10 §3.6` (a). A constant backoff also makes the policy's worst case computable, which the
`exponential` form, whose multiplier is the plugin's, is not. **Precedent**: the `recheck` policy
of `use.retries`, sized for its window (`04 §4.8` item 4), and D-70's derivation of timeouts from
the SLA they fit.

**Propagated**: `design/10-process-definition.md` §1.2, §2.2 rule 4, §3.6 (a) and *Retry and
timeout*, *Fixed waits*; `design/01-foundation.md` §4.2, §4.5; `design/03-approval-execution.md`
§1.2, §4.5 item 4; `ADR/0012` (amendment block after the D-161 block); D-148.

### D-163 (M) A ceiling that fires while parked at a ceiling re-arms, and keeps the first ceiling's return state

**Accepted.**

**Decision**: `afterLifetime` (`design/10-process-definition.md` §3.6 (a)) gains a case before
`ceiling`: when the lifetime arm completes while `returnStage = ceiling`, the process re-arms the
lifetime fork at its current stage without running `ceilingEntry` or `park`. `returnStage` is
`ceiling` exactly while a lifecycle or cancel stage taken from the ceiling wait is running: only the
ceiling stage's `leave` writes that value, and `backToProcess` restores the saved one on unpark.

**Rationale**: in that state the instance is already parked at the open ceiling (`01 §3.7` has no
`parked → parked` edge), and that ceiling's task already has its waiter and its SLA tick (D-129).
Running `ceilingEntry` would overwrite the first ceiling's `ceilingReturnStage`,
`ceilingReturnLoop` and `ceilingReturnBack`, so a later unpark would resume at the cancel or
lifecycle stage instead of the stage the first ceiling interrupted. This follows the verdict-park
case of the same switch (D-121): an instance already parked by its own clock re-arms and is not
parked again. A cancel authorized from that stage enters the unwind, which the `unwinding` case
already covers. Closes the edge case left open by D-161.

**Propagated**: `design/10-process-definition.md` §3.6 (a) `afterLifetime` and the lifetime
paragraph of fragment (d); D-121, D-161.

### D-164 (H) The ceiling wait writes its own request reference, and a ceiling restores the payload of the stage it interrupted

**Accepted (2026-09-28).** *(amends D-121, D-147, D-161)*

**Decision**: (1) the ceiling wait's resolution arm writes a new member `ceilingRequestRef`, and
`resolveCeilingTask` sends `requestRef: $context.ceilingRequestRef`; `requestRef` stays with the
stage the ceiling interrupted. (2) `ceilingEntry` also saves the interrupted stage's payload —
`lifecycleEventId`, `triggerKind`, `cancelRequestRef`, `requestRef` and `taskRef` — as
`ceilingReturnEventId`, `ceilingReturnTriggerKind`, `ceilingReturnCancelRef`,
`ceilingReturnRequestRef` and `ceilingReturnTaskRef`, each under the same "keep the first ceiling's
value while `nextStage = ceiling`" `if` as `ceilingReturnStage` (D-163), and `backToProcess`
restores them. (3) `backToProcess` writes `preAdmitted: false`, a literal, instead of saving
`preAdmitted`: an interrupted lifecycle stage re-admits the restored event, and `admit-trigger`'s
event-scoped key `{tenant}:{eventId}:admit-trigger:listen`, whose fingerprint covers
`triggerKind` (`02 §2.1`, §3.3), answers it with the admission already settled for it. The saved
members are payload, not routing members, so rule 1's member lists are unchanged except that
`backToProcess` joins the writes where `preAdmitted` is `false`.

**Which members, and the sweep.** A member needs saving when a checkpoint call that a re-entered
stage re-issues reads it from `$context`, and something between `ceilingEntry` and
`backToProcess` can write it: the ceiling stage itself, the ceiling wait's four arms
(resolution, SLA, lifecycle, cancel), and the lifecycle and cancel stages taken from them, whose
`back` returns to the ceiling. Checked:
- *Written there and read by a re-issued call* — saved: `lifecycleEventId` and `triggerKind`
  (lifecycle arm; read by `admitAcceptance`, `admitHold`, `applyHold`, `admitLifecycle`,
  `terminalEvent`); `cancelRequestRef` (cancel arm; read by `authorize`); `requestRef` (the
  ceiling resolution arm before (1); read by `resolveTask`, `verifyOverride` and the eligibility
  call). `taskRef` is saved with `requestRef` though no ceiling writer is left, so the restore is
  the whole payload of the failure stage's `resolveTask`.
- *Written there, but every reader recomputes it on re-entry* — not saved: `admission` and
  `supersededByOrderVersion` (the interrupted stage re-runs its admission; `preAdmitted: false`
  sends a lifecycle stage through `admitLifecycle`), `terminate` (`terminalEvent` re-runs),
  `cancelAuthorized` (`authorize` re-runs), `resolution` (`resolveTask` re-runs; no `enter`
  reaches `onResolution` directly).
- *Ceiling-only members*: `ceilingTaskRef`, `ceilingSlaRound`, `ceilingSubject`, `ceilingRound`,
  and now `ceilingRequestRef`, which no other stage reads.
- *Read by a re-issued call, but nothing there writes it*: `resumeEventId` and `suspensionRef`
  (the ceiling wait has no hold or resume arm, `06 §4.7` item 7), `heldStage`, `heldLoop`,
  `eligibilityTrigger`, `slaRound`, `wave1LineRefs`, `eligibleLineRefs`, the rounds and attempt
  keys.
- *Outside the ceiling*: a shared arm of a stage fork (hold, resume, lifecycle, cancel) writes only
  `lifecycleEventId`, `resumeEventId`, `triggerKind`, `preAdmitted`, `cancelRequestRef` and
  `heldTicks`, and the stage it interrupted re-enters at its `stageLoop`. No checkpoint call a
  stage re-enters at reads one of them: `recordDecision`, `resolveTask`, `resolveCompensationTask`,
  `compensate`, `wave1`, `reflectVerdict`, `preActivation` and `reportCompleted` read their own
  stage's members, and the calls that do read them (`admitHold`, `applyHold`, `admitResume`,
  `admitLifecycle`, `terminalEvent`, `authorize`, `admitAcceptance`) run only on a fresh entry
  right after their own arm, or re-enter at a wait whose next arm writes them afresh. The unwind's
  cancel arm can overwrite `cancelRequestRef` with a denied request, but the unwind re-enters at
  `compensate` or its resolution wait and never re-runs the fence with it; a ceiling never
  interrupts the unwind (`afterLifetime`'s `unwinding` case). No other pair found.

**Rationale**: the ceiling interrupts a stage wherever it is, and its wait consumes signals and
events of its own. When those were written into the members the interrupted stage's checkpoint
call reads, the call re-issued after the unpark carried the ceiling's payload: `resolveTask` for
task A under the ceiling task's request (wrong key; A's consumed request lost, breaking D-147),
an `admitHold` presenting an `OrderAmended` event id as `OrderHeld` under a key that event's own
admission settled with another fingerprint (`idempotency-key-conflict`, `01 §4.3`), or an
`authorize` for the ceiling wait's cancel request instead of the stage's own (re-review E-1,
E-2). **Precedent**: `ceilingTaskRef` and `ceilingSlaRound` (D-121, D-129), the ceiling task's
own members kept apart from the failure stage's `taskRef` and `slaRound` for exactly this reason;
and `ceilingReturnStage`, `ceilingReturnLoop` and `ceilingReturnBack` with their nested-ceiling
guard (D-161, D-163), which the payload members copy. The literal `preAdmitted: false` follows
D-161's rule that every write that goes with `lifecycleEventId` clears it, and the replay follows
D-147: the call is re-issued under its unchanged key.

**Propagated**: `design/10-process-definition.md` §2.2 rule 1, §3.6 (a) `ceilingEntry`, (c) the
task-resolution arm's comment, (d) the ceiling wait, `resolveCeilingTask`, `backToProcess` and the
description, (f) the description; D-121, D-147, D-161.

### D-165 (H) A wave answer routes deferred, then failed, then lapsed, and no entry drops what another has not folded

**Accepted (2026-09-28).**

**Decision**: (1) `onWave2` routes `deferred[]`, then `failed[]`, then `lapsed[]` — the order
`onWave1` already uses for deferred and failed — so a line failed in the same answer as a lapsed
one reaches fragment (c) before the rebuild. (2) The lapsed lines are carried in a new member
`pendingLapsed`, which every wave-2 answer extends (`unique`), `rebuildLapsed` sends and clears,
and `initEligibility` initialises; `onEvaluate` gains a `lapsed` case after `undispatched`, so the
rebuild runs when the failure stage's retry, last close or verified override returns to the
barrier. (3) `createTasks` no longer clears `wave1Failed` and `wave2Failed`; `lineFailure1` and
`lineFailure2`, the only entries that fold those lists into `failureSubjects`, clear their own
list in the same `set`. Any other line-scope entry — `sweepFailure`, `heldPollFailure`,
`resumeFailure`, `wave1Exhausted`, `wave2Exhausted` — leaves both lists for the wave's next
answer, which routes them.

**Why not fold both lists into every line-scope entry.** `create-manual-task` takes one
`sourceStep` and one `sourceAttempt` per call (`07 §3.3`, D-116); a wave's failed lines folded
into a sweep's or a resume's call would be recorded against another operation's attempt, and a
retry would mint the attempt of the wrong family (D-119). Clearing at the folding entry keeps each
task's source step exact; `sourceAttempt` is the attempt of the call that folds the list. It costs
only a later task for the other list.

**Why deferred stays first.** `05 §4.5` item 4 routes a deferral to its loop, and the loop is the
deferred lines' only carrier (`enterDeferral1`, `wave2Again`); a failure first would leave them
behind, since `evaluate-activation-eligibility` names a line whose intent is live and deferred in
neither `undispatchedLineRefs` nor a failure. The failed and lapsed lists accumulate across
answers, so the deferral delays them and drops neither.

**The sweep.** Every switch that tests a failed list against another: `onWave1` (deferred before
failed; unchanged, and now safe because only `lineFailure1` clears `wave1Failed`), `onWave2`
(fixed), `onWave1Reread`, `onWave2Reread` and `onSweep` (failed first already), `afterHeldPoll`
and `onResume` (deferred failures before any dispatch, and their lists are the operation's own
answer, consumed by the entry that reads them). `complete` in `onWave2` is now reached only with
`wave2Deferred`, `wave2Failed`, `pendingLapsed` and `wave2Pending` all empty. Every `then:` of the
changed lists names a sibling, and the stage's `enter` switch needs no new case: the return to the
barrier re-enters at `barrierLoop`, whose `evaluate` routes `pendingLapsed`.

**Rationale**: `05 §4.5` item 6 lists a line in `failed[]` exactly once, so a list the definition
drops is never listed again. `onWave2` sent a lapsed-and-failed answer to the rebuild, and
`createTasks` cleared both wave lists on every entry, including a sweep, held-poll or resume
failure that had not folded them; a line failed alongside a lapsed one was then never given a
task and the order could report `completed` (re-review E-3). **Precedent**: the accumulating
`wave1Failed` and `wave2Failed` exports themselves, which already carry a failed line across a
deferral; `resumeFailure`, which clears `suspensionRef` in the entering `set` that consumes it;
and `onEvaluate`'s `undispatched` case (D-119), where the barrier routes lines it owes wave 1
before releasing. `ceilingEntry` already relies on a `set` reading `$context` as it was before
the task (it copies `nextStage` while writing it), as `lineFailure1` and `lineFailure2` now do.

**Propagated**: `design/10-process-definition.md` §3.6 (b) `initEligibility`, `onEvaluate`,
`lineFailure1`, `wave2`, `onWave2`, `rebuildLapsed`, `lineFailure2` and the description, (c)
`createTasks`. Related: D-54, D-116, D-119.

**Clarified (2026-09-28, residuals of the re-review)**: the rationale said clearing at the folding entry keeps each task's "source exact"; it keeps the source **step** exact, and `sourceAttempt` is the attempt of the call that folds the list (V-E-3).

### D-166 (M) A completion report never answers terminal-event, and every terminate-instance names its terminal outcome

**Accepted (2026-09-28).**

**Decision**: (1) `report-outcome` with `outcome: completed` answers `permanent-failure`
`version-mismatch`, not `reportedOutcome = terminal-event`, when Lifecycle refuses
`not-admissible` for a terminal order (`06 §3.6` `inst-ro-call`, `06 §4.9`); its output comment in
`10 §3.6` (b) states `reportedOutcome = completed` and `lifecycleCall` ∈ `acknowledged` · `held`,
and the definition keeps `onReport` as it is. (2) Each `terminate-instance` body names
`terminalOutcome`: `completed` for `terminateCompleted`; `aborted` for `terminateRejected` and for
`terminateAborted` on every `terminationKind` of the unwind (`compensated`, `superseded`,
`terminal-order-event`).

**Rationale**: (1) The terminal case cannot arise for a completion. `reportCompleted` is reached
only after `spawnSent`, so Lifecycle has recorded `spawn_signal_at`; from then on it leaves
`in_fulfillment`, and an `on_hold` whose pre-hold state is `in_fulfillment`, only by this gear's
reports (Lifecycle `01 §4.3` rows 13, 14, 16, 26, 27): the ordinary cancel of row 15 needs no
spawn signal, row 23 applies the pre-hold state's own guard, and row 24 and the missing
`in_fulfillment → expired` row exclude expiry. The unwind's reports, the only others, never
return to `reportCompleted`. Routing the answer would have needed a terminal event the definition
does not hold — the fence's `terminal-event` trigger requires a settled
`terminate-on-terminal-event` for a `triggerEventId` (`06 §3.6` `inst-fence-cause`) — and every
re-report would enqueue `OrderFulfillmentAborted` again under a new round. So the answer the design
excludes is a refusal, not a route (re-review E-5). (2) `01 §3.3` lists `terminalOutcome`
(`completed` · `aborted`) as a required input and the three calls sent none (re-review E-6);
`02 §2.1` and §4.3 already set `aborted` for superseded and terminal-event terminations, and the
sweep's cancel path of `01 §4.16` presents `aborted` with `compensated`. **Precedent**: `06 §4.9`'s
`not-admissible otherwise` row, which answers `version-mismatch` for a Lifecycle state the design
excludes (D-109, D-111), and `orderTaskExhausted` in `10 §3.6` (c), where an answer the design
rules out faults the invocation rather than gaining a route.

**Propagated**: `design/06-saga-and-compensation.md` §3.3 `report-outcome` output, §3.6
`inst-ro-call`, §4.9; `design/10-process-definition.md` §3.6 (a) `terminateRejected`, (b)
`reportCompleted`, `terminateCompleted`, (c) `terminateAborted`. Related: D-109, D-111.

**Clarified (2026-09-28, residuals of the re-review)**: the pairing — `terminationKind` `completed` ↔ `terminalOutcome` `completed`, every other kind ↔ `aborted` — is stated in `design/01` §3.3 `terminate-instance` `input` and enforced by the registered input schema: a mismatched pair is a 400 before registry resolution, which faults the invocation (D-114), not `version-mismatch`, whose 409 `*transient` would re-issue (V-E-2). `design/10` §3.6 (a) `terminateRejected` cites it.

**Amended by D-188 (2026-09-28)**: the terminal case can arise for a completion after all — the order `completed` by this gear's own report, re-issued after Lifecycle's 24-hour key window. When the read shows `completed` at this version with every line `activated` under the reported subscription identifiers, `report-outcome` answers `lifecycleCall = already-applied`, which `onReport` routes with `acknowledged`; any other terminal state keeps `permanent-failure` `version-mismatch`.

### D-167 (H) A settled key's answer lives on its registry row; the step log is recovery history with its own purge grant

**Accepted (2026-09-28).** *(amends D-103, D-104)*

**Decision**: (1) `owf_idempotency_registry` gains `settled_output` (`{formatVersion: 1, status,
body}`: the success body, or the HTTP status and `error_code` of a settled failure), written in
the settlement transaction by the envelope or by `settle-from-lookup`, immutable once `status =
settled`, and CHECKed non-null exactly on a settled row. An absorbed replay answers from it.
`outcome_ref` stays as a join link to the step-log row, with no FK and never dereferenced by a
replay. (2) `owf_step_log.result` is the concluded attempt's diagnostic record for 90 days; nothing
that outlives the window reads it. The sweep and compensation name an intent by the
`transition_request_id` on its `owf_provisioning_intent` row. (3) `owf_step_log` has no UPDATE
grant and a DELETE grant held by the `retention-purge` role alone. Its 90-day window runs from
`received_at` whatever the instance's state. The purge roster's "never a row whose instance is not
terminal" is scoped to the ≥ 400-day group.

**Rationale**: a replay recovered its output through `outcome_ref` from a step-log row purged at
90 days, while registry rows are kept as long as a replay can arrive and an instance can outlive
90 days under fresh `P90D` ceilings. A replay after the purge would have found a settled key with
no answer (re-review A-1). `01 §3.7` also listed `owf_step_log` among tables with no DELETE grant
while the purge deletes it. **Precedent**: Lifecycle settles its idempotency record with an
"immutable settled_response" and replays that snapshot rather than a rebuilt response (Lifecycle
`01 §3.6` *Transition commit* `inst-settle-success`, `01-foundation.md:768`; `orders_idempotency`,
`01-foundation.md:1834-1851`).

**Propagated**: `design/01-foundation.md` §3.6 (the replay sequence and its description), §3.7
(`owf_step_log`, `owf_idempotency_registry`, *Partitioning, retention and immutability*), §3.8
(retention purge), §4.3; `DESIGN.md` §3.7 (inventory and retention register). Related: D-103,
D-104.

### D-168 (H) A settlement that cannot commit aborts whole and answers the canonical infrastructure Problem

**Accepted (2026-09-28).** *(amends D-103)*

**Decision**: a unit of work rolls back entirely, and no later step runs, when the audit append
or its encoding fails, when the producer enqueue fails (schema validation, serialization, the 64
KiB bound, the enqueue itself), when any other statement fails, or when the holder fence matches
zero rows. It records nothing about the failure. A single-transaction operation leaves the
registry as it found it (none, or `open`). A committed-lease operation leaves its record
`in_flight` until the lease dies, and then §4.3 *Lease-expired* applies. The caller gets the
canonical `ServiceUnavailable` (503) for a known temporary unavailability (a lost connection, a
serialization or deadlock failure, a lock or statement timeout, the zero-rows fence). Anything
else gets the canonical `Internal` (500): an audit encoding failure, an enqueue the SDK refuses,
any other unexpected persistence error. Neither carries a Workflow reason, so the catalogue counts
are unchanged. A deterministic failure is not settled as a permanent failure. No `catch` of the
canonical definition matches 500, so the invocation faults and the liveness pass raises
`invocation-dead`.

**Why not 503 for everything.** `*transient` retries 429, 503, 504 and 409 (`10 §2.2`). A 503 for
a deterministic failure would burn five attempts on the same failure before the same fault.
**Why not settle it permanent.** The transaction that would record it is the one that failed, and
a settlement without its declared event breaks `01 §4.7`.

**Rationale**: only the zero-rows fence had a defined abort (re-review B-4); a failed append, a
failed enqueue and a database error left the registry state and the answer unstated.
**Precedent**: Lifecycle `01 §3.6` *Transition commit* `inst-if-audit-fails` ("roll back the
entire transaction, including … idempotency changes, then RETURN the sanitized canonical
infrastructure Problem") and `inst-enqueue-outbox` ("IF validation, serialization or enqueue
fails, abort the transaction"), `01-foundation.md:765-767`; and its *Infrastructure-error
termination* mapping of a temporary outage to 503 and other persistence failures to 500
(`01-foundation.md:853-862`). The 500 fault follows the definition's own `unknownStage` and
`orderTaskExhausted` (`10 §3.6`).

**Propagated**: `design/01-foundation.md` §3.3 (the envelope guarantee, the outcome table), §3.6,
§3.7 (*A settlement that cannot commit aborts whole*), §4.3, §4.17 *Append rule*; `DESIGN.md` §1.2
(the audit NFR's verification). Related: D-64, D-103, D-114, D-132.

**Clarified (2026-09-28, residuals of the re-review)**: (1) "No `catch` of the canonical definition matches 500" was not true of `overdueCheck`, whose outer `catch` had no `errors` filter and no `when`, so a deterministic 500 was swallowed every tick (V-A-2). It now carries `when` status ∈ {408, 429, 503, 504, 409} and no type filter, as `compensate`'s outer catch does, so a spent budget or a timeout is still the next tick's and a 500 faults the invocation. Every other outer `catch` of `design/10` already filters by status or `when`. (2) The abort is its own row, `aborted`, in `design/01` §3.3's closed outcome set — 503 when temporary, 500 when deterministic, no catalogue reason — rather than a clause of `retryable-failure`, whose record is left `open`, which a committed-lease abort does not do (V-A-3).

### D-169 (H) `start-instance` reads the platform before its one transaction

**Accepted (2026-09-28).** *(amends D-103, D-137)*

**Decision**: `start-instance` is an exception to the record-only one-transaction shape, as
`admit-trigger`, `apply-hold` and `apply-resume` are. Once its key resolves to a first call or a
re-run, it reads the platform invocation record outside any transaction. A transient failure of
that read answers the canonical `ServiceUnavailable` (503) and writes nothing: no instance, no
binding, no step record, no audit entry, and the registry record as it was. Then one transaction
re-resolves the key under the registry row lock and does the rest: it compares the version,
inserts the instance and the binding, writes `instance-start` and settles. It commits no lease and
writes no `step-start`.

**Rationale**: the effect was described as "in one transaction" while it contained an outside
read, and the one-transaction list named only three 5 s operations with outside reads (re-review
B-5, A-6). Holding a transaction open across a platform call would pin a connection and the
registry row lock on the platform's latency. **Precedent**: the control gateway reads the same
invocation record "before any row is written" and then records in one transaction (`09 §3.6`
`inst-cs-live`, `inst-cs-record`), and the PDP outage at `inst-owf-step-pdp` answers the same
canonical 503 with nothing recorded. `admit-trigger`'s committed lease before its Lifecycle read is
not followed: here it would add one more pre-admission audit append for a read that changes
nothing.

**Propagated**: `design/01-foundation.md` §3.3 (`start-instance`, *What each receipt records*),
§3.6 (*`start-instance` binds the definition version*), §3.7 (*A record-only operation holds its
lease*), §4.3. Related: D-76, D-137.

### D-170 (M) One table says what each receipt records; `audit_kind` is the success kind

**Accepted (2026-09-28).** *(amends D-103)*

**Decision**: `01 §3.3` *What each receipt records* maps every receipt class to its step-log row
and its audit entry, and `audit_kind` names the success settlement's kind only. The rules:
- A call refused before registry resolution writes neither a row nor an entry: it has no registry
  record for the NOT NULL `receipt_ordinal`, and its key may be invalid.
- The start of an attempt writes `step-start` only where a lease commits on its own (the
  committed-lease operations).
- A retryable failure writes `retry`, and a deadline cut `timeout`.
- A permanent failure writes the `audit_kind` carrying the reason, or `step-completion` where that
  kind needs a `phase_to`.
- An absorbed replay writes an `absorbed` row (a new `owf_step_log.outcome` member) and no entry.
- A key conflict writes a `permanent-failure` row and no entry.
- `still-processing` and `aged-out` write their row and no entry.
- An aborted unit of work writes nothing.
§4.6's completeness check counts settlements and committed leases, not receipts.

**Rationale**: the envelope wrote kinds other than `audit_kind` (the replay sequence's retry entry),
`timeout` was never assigned, and nothing said what the non-settling receipts write (re-review
B-6). **Precedent**: `02 §3.6`'s duplicate absorption already writes a step-log row for an absorbed
attempt and no audit entry, and Lifecycle's registry writes nothing on a refusal before
resolution; no platform precedent fixes an audit kind per receipt class, so this is the smallest
table consistent with the existing kinds and the phase-shape CHECKs.

**Propagated**: `design/01-foundation.md` §3.3 (contract table, *What each receipt records*), §3.6
(the replay sequence), §3.7 (`owf_step_log`), §4.6, §4.13; `design/02-triggers-and-start.md` §3.7;
`DESIGN.md` §1.2. Related: D-103.

**Clarified (2026-09-28, residuals of the re-review)**: `owf_step_operation.audit_kind` reads "its **success** settlement writes (§3.3 *What each receipt records*)" (V-A-4); `owf_step_log.settled_at` is the commit time of the transaction that wrote the row and `deadline_at`, on a row whose receipt ran no effect, the deadline that would have applied, so the non-settling rows of the table have defined values (V-A-9).

### D-171 (H) An early evaluation settles success with `due: false`

**Accepted (2026-09-28).** *(amends D-102's application in `01 §4.15`)*

**Decision**: an operation that finds a stored deadline not yet reached by database time settles
**success** with `due: false` and the next round. It does not act and never answers
`retryable-failure`.

**Rationale**: `01 §4.15` required `retryable-failure` for an early wake-up. That answer contradicts
D-102's rule that a re-check answer recording nothing is a settled success returning the next
round, would leave the key `open`, and would spend the task's retry budget on a clock difference.
It is also not what the definition expects: `waitExpected` routes on `due` (re-review A-3).
**Precedent**: D-102 rule 1 itself and `10 §3.6` (b)'s re-check loop.

**Propagated**: `design/01-foundation.md` §4.15. Related: D-102.

**Clarified (2026-09-28, residuals of the re-review)**: `DESIGN.md` §4.5 still said an early wake-up answers `retryable-failure`; it now states this decision, and its clock-skew sentence states `design/01` §3.3's refusal before registry resolution (the canonical 503, nothing recorded) (V-A-1).

### D-172 (H) A pre-admission `admit-trigger` audit entry may carry no seller

**Accepted (2026-09-28).** *(amends D-76; D-60 unchanged)*

**Decision**: `owf_audit_entry.seller_tenant_id` is nullable only on a pre-admission
`admit-trigger` entry whose seller is not yet known: the `step-start`, and the settlement entry
of an attempt whose Lifecycle read failed or returned another resource tenant's order. A CHECK
admits NULL only with `event_kind` ∈ `step-start` · `step-completion` · `retry` · `timeout` and a
`step_id` naming `admit-trigger`. The writer rule carries the half a CHECK cannot: only the
guarded pre-admission append of D-173 writes NULL, and every instance-path entry copies the
instance's seller. A NULL satisfies no seller-scoped predicate (`Eq`, `In`, `InTenantSubtree`),
and no read may treat it as a wildcard. Such an entry is read as part of its chain, by
`correlation_id` under the instance later bound to it, and before that only by the SELECT-only
verifier and checkpoint worker. `09` exposes no audit route today.

**Rationale**: the column was NOT NULL, but D-76 resolves the seller from the Lifecycle read that
`admit-trigger` makes after its `step-start`, and a failed read yields none, so the first
pre-admission entry could not be written (re-review B-1). The frozen v1 encoding already frames
NULL as `0x00`, distinct from a framed UUID (`01 §4.17`), so the digest is defined and D-60 needs
no new `hash_version`. **Precedent**: Lifecycle's own audit table leaves its tenancy axes NULL
where the aggregate was not resolved — `audit_tenant_id`, `resource_tenant_id` and `order_id` of
`orders_transition_audit` are "NULL for unresolved refusals" (Lifecycle `01 §3.7`,
`01-foundation.md:1606-1613`, Lifecycle D-98, D-104) — and its idempotency record keeps
`order_id` NULL while no order exists (`01-foundation.md:1827`). Both are nullable axes scoped to
the rows written before the owning record is known.

**Propagated**: `design/01-foundation.md` §3.7 (tenancy note, `owf_audit_entry` schema,
constraints and the seller-scoped read), §4.11, §4.17 *Acceptance evidence*;
`design/02-triggers-and-start.md` §3.6 `inst-at-settle`, §3.7. Related: D-60, D-76.

**Clarified (2026-09-28, residuals of the re-review)**: the CHECK is written as SQL, `step_id = 'admit-trigger' OR step_id LIKE 'admit-trigger:%'`, over the `step_id` form `design/01` §3.7 now states: `{operation}`, or `{operation}:{subject reference}` where one applies (V-A-8).

**Amended by D-186 (2026-09-28)**: "`09` exposes no audit route today" no longer holds: `GET …/workflows/{orderId}/audit` lists a NULL-seller pre-admission entry through the instance later bound to its correlation, and never one whose chain no instance was bound to.

### D-173 (H) READ COMMITTED; the pre-admission append allocates and checks in one statement, and `start-instance` collides with it symmetrically

**Accepted (2026-09-28).**

**Decision**: every Workflow transaction runs at READ COMMITTED. An audit append takes one of three
paths:
- **Instance path**, when the instance row exists: lock the row and increment `audit_sequence`.
- **Pre-admission append**, for a correlation with no instance row: read the chain's head, build
  and hash the row at head + 1, and insert it with `INSERT … SELECT … WHERE NOT EXISTS (instance
  for the correlation)`. Zero rows means an instance committed in between, so the writer appends
  through the instance path in the same transaction. A unique violation on `(correlation_id,
  sequence)` re-executes the transaction once, never the effect, and a second one aborts with the
  canonical 503 of D-168.
- **`start-instance`**, in its inserting transaction: read the head, initialise `audit_sequence` =
  head + 1 and write `instance-start` there. It collides and re-executes in the same way.

**Why the argument holds.** Paths 2 and 3 both write at head + 1 of a head they read, and the race
ends in one of two ways:
- `start-instance` committed before the pre-admission statement began. READ COMMITTED shows the
  `NOT EXISTS` the instance, so the append goes through the row lock.
- Otherwise both transactions insert the same `(correlation_id, sequence)`. The second waits on
  the first's uncommitted index entry, one fails, and it re-reads the head.

An entry above the instance's head would need a head that includes `instance-start`. That head is
visible only after `start-instance` committed, and the later `NOT EXISTS` then sees the instance.
So no pre-admission entry can land behind the counter.

**Rationale**: the pre-admission allocation "from the chain's current head … and the uniqueness
constraint arbitrates a race" left a window under READ COMMITTED. An entry committed after
`start-instance` read the head, at a sequence above it, would never conflict, and the instance's
counter would fall behind the chain for good. No isolation level was stated (re-review B-2).
**Precedent**: Lifecycle states READ COMMITTED and relies on unique constraints rather than
locks for its claims (Lifecycle `01 §3.6`, `01-foundation.md:1017-1022`). Toolkit-db offers only
session-level, non-blocking advisory locks (`libs/toolkit-db/src/advisory_locks.rs`: `Db::lock` is
one attempt, `try_lock` retries), with no transaction-scoped lock to serialise a chain that has
no row yet.

**Propagated**: `design/01-foundation.md` §3.3 (`start-instance` `audit_kind`), §3.7
(`owf_process_instance.audit_sequence`, `owf_audit_entry` `sequence`, constraints, *Chain
allocation*, the abort rule's 503 list), §4.17 *Append rule*, *Acceptance evidence*;
`design/02-triggers-and-start.md` §3.6 `inst-at-settle`. Related: D-59, D-60, D-168.

### D-174 (H) The quarantine counts from a per-family history of presented attempts

**Accepted (2026-09-28).** *(amends D-152)*

**Decision**: each `owf_process_instance.key_rounds` family entry gains `presented:
[{attempt, key}]`. The envelope records a minted attempt (≥ 1), with the full registry key, in the
transaction that first inserts a registry record for a key ending in that attempt. The history is
ordered by `attempt` and trimmed to the last three. `retry-step` judges each element by its key's
registry record at the time it runs:
- `in_flight` with a dead lease **counts**;
- `settled` (success, a permanent failure, or a lookup settlement) or `open` (a retryable failure
  that exhausted the budget) **resets** the run;
- a live lease does neither.

It refuses `poison-step` when the three most recent presented attempts all count. An unpresented
attempt has no element and is skipped, as D-152 required. No outcome is stored, because a dead
lease can still be settled by lookup and must then reset the run. A single-transaction operation
never leaves a counting record, so the guard trips only on operations that commit a lease.

**Rationale**: the attempt key is `…:{round}:{attempt}` and `key_rounds` held only the latest
`{round, attempt}`, so the round an attempt was presented under, and hence its registry record,
was recorded nowhere, and "presented" could not be evaluated (re-review B-7). **Precedent**: none
in the platform or the BSS gears for an operator-retry quarantine (D-152). The history reuses the
instance row's existing per-family counters and the registry's tombstone retention (D-104), which
keeps every presented key's record while the instance lives.

**Propagated**: `design/01-foundation.md` §3.3 (*Rounds and attempts* rule 3, `retry-step`),
§3.7 (`key_rounds`), §4.13. Related: D-102, D-104, D-119, D-152.

### D-175 (M) `terminate-instance` takes only the two permitted edges; from `suspended` or `parked` it refuses `fence-not-claimed`

**Accepted (2026-09-28).** *(amends D-82's application in `01 §3.3`)*

**Decision**: `terminate-instance` moves the phase only along `compensating → terminated` and
`started → terminated`. From `suspended` or `parked` it settles `permanent-failure` with
`fence-not-claimed` (400), writes `step-completion` carrying the reason, and closes nothing.

**Why this reason.** `version-mismatch` is `Aborted` (409). The definition's `*transient` catch
would re-issue it under the same key to the same refusal until the budget is spent.
`fence-not-claimed` is `FailedPrecondition` and names the precondition exactly: the only way from
these phases to `terminated` is through the cancellation fence. A 400 matches no `catch`, so the
invocation faults and the liveness pass raises `invocation-dead`, as `reportOutcome`'s
`fence-not-claimed` already does.

**Rationale**: the effect said "from any non-terminal phase per §3.7", but §3.7 permits only the
two edges: there is no `suspended → terminated` edge (D-82), and a parked instance is unwound
only through the fence (re-review A-2). **Precedent**: slice 06's own use of `fence-not-claimed`
for `compensate-order` and `report-outcome` without a fence (`06 §3.6` `inst-co-fence-guard`,
`inst-ro-gate`), and D-166, which answers a state the design excludes with a refusal that faults
rather than a route.

**Checked in `10 §3.6`**: `terminateAborted` follows the fence, so the phase is `compensating`.
`terminateRejected` and `terminateCompleted` follow a Lifecycle transition that succeeded, so
Lifecycle does not hold the order. The recorded phase is then `started` unless an Orders
suspension is still open because the resume that closed it at Lifecycle has not yet been
consumed. That residual is left to a follow-up review and is not routed here; until then such a call faults the invocation to `invocation-dead`.

**Amended by D-183 (2026-09-28)**: the residual is routed. Before `terminateRejected` and
`terminateCompleted` the definition closes such a suspension through `apply-resume`
`trigger: poll`, which now treats a `rejected` or `completed` order as no longer held, so
`terminate-instance` runs from `started`.

**Propagated**: `design/01-foundation.md` §3.3 (`terminate-instance`), §3.7 (the phase table's
note). Related: D-82, D-114, D-166.

### D-176 (L) The nesting invariant is enforced in four places, and §2.1 says so

**Accepted (2026-09-28).**

**Decision**: `01 §2.1` *Five bounds, two owners* names the four enforcement points that `01 §4.2`
states. They are the definition validation rule at publish (`10 §2.2` rule 4), the publish job's
`wave1`-versus-overdue check (`10 §4.2` step 1, D-159), the audited seller-policy write
(`07 §4.8` item 8, D-134, D-140), and readiness for a `deadline_ms` change.

**Rationale**: `01 §2.1` said "checked in two places" while `01 §4.2` says four (re-review A-8).
**Precedent**: `01 §4.2` itself; no behaviour changes.

**Propagated**: `design/01-foundation.md` §2.1. Related: D-126, D-134, D-159.

### D-177 (H) The six events publish to this gear's own topic, named in the abstract type's `x-gts-traits`

**Accepted (2026-09-28).** *(amends D-58; mirrors Lifecycle `01 §4.7` *Traits, and versioning*)*

**Decision**: the abstract `cf.bss.orders_workflow.event.v1~` declares
`x-gts-traits` `{ topic: gts.cf.core.events.topic.v1~cf.bss._.orders_workflow.v1,
allowed_subject_types: [gts.cf.bss.orders.order.v1~], partition_key: /subject }`, and every
concrete type's resolved schema keeps these values. The topic is a `TopicV1` instance this gear
owns, named in the package's default namespace `_` as Lifecycle names `…~cf.bss._.orders.v1`,
with no `retention` (the broker default). The gear registers it in `types-registry` at init with
the seven event types, before readiness. The `DbProducer` declares the topic and the
`gts.cf.core.events.event.v1~cf.bss.orders_workflow.*` pattern, and `prepare_all()` refusing
either leaves the instance not ready. `/subject` is set explicitly, overriding the SDK helper's
`/tenant_id`. Per-order ordering holds within this topic. The claim that Lifecycle's state events
for an order share its partition is withdrawn: they are on another topic, and the broker scopes
ordering to one (topic, partition). A consumer correlates the two streams by `orderId` and
`orderVersion`, never by partition. The produce grant on this topic and each consumer's grant
are provisioned for it separately (`UPSTREAM_REQS.md` §2.7).

**Rationale**: the six types declared no `x-gts-traits`. The SDK's `EventTraits` has
`deny_unknown_fields` and no default for `topic` (`event-broker-sdk/src/gts.rs`), so the derived
types could not be registered. `01 §4.7` also claimed co-partitioning with Lifecycle's events,
which would hold only on a shared topic (re-review P-2). The user decided on a gear-owned topic:
sharing Lifecycle's topic would put two gears' producers on one stream that Lifecycle's grant
names as its own. **Precedent**: Lifecycle `01 §4.7`'s traits block (topic, subject type,
`/subject`), its `prepare_all()` readiness gate (Lifecycle `01 §3.8`), and the SDK's rule that
the owning gear registers its topic and types at init (`gts.rs` module header).

**Propagated**: `design/01-foundation.md` §3.2 *Platform event producer adapter*, §3.4, §3.8,
§4.7; `DESIGN.md` §4.4; `ADR/0008`; `UPSTREAM_REQS.md` §2.7. Related: D-58.

### D-178 (H) A producer-registration rotation rejects every message queued under the old id

**Accepted (2026-09-28).** *(amends D-58; mirrors Lifecycle `01 §3.7` *Platform-managed producer persistence*)*

**Decision**: the design states the bulk case of `UnknownProducerRegistration::RegisterNew`.
When the broker answers `UnknownProducer`, the SDK registers a replacement for future enqueues
and returns `Reject` for the message (`Rotated`) and for every later message still carrying the
old id (`AlreadyRotated`). Every committed, unpublished process event in any of the 16 queue
partitions then becomes a toolkit dead letter. The operator sees a burst of pending dead letters
on `bss-orders-workflow-events` with the SDK's "producer_id … is unknown" reason, under the
pending-dead-letter alert. Process state is unchanged. This is named as the expected bulk case of
the shared dead-letter recovery ask. Re-drive republishes each event under the current producer
with its original id and payload, possibly after later events for the same orders. Consumers
absorb duplicates by event id and late events by `orderVersion` plus an authoritative read.
Until the ask lands, a rotation leaves those events permanently missing.

**Rationale**: the design registered with `RegisterNew` and treated identity loss as one
permanent fault per message, but the SDK (`producer/outbox.rs` `handle_unknown_producer`) rejects
the whole queued backlog (re-review P-1). **Precedent**: Lifecycle's sentence "Registration
rotation affects future enqueues and permanently rejects a message carrying the unknown old
producer identity, which is why consumers cannot treat the stream as a ledger", mirrored
verbatim; the bulk consequence is read from the SDK source, since Lifecycle states only the
single-message form.

**Propagated**: `design/01-foundation.md` §3.6 *Platform producer-outbox publication* step 5,
§3.7 *Platform-managed producer persistence*, §4.7; `DESIGN.md` §4.4; `ADR/0008`;
`UPSTREAM_REQS.md` §2.7. Related: D-58.

### D-179 (M) A settlement enqueues zero or more events of its declared type, at most one per subject

**Accepted (2026-09-28).** *(amends D-58's "one typed event per event-declaring step")*

**Decision**: `01 §4.7` and the §3.3 `declared_event` contract field read "zero or more events
of the declared type per settlement, exactly as the operation's contract states, at most one per
subject it names, enqueued in the settlement transaction". The 64 KiB bound applies to each
event. Each registering slice's row states its count. `open-gates` and `escalate-gate` emit one
per gate. `dispatch-wave1-create`, `dispatch-wave2-activate` and `reconcile-intent` emit one per
line. `begin-fulfillment` and `report-outcome` emit one for the order. `verify-override` emits
one for its line. `owf_step_operation.declared_event` keeps naming the type only.

**Rationale**: §4.7 required exactly one event per event-declaring step, while slice 05's dispatch
operations and slice 03's gate operations declare one per line or per gate (re-review B-3).
**Precedent**: Lifecycle's "construct exactly one typed event" per transition (`01 §3.6`
*Attempt Transition* step 24, `inst-enqueue-outbox`) is the base case. No platform or BSS gear
emits several events per settlement, so the per-subject rule has no precedent and is the
smallest rule that fits the contracts already written.

**Propagated**: `design/01-foundation.md` §3.1 *Outbox entry*, §3.1 relationships, §3.3, §3.6,
§4.7; `design/03-approval-execution.md` §3.3; `design/04-fulfillment-plan.md` §3.3;
`design/05-provisioning-intents.md` §3.3; `design/06-saga-and-compensation.md` §3.3;
`design/07-manual-tasks.md` §3.3; `DESIGN.md` §2.1; `ADR/0008`. Related: D-58.

**Clarified (2026-09-28, residuals of the re-review)**: `design/01` §4.7's heading reads "Declared events per settlement, and the six named process events only" (anchor updated in its TOC entry and in `design/10`), and `ADR/0008`'s consequences say "the producer messages its contract requires (zero or more)" (V-A-5, V-C-1, V-C-2).

### D-180 (H) The suspension closure port also clears `owf_process_instance.suspended`

**Accepted (2026-09-28).**

**Decision**: slice 08's suspension closure port `close_suspension(correlationId, closedReason)`,
called by `run-cancellation-fence` in fence step 1, closes the `open` or `resume_ahead`
suspension row and clears `owf_process_instance.suspended` in the fence's unit of work. On an
instance with no such row it changes nothing. The flag's writers are now: `apply-hold` sets it;
`apply-resume` and the closure port clear it. `unpark` still never clears it.

**Rationale**: `06 §3.6` `inst-fence-step1` already said the port clears the flag. `08 §3.7`
declared the port without saying so, and `01 §3.7` named only `apply-hold` and `apply-resume` as
the flag's writers (re-review C-1). The user decided that the port clears it. An unwind is not
paused by a hold (`06 §4.7` item 7). The unwind path has no `apply-resume`. A flag left set
would give the compensating legs a hold predicate they must not see. **Precedent**: the fence's
other step-1 closure port, slice 03's `close_open_approvals`, which settles the gate and park
state that slice owns in the same unit of work (D-149). The port pattern is D-153's: the owning
slice writes its own record through a port the caller invokes inside its transaction.

**Propagated**: `design/01-foundation.md` §3.3 (`park`/`unpark`), §3.7
(`owf_process_instance.suspended`); `design/08-hold-and-cancel.md` §3.7;
`design/05-provisioning-intents.md` §3.4. `design/06-saga-and-compensation.md` §3.6 is
unchanged. Related: D-82, D-149, D-153.

### D-181 (M) The step-retry route records its task resolution request through slice 07's intake port

**Accepted (2026-09-28).** *(amends D-85)*

**Decision**: slice 07's Operator Task Queue exposes the in-process port
`record_resolution_request(taskRef, action = retry, requestedBy, justification?,
idempotencyKey)`. Slice 09's Control Operation Gateway calls it inside its own unit of work for
`POST …/steps/{stepId}/retry`, naming the step's own task or the `invocation-dead` task. The port
inserts the `requested` row with the task's `row_version` read under its lock and answers the
`requestRef`. The one-pending-request index refuses a second request `version-mismatch`. The
audit entry, the projection and the delivery stay the gateway's. The port and the queue's own
routes are the only insert paths into `owf_task_resolution_request`. Slice 09 is built after 07,
so the call is a forward edge and adds no back-edge.

**Rationale**: `07 §3.7` said the table is "inserted only by" the Operator Task Queue, while
`09 §3.2`, `09 §3.6` `inst-cs-record` and D-85 had slice 09's gateway insert it for a step retry.
That breaks the sole-writer rule of `DESIGN.md` §3.7 (re-review C-3). **Precedent**: the
design's port pattern, where the owner writes its table through a port called in the caller's
unit of work (D-139's build-order edges; slice 05's re-arm port, D-153).

**Propagated**: `design/07-manual-tasks.md` §3.2, §3.7; `design/09-read-and-authz.md` §3.1,
§3.2, §3.3, §3.4, §3.6; `design/README.md`; `DESIGN.md` §3.7. Related: D-85, D-139, D-153.

### D-182 (M) The re-authorization mark is the cancellation fence's one rewritable stamp

**Accepted (2026-09-28).** *(amends D-84's application in `06 §3.7`)*

**Decision**: `owf_cancellation_fence.reauthorization_required_at` is the stated exception to the
fence's write-once stamps. `compensate-order` sets it on a `withdrawn` re-check at
`pre-compensation`. Only `run-cancellation-fence`, absorbing a newly authorized cancel against
the run (`06 §4.3`, applied in `inst-fence-claim`), clears it and replaces `cancel_request_ref`
in the same write. A later `withdrawn` re-check may set it again. `cancel_request_ref` changes
only in that absorption once set. `06 §3.7` names `run-cancellation-fence` as that writer.

**Rationale**: `DESIGN.md` §3.7 and `06 §3.7` said a stamp once set never changes, and `06 §3.7`
named only `compensate-order` as the mark's writer, while `06 §4.3` clears it on absorption
(re-review C-4). **Precedent**: none in the platform or the BSS gears for a clearable mark on a
fence row. The smallest rule states the one exception where the absorption table already
applies it.

**Propagated**: `design/06-saga-and-compensation.md` §3.7; `DESIGN.md` §3.7. Related: D-84,
D-106.

### D-183 (H) A suspension a Lifecycle resume overtook is closed by the poll before `terminate-instance`

**Accepted (2026-09-28).** *(amends D-130's poll rule in `08` `inst-ar-poll`; closes the residual D-175 left open)*

**Decision**: before `terminateRejected` and before `terminateCompleted`, the definition calls
`apply-resume` with `trigger: poll` under its next `resumePollRound` whenever it holds a
`suspensionRef` (`beforeReject` → `pollBeforeReject`, `beforeComplete` → `pollBeforeComplete`,
`10 §3.6` (a), (b)). The call is `pollHeld`'s, with the same body, key family and export. The poll
now treats a `rejected` or `completed` order at the instance's version as no longer held. It
closes the open suspension `resumed-by-read`, clears `owf_process_instance.suspended` and moves
the phase `suspended → started`. `terminate-instance` then takes its permitted
`started → terminated` edge. `on_hold`, a newer version and the other terminal states
(`cancelled`, `expired`, `fulfillment_failed`) still answer `still-held`, which leaves them to the
lifecycle arm as before. No switch follows the call. `still-held` cannot arise there, and were it
answered, `terminate-instance` refuses `fence-not-claimed` and the invocation faults
(D-114, D-175). `failedTaskRefs[]` is empty at both calls: before `begin-fulfillment` nothing is
deferred (D-143), and a completion implies every line `activated`, while a deferred failure stays
on a task that is not.

**Rationale**: D-175 recorded the residual. A Lifecycle resume can overtake an open Orders
suspension. One way is for the resume stage to answer `still-held` on the stage-level arm and
return with `suspensionRef` kept (`stillHeldBack`, D-141). Another is for a held wait's tick to
win the race against the resume's `listen`, since `afterHeldReflect` and `afterHeldWait` poll only
on every third tick. The next tick then calls `reflect-verdict` or the completion report on an
order Lifecycle no longer holds. Neither operation reads Orders' hold: `03 §3.6`
`inst-rv3-resolve` refuses only a terminal instance, and `06 §3.6` `inst-ro-completed` checks only
the fence and the completion predicate. So Lifecycle settles `rejected` or `completed`, and
`terminate-instance` is called from `suspended`, refuses, and faults to `invocation-dead`. The
poll could not close such a suspension as written, because it answered `still-held` on every
terminal order. `rejected` and `completed` differ from the other terminal states. Lifecycle
reaches them only by this gear's own reflection and completion report, from `pending_approval` and
`in_fulfillment` (Lifecycle `01 §4.3` rows 10 and 13), so a Lifecycle resume (row 22) came first
and no hold is in force. **Precedent**: the held-poll pattern itself, `pollHeld` and `pollResume`
(D-130, D-133), and the poll `onLeftPark` makes once on the way out of the park loop before a
stage that does not poll. The read is the D-141 rule that a suspension closes only when Lifecycle
no longer holds the order. For no switch on an answer that cannot arise, the unwind's
`reportOutcome` follows no switch for `held` (`10 §3.6` (c)). **Rejected**: routing a
rejection or completion taken while suspended through the fence (`terminal-event`). That
would move a completed order through compensation. Letting `terminate-instance` close the
suspension was rejected too, because it would add the `suspended → terminated` edge D-82 and
D-175 exclude.

**Propagated**: `design/10-process-definition.md` §3.6 (a), (b), (e), §4.1, §4.5;
`design/08-hold-and-cancel.md` §3.3, §3.6 `inst-ar-poll`, §4.7 item 10;
`design/01-foundation.md` §3.3 (`terminate-instance`, the poll's round row); `DESIGN.md` §1.2
(`fr-owf-hold-resume`). Related: D-82,
D-114, D-130, D-133, D-141, D-143, D-175.

**Clarified (2026-09-28, residuals of the re-review)**: `design/08` §4.7 item 10 said the pre-termination poll routes `failedTaskRefs[]` as item 8 does once `begin-fulfillment` has committed; it now exempts that call and states why its list is empty, as this decision does (V-E-1).

### D-184 (M) A consumed acceptance event and a consumed re-authorization signal are their own checkpoints

**Accepted (2026-09-28).** *(amends D-147)*

**Decision**: in the fulfillment stage (`10 §3.6` (b)), `afterEligibilityChange` routes the
acceptance arm to `enterAdmitAcceptance` (`stageLoop: admitAcceptance`) before `admitAcceptance`,
and the re-evaluation arm to `enterEligibility` (`stageLoop: eligibility`) before
`evaluate-payment-auth-eligibility`; the stage's `enter` switch gains the cases `acceptance`
(→ `admitAcceptance`) and `reevaluation` (→ `eligibility`). The acceptance checkpoint also covers
the evaluation `acceptanceRecorded` leads to: a re-issued admission answers its settled `advance`
and the evaluation re-runs under the same `evaluationSeq`. Every route on replaces the checkpoint:
`enterPlan` and `enterEligibilityWait` as before; `onAcceptanceAdmission`'s `back` now goes
through `enterEligibilityWait`; `onEligibility`'s `planFailed` case goes through a new
`enterPlanFailFast` (`stageLoop: planFailFast`, whose `enter` case already exists for the
failure stage's route, D-109); and `toLifecycle` writes `stageLoop: awaitEligibilityChange`, so a
`back` from fragment (f) waits again rather than re-admitting an acceptance whose `terminate`
answer `terminalEvent` then declined. The PT5M poll shares the re-evaluation route; its payload is
empty and its re-issue is absorbed.

**Rationale**: both calls consume an arm's payload after the fork, as `record-decision` and
`resolve-manual-task` did before D-147. The stage's checkpoint was `awaitEligibilityChange`, so a
lifetime ceiling that cancelled `process` during `admitAcceptance`, the evaluation after it, or the
evaluation a `reauthorize-requested` signal asked for re-entered the wait: the consumed
`OrderAcceptanceRecorded` event and the operator's re-authorization request were neither
re-issued nor delivered again, and only the PT5M poll re-read the payment status, under
`trigger: poll` with no `requestRef`. The re-issue keeps its key: `admit-trigger`'s event-scoped
key comes from `lifecycleEventId` and `triggerKind`, and the evaluation's
`{…}:evaluate-payment-auth-eligibility:{evaluationSeq}` from `evaluationSeq`, which only that
evaluation's own export advances. The body members `lifecycleEventId`, `triggerKind` and
`requestRef` are already saved by `ceilingEntry` and restored by `backToProcess` (D-164);
`eligibilityTrigger` needs no save, because no ceiling arm, and neither the lifecycle nor the
cancel stage taken from one, writes it (D-164's sweep). **Precedent**: D-147's
`enterRecordDecision`, `enterResolveTask`, `enterResolveCompensationTask` and
`enterResolveCeilingTask`, with D-147's rule that the routes back to the waiting fork go through
the stage's `enter…` task; and D-121's `enterParkLoop` before `park`.

**Sweep**: every other arm that sets a payload was checked. The decision arm (`recordDecision`) and
the failure stage's, the unwind's and the ceiling wait's task-resolution arms (`resolveTask`,
`resolveCompensationTask`, `resolveCeilingTask`) are already checkpoints (D-147). The hold,
resume, lifecycle and cancel arms enter a shared stage whose first call is its re-entry point, with
the payload restored after a ceiling (D-164). The confirmation arm's `reconcileHint` is a
`composable` wake-up, which the barrier's PT30S poll repeats over every due intent, so dropping it
loses nothing. The tick, probe, poll, SLA, retry-leg and lifetime arms carry no payload.

**Propagated**: `design/10-process-definition.md` §3.6 intro, (b) (`enter`,
`afterEligibilityChange`, `enterEligibility`, `enterAdmitAcceptance`, `onAcceptanceAdmission`,
`toLifecycle`, `onEligibility`, `enterPlanFailFast`) and its description; D-147.

### D-185 (M) A key ages while its instance lives only if its operation submits downstream; an aged-out key is re-issued under an operator-minted successor

**Accepted (2026-09-28).** *(amends D-104, D-105, D-108)*

**Decision**: (1) `owf_step_operation` gains `submits_downstream`. It is true for the nine
operations whose effect submits to a downstream: `dispatch-wave1-create`,
`dispatch-wave2-activate` and `compensate-order` (Subscriptions intents); `reflect-verdict`,
`begin-fulfillment`, `report-spawn-signal` and `report-outcome` (Lifecycle transitions); and
`open-gates` and `escalate-gate` (the Generic Approval gate submission and escalation command). It
is false for the other twenty-six. Those are record-only, or their only outside call is a read
(`admit-trigger`, `apply-hold`, `apply-resume`, `start-instance`, `obtain-verdict`,
`record-decision`, the slice-04 evaluations and `construct-and-freeze-plan`,
`reread-draft-liveness`, `reconcile-intent`, `verify-override`, `authorize-cancel`).
(2) A key of a `false` operation does not age while its instance is non-terminal. Its `expires_at`
is evaluated only once `terminal_outcome` is set. A `trigger`-family key whose admission started
no instance ages at `expires_at` as before. (3) A key of a `true` operation ages at `expires_at`
whatever the instance's state, and an aged-out key is never re-run. When its family holds an
`attempt` minted after the aged key's `created_at`, the envelope resolves the re-issued aged key
under the successor key, which carries the latest such attempt. It is a first call the first
time, and resolves as the successor's own record says thereafter. Without such an attempt the
answer stays `aged-out` (400). (4) The attempt is minted only on an operator's recorded decision,
through `retry-step`'s effect. Two new places run that effect. The control gateway's recording of
the `invocation-dead` task's `retry` (`09 §3.6` `inst-cs-record`) mints the next attempt of each
such family whose key resolves `aged-out`. The dead-instance unwind (`01 §4.16` item 2) does the
same after a recorded cancel. `reflect-verdict`'s existing 400 catch reaches its own order-scope
task, whose `retry` already mints the attempt. No `catch` is added to `10`. (5) The key lifetime
stays 30 days, is written once and is never extended. Its derivation no longer claims to be at or
above the maximum retry horizon.

**Rationale**: `01 §3.7` justified 30 days as "at or above the maximum retry horizon, which
includes manual-task resolution and one or more hold/resume cycles". That horizon is not bounded.
A lifetime-ceiling park waits for an operator. A hold lasts while Lifecycle holds the order, which
nothing bounds when its TTLs are unset. An `invocation-dead` task waits for its re-drive. Take a
checkpoint call interrupted before it settled: its key is `open`, or `in_flight` with a dead
lease. After more than 30 days it was re-issued under the unchanged key and answered `aged-out`.
No `catch` in `10` routes that 400, so the invocation faulted. The re-drive then re-issued the same
key and faulted the same way, and the order could only be cancelled. Keys age for one reason: so
that nothing is resubmitted downstream under a key the downstream may have forgotten (D-25). For
an operation that submits nothing downstream the reason does not apply. Its late re-run writes
only Orders' own rows, under the key's fingerprint, the instance `row_version` and the
operation's guards, exactly as a re-run inside the lifetime does, and a read re-reads. For an
operation that submits downstream the reason applies, so its key keeps aging. The new key its
re-issue needs comes from an operator, as rule 3 of `01 §3.3` *Rounds and attempts* already
requires of every attempt. The re-drive cannot carry that attempt: the platform re-runs the
faulted task with the same parameters. So the envelope resolves the key the definition still
holds under the minted successor, and records the arrival in the family's `presented` history
(D-174). A 400 `catch` was rejected. The plugin does not surface `error_code` (Q-11), so such a
catch cannot tell `aged-out` from a caller defect, and the failure rule (D-114) already sends both
to the `invocation-dead` task. Re-basing `expires_at` was rejected too, because re-basing a
downstream-submitting key is the resubmission the lifetime forbids. **Precedent**: none exact.
Lifecycle's registry is a 24-hour request cache with no process instance to outlive it (Lifecycle
`01-foundation.md:234-243`). Lifecycle's rule that expiry is logical and evaluated against
database time (`01 §4.2`, `01-foundation.md:2259`) is kept. The attempt that re-keys a step is
this design's own rule 3 (D-102).

**Propagated**: `design/01-foundation.md` §3.2 (idempotency registry), §3.3 (the outcome table,
`retry-step`), §3.7 (`owf_idempotency_registry` *Key lifetime*, `owf_step_operation`), §4.3,
§4.16; `design/09-read-and-authz.md` §3.1, §3.6 `inst-cs-record`; `design/07-manual-tasks.md`
§4.4, §4.6; `design/10-process-definition.md` §4.6; `design/05-provisioning-intents.md` §2.2;
`DESIGN.md` §3.3, §3.7; D-104, D-105, D-108.

**Amended by D-188, D-189 and D-190 (2026-09-28)**: the "no process instance to outlive it" of the precedent is this gear's problem at Lifecycle's end: a successor key changes the step key only, so a re-issue reaches Lifecycle after its 24-hour window under the same transition key and is answered by D-188's read-back, and reaches Generic Approval under the same request key, which `open-gates` now looks up before it submits (D-189). Rule (4)'s "`reflect-verdict`'s existing 400 catch reaches its own order-scope task" is withdrawn: its refusal is a settled answer, and an aged-out `reflect-verdict` key faults the invocation like every other, whose `invocation-dead` task mints the successor (D-190).

### D-186 (M) The Seller Operator reads an order's process audit trail through a paged, seller-scoped audit route

**Accepted (2026-09-28).** *(amends D-172; clarifies `02 §3.6` without a behaviour change)*

**Decision**: slice 09 registers `GET /bss-orders-workflow/v1/workflows/{orderId}/audit` as
`audit × read` on a new label, `gts.cf.bss.orders_workflow.audit.v1~`, served by a new Audit
Trail Reader. Only the Seller Operator is granted it, in seller scope (`S` `+pg`); every other
principal class is `—`. The read is one ordered set of the order's `owf_audit_entry` rows, built
from two disjoint branches with the scope applied before sorting and limiting. (i) An entry that
carries a seller is scoped on its own `seller_tenant_id` and `resource_tenant_id`. (ii) A
pre-admission entry with a NULL seller is scoped through the instance later bound to its
correlation. A NULL-seller entry of a chain no instance was bound to is not listed, and NULL is
never a wildcard. Paging is keyset on `(created_at, audit_id)`, default 50 and maximum 200,
clamped. The read fetches at most `limit + 1` rows through a new index
`(order_id, created_at, audit_id)`, which replaces `(seller_tenant_id, created_at)`. The cursor
is opaque, bound to the route, the order and the principal, and a bad cursor is refused with the
canonical `InvalidArgument` (400). A page is a live view, not an export. Each item returns the
entry's references, kind, phases, actor, idempotency key, catalogue reason, justification and
`createdAt`, and no hash field, no `audit_tenant_id` and no step-log `result`. The caller-facing
route count becomes sixteen (three pending). `02 §3.6` now states that a `start-instance`
refusal settled before the instance exists (`definition-not-bound`, `line-count-exceeded`) is
audited through the same pre-admission append as `admit-trigger`. That entry always carries the
seller, so branch (i) lists it.

**Rationale**: the PRD gives the Seller Operator the need to "view workflow status across orders
in seller scope with audit" (`PRD.md:149`). `01 §3.7` described a seller-scoped audit read and
indexed for it, but `09` exposed no route or resource type, so the complete audit log had no read.
The trail grows with process traffic: every settled re-check round, retry and timeout writes an
entry, and a `PT30S` gate loop writes thousands. An unpaged read would therefore be a
memory-amplification vector. **Precedent**: Lifecycle's audit read,
`GET /bss-orders-lifecycle/v1/orders/{orderId}/audit` (Lifecycle
`08-read-and-authz.md:75-82`, `:200-285`, `:390`, `:762-835`). It asks for a separate
`audit × read`, "not merely `order × read`" (`:622-623`), and grants the Seller Operator in
seller scope and no operational or service principal (`:1042`). It pages on
`(created_at ASC, audit_id ASC)` for exactly this amplification reason. It merges resolved and
unresolved rows as one ordered set (its D-98 and D-101) and returns actor, reason, idempotency and
correlation references, "never internal diagnostics". Two points are not followed. This gear
clamps an oversized page (`09 §2.2`) where Lifecycle refuses it. This gear keeps no read access
log, which Lifecycle writes for delegated and cross-tenant reads, because no read of this gear has
one and adding the store is outside this fix.

**Propagated**: `design/09-read-and-authz.md` §1.2, §2.2, §3.1, §3.2 (diagram, Audit Trail
Reader, endpoint mapping), §3.3, §4.1, §4.3; `design/01-foundation.md` §3.7 (`owf_audit_entry`:
`seller_tenant_id`, the index, *A NULL seller and the seller-scoped read*);
`design/02-triggers-and-start.md` §1.2, §3.6, §3.7; `DESIGN.md` §1.2, §3.2, §3.3, §3.5;
`design/README.md`; `ADR/0010`; `UPSTREAM_REQS.md` §2.8; D-172.

**Amended by D-191 (2026-09-28)**: "This gear keeps no read access log … adding the store is outside this fix" no longer holds: the audit read appends one `owf_read_access_log` row per request, served or refused, following Lifecycle's served-and-refused pattern (`design/09-read-and-authz.md` §3.3, §3.7).

**Clarified (2026-09-28)**: `design/07-manual-tasks.md` §4.5 said a failing attempt's `owf_step_log.result` "is readable only through the audit-logged support read of `09`", and `09` has no such read. It now says that no route exposes `result`, that the failing attempt's catalogue `reason` reaches the Seller Operator on this entry's audit read, whose items carry `reason` and `justification` and no `owf_step_log.result`, and that the raw `result` is readable only through database access under a role outside the gear's routes, since `design/01-foundation.md` §3.7 names no SELECT role on the step log. No behaviour changes.

### D-187 (L) A contended worker lock skips the pass; only a lock or database error is a coordination failure

**Accepted (2026-09-28).** *(amends D-62)*

**Decision**: `01 §3.8` states the two toolkit-db calls as the SDK does. `Db::lock` is a single
non-blocking attempt. Its `DbLockError::AlreadyHeld` means a peer holds the pass, and the worker
skips the pass until its next tick. `Db::try_lock` with `LockConfig` retries under the configured
bound. Its `Ok(None)` means the lock was not acquired within that bound, and the worker again
skips the pass. Only an `Err` from either call, or a database error during the pass, is a
coordination failure. On such a failure the worker stops scheduling, abandons the pass and
reacquires. A failed `release()` is reported and never read as proof of ownership.

**Rationale**: `01 §3.8` called `Db::try_lock` the "bounded non-blocking acquisition" and did not
say what a contended acquisition was. Its only rule, "stop scheduling further work on observed
coordination or database failure", could read `AlreadyHeld` — the normal answer whenever another
replica runs the pass — as a failure to act on. The SDK says otherwise. `Db::lock` is "a single
non-blocking attempt" that "returns `DbLockError::AlreadyHeld` on contention"
(`libs/toolkit-db/src/advisory_locks.rs:1617-1621`). `try_lock` is "with configurable
retry/backoff policy" and answers `Ok(None)` "if timed out or attempts exceeded"
(`libs/toolkit-db/src/lib.rs:573-582`, `advisory_locks.rs:1636-1641`). **Precedent**: Lifecycle
`01 §3.8`, "Contended passes skip/reschedule" and "release errors are reported, not treated as
proof of ownership" (`gears/bss/orders-lifecycle/docs/design/01-foundation.md:1948-1949`). The
same section repeats the "bounded non-blocking acquisition through `Db::try_lock`" wording
(`:1946-1947`); that is for its owner to correct.

**Propagated**: `design/01-foundation.md` §3.8; D-62.

### D-188 (H) A Lifecycle re-run past Lifecycle's 24-hour key window reads the order back and settles `already-applied`

**Accepted (2026-09-28).** *(amends D-102, D-166, D-185)*

**Decision**: (1) Rule 4 of `01 §3.3` *Rounds and attempts* (`inst-owf-round-lifecycle`) gains a
first case. On a Lifecycle `not-admissible`, the operation already reads the order through the
Lifecycle PDP-authorized order read. Before the `on_hold` and terminal cases, it compares that
read with the transition it submitted. When the order is at `orderVersion` in that transition's
target state, the operation settles success with the Lifecycle answer `already-applied`. It
records the answer in `owf_step_log.result`, runs its committed branch, and returns its ordinary
success output. (2) The targets are these. `reflect-verdict`: `pending_approval` for `required`,
`approved` for `not_required` and `granted`, and `rejected` for `denied`. `begin-fulfillment`:
`in_fulfillment`, which then sets `begin_fulfillment_committed_at` and enqueues
`OrderFulfillmentStarted`. `report-outcome`: `completed` for `acknowledge-completed`, where the
per-line read also shows every line `activated` under the subscription identifier the report
carries, and `fulfillment_failed` for `acknowledge-failed`. Both set
`lifecycleCall = already-applied`, which `onReport` routes with `acknowledged`. A `cancelled`
order keeps its terminal row (`reportedOutcome = terminal-event`), because an ordinary cancel
(row 15) reaches it too and the read does not show which. (3) `report-spawn-signal` needs no
read-back. Its row is a self-loop, so a past-window re-run is admitted to the guard
`spawn_signal_at IS NULL`, which refuses it `spawn-signal-already-recorded`. The operation
already answers that refusal `already-recorded`, a settled success; `05 §3.3` now says so. It
records no instant, and nothing reads one. (4) `UPSTREAM_REQS.md` §2.4 gains
`…-upreq-lifecycle-workflow-key-retention`. It asks Lifecycle to retain workflow-trigger-class
keys for at least 30 days, or to expose a read of a seam operation's stored outcome by its key.

**Rationale**: Lifecycle's registry is a 24-hour request cache. "Past the window a replayed key is
a new operation, so the window must exceed the longest caller retry horizon" (Lifecycle
`01-foundation.md:238-241`). This gear re-issues its four transition calls under unchanged keys
after a lease death or an interruption of days. D-185 keeps those keys aging at 30 days, and a
successor key changes the step key only: the Lifecycle keys of `begin-fulfillment`,
`report-spawn-signal` and `report-outcome` carry no attempt. Take a transition that committed
before its step settled, re-issued more than 24 hours later. It meets the version check, which
passes because a transition changes no commercial version. It then meets the state table, which
has no row for the trigger from the state already reached, so Lifecycle refuses it
`not-admissible` (Lifecycle `06-workflow-seam.md:352-356`). Rule 4 turned any such refusal it
did not recognise into `permanent-failure` `version-mismatch`. That faulted the invocation, and
the re-drive faulted the same way. `reflect-verdict` answered `moved` for an order already
`rejected`, and `report-outcome` answered `completed` on a `completed` order with the fault D-166
chose. The order read exposes state, version and the per-line projection. It does not expose the
recorded verdict, the failure reason or the audit, which the Workflow principal has no grant to
read (Lifecycle `08-read-and-authz.md:300-303`, `:1041-1042`). The target states suffice, because
only this gear's rows 7–11, 13, 14 and 26 reach them at an order version, and a resume only
restores a state reached before. **Precedent**: the `held` answer rule 4 already gives on the
same read, a settled success after a Lifecycle refusal (D-102). `begin-fulfillment`'s
short-circuit to `in-fulfillment` once the transition is committed (`04 §3.6`
`inst-bf-if-already-begun`). And the confirm-by-lookup rule for an aged-out intent
(`UPSTREAM_REQS.md` `SUB-O13`). **Rejected**: reading the audit to match the key, which the
Workflow principal is not granted; and treating every terminal `not-admissible` as applied, which
would report an ordinary cancel as this gear's own.

**Propagated**: `design/01-foundation.md` §3.3 (*Rounds and attempts* rule 4), §3.7 (*Which keys
age while the instance lives*), §4.3 (*Lease-expired*); `design/03-approval-execution.md` §3.3
`reflect-verdict`, §3.6 `inst-rv3-settle`, §4.4; `design/04-fulfillment-plan.md` §3.3, §3.6
`inst-bf-if-held`; `design/05-provisioning-intents.md` §3.2, §3.3 `report-spawn-signal`;
`design/06-saga-and-compensation.md` §3.2, §3.3 `report-outcome`, §3.6 `inst-ro-call`, §4.9;
`design/10-process-definition.md` §3.6 (a), (b); `UPSTREAM_REQS.md` §1.2, §2.4, §3; D-102,
D-166, D-185.

**Clarified (2026-09-28, verification V2)**: rule 4's tail in `design/01-foundation.md` §3.3 (*Rounds and attempts*, `inst-owf-round-lifecycle`) spoke of "the `on_hold` and terminal cases" and stated no terminal case. After `already-applied` and `held` it now names each operation's own: `reflect-verdict`'s `moved` and `refused` (`03 §4.4`, D-190), `report-spawn-signal`'s `not-dispatchable` (`05 §3.3`), and `report-outcome`'s `terminal-event` (`06 §4.9`). `begin-fulfillment` has no terminal answer and keeps `permanent-failure` `version-mismatch` (`04 §3.6` `inst-bf-if-held`). No behaviour changes (V2-3).

### D-189 (M) `open-gates` looks a gate's request up by its key before it submits, and adopts one it finds

**Accepted (2026-09-28).** *(amends D-185)*

**Decision**: (1) `open-gates` step 4 (`03 §3.6` `inst-og-submit`) looks each gate's request up
at Generic Approval by its approval-request key before it submits. If the service holds a request
under the key, `open-gates` adopts it and does not submit again. Otherwise it submits as before. A
look-up the service does not answer is `retryable-failure`, never a blind resubmission. (2) The
Generic Approval ask of `UPSTREAM_REQS.md` §2.3 gains two clauses. The service de-duplicates by
request key for the life of the gate, until the request is decided or cancelled. And it answers a
read of a request by its request key.

**Rationale**: the approval-request key is `resource_tenant_id` + `orderId` + `orderVersion` +
`gateId` and carries no attempt. A successor re-run after the step key aged out (D-185) presents
it again, possibly weeks after the first call submitted. The expectations contract says only "be
idempotent by request key" (`UPSTREAM_REQS.md` §2.3), with no window. The only read it offers is
the decision by `decisionEventId`. So a service that keeps its keys for a request-cache window, as
Lifecycle does for 24 hours, would open a second request for the same party, which is the failure
`open-gates`' derived `gateId` exists to exclude. **Precedent**: the confirm-by-lookup rule this
gear asks of Subscriptions: "after an idempotency key ages out, confirm outcomes by lookup rather
than resubmission" (`UPSTREAM_REQS.md` `SUB-O13`, `design/05-provisioning-intents.md` §4.1). In
phase 1 the stand-in never opens a gate (`03 §3.5`), so nothing changes until the service lands.

**Propagated**: `design/03-approval-execution.md` §3.5, §3.6 (the gate-open sequence,
`inst-og-submit`, its description); `design/01-foundation.md` §3.7 (*Which keys age while the
instance lives*), §4.3 (*Lease-expired*); `UPSTREAM_REQS.md` §2.3; D-185.

**Clarified (2026-09-28, verification V2)**: the finding was about what an adopted request is when it is not open, and when its window starts. `open-gates` step 4 (`design/03-approval-execution.md` §3.6 `inst-og-submit`) now covers each state. An adopted `open` request dates its gate's window from the request's opened instant as the service reports it. It sets `window_remaining_ms` = the pinned `escalation_window_ms` less the time elapsed since then, and never counts from the adoption. No `owf_approval_request` row can date it, because the earlier call never reached its settlement. An adopted decided request is opened like an open one. The service re-publishes its decision event until `record-decision` reads it by `decisionEventId`, so only `record-decision` decides a gate, as the gate state machine requires, and it applies the separation-of-duties guard. A `cancelled` request is not adopted. The operation settles `permanent-failure` `version-mismatch`, since the key carries no attempt and the gate has no other key, and the invocation fault leaves through the `invocation-dead` task (D-192). `UPSTREAM_REQS.md` §2.3 asks for a read that answers every state with the opened instant, and for that re-publication. This fills in D-189's "adopts one it finds" and needs no new decision (V2-7).

### D-190 (M) `reflect-verdict` answers a Lifecycle refusal as the settled answer `refused`, and the definition catches no 400 of it

**Accepted (2026-09-28).** *(amends D-112, D-185; ADR-0012 as amended)*

**Decision**: (1) A Lifecycle refusal of the reflection that is none of `held`, `moved` or the
applied transition of D-188 answers `reflected = refused`. It is a settled success that stamps
nothing, returns `nextRound`, and carries the Lifecycle reason as `refusalReason` and in
`owf_step_log.result`. It is no longer a `permanent-failure` `approval-reflection-refused` (400).
(2) Fragment (a) of `10 §3.6` drops the outer 400 `catch` around `reflect-verdict`. The call sits
under the plain transient `catch`, as every other protected call does, and `afterReflect`'s
existing `refused` case routes the output to `reflectionTask`. (3) The task's `retry` re-enters
under the next round and the `attempt` `retry-step` minted. (4) `approval-reflection-refused`
stays in the `01 §4.9` catalogue as the order-scope task's `failure_reason` (`07 §3.7`). The
count of 43 is unchanged. (5) `10 §4.6` no longer lists `reflect-verdict` as a caught route. A
genuine 400 of `reflect-verdict` — an aged-out key, a validation defect — faults the invocation
like every other, and the `invocation-dead` task's `retry` mints the successor attempt (D-185).

**Rationale**: the DSL `catch` filters on type and status only. The plugin does not surface
`error_code` (Q-11). So the 400 `catch` that caught `approval-reflection-refused` also caught
`idempotency-key-aged-out` and every other 400, and labelled each a reflection refusal. D-185
relied on that mislabel for the aged-out case. An answer the operation settles itself is routed on
its value, so no status has to be interpreted. **Precedent**: the design's settled-success
answers `held`, `due: false`, `deferred` and `unobtainable` (D-102). `record-decision`'s refused
decision answers success with `applied: false`, "never `permanent-failure`" (`03 §4.4`). And
`begin-fulfillment` moved its guard refusals to the settled `withheld` for the same reason, so
that "no `catch` has to tell a version conflict from a still-processing answer by status"
(`04 §3.6`).

**Propagated**: `design/03-approval-execution.md` §3.3 `reflect-verdict`, §3.6 `inst-rv3-settle`,
§4.4, §4.5 item 3; `design/10-process-definition.md` §3.6 (a) `reflectVerdict`, `afterReflect`,
`reflectionTask`, the approval-stage prose, §4.6; `design/01-foundation.md` §4.3 (*Aged-out key*),
§4.9; `design/07-manual-tasks.md` §3.3; `ADR/0012` (Rule 6); D-112, D-185.

**Amended by D-192 (2026-09-28)**: item (5)'s "the `invocation-dead` task's `retry` mints the successor attempt" holds once the platform confirms the re-drive. Until then that `retry` is `action-not-offered`, and the fault is resolved by the dead-instance unwind (`design/01-foundation.md` §4.16 item 2). `design/10-process-definition.md` §4.6 says the same.

**Clarified (2026-09-28, verification V2)**: the `reflect-verdict` row of `design/03-approval-execution.md` §3.3 had lost the separator between `compensation` and `reasons`, so `none` and the first reason ran into one cell. The row again has the header's eleven cells (V2-1). Fragment (c)'s failure-rule prose in `design/10-process-definition.md` §3.6 listed "`reflect-verdict`'s refusal (an order-scope task)" among the caught failures. It now says that refusal is the settled answer `refused`, routed on its output and not caught (V2-5).

### D-191 (M) The audit read is access-logged like Lifecycle's: one `owf_read_access_log` row per request, served or refused

**Accepted (2026-09-28).** *(amends D-48, D-104, D-186)*

**Decision**: (1) Slice 09 owns a new table, `owf_read_access_log` (`09 §3.7`). The Audit Trail
Reader is its sole writer and no route reads it. (2) Every request to
`GET …/workflows/{orderId}/audit` that reaches an access decision appends one row, one per page
and never one per item. The row carries `operation` = `audit-read`, the requested `orderId`
(`requested_order_ref`, no FK), the trusted `actor` and `actor_class`, the caller's
`subject_tenant_id`, the order's resource and seller axes when the prefetch resolved them, the
supplied delegation proof reference if any, `outcome` (`served` or `refused`), the
`refusal_reason` and `accessed_at`. It stores no page contents. (3) A served page commits its row
before the page is returned. If the append fails, the read answers the canonical
`ServiceUnavailable` (503) with no page. A refusal (`not-found`, `not-authorized`) is returned
unchanged when its append fails, and the failure is raised on the audit-integrity alert of
`DESIGN.md` §4.4. A cursor refused at boundary validation and a PDP outage append nothing. (4)
The table is append-only. No role holds an UPDATE grant, and the `retention-purge` role alone holds
DELETE. It is not trigger-protected or chained. Its retention is 90 days from `accessed_at`,
purged through that index whatever the instance's state. (5) Its axis is `subject_tenant_id` NOT
NULL. The order's axes are nullable, which makes it the seventh stated exemption from D-48. (6)
The progress read, the approver inbox, the operator task queue and the plan projection are not
access-logged. **Counts**: tables 28 → 29, ten engine and nineteen slice tables (was eighteen).
Mutable tables stay 19. D-48 exemptions 6 → 7.

**Rationale**: D-186 gave the gear its widest disclosure — every actor, idempotency key and
justification of an order's process — and kept no record of who read it. So a review could not
establish who had read a trail, which is the question an audit read raises first. **Precedent**:
Lifecycle's read surface writes `orders_read_access_log` "one entry per request, not per row …
Persist a required served log before returning data. Refused-log failure emits the required
infrastructure/security signal without turning refusal into access" (Lifecycle
`08-read-and-authz.md:660-666`). Its audit read writes it "on the same served-and-refused pattern
as the other read paths" (`:829-838`). A failed append "blocks a served response and never blocks a
refusal" (`:840-850`). The table has the same columns, adapted here: this gear has no order table,
so there is one no-FK `requested_order_ref` and no `order_id`. It is append-only with 90-day
retention (`:865-927`, `08 §4.5`), and its purge is in the retention sweep (Lifecycle `DESIGN.md`
§3.8). The tenant axis follows Lifecycle's scoping of an unresolved refusal by the caller's stored
`subject_tenant_id` (Lifecycle D-104, `08-read-and-authz.md:818-820`). **Where this differs from
Lifecycle.** Lifecycle logs a served read only on a supplied delegation proof or a resource tenant
that differs from the subject's (`08 §4.4`, `:1367-1384`). This gear's audit read logs every
request instead. It is the widest disclosure here too, as Lifecycle says of its own
(`:834-836`), and a Seller Operator's read of a customer's order meets the cross-tenant trigger
in the ordinary case anyway. The other reads are not logged. No grant of `09 §4.1` is a delegated
path, so the first trigger cannot arise on them. The second does arise whenever the caller's
tenant is not the order's resource tenant, and they are not logged for it. They disclose process
state, not the audit trail, and the order's content is read and logged through Lifecycle. That
divergence is stated in `09 §3.7`, not hidden.

**Propagated**: `design/09-read-and-authz.md` §3.2 (diagram, Audit Trail Reader), §3.3 (*Audit
read*, *Access log*), §3.7 (`owf_read_access_log`, *Which reads are access-logged*), §4.3;
`design/01-foundation.md` §3.7 (*Tenancy*, the D-48 exemptions), §3.8 (the `retention-purge`
roster); `DESIGN.md` §3.3, §3.7 (inventory,
counts, D-48 exemptions, retention register), §4.4 (*Audit integrity*); D-48, D-104, D-186.

**Clarified (2026-09-28, verification V2)**: `design/01-foundation.md` §4.11 still said "Six tables are exempt". It now says seven, and names `owf_read_access_log` as §3.7 and `DESIGN.md` §3.7 do (V2-2).

### D-192 (M) The operator re-drive of a dead invocation is an explicit readiness item that gates the action, not readiness

**Accepted (2026-09-28).** *(amends D-86, D-105)*

**Decision**: (1) `01 §3.8` *Readiness* names the re-drive as an explicit item of the platform
readiness gate. For the `platform` source the gear reports ready without it. The
`invocation-dead` task's `retry` stays `action-not-offered` until the platform confirms three
things: `:control` `retry` keeps `invocation_id`, it resumes at the faulted task, and it is valid
from `dead_lettered`. (2) `07 §4.4` states that until then the only remedy is the order cancel,
carried out as the dead-instance unwind (`01 §4.16` item 2), with Lifecycle's own cancel first
before fulfillment has begun (D-109). Every invocation fault lands there, including an aged-out
downstream key (D-185) and a deterministic `Internal` (500) (D-168). (3) The signals ask of
`UPSTREAM_REQS.md` §2.9 names those dependants and says why the clause gates the action and not
readiness. It stays `p1` (critical). `10 §3.3` *Generic control* says the same.

**Rationale**: D-86 and D-105 already made `retry` depend on the platform's confirmation. The
readiness gate of `01 §3.8` did not say so, and `UPSTREAM_REQS.md` §2.9 called its whole section
"the content of the readiness gate". So a reader could take the gate either to block readiness on
the re-drive or to ignore it. Since D-185 and D-168 route aged-out keys and deterministic 500s
through the invocation fault on purpose, the answer covers more than rare deaths. Readiness is
not held on it because the unwind is a complete remedy, if a lossy one: the order ends through
the cancel path, and the loss is the PRD amendment of `UPSTREAM_REQS.md` §4 item 11. Blocking
readiness would stop every order to protect the one whose invocation dies. **Precedent**: this is
how `09 §3.3` already reads D-86 ("until the platform confirms those properties the re-drive is
`action-not-offered`, and the fallback is the task's `cancel`"). It is also how the gear handles
its other unconfirmed platform asks, which leave the dependent action unoffered rather than the
gear unready: the pending dead-letter routes of `09 §4.1` stay unregistered until
`…-upreq-serverless-runtime-dead-letter-operator-visibility` is answered.

**Propagated**: `design/01-foundation.md` §3.8 (*Readiness*); `design/07-manual-tasks.md` §4.4;
`design/10-process-definition.md` §3.3 (*Generic control*); `UPSTREAM_REQS.md` §2.9 (preamble,
`…-upreq-serverless-runtime-signals`); D-86, D-105.

**Clarified (2026-09-28, verification V2)**: `ADR/0011`'s *Runtime gate* held readiness on "the platform asks below", and its signals ask covers the re-drive. It gains an amendment block that carves the re-drive out of readiness (V2-4). `design/10-process-definition.md` §4.6 described the re-drive as available, in the one failure rule and in the aged-out-key paragraph. It now adds "once the platform confirms it", and names the dead-instance unwind (`01 §4.16` item 2) as the remedy until then (V2-6).

### D-193 (H) PriceBook reaches this gear only through Lifecycle's Workflow SDK and its immutable-version read

**Accepted (2026-09-30).** *(amends D-66 as amended and D-131; carries Lifecycle `ADR/0008`, D-155, D-158)*

**Decision**: (1) Every Lifecycle call is a method of `OrdersLifecycleWorkflowV1`, obtained through
`ClientHub`: `reflect_approval`, `begin_fulfillment`, `report_spawn_signal`, `acknowledge`,
`workflow_cancel`, `hold`, `resume`, `get` and `get_version`. The REST paths this design used to
cite stay as correspondence text only. (2) The immutable commercial content of a fixed order
version — the lines, their selected items, the accepted `OrderPin` (plan revision, per-item chain
bindings, SKU versions, activation deadline), the overlap keys, the totals and the TCV — is read
inside step operations through `get_version(orderId, orderVersion)`; the current-state read `get`
serves admissibility and applicability only, and possession of an event or version reference grants
no authority. (3) Lifecycle publishes its state events as bounded projections plus a version
reference (Lifecycle D-158); that answers `…-upreq-lifecycle-thin-events`, which keeps its ID with
the residual of confirming the projection schema. (4) The retired catalog vocabulary goes:
`catalogPricePin` and `priceId` become the accepted `OrderPin`, `plan_revision_id` and `price_id`;
"Catalog / Plan & Price" becomes Pricing (PriceBook), Products and Rating, none of which this gear
calls — their facts reach it only as Lifecycle's stored order version.

**Rationale**: **Precedent**: Lifecycle D-155 (SDK parity — an SDK method enters the same authorized
application service as REST and is not an authorization bypass) and D-158 (expanded pins through
authorized immutable-version reads, events bounded). ADR-0013 keeps only references across the
engine boundary; a version reference plus an authorized read is the shape it always wanted, and it
removes the commercial payload from the consumed events without a thin variant of each. The
producer baseline is diffora `bss/products` at `16705a243`; the Lifecycle rewrite (ADR-0008,
D-150–D-168) is uncommitted in its worktree, so this entry cites it by decision number.

**Propagated**: `DESIGN.md` §2.2 (*No price computation*), §3.1, §3.5; `design/03-approval-execution.md`
§3.2, §3.4, §3.6; `design/04-fulfillment-plan.md` §1.3, §3.2, §3.4, §3.6; `design/05-provisioning-intents.md`
§2.2, §3.2, §3.5; `design/06-saga-and-compensation.md` §3.2, §3.3; `design/08-hold-and-cancel.md` §3.5;
`ADR/0013`; `UPSTREAM_REQS.md` §1.2, §2.4, §2.5, §5; `PRD.md` §1.4, §5.2, §6.5, §12 (`UPSTREAM_REQS.md` §4 item 16).

### D-194 (H) The accepted binding's activation deadline is checked early at freeze, at the re-check and before every activation submit; expiry after an activation compensates first

**Accepted (2026-09-30).** *(amends D-15, D-91, D-92, D-110; carries Lifecycle D-152, D-162)*

**Decision**: (1) `construct-and-freeze-plan` refuses to freeze a plan whose expected fulfillment
time is at or after the earliest `activation_deadline` of its lines: `planState = binding-expired`, a
plan-level failure that opens a plan task under either policy and needs no compensation;
`owf_fulfillment_plan.activation_deadline_at` (the earliest, exclusive) is written at freeze and is
immutable. (2) `re-check-pre-activation` aborts with `abort_record` `order-binding-expired` when any
line's deadline has elapsed, beside the stale-authorization leg. (3) `dispatch-wave2-activate`'s guard
refuses the whole call as the settled answer `bindingExpired: true` when any deadline has elapsed at
database time; nothing is submitted, and the definition routes it to the unwind
(`wave2BindingExpired`) under either policy with no manual task. (4) Subscriptions is the
authority: it verifies the accepted prices by comparison at activation (Lifecycle D-162), and its
refusal — the binding comparison, the deadline, a lost SKU protection, a source-version mismatch —
is a synchronous `wave2-activation-failed` with the cause preserved. (5) The fence maps all three
causes to Lifecycle's `failure_reason` `order-binding-expired` (Lifecycle feature 06 §4.4). Once
any line is active the path is the normal fence — void the drafts, cancel the activated lines,
evidence with `no_active_subscription_remains = true`, then `acknowledge failed` — never a
pre-activation abort. (6) A hold does not pause the deadline; `apply-resume` re-runs the wave-2
guard. (7) `order-binding-expired` is one catalogue reason (owner slice 04, raised also by slice 05's
guard; `ORDER_BINDING_EXPIRED`, FailedPrecondition, 400); `binding-expired` is a `planState` value
and a task path, never a second reason.

**Rationale**: **Precedent**: Lifecycle D-152 — state TTLs, hold/resume and payment-tolerance
elections neither extend the deadline nor authorise repricing; an expiry during fulfillment stops
dispatch and follows evidence-gated compensation, not a new expiry edge — and D-162, under which the
deadline is an early check and the receiver's comparison needs no clock agreement between the
gears. The three early checks are early aborts only, exactly as the overlap re-check is (D-91): a
receiver may still refuse, and that refusal has its own path.

**Propagated**: `design/04-fulfillment-plan.md` §2.2, §3.1, §3.3, §3.6, §3.7, §4.3, §4.6;
`design/05-provisioning-intents.md` §3.3, §3.6, §4.5, §4.6; `design/06-saga-and-compensation.md` §3.6,
§4.8; `design/07-manual-tasks.md` §3.3, §3.7, §4; `design/08-hold-and-cancel.md` §2.1;
`design/10-process-definition.md` §3.6 (b), §4.6; `design/01-foundation.md` §4.9; `UPSTREAM_REQS.md`
§2.1 (`SUB-O11`), §2.4; `PRD.md` §6.1 (note); D-77, D-105 (counts).

### D-195 (H) A provisioning intent carries the source version tuple as the accepted-binding reference, and `activated` means Subscriptions' `applied`

**Accepted (2026-09-30).** *(amends D-97 as amended, D-125; carries Lifecycle D-157, D-163, D-165, D-168)*

**Decision**: (1) Every forward intent carries the source tuple `(orderId, orderVersion,
orderLineId)`, which is also the accepted-binding reference: Subscriptions reads the accepted
composition through Lifecycle's `get_version` and verifies it at activation; no pin, price, item or
amount crosses from this gear. It also carries the explicit start intent (`SUB-O10`), the tenant
axes, the process `correlationId`, and the order and line external references snapshotted at the
first handoff (`owf_provisioning_intent.external_refs_snapshot`) and reused on every retry. The
intent key is unchanged. (2) `activate` carries the draft id, its expected version, the same tuple
and the actual instant as start. (3) Status mapping: Subscriptions' `applied` is this gear's
`activated`; `approved` (accepted, OSS pending) leaves the intent `submitted`; `oss_unconfirmed` is
`wave2-activation-failed`. (4) `SUB-O5` is the count read — active count, effective
`max_concurrent_active`, provenance per `(payer, overlap_scope_key)`, drafts excluded — over the
registry-owned `SUB-G1` key stored on the order line, never derived here. (5) `SUB-O11`'s refusal
kinds include the binding comparison, the deadline, a lost protection and a source-version
mismatch. (6) Instants: the actual activation is `serviceActivatedAt`; `customerAcceptedAt` is
mapped from Lifecycle's acceptance record by the receiver; `contractEffectiveAt` comes from the
Contract.

**Rationale**: **Precedent**: Lifecycle D-157 (versioned creation keys, actual start, fee-free
compensation), D-165 (the existing three instants and the OSS-async `activate`; `applied`, not
`approved`, is completion) and D-168 (external references travel order → create → billable fact →
invoice, snapshotted at the first handoff). The counterpart note explicitly keeps this gear's
tenant/order/version/line/wave/kind/attempt key: a line-only key would conflate amended orders and
rebuilt drafts. ADR-0013 is untouched — the tuple is a reference.

**Propagated**: `design/05-provisioning-intents.md` §2.1, §2.2, §3.3, §3.6, §3.7, §4.1;
`design/06-saga-and-compensation.md` §3.3; `design/04-fulfillment-plan.md` §2.1; `DESIGN.md` §3.1;
`UPSTREAM_REQS.md` §2.1 (`SUB-O5`, `SUB-O10`, `SUB-O11`, `SUB-O13`, *PriceBook provisioning amendment*); Q-08.

### D-196 (H) Acquisition lines are independent: the dependency graph, its Catalog source and the `blocked-upstream` outcome are withdrawn

**Accepted (2026-09-30).** *(supersedes D-16; amends D-77, D-92, D-105, D-110, `ADR/0004` and `ADR/0005` as amended)*

**Decision**: the plan is one `FulfillmentTask` per line and nothing else: no `DependencyEdge`, no
`dependency_graph`, no `catalog_topology_revision`, no `dependency_rank`, no acyclicity or
completeness check, no Catalog read. The two-wave barrier is unchanged — every draft before any
activation, and the expected-fulfillment instant — and is the whole ordering; the order within a
released wave is dispatch admission (`05 §4.3`). `planState` is `frozen` or `binding-expired`; the
reasons `invalid-dependency-graph` and `catalog-topology-unavailable` are withdrawn, and so is
`blocked-upstream`, because a reverse compensation walk over `execution_seq` has no upstream
subject to wait for (`owf_compensation_record.outcome` is `succeeded | failed-pending-escalation`;
`blocked_by_record_id` is dropped). The Catalog ask (`…-upreq-catalog-dependency-topology-read`)
and the topology row of the failure-reason coverage ask are withdrawn; PRD §6.1, §5.1, §7.1 and
acceptance criterion 5 are amended through `UPSTREAM_REQS.md` §4 item 15. The catalogue holds 41 reasons.

**Rationale**: under PriceBook a line is one plan revision with its selected items; an add-on is an
optional item of the same revision, inside the line; Pricing refuses bundle SKUs as plan items; and
nothing links one revision to another (Lifecycle `ADR/0008`, D-150, D-156). The "add-on requires its
platform plan" example the PRD gave is therefore inside one line, not an edge between two. The
alternative — keep the graph and define a `resolve_topology` producer whose owner is a release
prerequisite, which is how Lifecycle's ask on this gear reads ("unknown topology must not become an
empty dependency set") — would block order-taking on a contract nobody owns for a relation the
model does not have. That sentence is answered by there being no topology at all, not by an empty
graph; the Lifecycle owner is asked to reword it (hand-off note in
`docs/reviews/2026-09-30-orders-workflow-pricebook-fix-plan.md`). **Precedent** for the ordering
that remains: `ADR/0004`'s barrier, whose decision drivers never depended on topology.

**Propagated**: `design/04-fulfillment-plan.md` §1–§4 (principle `…-principle-lines-independent`,
constraint `…-constraint-bundle-one-line` retitled, `…-constraint-dependency-ordering-owner` and
`…-entity-dependency-edge` retired); `design/05-provisioning-intents.md` §3.2, §3.6;
`design/06-saga-and-compensation.md` §2.1, §3.3, §3.6, §3.7, §4.8; `design/07-manual-tasks.md` §3.3, §3.7, §4;
`design/08-hold-and-cancel.md` §3.2; `design/10-process-definition.md` §3.6 (b); `design/01-foundation.md`
§4.9; `ADR/0005`; `DESIGN.md` §1.2, §3.5; `PRD.md` §6.1 and criterion 5 (notes); `UPSTREAM_REQS.md`
§1.1, §1.2, §2.4, §2.5, §3, §4 item 15, §5.

### D-197 (H) The approval policy owner is this gear's approval-policy adapter; the Generic Approval service is withdrawn

**Accepted (2026-09-30).** *(amends D-12, D-14, D-189; answers Q-05 in part; carries Lifecycle D-166)*

**Decision**: approval policy — the requirement verdict, routing, thresholds, the decision, the
escalation path — is reached only through the **approval policy adapter**, a port of this gear
(`cpt-cf-bss-orders-workflow-component-approval-policy-adapter`) whose contract is PRD §9.2's
clauses (a)–(g) plus D-189's read-by-key and decision re-publication: `verdict(orderId,
orderVersion)`, `submit(request)`, `lookup_by_key(requestKey)`, `read_decision(decisionEventId)`,
`escalate(gate)`. Two implementations: `stand-in`, today's behaviour, answering `not_required` and
audited as the deciding authority by name (D-14); and `library`, the intended one, embedding
`cf-gears-bss-approval` (`Engine`, `ApprovalSubject`, `Store`) as Pricing and Products do — not
designed here, its gate-to-unit mapping is Q-14. The decision event stays a `listen` target; under
the library adapter it is published on this gear's own topic (D-177). "Generic Approval service" is
withdrawn from the PRD, `DESIGN.md`, `UPSTREAM_REQS.md`, the slices, the ADRs and the README; ask
2.3 becomes LOCAL and keeps its ID; the actor ID `…-actor-owf-generic-approval` is kept.

**Rationale**: **Precedent**: `gears/bss/libs/approval` is built and embedded by Pricing and Products
inside their own transactions; no approval service exists (upstream's `gears/approval-service` PRD
is a stub) and Lifecycle D-166 records the same conclusion for the sibling gear. The port keeps the
policy owner separate from the orchestration even when both live in this gear (Lifecycle R2), and
the deciding authority on every verdict names which implementation answered.

**Propagated**: `PRD.md` §1, §3.2 (*Approval Policy Owner*), §5, §6.2, §6.3, §6.5, §7.1, §9.2, §12, §13,
§15, §16, §17.1; `DESIGN.md` §1.3, §3.1, §3.5, §4; `UPSTREAM_REQS.md` §1, §2.3, §4 item 17;
`design/03-approval-execution.md` §1.1, §2.2, §3.2, §3.5, §3.6, §4.0; `design/01-foundation.md`,
`design/02-triggers-and-start.md`, `design/08-hold-and-cancel.md`, `design/09-read-and-authz.md`,
`design/10-process-definition.md`, `design/README.md` (rename); `ADR/0002`, `ADR/0006`, `ADR/0007`,
`ADR/0009`, `ADR/0011`, `ADR/0013`; Q-05, Q-14.

### D-198 (M) The approval request carries the stored TCV as an integer in minor units with its currency and scale

**Accepted (2026-09-30).** *(amends D-09's request record; carries Lifecycle D-154, D-167)*

**Decision**: `owf_approval_request` carries `tcv_minor bigint`, `currency text` and
`currency_minor_digits smallint`, all NOT NULL, read by `open-gates` from Lifecycle's `get_version`
and carried verbatim; `resolved_total numeric` is gone and the wording is "TCV", never "resolved
total (TCV)". The figure is Rating's net pre-tax annualised TCV as Lifecycle stores it; this gear
adds, converts and rounds nothing, and calls neither Rating nor Pricing (the Atlas trait map's
"called by Orders Workflow" for `PrePurchaseEvaluationV1` is wrong). A submitted version always
has a TCV (Lifecycle refuses to submit without totals), so a missing figure is a contract-violation
refusal before submission, and the fail-closed rule stands.

**Rationale**: **Precedent**: Lifecycle D-154 (Rating supplies whole-order figures and the TCV over
the exact selected prices; Orders stores integer minor-unit figures verbatim and neither sums nor
converts) and D-167 (the Rating request in resolve's vocabulary, periods `month` or `year`). A
`numeric` column invited a local reading of money that R4 forbids.

**Propagated**: `design/03-approval-execution.md` §3.2, §3.6, §3.7; `DESIGN.md` §2.2, §3.1; `PRD.md`
§3.2, §5.2, §6.2, §6.5, §9.2, §12 (`UPSTREAM_REQS.md` §4 item 16).

### D-199 (L) Seam notes: the payment amount is separate from the TCV, no Products reservation exists, and change orders wait for their own PriceBook pass

**Accepted (2026-09-30).** *(amends D-17 as amended by D-90; carries Lifecycle D-156, D-164, D-168)*

**Decision**: (1) A payment authorization request is bound to the order version's Rating figure in
the book currency, an integer in minor units — separate from the TCV the approval request carries;
who mints `payment_auth_request_ref` (Lifecycle's acceptance path or this gear's
`reauthorize-requested` signal) is open under Q-06, and Ledger settlement is never accepted as an
authorized outcome (Lifecycle D-168). (2) SKU protection through activation is inherited from the
accepted revision's `plan_item` references (Lifecycle D-164): this gear reserves nothing, holds no
receipt, and a forced retirement surfaces as a Subscriptions refusal on the provisioning-failure
path. (3) The market re-check compares the book currency and the producer-declared market
applicability with the payer profile (Lifecycle D-156); the composed algorithm is unchanged. (4)
The Change Orders PRD (`gears/bss/orders-changes`) asks this gear for a single transactional change
intent and still uses the retired vocabulary; it needs its own PriceBook pass before the change
intent is designed here.

**Rationale**: each is a fact of the counterpart's rewrite that this design would otherwise
contradict by silence; none warrants a design of its own here.

**Propagated**: `design/04-fulfillment-plan.md` §3.4, §3.5; `UPSTREAM_REQS.md` §2.2, §2.5; Q-06.

## Open Questions

### Q-01: Which durable-execution substrate backs the process — the OSS Workflow Engine or a BSS-local mechanism?

**Owner**: Architecture.

**Answered in two parts (2026-09-24).**

1. **The substrate choice is made** — D-65, `ADR/0011`: the serverless-runtime Temporal plugin
   executing a versioned platform definition; neither the OSS Workflow Engine of
   `PRD-workflow-engine-202501051430` nor a BSS-local mechanism. ADR-0001 is rewritten and no
   longer selects nothing.
2. **The PRD §15 evaluation is pending on the platform asks** — moved to **Q-12**. Of the four
   evaluation criteria, *which commercial data would sit in engine history* is answered "none" by
   D-66 (`ADR/0013`); *BSS/OSS boundary compatibility* is answered by the engine being a platform
   gear holding no commercial document; *gear-owned audit independent of engine purge* is answered
   by the record being complete without the history. *Isolation and retention of that history* —
   with its residency — cannot be asserted from the platform's documents and is Q-12.

**Amended (2026-09-26)**: part 2 overstated the first criterion. It was told three ways: "none"
here and in `UPSTREAM_REQS.md` §4 item 7, "met by construction" in `design/01-foundation.md` §1.1
and `design/10-process-definition.md` §1.1, and "bounded, not closed" in `DESIGN.md` §4.2. The one
answer is this: *which commercial data would sit in engine history* is **none in task data**. In
history there are identifiers (including `resource_tenant_id`), counters and array cardinalities
(D-131), fixed-member error answers (D-132), and the consumed events as published until
`…-upreq-serverless-runtime-consumed-event-member-storage` or `…-upreq-lifecycle-thin-events`
lands (D-66 as amended). The "met by construction" claims are withdrawn from 01 and 10, and the
registered PRD amendment now carries the qualifier. PRD §15 shows no answer yet, which is correct
until the amendment is applied.

*Superseded statement, retained for history:* no slice's saga-step, durable-timer, or
engine-history-isolation implementation could be finalized until the substrate was chosen; target
date 2026-09-30 per `gears/bss/orders-workflow/docs/PRD.md` § `15. Open Questions`.

### Q-02: The Generic Approval escalation threshold — the one PRD-deferred numeric value this design deliberately leaves unset

**Owner**: Product.

**Narrowed**: the engineering half of this question is now closed. The retry backoff curve, maximum
attempts, per-attempt timeout, step deadline, nesting-invariant enforcement, gear-wide retry
budget, per-order concurrency, reconciliation-sweep ladders (D-45 as split by D-52), and
dead-letter delivery cap are fixed as working baselines in D-39 through D-46 and D-52, each with
its derivation, and each proposed into the program-wide non-functional workshop rather than
asserted as settled. Generic Approval outage **detection** is likewise fixed (D-46). The
**aggregate** in-flight cap is the one baseline D-44 originally declined to state; it now carries
the placeholder 256 concurrent intents with its derivation, explicitly standing in for the
Little's-Law figure until measured capacity exists, so the queue-depth cap and the load test have
something to be configured against.

**Still open**: the numeric sign-off on the threshold at which a fail-closed park becomes an
operator-visible incident. The *form* is no longer open — D-46 and ADR-0007 now state it
relationally against the Orders Lifecycle `submitted` TTL, as `min(30 min, 0.25 x
lifecycle_submitted_ttl)`, with an escalation lead time of `max(4 h, 0.25 x
lifecycle_submitted_ttl)`, and both multipliers are now accepted. What remains outstanding is not a
decision but a dependency: Orders Lifecycle still owes visibility of the TTL itself, raised as an
upstream ask in `UPSTREAM_REQS.md`. A bare number chosen on this side would either assume a TTL
value this gear cannot see or silently become the platform's commercial answer, which is why the
relational form is the answer rather than a placeholder for one.

**Blocked**: the threshold cannot be *configured* until the Lifecycle TTL is readable, and has
nothing to apply to until a Generic Approval service exists (Q-01 and the phase-1 inertness
disclosure). The working baselines in D-39 through D-46 and the cross-cutting decisions in section
L are implementable now, subject to confirmation or adjustment by the non-functional workshop.

### Q-03: Does a bounded hold/resume cycle count or bound the total elapsed non-terminal lifetime of an order, beyond the single-cycle remaining-window guarantee this design already keeps?

**Owner**: Product (the fulfillment-operator/product owner named in the PRD's overdue-escalation
rationale).

**Resolved in form, open on the value**: D-53 answers the mechanism — no cap on cycles, and an
unconditional non-pausable `max_process_lifetime` of 90 days armed at process start, and the
90-day figure is accepted. The previously recorded interim backstop, the
overdue-fulfillment escalation window (slice 07), is **withdrawn as a backstop**: it only runs
while the order is `in_fulfillment`, so it bounds nothing for an order cycling between hold and
resume before fulfillment begins, which is the case this question actually asks about. What remains
open is nothing on this gear's side; this mirrors the same hardening item the
sibling Orders Lifecycle design set records for its own hold/expiry surface.

### Q-04: The SUB-O renumbering and the missing OrderAmended re-verdict requirement — two required PRD amendments

**Owner**: Architecture.

**Blocked**: PRD §13 and its in-body citations still read the pre-fork `SUB-O6`–`SUB-O9` numbers,
which collide with the canonical `SEAMS.md` register's own, unrelated `SUB-O6`; until the PRD is
amended to cite `SUB-O11`–`SUB-O14`, any reader working from the PRD alone will misidentify which
ask is which. Separately, PRD §6.2 does not yet state that `OrderAmended` must re-obtain the
approval-requirement verdict for the new order version and reflect it onward from `submitted`;
until that one-to-two-sentence addition lands, an amended order's approval requirement is
underspecified at the PRD level even though this design's approval-execution slice already assumes
the re-verdict behaviour.

### Q-05: The Generic Approval service has no specification anywhere in this repository, leaving the whole approval capability inert in phase 1

**Owner**: Architecture.

**Blocked**: multi-party gates, escalation beyond the phase-1 stand-in, the Approver Inbox, and
acceptance criteria #1–#4a are inert until a Generic Approval spec exists; this design's phase-1
stand-in (returning "approval not required", audited) is recorded as the deciding authority by
name only as an interim measure, not as a second policy author, and cannot be extended until the
spec lands.

**Amended by D-197 (2026-09-30)**: no specification is awaited any more. The approval policy owner is this gear's approval-policy adapter, whose intended implementation embeds `cf-gears-bss-approval` as Pricing and Products do (Lifecycle D-166 records the same). What still keeps multi-party gates, escalation and the inbox inert is the library adapter itself: its gate-to-unit mapping is Q-14. The stand-in stays the deciding authority by name until then.

### Q-06: Payments has no specification and no register, so the payment-authorization ask has no owner; only "provision first, collect after" is expressible

**Owner**: Architecture, with Product.

**Blocked**: the payment-authorization-outcome ask this gear raises has no receiving register to
land in, since no Payments capability spec exists anywhere in this repository. As a consequence,
only the provision-then-collect payment ordering is expressible in this design; a self-service
card checkout that collects before provisioning — with its own capture, SCA-challenge wait,
retry-with-another-instrument, and refund-as-reversal needs — has no home in the current flow and
cannot be implemented against this design until a Payments spec exists and the ordering, outcome
visibility, and reversal-artifact open questions in the PRD are resolved.

**Amended by D-199 (2026-09-30)**: two items are added to the question — the amount an authorization request is bound to (the order version's Rating figure in the book currency, in integer minor units, separate from the TCV the approval request carries) and who mints `payment_auth_request_ref` (Lifecycle's acceptance path or this gear's `reauthorize-requested` signal). Ledger settlement is not authorization (Lifecycle D-168).

### Q-07: Tension between asynchronous outbox publication and the PRD's p95 < 30 s process-event delivery target

**Owner**: Architecture.

**Resolved by D-58** — drain cadence and profile are the platform producer outbox's
(`bss-orders-workflow-events`, `Partitions::of(16)`, high-throughput profile); the p95 < 30 s
target is measured as producer-queue lag from enqueue to broker acceptance (Lifecycle Q-16
pattern), with `DESIGN.md` §4.4 alerting on it and `ADR/0008` *Confirmation* item 9 requiring the
measurement at expected load. Commit success alone remains no evidence of the target; a relaxation
of the number is a Product decision routed through the NFR workshop (Q-02), not a design change.

*Superseded statement, retained for history:* process events published asynchronously from an
outbox row written alongside the audit entry and drained on a schedule this design did not fix, so
the target had no committed mechanism shown to meet it.

### Q-08: SUB-O5 (overlap-scope-key presence read) is unagreed, leaving the pre-activation overlap check unevaluable; SUB-O1 (compensation cancellation reason) is critical and unagreed, leaving the activated-cancel reason unspecifiable from this side

**Owner**: Architecture (joint ask toward Subscriptions).

**Blocked**: until `SUB-O5` lands, an unevaluable pre-activation overlap read fails closed
rather than passing silently, but the overlap check itself cannot be evaluated as designed until
the read exists upstream.

**Amended (2026-09-26)**: the fail-closed outcome is not a collision. Per D-91 an unevaluable
re-check follows Lifecycle's `defer` ladder and then aborts with `overlap-read-unevaluable`
(Lifecycle `failure_reason` `overlap-presence-unevaluable`, `design/06-saga-and-compensation.md`
§4.8), so an outage upstream is never recorded as a commercial collision. Until `SUB-O1` lands, the activated-cancel
compensation leg has no cancellation-reason value to send, since reason values ride event payloads
consumers key on and adding one after Billing consumes the contract would be a breaking change;
this design names the requirement and defers the value rather than inventing a placeholder.

**Amended by D-195 (2026-09-30)**: the read is Lifecycle's count amendment — the active count, the effective `max_concurrent_active` and the policy provenance per `(payer, overlap_scope_key)`, drafts excluded — and the key is Subscriptions' registry-owned `catalogSubscriptionProductKey` of `SUB-G1`, stored on the order line at submit and reused as stored (Lifecycle D-126, D-153, D-163; the derivation proposed to Subscriptions is the SKU of the line's paid recurring item). `UPSTREAM_REQS.md` §2.1 carries the wording; the fail-closed posture is unchanged.

### Q-09: Do the Orders gears and Pricing converge on toolkit-db session advisory locks or on the `gears/bss/libs/coord` fenced lease for worker coordination?

**Owner**: Architecture (joint decision of the Orders Lifecycle, Orders Workflow and Pricing
owners).

**Open**: D-62 selects `toolkit_db::Db::lock` for this gear's roster, as Lifecycle D-92 did, and
both gears rest correctness on per-worker transactional rechecks because a session advisory lock
is neither a TTL lease nor a fence. Pricing coordinates through `gears/bss/libs/coord`, a
DB-backed TTL lease with an in-transaction fence. Two primitives for one job across three sibling
gears is a maintenance and review cost, and the fenced lease would let a worker prove ownership
inside its writing transaction, which the advisory lock cannot. Neither Orders gear should switch
alone: the answer is one contract for all three, decided with the Pricing owner, and until it
lands this gear's roster runs on `Db::lock` with its rechecks intact.

### Q-10: May a seller-scoped role publish definition fragments, or tenants author Functions, in phase 1?

**Owner**: Architecture, with Product.

**Open; recommendation: no.** ADR-0012 grants publish to the platform operator only. A
seller-scoped role that publishes a fragment of the definition — a seller's own escalation arms,
waits or SLA branches — and tenant-authored Functions called from the definition would let the
adjustability contract reach the people who own the commercial policy, but each is a new
authorship surface over a `p1` order-taking process: the validation hook would have to fence a
fragment in the context of a definition it did not publish, a Function could only target
`composable` operations and would run tenant code inside the platform's invocation, and the
platform's publishing governance and publish audit are themselves unaddressed
(serverless-runtime `NEXT_ADR_SCOPE.md` lines 26 and 48). The recommendation is that phase 1 grants
neither, and that the question be reopened only after the pre-publish validation hook and publish
audit asks of `UPSTREAM_REQS.md` §2.9 land. The PRD §15 row asking the same question is a
registered PRD amendment (`UPSTREAM_REQS.md` §4 item 7).

**Review trigger**: the definition-versioning and validation-hook ask agreed by serverless-runtime.

**Amended (2026-09-26)**: no `call` targets a Function at all now (D-136), so a tenant-authored
Function would need a new rule, not only a role. A seller's escalation, overdue and SLA windows
need no fragment: they are values of the seller's policy (D-134). The publish role is held by the
definition publish job (D-138).

### Q-11: Does the platform's DSL express the hold pattern and the other constructs the definition needs, or do they need Functions?

**Owner**: Architecture (joint with the serverless-runtime owners).

**Open.** The canonical definition of `design/10` relies on six things the Serverless Workflow
DSL 1.0.0 may or may not provide as the platform's plugin implements it
(`design/10-process-definition.md` §4.5):

1. **(i)** whether the plugin accepts a runtime-expression `wait` duration **as an extension**:
   the 1.0.0 `wait` accepts only an inline duration object or an ISO 8601 string (dsl-reference.md
   *Wait*, *Duration*), so a computed wait — the remaining escalation window, the
   expected-fulfillment instant, the SLA remainder, a deferral's `retryAfterMs` — is not
   expressible in the spec itself; until the plugin answers, the bounded re-check loop of D-70 as
   amended applies;
2. **(ii)** the Problem body's `error_code` visible on `$error`, so a `catch` can tell
   `idempotency-key-conflict` from `still-processing` before the retry budget is spent;
3. **(iii)** a dynamic parallel construct, so a wave could fan out per line inside the definition;
4. **(iv)** a `listen` inside a competing `fork` that is cancellable without losing an event
   delivered during cancellation;
5. **(v)** **the hold pattern** as a whole — a hold arm that wins a competing `fork` and cancels
   the escalation `wait`, a resume wait, and a re-armed `wait` of the remainder Orders returns —
   natively, without a Function;
6. **(vi)** that a `catch` carrying only `retry` re-raises the last error once its limit is spent,
   so the task faults: the DSL states nothing about what follows the last attempt (dsl.md
   *Retries*, dsl-reference.md *Catch*), and every retry-only `catch` of the canonical definition,
   `design/10` §4.6 and §2.2 rule 6 rely on it (D-126).

**Fallbacks in force until answered**: (i) and the re-armed remainder of (v) the bounded
re-check loop of D-70 as amended — a fixed-granularity `wait`, a `call` to the operation that
owns the deadline and a `switch` that loops while it answers `due: false`; no Function sleeps,
because a Function is bounded by platform timeout limits and durable waits belong to Workflows
(serverless-runtime DESIGN.md lines 579, 582); (ii) bounded by the retry budget; (iii)
settled as one `call` per wave carrying `lineRefs[]` (D-79); (iv) **no general fallback**: the
design depends on the platform retaining an event delivered between listens, the ask
`cpt-cf-bss-orders-workflow-upreq-serverless-runtime-event-retention-between-listens` (D-124;
the earlier "covered by the poll arms and the re-entry of every stage loop" was wrong and is
withdrawn), and only a lost resume is recovered: by the stopgap poll of D-130 in the approval
stage's resume wait, and by the same poll in every other wait that holds a recorded suspension
(D-133); (vi) assumed, and a readiness item, because no CI test can observe it.

**Review trigger**: the platform readiness gate (`…-upreq-serverless-runtime-readiness-gate`).

### Q-12: Engine-history isolation, retention and residency — the pending half of Q-01

**Owner**: Architecture (with the serverless-runtime owners and the residency owner).

**Open.** ADR-0013 bounds engine history to references, but `correlationId`, `orderId`,
`orderVersion` and `resource_tenant_id` (with the rest of the D-131 vocabulary: record references,
counters, cardinalities) with their timestamps still sit in a Temporal persistence backend whose location and
retention the platform sets (serverless-runtime ADR-0004 line 100; `TenantRuntimePolicy`,
serverless-runtime DESIGN.md line 739), and the platform has no data-classification model for
execution history (`NEXT_ADR_SCOPE.md` line 23). PRD §15 requires the isolation, retention and
residency of that history to be assessed. It cannot be, from this side, until the platform states
them: the upstream ask is `cpt-cf-bss-orders-workflow-upreq-serverless-runtime-history-residency-retention`.
Until it closes, the platform path is not ready for a residency-bound tenant (`DESIGN.md` §2.2),
and the "commercial data in engine history" threat is *bounded, not closed* (`DESIGN.md` §4.2).

**Review trigger**: the platform readiness gate; Q-01 closes fully when this question does.

### Q-13: Which caller-facing route or event sends `reauthorize-requested` and `unpark-requested`?

**Owner**: Architecture, with Product.

**Open.** The canonical definition has arms for two operator signals that no route in this design
set can send: `reauthorize-requested` (a payment re-authorisation, slice 04) and `unpark-requested`
(an operator's release of an instance parked at the lifetime ceiling). `design/09-read-and-authz.md`
§2.1 forbids Orders to deliver a signal from anywhere but an authorized control operation, but
that binds only Orders: `:plugin-control` is authorized platform-side (`ADR/0010`), so any caller
the platform authorizes on the invocation can deliver either signal. **Amended (2026-09-26)** by
D-122: the `unpark-requested` arm is therefore removed from the canonical definition now, not at
the readiness gate; the signal type stays reserved, `unpark` of a ceiling park refuses without a
recorded operator retry of the ceiling's task, and the ceiling task's `retry` is the one route
out besides the order cancel. The `reauthorize-requested` arm stays: what it calls,
`evaluate-payment-auth-eligibility`, re-reads Payments, the authority for the outcome, so a
signal no Orders route recorded can only cause an early re-evaluation. The question is which caller-facing route — with its catalogue pair, scope,
idempotency key and request record — or which event sends each; this register does not invent
them. **If neither has an origin by the platform readiness gate, both arms are removed from the
canonical definition** and the corresponding cases fall back to the paths that exist (the
`OrderAcceptanceRecorded` and poll arms for re-authorisation; the unwind path for a lifetime-ceiling
park).

**Review trigger**: the platform readiness gate.

### Q-14: How does an approval gate map onto a `cf-gears-bss-approval` unit once the library adapter replaces the stand-in?

**Owner**: Architecture, with Product for the requirement thresholds.

**Open.** D-197 makes this gear's approval-policy adapter the policy owner and names the built
library as its intended implementation, without designing the mapping. The candidate this
register expects, to be confirmed or replaced: a gate is one approval unit (`ApprovalSubject` kind
`order_version`, `ref_type` the order, one unit per gate party, `validate_submit` carrying the
TCV-threshold policy); the Approver Inbox decision route is `Engine::approve` / `Engine::reject`
with the client's `seen_generation`, the library's submitter exclusion doubling D-56's separation
of duties; the requirement verdict, the thresholds and the sequential positions live in
`owf_seller_policy` (D-140) since the library holds quorum within a unit but no routing; the four
library tables are created under this gear's prefix through `bss_approval::ddl::apply_up(manager,
"owf_", Some("bss"))`; the decision event is enqueued on this gear's own topic (D-177) in the same
transaction as `Engine::approve`; escalation stays this gear's (D-10). What the library does not
give — the requirement verdict, positions, the escalation path — stays in this gear's record.

**Review trigger**: before the library adapter is bound; answering it closes the §15 approval row
of the PRD and the open item of `UPSTREAM_REQS.md` §2.3.

## What This Design Set Does Not Claim

Following the sibling Orders Lifecycle design set's own precedent: the coherence of this design
set — that its ten `design/` documents (the foundation, eight capability slices and the process
definition) and this `DECISIONS.md` register agree with each other and with the PRD — is asserted
here by reviewer-checkable statements, not enforced by CI. This is
because `.cf-studio/config/artifacts.toml` excludes `docs/design/*.md` and `DECISIONS.md` from
`cfs` autodetect, and `cfs` hardcodes its artifact-kind set, so a `DESIGN_SLICE` kind cannot be
registered for either document type. Only `DESIGN.md`, `ADR/*.md`, and `UPSTREAM_REQS.md` are
gate-enforced by `cfs validate --artifact` for this gear. Nothing in this register should be read
as CI-verified; every propagation address above was checked by hand against the heading it names,
and that hand-check is the only guarantee this document offers.

Two further non-claims follow from D-65. **The canonical definitions are documentation until the
platform readiness gate passes**: serverless-runtime has no host, SDK or Temporal plugin in this
repository, so nothing in `design/10-process-definition.md` has been parsed by the platform,
published to a registry or executed, and the YAML fragments are asserted against the Serverless
Workflow v1.0.0 specification by reading, not by a tool. **No claim is made that the platform
validation hook exists**: the ADR-0012 fence at publish time is the CI conformance test this design
specifies (also not yet implemented) plus the platform-operator publish role; the consumer-registered
pre-publish hook is an upstream ask (`UPSTREAM_REQS.md` §2.9), and every platform capability this
register relies on is cited to a serverless-runtime file and line or registered as such an ask.


## Traceability

| Decision | Slice | Consuming document |
|----------|-------|---------------------|
| D-01–D-04 | 01 Process engine | `design/01-foundation.md` |
| D-05–D-08 | 02 Triggers and start | `design/02-triggers-and-start.md` |
| D-09–D-14 | 03 Approval execution | `design/03-approval-execution.md` |
| D-15–D-18 | 04 Fulfillment plan | `design/04-fulfillment-plan.md` |
| D-19–D-20 | Commercial policy (owned elsewhere) | `PRD.md` § 15 |
| D-21–D-26 | 05 Provisioning intents | `design/05-provisioning-intents.md` |
| D-27–D-31 | 06 Saga and compensation | `design/06-saga-and-compensation.md` |
| D-32–D-34 | 07 Manual tasks | `design/07-manual-tasks.md` |
| D-35–D-36 | 08 Hold and cancel | `design/08-hold-and-cancel.md` |
| D-37–D-38 | 09 Read and authorization | `design/09-read-and-authz.md` |
| D-39–D-40 | K Tuning baselines (engine) | `design/01-foundation.md` §4.2, §4.5 |
| D-41–D-42 | K Tuning baselines (engine) | `design/01-foundation.md` §4.2 |
| D-43 | K Tuning baselines (engine) | `design/01-foundation.md` §4.5 |
| D-44 | K Tuning baselines (concurrency) | `design/01-foundation.md` §4.12, §4.8 |
| D-45 | K Tuning baselines (intent path) | `design/05-provisioning-intents.md` §4.2 |
| D-46 | K Tuning baselines (approval path) | `design/03-approval-execution.md` §4.0 |
| D-47 | L Cross-cutting (keys) | `design/03-approval-execution.md` §3.7, `design/05-provisioning-intents.md` §3.7, `design/06-saga-and-compensation.md` §2.1 |
| D-48 | L Cross-cutting (tenancy) | every slice §3.7; `DESIGN.md` §3.7 table registry |
| D-49 | L Cross-cutting (concurrency control) | `design/01-foundation.md` §3.7, `design/07-manual-tasks.md` §3.7, `design/09-read-and-authz.md` §3.3 |
| D-50 | L Cross-cutting (audit) | `design/01-foundation.md` §3.7, §4.6 |
| D-51 | L Cross-cutting (process phase) | `design/01-foundation.md` §3.7, `design/03-approval-execution.md` §3.6 |
| D-52 | L Cross-cutting (sweep) | `design/05-provisioning-intents.md` §4.2 |
| D-53 | L Cross-cutting (lifetime bound) | `design/08-hold-and-cancel.md` §3.6, `design/01-foundation.md` §4.2 |
| D-54 | L Cross-cutting (partial failure) | `design/04-fulfillment-plan.md` §3.6, `design/07-manual-tasks.md` §3.6 |
| D-55 | L Cross-cutting (remediation) | `design/07-manual-tasks.md` §3.6, `design/06-saga-and-compensation.md` §3.6 |
| D-56 | L Cross-cutting (separation of duties) | `design/03-approval-execution.md` §3.3, `design/09-read-and-authz.md` §4.1 |
| D-57 | K Tuning baselines (approval path) | `design/03-approval-execution.md` §4.2, `ADR/0007-cpt-cf-bss-orders-workflow-adr-fail-closed-verdict-park.md`, `UPSTREAM_REQS.md` (`…-upreq-submitted-ttl-visibility`) |
| D-58 | L Cross-cutting (process events) | `ADR/0008-cpt-cf-bss-orders-workflow-adr-outbox-process-events.md`, `design/01-foundation.md` §3.2, §3.6, §3.7, §4.7, `DESIGN.md` §3.4, §3.7, §3.8, §4.5, `design/02-triggers-and-start.md` §2.1, `design/05-provisioning-intents.md` §2.1, `UPSTREAM_REQS.md` §2.7 |
| D-59 | L Cross-cutting (audit) | `design/01-foundation.md` §1.2, §3.1, §3.2, §3.7, §3.8, §4.6, §4.17, `DESIGN.md` §3.7, §4.2, §4.3 |
| D-60 | L Cross-cutting (audit hash contract) | `design/01-foundation.md` §3.7, §4.17, `DESIGN.md` §4.2 |
| D-61 | L Cross-cutting (audit identity) | `DESIGN.md` §4.3, §4.2, `design/01-foundation.md` §3.1, §3.7, §4.17, `UPSTREAM_REQS.md` §2.6 |
| D-62 | L Cross-cutting (worker coordination) | `design/01-foundation.md` §1.3, §3.4, §3.8, §4.15, `DESIGN.md` §1.3, §2.2, §3.4, §3.8, §4.1, §4.2, §4.5, Q-09 |
| D-63 | L Cross-cutting (authorization) | `ADR/0010-cpt-cf-bss-orders-workflow-adr-platform-pdp-authorization.md`, `design/09-read-and-authz.md` §2, §3, §4.1, §4.2, §4.4, `DESIGN.md` §2.2, §3.4, §3.5, §3.7, §4.2, §4.8, `design/03-approval-execution.md` §3.2, §3.3, `design/07-manual-tasks.md` §3.2, §3.3, `UPSTREAM_REQS.md` §2.8, D-37 |
| D-64 | L Cross-cutting (error contract) | `design/01-foundation.md` §3.2, §3.3, §4.9, `DESIGN.md` §3.3, `design/03-approval-execution.md` §3.3, `design/09-read-and-authz.md` §3.3, §4.4 |
| D-65 | M Platform definition (ADR-0011) | `ADR/0011`, `ADR/0001`, `design/10-process-definition.md`, `design/01-foundation.md` §1.1, §3.2, §3.3, `DESIGN.md` §1, §2.1, §3.2, §3.5, §4.9, `design/README.md`, Q-01, Q-12, `UPSTREAM_REQS.md` §4 |
| D-66 | M References, not payloads (ADR-0013) | `ADR/0013`, `design/01-foundation.md` §2.1, §3.3, §4.14, `design/10-process-definition.md` §2.1, §2.2, every slice §3.3, `DESIGN.md` §2.1, §4.2, §4.3, `UPSTREAM_REQS.md` §2.3, §2.4, §2.9 |
| D-67 | M Protected-step fence (ADR-0012) | `ADR/0012`, `design/10-process-definition.md` §2.2, §4.1, §4.2, §4.6, every slice §3.3, `design/09-read-and-authz.md` §3.1, `DESIGN.md` §2.2, §4.2 |
| D-68 | M Pinning and publish roles (ADR-0012) | `ADR/0012`, `design/01-foundation.md` §3.3, §3.7, `design/10-process-definition.md` §2.2, §4.3, `DESIGN.md` §3.6, `UPSTREAM_REQS.md` §2.9, Q-10 |
| D-69 | M Step operations and the step surface | `design/01-foundation.md` §3.2, §3.3, §3.7, `design/09-read-and-authz.md` §3.1, §3.3, §4.1, `DESIGN.md` §3.3, §4.2, D-37, D-48, `ADR/0010` |
| D-70 | M Timers and retry policy to the platform | `design/01-foundation.md` §3.7, §4.2, §4.4, `design/03-approval-execution.md` §3.7, `design/08-hold-and-cancel.md` §3.2, §3.7, `design/10-process-definition.md` §2.2, `DESIGN.md` §3.7, §4.8; D-02, D-10, D-11, D-39–D-46, D-49, D-53 |
| D-71 | M Three-worker roster | `design/01-foundation.md` §3.8, `design/05-provisioning-intents.md` §3.8, `DESIGN.md` §3.8, D-62, Q-09 |
| D-72 | M Inbound dead letters to the platform | `ADR/0009`, `design/01-foundation.md` §3.3, §3.7, §4.8, `design/07-manual-tasks.md` §3.7, `design/09-read-and-authz.md` §3.1, §4.1, `DESIGN.md` §3.7, §4.4, §4.5, `UPSTREAM_REQS.md` §2.9, §4; D-04, D-05, D-44 |
| D-73 | M Start via the platform event trigger | `design/02-triggers-and-start.md` §2.2, §3.3, `design/10-process-definition.md` §2.2, §3.3, `design/09-read-and-authz.md` §3.1, §3.3, §4.1, `DESIGN.md` §3.3, `UPSTREAM_REQS.md` §2.8, §4, `ADR/0010` |
| D-74 | M Key families | `ADR/0006`, `design/01-foundation.md` §3.3, `design/02-triggers-and-start.md` §2.1, `design/04-fulfillment-plan.md` §3.3, §4.1, `design/06-saga-and-compensation.md` §3.3; D-47 |
| D-75 | M Supersession | `design/02-triggers-and-start.md` §4.3, §4.7, `design/10-process-definition.md` §3.6; D-06, D-07, D-49 |
| D-76 | M Seller axis at start | `design/01-foundation.md` §3.3, `design/02-triggers-and-start.md` §3.3 |
| D-77 | M Reason catalogue | `design/01-foundation.md` §3.3, §4.9, slices 02, 03, 04, 05, 06, 07 §3.3, `DESIGN.md` §3.3; D-64 |
| D-78 | M Barrier and park patterns | `ADR/0004`, `ADR/0007`, `design/03-approval-execution.md` §3.3, §4.2, `design/04-fulfillment-plan.md` §3.3, `design/05-provisioning-intents.md` §3.3, `design/10-process-definition.md` §3.6; D-12, D-23, D-24 |
| D-79 | M One call per wave | `design/05-provisioning-intents.md` §3.3, §4.3, `design/10-process-definition.md` §2.2 |
| D-80 | M Definition routing conventions | `design/10-process-definition.md` §3.6, §4.5, `design/08-hold-and-cancel.md` §3.3, §3.6, §4.7, `design/01-foundation.md` §3.7, `design/03-approval-execution.md` §4.5, `design/07-manual-tasks.md` §4.8, `ADR/0011`; D-35 |
| D-81 | M Shared unwind path | `design/10-process-definition.md` §3.6, §4.1, `design/06-saga-and-compensation.md` §3.3 |
| D-82 | M Lifetime-ceiling park from hold | `design/01-foundation.md` §3.7, `design/08-hold-and-cancel.md` §2.1, §4.8; D-53 |
| D-83 | M compensate-order | `ADR/0005`, `design/06-saga-and-compensation.md` §3.2, §3.3, §3.7, `design/10-process-definition.md` §3.6 |
| D-84 | M Cancel re-check and withdrawn authority | `design/06-saga-and-compensation.md` §3.2, §3.7, `design/08-hold-and-cancel.md` §3.3, §4.3, `design/10-process-definition.md` §3.6 |
| D-85 | M Cancel request record | `design/09-read-and-authz.md` §3.1, §3.7, `design/08-hold-and-cancel.md` §3.3, `design/07-manual-tasks.md` §3.7, `DESIGN.md` §3.7 |
| D-86 | M Operator re-drive | `design/09-read-and-authz.md` §3.3, `design/01-foundation.md` §3.3, `UPSTREAM_REQS.md` §2.9, `DESIGN.md` §4.5 |
| D-87 | M Outage threshold and probe arm | `design/03-approval-execution.md` §4.1, §4.2, `design/08-hold-and-cancel.md` §3.7, `design/10-process-definition.md` §3.6; D-11, D-46 |
| D-88 | M Routing plan | `design/03-approval-execution.md` §3.7, §4.3 |
| D-89 | M Tolerate-failure evaluator | `design/04-fulfillment-plan.md` §3.2, §3.3, `UPSTREAM_REQS.md` §4 |
| D-90 | M Payments coupling | `design/04-fulfillment-plan.md` §3.4, §3.6, `design/10-process-definition.md` §3.6, `design/09-read-and-authz.md` §4.1; D-17 |
| D-91 | M Pre-activation re-check | `design/04-fulfillment-plan.md` §4.2, `design/01-foundation.md` §4.9, Q-08, `UPSTREAM_REQS.md` §2.1; D-15 |
| D-92 | M Plan-level failures | `design/04-fulfillment-plan.md` §4.3, `design/07-manual-tasks.md` §3.7 |
| D-93 | M Sizes | `design/04-fulfillment-plan.md` §4.6, `DESIGN.md` §4.1 |
| D-94 | M Automatic compensation route | `design/04-fulfillment-plan.md` §3.6, `design/07-manual-tasks.md` §4.3; D-54, D-55 |
| D-95 | M Operator retry state | `design/04-fulfillment-plan.md` §3.7, `design/05-provisioning-intents.md` §3.3, `design/07-manual-tasks.md` §3.3 |
| D-96 | M Admission deferral | `design/01-foundation.md` §4.12, `design/05-provisioning-intents.md` §2.1, §3.7, §4.3, `DESIGN.md` §3.7; D-44 |
| D-97 | M Intent statuses and Subscriptions tuple | `design/05-provisioning-intents.md` §3.7, §4.1, `UPSTREAM_REQS.md` §2.1, §2.9; D-131 |
| D-98 | M Manual-task reasons and SLA classes | `design/07-manual-tasks.md` §3.7, §4.1, §4.7 |
| D-99 | M Remediation hold and exhaustion | `design/07-manual-tasks.md` §3.3, §4.3, `design/04-fulfillment-plan.md` §4.4, §4.7 |
| D-100 | M Task actions and keys | `design/07-manual-tasks.md` §3.3, `design/09-read-and-authz.md` §3.3, §4.1, `DESIGN.md` §3.3, `ADR/0010` |
| D-101 | M Task-queue sort key | `design/07-manual-tasks.md` §3.3, `design/09-read-and-authz.md` §2.2 |
| D-102 | M Rounds and attempts for re-invokable operations | `design/01-foundation.md` §3.3, §3.7, §4.3, §4.14, `design/03-approval-execution.md` §3.3, §3.6, §4.1, §4.4, §4.5, `design/04-fulfillment-plan.md` §3.3, §3.6, §4.1, §4.8, `design/05-provisioning-intents.md` §3.3, §3.6, §4.5, `design/06-saga-and-compensation.md` §3.3, §3.6, `design/07-manual-tasks.md` §3.3, §3.6, §4.2, `design/08-hold-and-cancel.md` §3.3, §3.6, `design/10-process-definition.md` §3.1, §3.6, `UPSTREAM_REQS.md` §2.9, `ADR/0006`, `ADR/0011`; D-74, D-78 |
| D-103 | M Registry lease fence and dead-lease resolution | `design/01-foundation.md` §3.3, §3.6, §3.7, §3.8, §4.3, `design/05-provisioning-intents.md` §3.2, §3.6, §4.4, `design/06-saga-and-compensation.md` §3.6, `DESIGN.md` §3.7, §3.8; D-03, D-71 |
| D-104 | M No partitioned tables; registry tombstones | `design/01-foundation.md` §3.7, §3.8, every slice §3.7, `DESIGN.md` §3.7, §4.1, `design/09-read-and-authz.md` §3.7, §4.3; Lifecycle D-91 |
| D-105 | M Dead invocation: liveness pass, re-drive, fallback unwind | `design/01-foundation.md` §3.3, §3.7, §3.8, §4.9, §4.13, §4.16, `design/05-provisioning-intents.md` §3.8, `design/06-saga-and-compensation.md` §3.2, §3.6, §3.8, `design/07-manual-tasks.md` §3.3, §3.7, §4.1, §4.4, §4.7, §4.8, `design/09-read-and-authz.md` §3.3, `design/10-process-definition.md` §3.3, §3.8, §4.4, `DESIGN.md` §1.2, §3.3, §3.8, §4.2, §4.4, §4.5, §4.9, `UPSTREAM_REQS.md` §2.9, §3, §4, `ADR/0001`, `ADR/0005`, `ADR/0012`; D-64, D-71, D-86 |
| D-106 | M Invocation binding and the fence's recorded cause | `design/01-foundation.md` §3.3, §3.6, §3.7, `design/02-triggers-and-start.md` §3.6, `design/06-saga-and-compensation.md` §3.3, §3.6, `design/09-read-and-authz.md` §2.2, §4.1, §4.2, `design/10-process-definition.md` §3.6, `DESIGN.md` §3.3, §4.2, `UPSTREAM_REQS.md` §2.9, `ADR/0012`; D-67 |
| D-107 | M Start checked against the order; bindings released with the definition | `design/02-triggers-and-start.md` §2.1, §2.2, §3.1, §3.3, §3.6, §3.8, §4.7, `design/10-process-definition.md` §3.3, §3.6, §3.8, `DESIGN.md` §3.5, §4.2, `UPSTREAM_REQS.md` §2.9; D-73, D-76 |
| D-108 | M `retry-step` in-process only | `design/01-foundation.md` §3.3, `design/07-manual-tasks.md` §4.6, `design/09-read-and-authz.md` §3.1, §4.2, §4.5, `design/10-process-definition.md` §2.2, §4.1, `DESIGN.md` §3.3, §3.5, §4.2, `UPSTREAM_REQS.md` §2.8; D-67, D-69 |
| D-109 | M Reporting only from fulfillment; pre-fulfillment cancel is Lifecycle's | `design/01-foundation.md` §4.16, `design/04-fulfillment-plan.md` §3.6, §4.3, §4.8, `design/05-provisioning-intents.md` §3.3, §4.5, `design/06-saga-and-compensation.md` §3.2, §3.6, §4.1, §4.9, `design/07-manual-tasks.md` §4.4, `design/08-hold-and-cancel.md` §3.3, §3.6, §4.7, `design/09-read-and-authz.md` §3.3, §3.7, `design/10-process-definition.md` §3.6, `DESIGN.md` §3.3, §3.6; D-84, D-92, D-105 |
| D-110 | M Failure reason resolved from the record and mapped | `design/04-fulfillment-plan.md` §3.6, §4.3, `design/06-saga-and-compensation.md` §3.3, §3.6, §3.7, §4.3, §4.8, `design/10-process-definition.md` §3.6, `UPSTREAM_REQS.md` §1.1, §1.2, §2.4, §3, §5; D-106 |
| D-111 | M Report follows the fence row; cancel reason; every Lifecycle answer | `design/04-fulfillment-plan.md` §2.1, `design/06-saga-and-compensation.md` §3.2, §3.3, §3.6, §3.7, §4.3, §4.7, §4.9, `design/09-read-and-authz.md` §3.3, §3.7, `design/10-process-definition.md` §3.6, `DESIGN.md` §3.3, §3.6 |
| D-112 | M `reflect-verdict` wire form and refusals | `design/03-approval-execution.md` §3.3, §3.6, §4.4, `design/10-process-definition.md` §3.6 |
| D-113 | M No Workflow acceptance pre-check | `design/04-fulfillment-plan.md` §1.2, §3.2, §3.3, §3.6 |
| D-114 | H One failure rule: fault unless a subject | `design/10-process-definition.md` §2.2, §3.6, §4.6, `design/01-foundation.md` §3.3, §4.5, §4.8, §4.13, `design/02-triggers-and-start.md` §4.2, §4.7, `design/04-fulfillment-plan.md` §4.8, `design/06-saga-and-compensation.md` §4.7, `design/07-manual-tasks.md` §3.3, §3.7, §4.1, §4.4, §4.8, `design/08-hold-and-cancel.md` §1.1, §1.2, §2.1, §3.2, §3.3, §3.6, §4.7, `design/09-read-and-authz.md` §4.4, §4.6, `DESIGN.md` §1.2, §4.4; D-84, D-105 |
| D-115 | M No task for a pre-fence authority denial | `design/08-hold-and-cancel.md` §1.2, §3.2, §3.3, §3.6, §4.3, `design/09-read-and-authz.md` §4.4, `design/07-manual-tasks.md` §3.2, §3.3, §4.4, `design/10-process-definition.md` §3.6, `ADR/0010`, `UPSTREAM_REQS.md` §2.8; D-84 |
| D-116 | M Task body: subject reason and cause, failing call's tail | `design/07-manual-tasks.md` §3.2, §3.3, §3.6, §3.7, `design/08-hold-and-cancel.md` §3.3, §3.6, `design/10-process-definition.md` §3.6, `design/06-saga-and-compensation.md` §4.8 |
| D-117 | M Remediation hold until the last open task | `design/07-manual-tasks.md` §3.3, §3.6, §4.3, §4.8, `design/05-provisioning-intents.md` §4.5, `design/10-process-definition.md` §3.6; D-99 |
| D-118 | M Escalation and park-clock algorithms | `design/03-approval-execution.md` §3.2, §3.3, §3.6, §3.7; D-87, D-102 |
| D-119 | H Retried and never-written lines re-dispatched from the record | `design/04-fulfillment-plan.md` §3.3, §3.6, §3.7, §4.8, `design/05-provisioning-intents.md` §1.1, §1.2, §2.2, §3.2, §3.3, §3.6, §3.7, §4.2, §4.4, §4.5, §4.6, `design/07-manual-tasks.md` §3.3, §3.6, `design/01-foundation.md` §3.3, §4.3, `design/06-saga-and-compensation.md` §2.1, `design/10-process-definition.md` §3.6, `DESIGN.md` §1.2, `ADR/0006`; D-21, D-95, D-102, D-117 |
| D-120 | M The read after a dispatch 409 is routed | `design/10-process-definition.md` §3.6, `design/05-provisioning-intents.md` §3.3, §4.5; D-102 |
| D-121 | H Ceiling parks only a running, unparked instance; ceiling rounds | `design/10-process-definition.md` §3.6, `design/01-foundation.md` §3.3, §3.7, `design/07-manual-tasks.md` §3.2, §3.3, §3.6, §3.7, §4.8, `design/03-approval-execution.md` §2.1, `design/08-hold-and-cancel.md` §4.5; D-53, D-82, D-102 |
| D-122 | H Ceiling wait consumes only its task; no unrecorded unpark | `design/10-process-definition.md` §2.2, §3.1, §3.3, §3.6, `design/01-foundation.md` §3.3, `design/07-manual-tasks.md` §4.4, §4.8, `DESIGN.md` §3.1; D-106, Q-13 |
| D-123 | M Escalation re-check on the probe tick | `design/10-process-definition.md` §1.2, §3.6, `design/03-approval-execution.md` §1.2, §3.6, §4.5, `design/08-hold-and-cancel.md` §1.2, §3.3, §3.6, §4.7, `DESIGN.md` §1.2, §4.1, `ADR/0011`; D-70, D-80 |
| D-124 | M Events between listens: platform retention | `design/10-process-definition.md` §4.4, §4.5, `design/08-hold-and-cancel.md` §4.7, `DESIGN.md` §1.2, `UPSTREAM_REQS.md` §2.9, §3; Q-11 |
| D-125 | M Worker trips handed to the definition | `design/05-provisioning-intents.md` §3.2, §3.3, §3.6, §3.7, §4.5, `design/07-manual-tasks.md` §3.6, `design/08-hold-and-cancel.md` §3.2, §3.6, `design/10-process-definition.md` §3.6; D-105, D-120 |
| D-126 | H Validation rules over definition-held values and a routing graph | `design/10-process-definition.md` §1.2, §2.2, §3.2, §3.6, §4.1, §4.5, §4.6, `design/07-manual-tasks.md` §4.8, `design/01-foundation.md` §4.2, `ADR/0012`, `ADR/0011`; D-67, Q-11 |
| D-127 | M Every required Workflow trait declared | `design/10-process-definition.md` §3.1, `UPSTREAM_REQS.md` §2.9, `ADR/0011`; D-86, D-105 |
| D-128 | M Engine history growth asked of the plugin | `design/10-process-definition.md` §3.1, §3.6, `UPSTREAM_REQS.md` §2.9, §3, `DESIGN.md` §4.1; D-123 |
| D-129 | M Ceiling task SLA tick; ceiling park ended by the order cancel | `design/10-process-definition.md` §3.6, `design/07-manual-tasks.md` §3.3, §3.6, §4.4, §4.8, `design/01-foundation.md` §3.3; D-105, D-122 |
| D-130 | M Resume-wait poll of the hold | `design/10-process-definition.md` §3.6, §4.1, §4.4, §4.5, `design/08-hold-and-cancel.md` §3.3, §3.6, §3.7, §4.7, `design/01-foundation.md` §3.3, `design/07-manual-tasks.md` §3.3, `UPSTREAM_REQS.md` §2.9; D-124, Q-11 |
| D-131 | H Closed reference vocabulary; residual stated in full | `ADR/0013`, `design/01-foundation.md` §1.1, §2.1, `design/10-process-definition.md` §1.1, §2.1, §2.2, `DESIGN.md` §4.2, §4.3, `UPSTREAM_REQS.md` §2.9, §4; D-66, Q-01, Q-12 |
| D-132 | M Step-route error answers carry fixed members only | `ADR/0013`, `design/01-foundation.md` §4.9, §4.11, `design/10-process-definition.md` §2.1, §2.2, `DESIGN.md` §4.2, `UPSTREAM_REQS.md` §2.9; D-66 |
| D-133 | M Every wait holding a recorded suspension polls the hold | `design/10-process-definition.md` §3.6, §4.1, §4.4, §4.5, `design/08-hold-and-cancel.md` §3.2, §3.3, §4.7, `design/01-foundation.md` §3.3, `DESIGN.md` §1.2, `UPSTREAM_REQS.md` §2.9; D-124, D-130, Q-11 |
| D-134 | H Business windows are seller-policy values pinned on the record | `design/01-foundation.md` §2.1, §4.2, §4.4, `design/03-approval-execution.md` §3.2, §3.6, §3.7, `design/04-fulfillment-plan.md` §2.2, §3.6, §3.7, §4.7, §4.8, `design/07-manual-tasks.md` §1.2, §3.2, §3.6, §3.7, §4.1, §4.8, `design/10-process-definition.md` §1.1, §1.2, §2.2, §3.6, §4.7, `DESIGN.md` §1.2, §4.7, §4.8, `UPSTREAM_REQS.md` §4 item 14, `ADR/0011`; D-70, D-98, D-126 |
| D-135 | M `p1` composables and shared arms required on their paths; pinned enums tracked | `design/10-process-definition.md` §2.1, §2.2, §4.1, §4.7, `design/01-foundation.md` §3.1, §3.3, `design/03-approval-execution.md` §3.3, `design/07-manual-tasks.md` §3.3, `DESIGN.md` §2.2, `ADR/0011`, `ADR/0012`; D-126 |
| D-136 | M One adjustability table; enforced items and guidance; no Function call | `design/10-process-definition.md` §1.1, §2.1, §2.2, §4.7, `design/02-triggers-and-start.md` §4.7, `design/03-approval-execution.md` §4.5, `design/04-fulfillment-plan.md` §4.8, `design/05-provisioning-intents.md` §4.5, `design/06-saga-and-compensation.md` §4.7, `design/07-manual-tasks.md` §4.8, `design/08-hold-and-cancel.md` §4.7, `design/09-read-and-authz.md` §4.6, `DESIGN.md` §4.7, `ADR/0011`, `ADR/0012`; Q-10 |
| D-137 | H Binding version from the invocation record; publisher from the registry | `design/01-foundation.md` §3.3, §3.6, §3.7, `design/10-process-definition.md` §2.2, §3.1, §3.3, §3.6, §4.2, §4.3, `DESIGN.md` §3.1, §3.6, §4.2, `UPSTREAM_REQS.md` §2.9, `ADR/0012`; D-68, D-105 |
| D-138 | M Publish job, behavioural gate, forward rollback | `design/10-process-definition.md` §2.2, §3.2, §3.3, §3.7, §3.8, §4.2, §4.3, `design/01-foundation.md` §3.7, `DESIGN.md` §2.2, §3.6, §3.8, §4.7, `UPSTREAM_REQS.md` §2.9, §3, `ADR/0011`, `ADR/0012`; D-68, D-107 |
| D-139 | M Build order from in-process calls; back-edges | `design/README.md`, `ADR/0002`, `DESIGN.md` §3.2 |
| D-140 | H Seller-policy store: foundation table, policy channel, one port, pinned revision | `design/01-foundation.md` §2.1, §3.4, §3.7, §4.2, §4.11, `design/03-approval-execution.md` §3.6, `design/04-fulfillment-plan.md` §3.6, `design/07-manual-tasks.md` §4.1, §4.8, `design/10-process-definition.md` §4.7, `DESIGN.md` §3.7, §4.7, §4.8, `UPSTREAM_REQS.md` §4 item 14; D-48, D-59, D-63, D-69, D-104, D-134 |
| D-141 | M A resume closes only a suspension Lifecycle no longer holds | `design/08-hold-and-cancel.md` §2.2, §3.3, §3.6, §4.2, §4.7, `design/10-process-definition.md` §3.6; D-87, D-130 |
| D-142 | M Every stage wait is a fork with the shared arms | `design/10-process-definition.md` §2.2, §3.6, `ADR/0012`; D-120, D-133, D-135 |
| D-143 | M No wave dispatch without begin-fulfillment; `beginResult` pinned | `design/10-process-definition.md` §2.2, §3.6, §4.1, `design/08-hold-and-cancel.md` §4.7, `ADR/0012`; D-126, D-130, D-133, D-135 |
| D-144 | H Rule 1 decides every route on tracked members; ten pinned members | `design/10-process-definition.md` §2.2, §3.6, §4.1, `design/07-manual-tasks.md` §3.6, §4.7, §4.8, `design/05-provisioning-intents.md` §4.5, `ADR/0012`; D-126, D-135, D-143 |
| D-145 | M Sent spawn leaves the held-spawn loop; not-dispatchable re-check waits there | `design/10-process-definition.md` §3.6, `design/04-fulfillment-plan.md` §4.8; D-109 |
| D-146 | H The failure stage never waits with no open task | `design/10-process-definition.md` §3.6, `design/07-manual-tasks.md` §3.3, §4.4, §4.8; D-117, D-129 |
| D-147 | M A call that consumes an arm's payload is its own checkpoint | `design/10-process-definition.md` §3.6; D-121 |
| D-148 | M Escalation bound counts every call between two fires | `design/10-process-definition.md` §1.2, §3.6, `design/03-approval-execution.md` §1.2, §4.5, `design/08-hold-and-cancel.md` §1.2, `DESIGN.md` §1.2; D-123 |
| D-149 | H Each ceiling's task is its own row, closed by the fence | `design/07-manual-tasks.md` §3.2, §3.6, §3.7, §4.3, §4.4, §4.7, `design/06-saga-and-compensation.md` §3.4, §3.6, `design/10-process-definition.md` §3.6; D-121, D-122, D-129 |
| D-150 | L Poll task tail names its suspension; only apply-hold resets the round | `design/10-process-definition.md` §3.6, `design/07-manual-tasks.md` §3.3, `design/08-hold-and-cancel.md` §4.7, `ADR/0013`; D-116, D-130, D-133 |
| D-151 | M Liveness pass observes the SLA of tasks with no live waiter | `design/01-foundation.md` §3.7, §3.8, `design/07-manual-tasks.md` §3.3, §3.6, §4.2, §4.8; D-105 |
| D-152 | M Quarantine counts only presented attempts | `design/01-foundation.md` §3.3, §4.13; D-119 |
| D-153 | M Re-arm of an unresolved intent is slice 05's port | `design/05-provisioning-intents.md` §3.2, §3.7, §4.4, `design/07-manual-tasks.md` §3.4, §3.6; D-119, D-125 |
| D-154 | M `create-manual-task` verifies the naming route | `design/07-manual-tasks.md` §3.6; D-116, D-119 |
| D-155 | M `supersededByOrderVersion` from admit-trigger | `design/10-process-definition.md` §3.6, `design/02-triggers-and-start.md` §4.3, `ADR/0013` |
| D-156 | L Formats of every listed identity member | `ADR/0013`, `design/10-process-definition.md` §2.2; D-131 |
| D-157 | M Limits sized from long-wait residency and three ceilings | `design/10-process-definition.md` §3.1, `DESIGN.md` §4.1, `UPSTREAM_REQS.md` §2.9; D-127 |
| D-158 | M Rollback document carries its version; failed gate rolled back; ticks compared | `design/10-process-definition.md` §3.8, §4.2, §4.7, `DESIGN.md` §3.6, `ADR/0012`; D-138 |
| D-159 | M Overdue bound checked both ways over the live version set | `design/01-foundation.md` §3.7, §4.2, `design/07-manual-tasks.md` §4.8, `design/10-process-definition.md` §2.2, §4.2, §4.7, `ADR/0012`; D-134, D-140 |
| D-160 | M Configuration revisions in an append-only table | `design/01-foundation.md` §3.2, §3.7, §3.8, §4.11, `design/09-read-and-authz.md` §3.7, `DESIGN.md` §3.7, §4.7, §4.8; D-48, D-63, D-69, D-104, D-140 |
| D-161 | H Admission answers only its own event; ceiling restores the return stage | `design/10-process-definition.md` §2.2, §3.6, `ADR/0012`; D-121, D-144 |
| D-162 | M Gate-loop calls retry under a policy that fits their timeout | `design/10-process-definition.md` §1.2, §2.2, §3.6, `design/01-foundation.md` §4.2, §4.5, `design/03-approval-execution.md` §1.2, §4.5, `ADR/0012`; D-148 |
| D-163 | M A ceiling while parked at a ceiling re-arms and keeps the first ceiling's return state | `design/10-process-definition.md` §3.6 (a), (d) |
| D-164 | H Ceiling wait's own request ref; a ceiling restores the interrupted stage's payload | `design/10-process-definition.md` §2.2, §3.6 (a), (c), (d), (f); D-121, D-147, D-161 |
| D-165 | H Wave answers route deferred, failed, lapsed; only the folding entry clears a failed list | `design/10-process-definition.md` §3.6 (b), (c); D-54, D-116 |
| D-166 | M Completion never answers terminal-event; terminate-instance names terminalOutcome | `design/06-saga-and-compensation.md` §3.3, §3.6, §4.9, `design/10-process-definition.md` §3.6 (a), (b), (c); D-109, D-111 |
| D-167 | H Settled answer on the registry row; step-log purge grant | `design/01-foundation.md` §3.6, §3.7, §3.8, §4.3, `DESIGN.md` §3.7; D-103, D-104 |
| D-168 | H A settlement that cannot commit aborts whole; 503 or 500 | `design/01-foundation.md` §3.3, §3.6, §3.7, §4.3, §4.17, `DESIGN.md` §1.2; D-64, D-103, D-114, D-132 |
| D-169 | H `start-instance` reads the platform before its one transaction | `design/01-foundation.md` §3.3, §3.6, §3.7, §4.3; D-76, D-137 |
| D-170 | M What each receipt records; `audit_kind` is the success kind | `design/01-foundation.md` §3.3, §3.6, §3.7, §4.6, §4.13, `design/02-triggers-and-start.md` §3.7, `DESIGN.md` §1.2; D-103 |
| D-171 | H An early evaluation settles success with `due: false` | `design/01-foundation.md` §4.15; D-102 |
| D-172 | H Pre-admission audit entries may carry no seller | `design/01-foundation.md` §3.7, §4.11, §4.17, `design/02-triggers-and-start.md` §3.6, §3.7; D-60, D-76 |
| D-173 | H READ COMMITTED; guarded pre-admission append; symmetric start-instance collision | `design/01-foundation.md` §3.3, §3.7, §4.17, `design/02-triggers-and-start.md` §3.6; D-59, D-60, D-168 |
| D-174 | H Quarantine counts from a per-family presented-attempt history | `design/01-foundation.md` §3.3, §3.7, §4.13; D-102, D-104, D-119, D-152 |
| D-175 | M terminate-instance on the two permitted edges; `fence-not-claimed` from suspended or parked | `design/01-foundation.md` §3.3, §3.7; D-82, D-114, D-166 |
| D-176 | L Nesting invariant: four enforcement points in §2.1 | `design/01-foundation.md` §2.1; D-126, D-134, D-159 |
| D-177 | H Own topic `…~cf.bss._.orders_workflow.v1` in the abstract type's `x-gts-traits`; no co-partitioning with Lifecycle | `design/01-foundation.md` §3.2, §3.4, §3.8, §4.7, `DESIGN.md` §4.4, `ADR/0008`, `UPSTREAM_REQS.md` §2.7; D-58 |
| D-178 | H Producer rotation rejects every message queued under the old id | `design/01-foundation.md` §3.6, §3.7, §4.7, `DESIGN.md` §4.4, `ADR/0008`, `UPSTREAM_REQS.md` §2.7; D-58 |
| D-179 | M Zero or more events of the declared type per settlement, at most one per subject | `design/01-foundation.md` §3.1, §3.3, §3.6, §4.7, `design/03`–`07` §3.3, `DESIGN.md` §2.1, `ADR/0008`; D-58 |
| D-180 | H Suspension closure port clears `owf_process_instance.suspended` | `design/01-foundation.md` §3.3, §3.7, `design/08-hold-and-cancel.md` §3.7, `design/05-provisioning-intents.md` §3.4; D-82, D-149, D-153 |
| D-181 | M Step-retry route records through slice 07's intake port | `design/07-manual-tasks.md` §3.2, §3.7, `design/09-read-and-authz.md` §3.1–§3.4, §3.6, `design/README.md`, `DESIGN.md` §3.7; D-85, D-139, D-153 |
| D-182 | M Re-authorization mark: the fence's one rewritable stamp | `design/06-saga-and-compensation.md` §3.7, `DESIGN.md` §3.7; D-84, D-106 |
| D-183 | H Poll closes an overtaken suspension before terminate-instance; `rejected`/`completed` are not held | `design/10-process-definition.md` §3.6 (a), (b), (e), §4.1, §4.5, `design/08-hold-and-cancel.md` §3.3, §3.6, §4.7, `design/01-foundation.md` §3.3, `DESIGN.md` §1.2; D-130, D-133, D-141, D-175 |
| D-184 | M Consumed acceptance event and re-authorization signal are their own checkpoints | `design/10-process-definition.md` §3.6 intro, (b); D-147, D-164 |
| D-185 | M Only downstream-submitting keys age while the instance lives; aged-out re-issued under an operator-minted successor | `design/01-foundation.md` §3.2, §3.3, §3.7, §4.3, §4.16, `design/09-read-and-authz.md` §3.1, §3.6, `design/07-manual-tasks.md` §4.4, §4.6, `design/10-process-definition.md` §4.6, `design/05-provisioning-intents.md` §2.2, `DESIGN.md` §3.3, §3.7; D-104, D-105, D-108 |
| D-186 | M Seller-scoped, keyset-paged audit read route; start-instance refusals audited pre-admission | `design/09-read-and-authz.md` §1.2, §2.2, §3.1–§3.3, §4.1, §4.3, `design/01-foundation.md` §3.7, `design/02-triggers-and-start.md` §1.2, §3.6, §3.7, `DESIGN.md` §1.2, §3.2, §3.3, §3.5, `design/README.md`, `ADR/0010`, `UPSTREAM_REQS.md` §2.8; D-172 |
| D-187 | L Contended worker lock skips the pass; only an error is a coordination failure | `design/01-foundation.md` §3.8; D-62 |
| D-188 | H Past-window Lifecycle re-run reads the order back; `already-applied`; Lifecycle key-retention ask | `design/01-foundation.md` §3.3, §3.7, §4.3, `design/03-approval-execution.md` §3.3, §3.6, §4.4, `design/04-fulfillment-plan.md` §3.3, §3.6, `design/05-provisioning-intents.md` §3.2, §3.3, `design/06-saga-and-compensation.md` §3.2, §3.3, §3.6, §4.9, `design/10-process-definition.md` §3.6 (a), (b), `UPSTREAM_REQS.md` §1.2, §2.4, §3; D-102, D-166, D-185 |
| D-189 | M `open-gates` looks a gate's request up by key and adopts it; Generic Approval ask extended | `design/03-approval-execution.md` §3.5, §3.6, `design/01-foundation.md` §3.7, §4.3, `UPSTREAM_REQS.md` §2.3; D-185 |
| D-190 | M `reflect-verdict` answers its refusal `refused`; no 400 catch | `design/03-approval-execution.md` §3.3, §3.6, §4.4, §4.5, `design/10-process-definition.md` §3.6 (a), §4.6, `design/01-foundation.md` §4.3, §4.9, `design/07-manual-tasks.md` §3.3, `ADR/0012`; D-112, D-185 |
| D-191 | M Audit read access-logged: `owf_read_access_log`, one row per request, served or refused | `design/09-read-and-authz.md` §3.2, §3.3, §3.7, §4.3, `design/01-foundation.md` §3.7, §3.8, `DESIGN.md` §3.3, §3.7, §4.4; D-48, D-104, D-186 |
| D-192 | M Dead-invocation re-drive: explicit readiness item gating the action, not readiness | `design/01-foundation.md` §3.8, `design/07-manual-tasks.md` §4.4, `design/10-process-definition.md` §3.3, `UPSTREAM_REQS.md` §2.9; D-86, D-105 |
| D-193 | H PriceBook through Lifecycle's Workflow SDK and `get_version`; retired vocabulary | `DESIGN.md` §2.2, §3.1, §3.5, `design/03` §3.2, §3.4, §3.6, `design/04` §1.3, §3.2, §3.4, §3.6, `design/05` §2.2, §3.2, §3.5, `design/06` §3.2, §3.3, `design/08` §3.5, `ADR/0013`, `UPSTREAM_REQS.md` §1.2, §2.4, §2.5, §5, `PRD.md`; D-66, D-131 |
| D-194 | H Activation deadline checked at freeze, re-check and wave-2 guard; expiry after activation compensates first; `order-binding-expired` | `design/04` §2.2, §3.1, §3.3, §3.6, §3.7, §4.3, §4.6, `design/05` §3.3, §3.6, §4.5, §4.6, `design/06` §3.6, §4.8, `design/07` §3.3, §3.7, §4, `design/08` §2.1, `design/10` §3.6 (b), §4.6, `design/01` §4.9, `UPSTREAM_REQS.md` §2.1, §2.4; D-15, D-91, D-92, D-110 |
| D-195 | H Intent carries the source version tuple as the accepted-binding reference; `activated` = `applied`; `SUB-O5` counts | `design/05` §2.1, §2.2, §3.3, §3.6, §3.7, §4.1, `design/06` §3.3, `design/04` §2.1, `DESIGN.md` §3.1, `UPSTREAM_REQS.md` §2.1; D-97, D-125, Q-08 |
| D-196 | H Lines are independent: dependency graph, Catalog source and `blocked-upstream` withdrawn; catalogue 41 | `design/04` §1–§4, `design/05` §3.2, §3.6, `design/06` §2.1, §3.3, §3.6, §3.7, §4.8, `design/07` §3.3, §3.7, §4, `design/08` §3.2, `design/10` §3.6 (b), `design/01` §4.9, `ADR/0005`, `DESIGN.md` §1.2, §3.5, `PRD.md` §6.1, §12, `UPSTREAM_REQS.md` §1, §2.4, §2.5, §3, §4, §5; D-16, D-77, D-92, D-105, D-110 |
| D-197 | H Approval policy owner = this gear's approval-policy adapter (stand-in, library); Generic Approval service withdrawn | `PRD.md` §1, §3.2, §5, §6, §9.2, §12, §13, §15, §16, `DESIGN.md` §1.3, §3.1, §3.5, §4, `UPSTREAM_REQS.md` §1, §2.3, §4, `design/03` §1.1, §2.2, §3.2, §3.5, §3.6, §4.0, `design/01`, `02`, `08`, `09`, `10`, `README`, `ADR/0002`, `0006`, `0007`, `0009`, `0011`, `0013`; D-12, D-14, D-189, Q-05, Q-14 |
| D-198 | M Approval request carries `tcv_minor`, `currency`, `currency_minor_digits` from `get_version` | `design/03` §3.2, §3.6, §3.7, `DESIGN.md` §2.2, §3.1, `PRD.md` §3.2, §5.2, §6.2, §6.5, §9.2, §12; D-09 |
| D-199 | L Payment amount separate from TCV; no Products reservation; market wording; change orders deferred | `design/04` §3.4, §3.5, `UPSTREAM_REQS.md` §2.2, §2.5; D-17, D-90, Q-06 |

Highest decision number used: **D-199**; highest question number: **Q-14**. Numbering is one continuous sequence across the whole
register; there are no parts.
