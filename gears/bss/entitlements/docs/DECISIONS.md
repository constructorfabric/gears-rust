---
description: "Decisions and open questions of the Entitlements application layer."
---

# Entitlements - Decisions

<!-- toc -->

- [Status board](#status-board)
- [Decisions](#decisions)
  - [ENT-D-01 Provider-independent application boundary](#ent-d-01-provider-independent-application-boundary)
  - [ENT-D-02 Initial readiness before active (IA-2026-10-18)](#ent-d-02-initial-readiness-before-active-ia-2026-10-18)
  - [ENT-D-03 Rule semantics here, storage and acceptance in Pricing](#ent-d-03-rule-semantics-here-storage-and-acceptance-in-pricing)
- [Open questions](#open-questions)

<!-- /toc -->

Decision ids are `ENT-D-NN`; open questions are `ENT-QNN`. A decision carries a **Propagated** line naming the documents that cite it. Open questions are gates: a profile that depends on an open question stays unavailable until its owners close it.

## Status board

| ID | Sev | Decision | Status |
| --- | --- | --- | --- |
| ENT-D-01 | H | Entitlements owns the provider-independent application boundary; native LM/OSS are adapters | ACCEPTED |
| ENT-D-02 | H | Initial usable readiness precedes Subscriptions' active commit (IA-2026-10-18) | ACCEPTED |
| ENT-D-03 | H | Rule semantics owned here; rules stored on PlanRevision and frozen in Pricing acceptance | ACCEPTED |

## Decisions

### ENT-D-01 Provider-independent application boundary

- **Where**: [DESIGN.md](DESIGN.md) §1.1 and §3.3.3; [ADR 0001](ADR/0001-application-boundary.md).
- **Problem**: BSS must not require the platform's own License Manager or OSS to apply what was bought. A deployment may run a native system, a third-party licensing or service system or a customer integration.
- **Decision**: The application of commercial posture depends on abstract capabilities implemented by deployment-selected adapters: entitlement fulfillment, resource execution and readiness evidence. Entitlements owns this application boundary; inventory and usage evidence remain Subscriptions consumer contracts. The native License Manager, a third-party system and a customer integration are alternative adapters of the same capability profile; none is mandatory because another deployment uses it. Provider-specific APIs, reply schemas and status mappings belong to adapter specifications.

  The core keeps desired state, correlated pending, applied and failure observations, and the end-to-end idempotency, stale-action and recovery guarantees; a vendor need not expose our revision field or state machine. Required capabilities are explicit per product profile; an explicitly inapplicable capability differs from an unavailable required adapter, and an absent required adapter is never fabricated success. A native provider PRD is not a completion gate of this design; enabling the runtime still needs the selected adapter to satisfy its applicable guarantees.

  Subscriptions publishes the commercial posture and consumes readiness; it holds no fulfillment adapter contract.
- **Platform binding**: Entitlements hosts the capability plugins using the platform SDK, Types Registry and scoped ClientHub pattern. Public consumer and plugin contracts are separate; no new BSS resolver gear is needed. Persist the selected instance, contract/profile, stable provider target and observation authority before dispatch and retain them through revisions/replay. Discovery changes cannot redirect existing assignments, including when the same GTS ID is reconfigured. Tenant Resolver and AuthZ remain the shared topology and authorization services; License Resolver remains read/check only.

  See DESIGN §3.3.3 and §3.4, PRD fencing and E-AC25 to E-AC27, and the foundation/reconciliation allocation in DECOMPOSITION.
- **Propagated**: DESIGN.md §1.1; features/02-initial-readiness.md; features/03-quota-projection.md; features/04-license-grants.md; features/05-resource-execution.md; features/06-reconciliation-recovery.md; ADR/0001-application-boundary.md

### ENT-D-02 Initial readiness before active (IA-2026-10-18)

- **Where**: [PRD.md](PRD.md) §6.2 and the documents listed under Propagated.
- **Problem**: Initial activation must not begin the commercial period before the purchased service is usable. Readiness and activation require a recoverable decision boundary across separate owners.
- **Decision**: Initial activation requires confirmed usable service before active and before the service period and accrual start. Entitlements is the provider side of that gate. It persists the complete required-assignment and producer manifest for the activation operation before dispatch, makes the preparation assignments available through the selected adapters while the subscription is still `preparing`, accepts readiness observations by authorized scope, concrete assignment and desired revision, and reports ready only when every assignment of the manifest is ready for the exact desired revision.

  No `subscriptionId` crosses the adapter boundary. Entitlements keeps the intake-to-assignment mapping; Subscriptions binds an aggregate manifest result to its saved operation and posture. Subscriptions is the consumer side: its activation operation stays pending and the subscription stays `preparing` until that evidence arrives, and its commit then rechecks fences and writes active, fresh local T, receipt and outbox together. Entitlements never assigns T and never treats a ready event as completion.

  Required initial assignments cover creation or linking of the account, the deployment included in the purchase and usable permissions; first login or a later unrelated purchase is not required. Late or unknown results keep their recovery; no readiness is not failure; withdrawal fences local activation and late external work with separate cleanup and financial obligations. Renewals, resume, trial conversion and modifications of an active subscription keep their existing contracts.
- **Held readiness (SUB-D-57)**: Entitlements holds an operation/posture/manifest-bound readiness commitment until Subscriptions' serialized activation or terminal non-activation decision is recovered. No timeout-only release; ordinary revoke/cleanup follows that decision. Emergency security denial is immediate with incident recovery and no fictitious rollback. Authorized preparation use before T, including never-activated work, is platform-borne under bounded enabled profiles. The same two inter-gear fact contracts carry commitment and resolution; no adapter subscription identity is added.
- **Credit-trial coverage**: Billing commits the reserved bonus before preparation, with fixed accepted scope/terms and exact activation operation. Entitlements prepares usable access under that commitment; successful Subscriptions T makes the same grant effective. Later Billing recording is recovery/accounting, not another decision or initial-access gate. Pre-T preparation is platform-borne without bonus drawdown. Unknown outcomes retain protection; terminal non-activation releases it and later cancellation prevents revival. E-U07/ENT-Q14 gate exact contracts, bounded exposure/enforcement and executed cases.
- **Limits**: Readiness/resolution field names are semantic candidates, not shipped SDKs. Provider retention/emergency mapping, exposure bounds, authoritative time and Rating/Billing treatment, exact recovery contracts and runtime proof remain open under ENT-Q09 to ENT-Q15. These contracts do not authorize changed purchased validity or bonus beyond accepted terms. They do not implicitly enable quota profiles.
- **Propagated**: PRD.md §2; DESIGN.md §1.1, §3.3.5; features/02-initial-readiness.md; features/04-license-grants.md; UPSTREAM_REQS.md §2.1

### ENT-D-03 Rule semantics here, storage and acceptance in Pricing

- **Where**: [PRD.md](PRD.md) §6.3 and the documents listed under Propagated.
- **Problem**: Rule meaning, commercial acceptance and metric evidence need distinct owners. Replay must reproduce accepted provider rights without reading mutable plan data.
- **Decision**: Entitlements owns the rule schema and its semantics: `PlanQuotaRule` kinds `Fixed` and `PerPlanUnit`, exact conversion, validity, suspend and revoke policy, and grant definition semantics. Pricing stores the rule as an optional typed block on the immutable PlanRevision, next to the trial block, validates it at publication against Products metric evidence, and freezes it in the same acceptance receipt as the price bindings. Products supplies metric, unit and subject kinds and the versioned mapping to the enforcement metric registry.

  One acceptance evidence source exists: the receipt. Acceptance fixes the cap semantics and the tenant axes; it is not permission to apply. Derivation reads the frozen rule from the receipt together with the committed posture and never the live plan; a rule change needs a new accepted revision even when the price is unchanged.
- **Propagated**: PRD.md §6; DESIGN.md §2; features/03-quota-projection.md

**Preparation and evidence.** DESIGN §3.3 distinguishes operation-correlated aggregate readiness from adapter observations and carries complete disposition/hold intent in posture revisions. Empty manifests have explicit results; hold-only changes still publish. Initial allocation preparation uses frozen purchased capacity and scoped resource/actions authority before T, with occupied/reserved resources counted immediately. Activation continues the same source/member/allocation identities without reset or duplicate allowance. Platform-borne preparation does not waive occupancy. Fenced non-activation blocks new allocations and permits only authorized cleanup; confirmed resource release is required to free counted capacity. Unknown outcomes retain protection.

Consumable-limit checks carry a recognizable purpose marker. Execution and quota/accounting behavior are provider-owned; labeling implies neither dry-run nor zero consumption and requires no service allowance; ENT-Q15 tracks provider delivery and conformance. The E-U03 grant requirements cover typed definitions, publication validation, frozen receipt evidence and atomic replay; quota-only adoption does not close E-U03 for grants. These requirements define the boundary; provider ratification and runtime evidence remain open.

## Open questions

| ID | Question | Owners | Blocks |
| --- | --- | --- | --- |
| ENT-Q01 | Pricing adoption of the typed rules, and which Products/provider descriptor supplies each allocation metric, unit and authorized subject kind. Existing usage semantics may be reused only when enforcement equivalence is proved. | Pricing, Products, Entitlements | quota profile |
| ENT-Q02 | The exact rule applicability and change-order delta mapping. NewSaleQuery's single plan quantity must remain unambiguous; any independent component-unit model requires a versioned extension. | Pricing, Orders, Entitlements | quota profile |
| ENT-Q03 | purchased caps remain desired rights; independent provider restrictions affect actual use and are reported as fulfillment discrepancies, not silent amendments. Known incompatibility blocks sale; post-purchase mismatch retains remediation and blocks affected required initial readiness. Ordinary changes respect held-readiness fencing; emergency security is immediate. Exclusive commercial-source/no-pooling scope remains; exact provider precedence, scope/version/evidence and provider-restriction conformance remain OPEN. | Quota Enforcement, Products, Entitlements, Subscriptions | quota profile |
| ENT-Q04 | The selected enforcement/entitlement adapter, source namespace authority, full-set atomicity boundary and permission model for proposed apply/read operations. | Quota Enforcement, Entitlements | quota profile |
| ENT-Q05 | Hard suspension/revoke behavior, fallback denial, existing allocations, cap-below-used policy and any separately authorized export access. | Quota Enforcement, Products, Entitlements | quota profile |
| ENT-Q06 | Exact validity boundary/clock mapping, allowed calendars and counter identity, especially for subscription renewal that does not align with UTC QE windows. | Quota Enforcement, Entitlements | quota profile |
| ENT-Q07 | Accepted continuation behavior for quota terms, supported lot combinations, late expiry handling, tombstone retention and safe administrative recovery. | Pricing, Quota Enforcement, Entitlements | quota profile |
| ENT-Q08 | SDK/profile versions, executed conformance evidence and provider readiness. Separate accepted design semantics from implemented code and from deployed capability. | Quota Enforcement, Entitlements | quota profile |
| ENT-Q09 | Rights-feed transport and recovery: event delivery, polling or another reliable feed from the selected adapter, and how authoritative assignment state and sequence are recovered after missed events, restart or retention gaps. | Entitlements, selected adapter | initial readiness profile |
| ENT-Q10 | Native schema and identity mapping: how the adapter maps its native identities to `scopeId`, `assignmentId` and `desiredRevision`, so several environments under one broad right stay distinct and retired identities are not reused. | Entitlements, selected adapter | initial readiness profile |
| ENT-Q11 | SUB-D-57: held readiness and one Subscriptions activation/non-activation fence; timeout never releases. Exact commitment/resolution SDK, authenticated recovery, provider retention and emergency mapping plus race fixtures remain OPEN. No atomic uptime guarantee. | Entitlements, Subscriptions, selected adapter | initial readiness profile |
| ENT-Q12 | versioned profile duration/deadline saved per attempt; timeout or definitive failure closes only the attempt through the Subscriptions activation fence. Preserve purchase/payment for Orders remediation; recover applied or terminal non-activation before cleanup and prove safe predecessor effects before succession. Numeric durations, exact clock/provider/cleanup mapping and E-AC35/E-AC36 evidence remain OPEN. | Entitlements, Subscriptions, Orders, selected adapter | initial readiness profile |
| ENT-Q13 | COST SUB-D-57: authorized pre-T/never-activated preparation use is platform-borne. Exact profile timing/capability/usage/admission bounds, financial authorization and authoritative usage-to-T/accounting mapping remain OPEN; preparation itself creates no bonus drawdown; credit commitment delivery is tracked under ENT-Q14 and quota preparation remains separate. | Entitlements, Subscriptions, Rating, Billing | initial readiness profile |
| ENT-Q14 | Billing commitment before readiness, grant effective at Subscriptions T and later same-grant recording without an access gate. Exact authenticated coverage/assignment mapping, exposure/enforcement bounds, resolution recovery and E-AC31 to E-AC33 execution remain OPEN. | Billing, Subscriptions, Entitlements, selected adapter | credit-trial initial readiness profile |
| ENT-Q15 | allocation preparation under frozen accepted capacity and scoped authority before T; real occupancy and same-source continuity across activation. Exact provider authority/fence, held continuity, confirmed-release/late-effect recovery and preparation continuity and release evidence remain OPEN. Readiness checks carry purpose for the deferred consumption profile; execution and quota treatment remain provider-owned. | Entitlements, Subscriptions, Quota Enforcement, resource provider | allocation-dependent initial readiness |

The quota questions ENT-Q01 to ENT-Q08 are closed by a joint Pricing, Products and Entitlements schema agreement followed by a Quota Enforcement conditional-apply and recovery contract. The readiness questions ENT-Q09 to ENT-Q15 are closed per adapter profile; a profile lacking that contract cannot claim a safe initial activation on a cached ready event.
