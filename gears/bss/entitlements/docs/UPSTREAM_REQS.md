---
description: "Inbound requirements from Subscriptions and the provider prerequisites of the Entitlements application layer, with ownership and acceptance evidence."
---

# UPSTREAM_REQS: Entitlements

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Requesting gears](#12-requesting-gears)
- [2. Requirements](#2-requirements)
  - [2.1 Subscriptions](#21-subscriptions)
  - [2.2 Pricing and Products](#22-pricing-and-products)
  - [2.3 Quota and license adapters](#23-quota-and-license-adapters)
  - [2.4 Service adapter and operators](#24-service-adapter-and-operators)
  - [2.5 Provider prerequisites and bilateral acceptance](#25-provider-prerequisites-and-bilateral-acceptance)
  - [2.6 Hook plugin execution](#26-hook-plugin-execution)
- [3. Priorities](#3-priorities)
- [4. Traceability](#4-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

This document records consumer requirements and the external contracts Entitlements needs. The [PRD](PRD.md) covers each incoming requirement. [DESIGN](DESIGN.md) and [features](DECOMPOSITION.md) describe how to meet them. TARGET means proposed behavior. It does not claim implementation or another owner's approval.

### 1.2 Requesting gears

| Consumer | Needs from Entitlements |
| --- | --- |
| Subscriptions | Fenced intake of `CommercialPostureV1`; application of every committed revision to the enabled providers; aggregate `ReadinessObservationV1` for the complete manifest of an activation operation/posture revision; application state and findings readable per posture revision |
| Pricing | Versioned application-rule schemas for its plan authoring, validation and accepted snapshots. |
| Products | Stable references to its reviewed commercial definitions, GTS semantics and qualified profiles. |
| Quota, license and resource adapters | Authorized desired state with stable source identities, ordered revisions and recovery by the original operation. |
| Platform operator | Desired versus applied state, findings and authorized repair without direct edits of provider records |

## 2. Requirements

### 2.1 Subscriptions

#### Commercial posture

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-upreq-commercial-posture`

Entitlements MUST accept committed `CommercialPostureV1` from the Subscriptions delivery and recovery contracts. Subscriptions commits each posture with its lifecycle change. Its workers deliver the posture through the Entitlements client; an outbox transport must preserve the same contract.

The posture MUST retain its ordering tenant, subscription, revision, digest and operation identity. It includes resource/payer/seller tenant axes, complete capability members, committed quantity, validity, access posture and accepted-snapshot provenance. Lot IDs explain quantity history; Entitlements MUST NOT recalculate quantity or read live plan terms. Subscriptions supplies each member's effective validity interval, holder scope and current access posture. Pricing supplies the accepted definitions, bindings and policy limits. The owner SDK must carry those member inputs under E-U06; document-wide access is not a substitute. Qualification must prove mixed member states, changed validity, scope validation and replay against the original accepted terms.

Preparation context MUST retain the accepted policy/version, authorized start and deadline. Terminal resolution retains its exact reason code, including unfamiliar owner-valid codes on `closed_without_activation`. The Subscriptions SDK and GTS schema must support lossless decoding and canonical replay of those codes. Outcome, version, authority and attempt-binding validation remain strict; the reason supplies no additional cleanup authority (E-U06, E-AC41). Execution context includes `lifecycleAction` and complete versioned service dispositions, including cutoff, hold, schedule and deletion authority. These updates advance the aggregate revision even without a status change. DESIGN §3.3.1 defines the fields.

Entitlements MUST authenticate the producer and authorize scope before accepting a revision. It MUST refuse stale or conflicting input, return the existing result for an identical replay, and retain earlier outcomes when a newer revision becomes the target. Every enabled capability receives a complete desired state under the saved source identity.

Delivery and application MUST remain readable per revision. A dispatch acknowledgment is not application. The [delivery state](features/06-reconciliation-recovery.md#delivery-state) distinguishes incomplete and failed delivery; structured outcomes retain pending, refused and unknown cases. Quota application follows [DESIGN §3.3.4](DESIGN.md#334-quota-apply-and-read), including accepted/applied head separation and recovery of partial or uncertain effects.

- **Source**: Subscriptions [UPSTREAM_REQS S-U04](../../subscriptions/docs/UPSTREAM_REQS.md#25-provider-prerequisites-and-bilateral-acceptance) and [DESIGN §3.3.5](../../subscriptions/docs/DESIGN.md#335-entitlement-and-retention-contracts).
- **Status / owner**: TARGET, Entitlements. Contract version JOINT OPEN with Subscriptions under E-U06.
- **Acceptance**: A replayed posture is a no-op. A conflicting digest is refused and visible. Subscriptions recovers lost replies through `read_posture_receipt`. If the result is absent or unknown, it retries the saved payload and identity. Applying revision 7 after revision 8 revoked access cannot restore access at any provider.

#### Readiness gate

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-upreq-readiness-gate`

For each activation, Entitlements MUST save the complete assignment/producer manifest before dispatch. Preparation runs while the subscription is `preparing`. The aggregate `ReadinessObservationV1` MUST bind the ordering tenant, scope, operation, posture revision/digest and immutable manifest identity/version/digest. It retains the complete roster, producer sequence and evidence. DESIGN §3.3.2 defines the envelope.

Subscriptions saves the operation and posture before dispatch and binds the matching manifest result. No assignment-allocation handshake or vendor callback is required. A manifest with no required assignments and no readiness hooks needs explicit evidence that the profile requires no preparation and a retained aggregate result.

Entitlements MUST commit observations and sequence changes with their outbox. Authorized operation recovery returns the complete result after a gap or lost event. It MUST authenticate each adapter producer against the saved manifest. It reports `ready` only when every required assignment meets its pinned criterion and output checks for the exact revision, and every required readiness hook passes for the current prerequisite epoch/digest. Neither readiness envelope carries a subscription identifier.

Subscriptions remains `preparing` until the matching aggregate reports `ready`. Its guarded commit then writes active status, T, receipt and outbox atomically. Receipts, broker acknowledgments and a license record alone are not readiness. A late `ready` for a withdrawn revision cannot activate, but its effect evidence remains available for cleanup.

- **Source**: Subscriptions [initial activation readiness contract](../../subscriptions/docs/features/05-entitlements.md#initial-activation-readiness-contract) (IA-2026-10-18, SUB-D-55); the provider-side rule is ENT-D-02 in [DECISIONS](DECISIONS.md).
- **Status / owner**: TARGET, Entitlements. Envelope JOINT OPEN with Subscriptions under E-U06; SUB-D-57 defines held-readiness arbitration and platform-borne preparation cost; attempt timeout/failure closes only through the Subscriptions fence with purchase/payment preserved for Orders remediation; exact resolution/recovery, clock/profile, cleanup and bounded exposure/accounting contracts remain `ENT-Q11` to `ENT-Q13` in [DECISIONS](DECISIONS.md).
- **Acceptance**: Recover lost readiness notifications through the producer's authoritative read. Ready cannot activate an attempt that withdrawal has fenced. Reject wrong-tenant observations without leaking information. An older sequence cannot overwrite a newer ready, withdrawn or failed result.

Products publishes `ActivationRequirementV1` with qualification, and Pricing freezes the complete accepted requirement under E-U03/E-U04. Before purchase commitment or capture, Orders obtains a compatible durable configuration binding from Subscriptions under E-U06/E-U10. Subscriptions resolves Settings and saves mode, policy and qualified profile for the exact acquisition/acceptance. Activation consumes that binding. [Feature 02](features/02-initial-readiness.md#pre-purchase-activation-configuration) defines fields, scope and idempotent recovery for preparation with or without hooks. This amends the activation-time selection of SUB-D-85 and requires owner adoption. E-AC59 proves the pre-purchase guard and Settings-change race. V1 `no_preparation` services have no hooks.

### 2.2 Pricing and Products

Pricing owns plan rules and freezes them with prices in the receipt referenced by the posture. Products owns the reviewed commercial quota catalog, GTS metric and unit identities, accounting/scope policies, historical versions and qualification. Entitlements defines the application schema and preserves accepted meaning. Their obligations are E-U03 and E-U04.

### 2.3 Quota and license adapters

Adapters require authorized commands bound to the resource tenant and verified subject scope. Source identity or a caller-supplied tenant name is not authority. Reads and recovery need the same scope checks as writes. Entitlements never adopts another source's record by matching subject and metric. E-U01 and E-U02 record the reciprocal adapter contracts. E-U02 fences each logical license right by its own assignment source, revision and digest. Quantity changes retain that identity. Revoking one assignment must not affect another; lost replies recover the original assignment operation.

OSS owns resource allocation, usage counters, releases and enforcement. Entitlements receives application evidence, not per-resource quota accounting events. It exposes no synchronized view of used, reserved or remaining quota.

### 2.4 Service adapter and operators

Service fulfillment adapters receive tenant and logical assignment intent, with operation identity and assignment generation. Neither requests nor results require native resource IDs, physical generations or a resource roster. OSS owns internal resources and execution; the adapter may call a tenant-level or service-level API directly. Retries keep the original identity. Operators need desired state, application evidence, findings and authorized repair. Entitlements never edits a provider's counter, license or resource record directly.

### 2.5 Provider prerequisites and bilateral acceptance

These gates track adoption of the contracts specified in DESIGN and features, plus executed integration evidence. They do not defer BSS field definitions to implementation or claim delivered support. Each qualification record names the accepted versions, producer, consumer, adapter/profile, unsupported cases and executed evidence. Every readiness-enabled capability profile pins `readinessFailureClasses`, mapping registered refusal reasons to retryable or definitive failure. Qualification covers supported reasons, unclassified-reason fallback, partial/unknown recovery and open fulfillment discrepancies under E-AC07. Subscriptions owns attempt closure.

| ID | Owner | Required contract / status | Closure signal | Blocks |
| --- | --- | --- | --- | --- |
| E-U01 | Quota application adapter owner and Entitlements | The Commercial Quotas adapter contract (ENT-D-11), summarized below. EXTERNAL OPEN. | Stale and in-flight work cannot undo a newer accepted revision. Retry recovers the original result after timeout/restart. Complete application is confirmed for the exact scope. | Quota application and quota-dependent readiness. |
| E-U02 | License adapter | Scoped desired grants, stale-action prevention, original-operation recovery and readiness during preparation. No subscription ID crosses the adapter. EXTERNAL OPEN. | An old issue cannot undo a newer revoke. Lost results recover the same grant. Every required assignment supplies readiness evidence. | License grants and license-dependent readiness. |
| E-U03 | Pricing | Revision-owned quota/grant/service-binding authoring, full-set publication and acceptance checks, atomic price/rule snapshot, immutable historical reads. Validate grant and resource profiles against Products qualification (ENT-D-22). EXTERNAL OPEN. | Changed terms require new acceptance. Receipts resolve every selected rule/binding and digest. Quota, grant and service cases separately prove frozen replay and refusal of missing evidence. | Sales carrying quota/grant rules or service bindings. |
| E-U04 | Products | Reviewed commercial quota definitions, registered GTS metric/unit/accounting/scope identities, localized descriptions, immutable versions, qualified profile evidence and availability. Also maintain license/resource qualification (ENT-D-22). EXTERNAL OPEN. | Each accepted rule resolves exact commercial meaning and a qualified complete set. Unsupported scopes or withdrawn qualification block new sales. | Publication and sale of affected capability profiles. |
| E-U05 | Selected resource adapter | Assignment-bound service intents, confirmed effective times, pending/unknown/partial outcomes, data-preserving suspension, retention/export and confirmed hold/disposal protection. No mandatory resource mapping or inventory. EXTERNAL OPEN. | Tenant-level fulfillment, late service stop, hold/disposal races and stale commands against a successor assignment pass. E-AC43 retains unresolved effects after supersession. Deletion stays disabled without the proven execution fence. | Resource execution and automatic deletion. |
| E-U06 | Subscriptions | Agreed posture, readiness and outcome contracts; held readiness under ENT-D-24; precommit quota exclusivity and its retained proof. Adoption work is listed below. JOINT OPEN. | Both gears agree versions and prove replay/conflict, manifest completeness, lost results, withdrawal, timeout and partial-effect recovery. E-AC35/E-AC36 and E-AC41/E-AC42 cover closure and safe successors. E-AC77 proves independent startup, peer recovery, readiness event publication/consumption and polling without event delivery. | Intake, readiness reporting and activation profiles that need preparation. |
| E-U09 | Hook plugin owners, Entitlements and platform security | Typed ToolKit hook execution with pinned implementations, restricted identity and durable recovery. EXTERNAL OPEN; details in §2.6. | E-AC50 to E-AC58 pass against the selected plugin without Serverless, including lost starts, expired keys, safe payloads and stale results. | Configured hooks only; services without hooks are unaffected. |
| E-U10 | Orders / Workflow | Require the Subscriptions activation configuration binding for the exact acquisition and Pricing acceptance before purchase commitment or payment capture. Carry it through create/activation and recover unknown binding results. JOINT OPEN with E-U03/E-U04/E-U06. | E-AC59 proves incompatible mode refusal before commitment/capture, guarded binding association, lost-reply recovery and pinned activation after Settings change. | Sales of profiles using this activation requirement. |
| E-U08 | Platform authorization, operations and audit/security owners | Entitlements service authorization under the [commercial model §3.4](../../../../docs/arch/bss-commercial-model/DESIGN.md#34-internal-dependencies) and ENT-D-04. EXTERNAL OPEN. | Target-scoped evaluation, service-only methods and authorized worker discovery pass. Owner approval ends the interim contract. Retention adds the versioned audit/hold/identity-erasure policy and E-AC79 proof. | Until then, every worker and service call uses the ENT-D-04 interim. Automatic local deletion and identity erasure stay disabled until their policy and conformance are adopted; this does not disable fulfillment recovery. |

**Release gates.** Apply these requirements per the [PRD release table](PRD.md#51-in-scope). E-U09 hooks are v2. The output-delivery portions of E-U02/E-U05 and the service-disposition portions of E-U05/E-U06 are v2; their core application and recovery contracts remain v1. V2 clauses stay specified but do not block a v1 profile that does not need them. Purchase qualification must reject profiles requiring a deferred capability.

**Quota application (E-U01).** Use the [Commercial Quotas contract](../../docs/quotas/DESIGN.md#334-quota-application-adapter-contract), merged in [PR #5973](https://github.com/constructorfabric/gears-rust/pull/5973). It requires versioned application, source-state reads and recovery by the original operation. The source is `(authenticated namespace, resource tenant, assignmentId)`.

The adapter reports its highest accepted revision/digest separately from its last fully applied revision/digest. A later refusal cannot lower the accepted head. Recorded retries recover the saved result; stale unseen requests and conflicting content cannot create effects. Closure blocks new grants while retaining reads and recovery. The adapter validates source authority, scope and exclusivity proof without receiving payer or purchase payloads.

Results are `pending`, `applied`, `refused` or `unknown`. Only complete confirmed application is `applied`. Known partial work stays `pending`; uncertain effects stay `unknown`. Unresolved effects cannot be reported as a definitive refusal. One OSS transaction is not required. The adapter design owns internal sequencing, recovery, receiving OSS components, downstream APIs and permissions.

V1 qualifies initial installation and compatible increases separately. Optional region/pool profiles must enforce the selected scope; a full allowance cannot be copied into independent scopes. Numerical decreases, consumption, pooling, arbitrary partitions and migrations are deferred. Application evidence includes ordered discrepancies when the purchased limit cannot be fulfilled. Ordinary resource use is not a delivery discrepancy. OSS accounting components, events and tests stay outside this BSS contract.

**Catalog and plans (E-U03/E-U04).** Adapter discovery and authorized manual input enter Products as candidates for review. Published definitions provide localized names and descriptions with English fallback. Registry metadata alone cannot publish an offer. Pricing lists applicable approved definitions and stores quota rules on PlanRevision, with fixed or per-plan-unit amounts and exact scope selections. Price and quota terms are accepted together, but charging policy remains separate. A prepaid limit and a usage-priced limit can have the same quota amount. Withdrawn qualification blocks new sales and preserves existing obligations.

**Purchase component correlation (E-U06/E-U08).** Subscriptions allocates and retains `fulfillmentRef` for each purchased component, includes it in digest-covered posture members and exposes it through its authorized component read. Entitlements retains assignment/generation/output associations and provides resource-tenant read filters. Subscription access alone grants no output access. The reference never enters adapter or hook payloads. Feature 06 defines owner SDK/GTS adoption, lifecycle, scope and E-AC69 conformance.

**Retention adoption (E-U08, with E-U06 and adapter owners).** Platform operations supplies versioned detail-retention periods; the audit/security owner supplies the audit retention, hold and identity-erasure policy, including backup/restore behavior. Entitlements implements safe compaction and erasure under [feature 06](features/06-reconciliation-recovery.md#retention-and-safe-compaction). Subscriptions and selected adapters qualify any bounded replay/namespace-retirement contract. Without that proof, keep compact fences. E-AC79 must pass before enabling deletion; a recovery timeout is not a retention bound.

**Peer availability (E-U06).** Both owners implement [DESIGN §3.4](DESIGN.md#34-internal-dependencies): no mutual startup dependency or synchronous callback chain, lazy peer client resolution and retained work during outages. Subscriptions polls readiness even without notifications. E-AC77 must pass with either peer absent at startup, later availability, lost notifications and unresolved activation outcomes. A missing client does not close a generation or permit cleanup.

**Subscriptions adoption (E-U06).** The SDK owns posture intake answers and full readiness results under ENT-D-12. E-U06 also owns the readiness event contract, publisher and consumer; no readiness callback is required. Remaining SDK adoption covers:

- The exact `read_posture_receipt` contract, redelivery of saved payloads and lost-reply recovery (E-AC62).
- Closure reasons that preserve unfamiliar owner-valid values during SDK/GTS decoding.
- Logical `serviceDispositions` and the scoped `read_service_fulfillment` read.
- Each member's effective validity, holder scope and access posture.
- Separate readiness and closed result variants for storage and reads, including closure before a manifest and ordered settlement updates (ENT-D-08, E-AC60).
- The readiness profile reference/version in preparation context (ENT-D-10/ENT-D-21).
- The quota-domain claim in posture (ENT-D-11).

E-U06 owns the durable pre-purchase activation configuration binding: Subscriptions checks the requirement in the retained Pricing receipt against effective Settings and pins the selected mode/policy/profile. E-U10 makes Orders require and carry that binding before commitment/capture. Activation uses it rather than live Settings; no hook fields are added to the readiness result. Subscriptions owns recoverable exclusivity before purchase commitment or capture. Entitlements validates and retains that claim; it does not replace purchase admission with a later adapter conflict.

**Output retrieval (v2, E-U02/E-U05).** Both output-capable adapters accept the host-derived `OutputRetrievalPolicyV1`: exact stream/link mode and bounded link lifetime/absolute expiry. Feature 04 defines the shared SDK fields and host/adapter validation. The public caller cannot override the accepted profile.

**Readiness implementation and qualification (E-U06).** The [Subscriptions shared readiness result contract](../../subscriptions/docs/features/05-entitlements.md#shared-readiness-result-contract) is the single definition for both endpoints. Subscriptions owns shared SDK types, lossless integer decoding, consumer validation and immutable resolution reads. Entitlements owns matching result production, GTS registration, full-result recovery and durable sequence/publication state. Both owners must agree versions and run E-AC60/E-AC71 fixtures for variants, conditional evidence, complete bounded rosters, closure before manifest creation, ordered settlement updates and historical replay. Reject unsupported versions and incompatible payloads; no weaker-envelope fallback. E-U06 remains open until implementation and joint conformance pass.

**Authorization (E-U08).** The platform contract requires a narrowly authorized Entitlements service account, client-credentials context and single-target scope on every call. Worker discovery uses an authorized tenant roster in pages of at most 200. Internal service access across a self-managed barrier needs explicit authority. Subscriptions tracks the same work as S-U20. Until it is delivered, ENT-D-04 defines the discovery warrant, above-target allow and first-party identities. No worker receives a scope over all tenants.

Every adapter must prove the guarantees of its selected profile. No native OSS component is required by the BSS quota contract. Each selected integration also proves the [SDK/plugin and durable binding contract](DESIGN.md#333-adapter-capability-profile): correct schema/scoped-client registration, accepted profile/version, stable target identity, authorized observation producer, and recovery without rediscovery or fallback. E-AC25 to E-AC27 cover concurrent binding, restart, missing clients and same-ID target drift.

Registry metadata does not close a gate. Adapters use supported provider APIs; direct writes to provider databases are forbidden. Recovery must work without broker delivery through authorized reads.

**Customer deliverables (v2).** E-U02/E-U05 require the [fulfillment-output contract](DESIGN.md#337-fulfillment-outputs) when a profile includes license files/keys or service outputs. Outputs retain identity/version, ordered observations, protected-content ownership and retention. Recovery and retrieval are scoped. Release, replacement and revocation rules prevent unauthorized delivery; download retries cannot issue another license. E-AC46 to E-AC49 cover loss, authorization, races and readiness. The profile stays disabled until SDK schemas, content-store mapping and retrieval/revocation bounds are accepted. An opaque reference alone does not prove that content is retrievable.

**Accepted receipt read (E-U03/E-U06/E-U08).** Use existing `PricingAcceptanceV1.acceptance` with `AcceptanceQuery.catalog.tenant_id` and `acceptance_id`. E-U03 extends its versioned result with complete retained quota/grant/service application evidence; it must not require reconstruction from live plan data. E-U06 supplies catalog, acceptance, terms digest and exact acquisition provenance. E-U08 supplies the Entitlements principal’s Pricing acceptance-read authorization in that catalog, scoped to the target resource. Feature 01 defines checks and E-AC68. No new read method, seller impersonation or broad catalog authority is implied.

**Metric service reference (E-U01/E-U03/E-U04).** Extend the Products-owned metric descriptor with optional typed `service_type_ref` to one adapter-owned service descriptor. Service-specific metrics retain it; shared metrics use qualified mappings. Discovery, catalog reads and Pricing snapshots must preserve and validate it. Consumers cannot ignore it or infer it from a GTS namespace. The shared Commercial Quotas schema must adopt this extension; Entitlements does not fork that wire contract. Feature 03 and E-AC67 define qualification, historical replay and canonical-domain checks. Existing metrics cannot be silently rebound.

**Service binding (E-U03/E-U04/E-U05/E-U06).** The adapter publishes its immutable execution service-type descriptor and supported profile pairs. Products qualifies the SKU-version mapping; Pricing freezes it with parameters, quantity semantics and policies in the accepted snapshot. Subscriptions supplies the explicit accepted binding reference. Feature 05 defines SDK/GTS fields and E-AC66. Runtime selection never infers the service from a SKU name; native offering/instance IDs and their assignment mapping remain in the adapter.

**Grant policies (E-U02/E-U03/E-U06).** Feature 04 defines the closed v1 holder, validity, suspension and revocation values. Pricing freezes and validates them; Subscriptions supplies matching member authority; the selected license adapter proves the requested access behavior. E-AC65 covers shared validation, historical replay, explicit unbounded validity and source-scoped suspension/revocation. Native execution and seat assignment remain outside BSS.

**Recovery policy (E-U01/E-U02/E-U05/E-U06).** Capability qualification supplies finite values for the versioned policy in feature 06 and proves original-operation recovery, safe replay and remediation evidence. The core policy covers pre-adapter dependency and Subscriptions resolution reads. E-AC64 proves persistence, escalation and replacement limits. These settings control Entitlements scheduling; they do not configure OSS retries or replace the Subscriptions activation deadline.

**Finding access (E-U06/E-U08).** Subscriptions correlates intake/preparation findings through the retained receipt or operation/generation. Platform authorization grants finding reads and operator actions in the subject owner tenant. Intake/preparation findings use ordering scope; assignment/hook findings use resource scope. Feature 06 defines the SDK/GTS subjects, event boundary and E-AC63 qualification. A linked record never grants access to another tenant.

**Security blocks.** E-U01/E-U02/E-U05 adapters adopt the assignment-scoped `security_block` observation and explicit authenticated clearance in feature 02. E-U06 proves aggregate priority and no activation while a required block is active. E-AC61 covers optional assignments, incident identity, stale/foreign clearance, restart and gap recovery. This adds no quota application status or OSS security-control API.

**Pause-aware progress (E-U06/E-U08).** Qualify the existing scoped `get_operation_in` read for the Entitlements service principal. It must return current activation phase, paused state and deadline, with terminal resolution when available. Feature 02 suppresses deadline findings for paused or unknown progress and uses the updated deadline after unpause. Fixtures cover stale reads, pauses without posture delivery and separate held-age monitoring. This adds no pause event or new API.

**Held readiness (SUB-D-57).** E-U06 includes a durable commitment and immutable applied/non-activation resolution from Subscriptions. Posture carries the resolution or its reference. The Subscriptions operation recovery read must return the same result after lost or superseded delivery, under the Entitlements service identity and ordering tenant scope. It accepts the saved operation identity and supports verification against `resolutionRef`, preparation binding and decision version.

A pending or missing result is not closure. Entitlements retains durable recovery work and retries until it obtains the exact terminal result. Owner SDK adoption and conformance must cover activation followed by suspension before delivery, stale redelivery, restart and conflicting results (E-AC32). Preparation-enabled E-U01/E-U02/E-U05 adapters retain obligations while outcomes are unknown and expose `read_preparation_settlement` under their saved binding.

Its typed evidence confirms source closure, resolved or isolated effects and the authorized safe successor scope, as defined in feature 02. The quota application contract remains unchanged; this is a separate preparation interface. E-U06 consumes the generation settlement. Qualification proves closure/dispatch races, optional effects, isolation and stale cleanup without OSS inventory (E-AC36).

Under ENT-Q13, Subscriptions and Rating/Billing own the evidence that separates preparation from active service. Authorized pre-T and never-activated preparation remains platform-borne. E-AC28 to E-AC30 cover lost publication, withdrawal races, missing outcomes, emergency denial and late usage evidence. Owner SDK, profile bounds and runtime gates remain open.

### 2.6 Hook plugin execution

**Release: v2 / p2.** E-U09 does not gate v1.

E-U09 covers [DESIGN §3.3.10](DESIGN.md#3310-extension-hooks) and the [feature 07 contract](features/07-extension-hooks.md). Entitlements owns the typed hook SDK, payloads, local journal, outbox and result validation. Each integration plugin owns execution, durable admission/result recovery and downstream retries. Platform security and the plugin owner qualify scoped identities, credentials and permitted actions.

| Required capability | Adoption and proof required |
| --- | --- |
| Qualified ToolKit plugin | Register the versioned specification and instance, resolve the scoped client and pass E-AC50 to E-AC58 without Serverless. A registry record or mock alone does not qualify production. |
| Pinned implementation | Validate the exact implementation version, code/configuration digest and policy before execution. Result reads and internal retries retain the same binding. Changed code under the same instance/version is a conflict. |
| Typed result recovery | Return the saved terminal result and safe error for an authorized execution. Retain admission and results across process restart, including in-process plugins. |
| Idempotent start | Same key and content returns the original execution; changed content conflicts. Concurrent starts, worker lease loss and a lost reply cannot admit another execution within the qualified window. |
| Admission deadline and durable lookup | Reject starts beyond their saved admission deadline, including stale workers after deduplication expiry. Recover by original key or execution ID. Missing or expired history remains unknown, not proof of non-admission. |
| Scoped identity and approved code | Authorize the source resource tenant. Use restricted plugin credentials for downstream calls; never inherit caller credentials. Readiness handlers have read-only access. In-process code is trusted and must pass code review and policy/denied-action tests; the plugin interface is not a sandbox. |
| Bounded execution and cancellation | Enforce execution bounds across retries. Cancellation is best effort unless terminal evidence proves it. Retain late outcomes for recovery. |
| Retained bindings and payload limits | Retain implementations and configuration while source assignments and final deliveries need them. Qualify complete readiness payloads and bounded retry settings. A security disablement reports unavailability, never fallback or implicit success. |
| Admission accounting | Prove admission or durable non-admission for the original key. Non-admission prevents a delayed start. Test reservation, one-time consumption/release, restart, lost replies and outage without charging unstarted checks (E-AC55). Start delivery and admitted-check budgets remain separate. |

Completion notifications are optional; polling the typed result read is sufficient. A future Serverless-backed plugin must meet the same contract and define its runtime mapping in its own design. No Serverless implementation or SDK change gates the v2 hook capability.

E-U06 adds no hook fields to the readiness result. The pinned profile and manifest digest cover local bindings. Subscriptions reads only aggregate status and reason.

Invalid or unqualified bindings block new offers. An outage after acceptance preserves the obligation and recovery evidence. Report unresolved readiness or notice failure. Never silently omit a configured hook.

ENT-Q30 tracks external adoption and executed proof. This gate blocks enabling hooks, not writing their SDK fields or feature procedures. Profiles without hooks retain their current behavior and serialized digests.

## 3. Priorities

Both incoming requirements are `p1`: they gate the first activation profile that depends on initial readiness and every posture application. An implementation can develop against contract fixtures, but mocks never close an external production gate. [DECOMPOSITION](DECOMPOSITION.md) allocates each active PRD requirement to one primary feature and names its prerequisites.

## 4. Traceability

- [PRD](PRD.md): `Covers` links map the two incoming requirement ids to product requirements.
- [DESIGN](DESIGN.md): architecture and contract authority; [§4.1](DESIGN.md#41-capability-gates) lists what each open gate blocks.
- [Commercial Quotas](../../docs/quotas/DESIGN.md), merged in [PR #5973](https://github.com/constructorfabric/gears-rust/pull/5973): E-U01 maps to G04-G06, E-U03/E-U04 to G02, E-U06 to G03, and application operations to G07. Check references against the accepted version before qualification.
- [DECISIONS](DECISIONS.md): accepted design decisions and the status of each owner integration question.
