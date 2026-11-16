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
  - [ENT-D-04 Service authorization under single-target scopes](#ent-d-04-service-authorization-under-single-target-scopes)
  - [ENT-D-05 One posture per service and no Accounts dependency](#ent-d-05-one-posture-per-service-and-no-accounts-dependency)
  - [ENT-D-06 Posture outcome per revision](#ent-d-06-posture-outcome-per-revision)
  - [ENT-D-07 Assignment identity per preparation generation](#ent-d-07-assignment-identity-per-preparation-generation)
  - [ENT-D-08 Cleanup obligations and settlement after non-activation](#ent-d-08-cleanup-obligations-and-settlement-after-non-activation)
  - [ENT-D-09 Failure class of preparation and the closure signal](#ent-d-09-failure-class-of-preparation-and-the-closure-signal)
  - [ENT-D-10 One derivation path and the readiness profile](#ent-d-10-one-derivation-path-and-the-readiness-profile)
  - [ENT-D-11 Quota application follows the Commercial Quotas specification](#ent-d-11-quota-application-follows-the-commercial-quotas-specification)
  - [ENT-D-12 Posture-owner contract with Subscriptions](#ent-d-12-posture-owner-contract-with-subscriptions)
  - [ENT-D-13 Recovery from a refusal](#ent-d-13-recovery-from-a-refusal)
  - [ENT-D-14 Unknown outcomes and findings](#ent-d-14-unknown-outcomes-and-findings)
  - [ENT-D-15 The posture carries the tenant axes of the subscription](#ent-d-15-the-posture-carries-the-tenant-axes-of-the-subscription)
  - [ENT-D-16 Only a posture revision changes provider state](#ent-d-16-only-a-posture-revision-changes-provider-state)
  - [ENT-D-17 Position toward neighbouring designs](#ent-d-17-position-toward-neighbouring-designs)
  - [ENT-D-18 Public surface](#ent-d-18-public-surface)
  - [ENT-D-19 License and resource plugin contracts](#ent-d-19-license-and-resource-plugin-contracts)
  - [ENT-D-20 Audit record and retention](#ent-d-20-audit-record-and-retention)
  - [ENT-D-21 Readiness profile schema](#ent-d-21-readiness-profile-schema)
  - [ENT-D-22 Qualification of license and resource profiles before sale](#ent-d-22-qualification-of-license-and-resource-profiles-before-sale)
  - [ENT-D-23 Fulfillment outputs](#ent-d-23-fulfillment-outputs)
  - [ENT-D-24 Held readiness between Subscriptions and Entitlements](#ent-d-24-held-readiness-between-subscriptions-and-entitlements)
  - [ENT-D-25 Typed hooks through ToolKit plugins](#ent-d-25-typed-hooks-through-toolkit-plugins)
- [Open questions](#open-questions)

<!-- /toc -->

Decision ids are `ENT-D-NN`; open questions are `ENT-QNN`. A decision carries a **Propagated** line naming the documents that cite it. The question register tracks external adoption and release evidence. The feature specifications define the BSS contracts now; an open integration gate is not missing feature detail. A profile stays unavailable until its owners provide the required evidence.

## Status board

| ID | Sev | Decision | Status |
| --- | --- | --- | --- |
| ENT-D-01 | H | Entitlements owns the provider-independent application boundary; selected adapters connect to service providers | ACCEPTED |
| ENT-D-02 | H | Initial usable readiness precedes Subscriptions' active commit (IA-2026-10-18) | ACCEPTED |
| ENT-D-03 | H | Application rules owned here; Products owns the quota catalog; Pricing stores and freezes plan rules | ACCEPTED |
| ENT-D-04 | H | One service principal; a single-target scope for every call; records owned by the ordering or the resource tenant | ACCEPTED |
| ENT-D-05 | H | One posture per service subscription; no call to Accounts; payer and seller axes are evidence only | ACCEPTED |
| ENT-D-06 | M | One posture outcome per revision: `failed` before `pending` before `applied` | ACCEPTED |
| ENT-D-07 | H | Every preparation generation has its own assignment identities; closure without activation closes its source keys for good | ACCEPTED |
| ENT-D-08 | H | Cleanup obligations per closed generation; Subscriptions admits a successor only after settlement | ACCEPTED |
| ENT-D-09 | M | Aggregate readiness is `pending`, `ready`, `failed_retryable`, `failed_definitive` or `blocked`; it is the only closure signal | ACCEPTED |
| ENT-D-10 | M | The capability flows derive and dispatch preparation; Entitlements owns the readiness profile | ACCEPTED |
| ENT-D-11 | H | The quota path follows the Commercial Quotas specification: adapter contract, result states, exclusivity claim, qualification in Products | ACCEPTED |
| ENT-D-12 | H | Four service-only methods; intake answers `taken`, `replay`, `stale`, `conflict` or `refused`; a digest conflict is repaired by a higher revision | ACCEPTED |
| ENT-D-13 | H | A restricting posture applies to the whole source without rule evidence; bounded retries; a provider head above the local head stops dispatch | ACCEPTED |
| ENT-D-14 | M | Recovery horizon from the capability profile; findings escalate to the platform operator; a disposition closes only the finding | ACCEPTED |
| ENT-D-15 | M | The posture carries the tenant axes fixed on the service subscription; payer and seller are evidence | ACCEPTED |
| ENT-D-16 | M | Only a posture revision changes provider state; for an inactive resource tenant only restricting commands are sent | ACCEPTED |
| ENT-D-17 | M | BSS application ownership; provider-specific mappings stay in adapter designs | ACCEPTED |
| ENT-D-18 | M | One SDK client, routes under `/bss-entitlements/v1`, service-only methods outside the router, GTS and permission inventory | ACCEPTED |
| ENT-D-19 | M | The license and resource plugins have the operations apply, read and recover; a hold is placed at the execution boundary | ACCEPTED |
| ENT-D-20 | M | One audit record per user or operator command; policy-based detail retention with durable replay and revision protection | ACCEPTED |
| ENT-D-21 | M | The readiness profile is an immutable record loaded from the deployment configuration, with one entry per capability | ACCEPTED |
| ENT-D-22 | M | Products holds the qualification of license and resource profiles before sale; Entitlements offers no support query | ACCEPTED |
| ENT-D-23 | M | Fulfillment outputs: three kinds with closed metadata, content behind the pinned adapter, bounded links, delivery states and routes | ACCEPTED |
| ENT-D-24 | H | Held readiness with Subscriptions: commitment in the ready result, resolution delivered in posture and recovered through the Subscriptions operation read, deadline owned by Subscriptions | ACCEPTED |
| ENT-D-25 | H | Typed ToolKit hook plugins and outbox delivery; exact bindings, limited authority, separate evidence and nonblocking notices | ACCEPTED |

## Decisions

**Release scope.** The decisions below describe the full target design. [PRD §5.1](PRD.md#51-in-scope) assigns outputs, service dispositions and hooks to v2/p2. V1 retains core service preparation, cleanup, security blocks and local retention protection. Deferred capabilities must not be required by an enabled v1 offer.

### ENT-D-01 Provider-independent application boundary

- **Where**: [DESIGN.md](DESIGN.md) §1.1 and §3.3.3; [ADR 0001](ADR/0001-application-boundary.md).
- **Problem**: Deployments may use different quota, license and resource systems. BSS needs one application contract that works with each qualified integration.
- **Decision**: Entitlements owns the application boundary for committed commercial posture. BSS selects adapters for quota application, license grants, resource execution and readiness evidence. Subscriptions owns commercial posture and consumes readiness. Its inventory and usage contracts remain separate.

  Entitlements stores desired state and correlated application results. It requires safe retries, protection from stale actions and recovery after lost results. Each adapter maps these guarantees to its provider APIs and statuses. Providers need not expose the same fields or state machine.

  Each product profile states which capabilities it needs. An unavailable required adapter cannot count as success. A capability explicitly marked not applicable needs no adapter. A native provider is not mandatory; each selected adapter must prove its profile before it can be enabled.
- **Platform binding**: Entitlements hosts plugins through the platform SDK, Types Registry and scoped ClientHub. Consumer and plugin contracts stay separate. This design adds no resolver gear.

  Before dispatch, save the selected instance, contract and profile versions, stable target and observation authority. Revisions and retries reuse that binding. Discovery changes, including a reconfigured GTS instance ID, cannot redirect existing work. BSS chooses the service route; the adapter design defines its OSS connection and receiving components. [Feature 01](features/01-posture-intake.md#adapter-route-configuration) owns the versioned route contract. Startup configuration matches exact accepted profiles and authorized scopes. Atomic snapshot activation fences new bindings; existing work retains its saved binding. A route cannot replace the purchase's readiness profile or commercial terms.

  Subscriptions and Entitlements have no peer startup `deps`. Each registers its own client and resolves the other lazily for work after initialization. Missing peers leave affected work pending under its original identity. A readiness event wakes the Subscriptions driver; polling the full result remains available without event delivery. Intake handlers do not wait for synchronous callbacks. DESIGN §3.4 and E-AC77 define startup, outages and recovery.

  Tenant Resolver and AuthZ remain the shared topology and authorization services. Runtime license checks remain outside the application contract. DESIGN §3.3.3 and §3.4 and E-AC25 to E-AC27 define the binding checks.
- **Propagated**: PRD.md §2.2, §6.6; DESIGN.md §1.1, §3.3.3; features/02-initial-readiness.md; features/03-quota-projection.md; features/04-license-grants.md; features/05-resource-execution.md; features/06-reconciliation-recovery.md; ADR/0001-application-boundary.md

### ENT-D-02 Initial readiness before active (IA-2026-10-18)

- **Where**: [PRD.md](PRD.md) §6.2 and the documents listed under Propagated.
- **Problem**: The service must be usable before its paid period starts. Readiness and activation belong to separate owners and need a recoverable handoff.
- **Decision**: Entitlements confirms initial readiness before Subscriptions commits active and starts the service period and accrual. It saves the complete assignment and producer manifest before dispatch. During `preparing`, adapters prepare the required access and resources. Entitlements accepts observations only for the authorized scope, assignment and desired revision. It reports `ready` only when all required assignments are ready for that revision and every configured readiness hook passes against current evidence (ENT-D-25).

  No `subscriptionId` crosses the adapter boundary. Entitlements keeps the intake-to-assignment mapping. Subscriptions binds the aggregate result to its saved operation and posture. Its activation commit rechecks the fences and writes active, a fresh local T, the receipt and outbox together. Entitlements neither assigns T nor treats a ready result as activation.

  Preparation covers account creation or linking, included deployment and usable permissions. It does not require first login or an unrelated purchase. Missing evidence is not failure. Late or unknown work remains recoverable. Withdrawal prevents activation and leaves cleanup and financial obligations with their owners. Renewal, resume and active-subscription changes keep their own contracts.
- **Admission boundary**: Subscriptions checks commercial and financial prerequisites before preparation and activation. Entitlements prepares the authorized capabilities and reports service readiness. Billing commitments stay outside the posture, manifest and adapter requests.
- **Held readiness (SUB-D-57)**: Hold the operation/posture/manifest-bound readiness commitment until Subscriptions' activation or terminal non-activation decision is recovered. A timeout cannot release it. Ordinary cleanup follows that decision. Emergency security denial takes effect immediately and triggers incident recovery; it does not undo past activation.

  Authorized preparation use before T, including never-activated work, is platform-borne within a qualified profile. The same inter-gear fact contracts carry the commitment and resolution. Adapters receive no subscription identity.
- **Limits**: ENT-Q09 to ENT-Q13 and ENT-Q15 require the selected profiles to prove retention, emergency behavior, exposure bounds, time mapping and recovery. SDK field names remain proposals. These gates cannot change accepted validity or commercial terms, or enable an unsupported quota profile.
- **Propagated**: PRD.md §2.2; DESIGN.md §1.1, §3.3.5; features/02-initial-readiness.md; features/04-license-grants.md; UPSTREAM_REQS.md §2.1

### ENT-D-03 Rule semantics here, storage and acceptance in Pricing

- **Where**: [PRD.md](PRD.md) §6.3 and the documents listed under Propagated.
- **Problem**: Rule meaning, commercial acceptance and metric definitions need clear owners. A retry must reproduce accepted rights without reading a changed plan.
- **Decision**: Entitlements owns the application rule schema: `Fixed` and `PerPlanUnit`, exact amount derivation, validity, access posture and grant semantics. Products owns the reviewed commercial quota catalog. Its versioned GTS definitions describe metrics, units, scope and accounting meaning, supported changes and qualified profiles. They describe what a limit counts, not how OSS maintains counters. A service-specific metric instance may carry `service_type_ref` to an exact adapter-owned service descriptor; generic metrics use explicit qualified mappings instead. Feature 03 defines this shared catalog extension. Namespace text never substitutes for that validated reference, and service identity cannot split canonical quota exclusivity.

  Pricing owns quota rules on `PlanRevision`. It validates the complete rule set against the Products catalog at publication and acceptance. It freezes the quota rules with the price terms in one acceptance receipt. Quota amounts and charging terms are independent: the same cap can belong to a prepaid, usage-priced or mixed offer.

  Derivation reads the frozen receipt through `PricingAcceptanceV1.acceptance` with explicit catalog tenant and acceptance ID, verifies purchase identity and digests, then uses Subscriptions' committed posture and effective quantity. Feature 01 defines the existing read and required E-U03 evidence extension. It never reads live plan terms. The original purchased quantity remains evidence. A rule change needs new acceptance even when the price stays the same. Entitlements checks exact metric, unit, policy, scope and digest references before applying the rule. A receipt alone authorizes no application.
- **Preparation and evidence**: Aggregate readiness and adapter application observations are separate contracts. Empty manifests have explicit results. A hold-only change still publishes a posture revision. Preparation uses the accepted limits under explicit authority. Activation continues the same assignment without a second quota grant. OSS owns allocation, usage and release throughout preparation and activation. Entitlements receives no per-resource quota events and keeps no usage ledger.

  Resource readiness and cleanup remain the resource adapter's contract. The adapter confirms whether a closed generation's effects are resolved or safely isolated before reuse. Uncertain effects remain recoverable. Consumption profiles are future scope; a readiness check does not enable one.
- **Limits**: E-U03 also covers grant definitions and service bindings, publication checks and frozen receipt evidence. Feature 05 pins selected SKU versions to the adapter-owned service type, profile, parameters and policies. Completing the quota path does not complete those contracts.
- **Propagated**: PRD.md §6; DESIGN.md §2; features/03-quota-projection.md

### ENT-D-04 Service authorization under single-target scopes

- **Where**: [DESIGN.md](DESIGN.md) §3.4 and the documents listed under Propagated; [commercial model DESIGN §3.4](../../../../docs/arch/bss-commercial-model/DESIGN.md#34-internal-dependencies).
- **Problem**: Intake belongs to the ordering tenant, adapter work belongs to the resource tenant, and workers run without a user request. The commercial model permits only single-target scopes (SUB-D-123, SUB-D-125 to SUB-D-127).
- **Decision**: Follow the service authorization contract in commercial model §3.4.

  *Principal.* Use one service account in the platform-root tenant with narrow action/resource roles assigned there. AuthN Resolver's client-credentials exchange supplies its security context. The gear must not build a context. A root role authorizes evaluation; it is not itself a scope.

  *Scope.* Each tenant call asks the policy decision point for a root-only, constrained scope with that tenant as context. Validate every returned branch: `owner_tenant_id` must equal the target. Reject multiple owner tenants, subtree or unknown predicates, and unfenced branches as authority/configuration errors. Do not treat an invalid scope as an ordinary denial or fall back. Pass valid scopes unchanged to storage.

  *Record ownership.* The ordering tenant owns intake, posture copies, per-revision posture outcomes, aggregate readiness results and intake/preparation-generation findings. Subscriptions reads these with its ordering-tenant scope. The resource tenant owns manifests, assignments, desired sets, adapter bindings, receipts, observations, assignment/hook findings and outputs. If these tenants differ, one local transaction may use both scopes, each only for its record types. Obtain scopes before the transaction. Never merge scopes or call the decision point under a database lock.

  *Intake.* Authorize the actual caller, not the Entitlements principal. Posture, preparation and withdrawal calls come from the Subscriptions service principal. Evaluate the intake action for both ordering and resource tenants. Payer and seller grant no authority for that intake (ENT-D-05). Pricing receipt reads are a separate owner call: use the accepted reference’s explicit catalog tenant and authorize acceptance read there through Pricing, as feature 01 specifies.

  *Workers.* Dispatch, readiness, reconciliation and recovery workers use the gear principal. Only these workers discover work. Page an authorized tenant roster, with at most 200 references per page. Every row read or write then uses its tenant's single-target scope. No worker gets an all-tenant data scope.

  *Barriers and tenant status.* User calls keep tenant barriers. A service may cross a self-managed barrier only through an explicitly authorized internal path. Request fields cannot bypass it. A valid scope does not prove the tenant exists or is active; check Tenant Resolver separately.

  *Service-only methods.* Intake and the readiness, posture outcome and service fulfillment reads used by Subscriptions are SDK-only. Do not register them in the public router. Check the principal assigned to each action; knowing a subscription or operation ID gives no access. Delivery, finding and output reads and operator commands use public paths under `PolicyEnforcer`.

  *Interim.* E-U08 has not delivered the exchange, service account/roles, target-fenced scope response or tenant roster. Until it does, Entitlements uses the documented Subscriptions interim (SUB-D-123, SUB-D-125 to SUB-D-127). Each exception remains a tracked gap.

  Workers use a discovery warrant to read only owner-tenant and row IDs across tenants, in configured batches. Each later row read/write uses that tenant's scope. The deployed decision point returns `owner_tenant_id IN [R]` for a grant anchored at ancestor R of target T. The interim constructs `owner_tenant_id IN [T]` from that allow and records R as proof. It refuses service callers behind a self-managed barrier.

  Adapter calls use an in-process first-party identity, logged at each construction. Subscriptions uses its own first-party identity for intake. Until its service principal is configured, policy alone limits the SDK-only methods, whose permissions go only to that identity. These exceptions end when E-U08 closes.
- **Limits**: E-U08 requires the client-credentials exchange, service account/roles, target-fenced root-only scope response and tenant roster. Subscriptions tracks these through S-U20 and its service-principal requirement. ENT-Q29 keeps permission confirmation and scale verification open. ENT-D-16 defines inactive-tenant behavior.
- **Propagated**: PRD.md §2.2, §4.1, §8, §13; DESIGN.md §3.3.1, §3.4, §3.8, §4.1; features/01-posture-intake.md; features/06-reconciliation-recovery.md; DECOMPOSITION.md; UPSTREAM_REQS.md §2.5

### ENT-D-05 One posture per service and no Accounts dependency

- **Where**: [PRD.md](PRD.md) §5.2 and the documents listed under Propagated; [Accounts PRD](../../accounts/docs/PRD.md); [commercial model DESIGN §2.1](../../../../docs/arch/bss-commercial-model/DESIGN.md#21-design-principles); [resale ADR](../../docs/ADR/0003-cpt-cf-bss-adr-multi-tier-resale-model.md).
- **Problem**: Accounts owns account use, Subscriptions records billing assignments, and one service may have several commercial legs. These facts must not create extra service rights.
- **Decision**: Entitlements calls no Accounts API, consumes no Accounts event and stores no account, offer, account-use grant or billing assignment.

  A revoked offer, blocked account, expired account-use grant or supply failure changes desired provider state only through a new Subscriptions posture. Entitlements applies that posture without needing the financial cause.

  One service subscription has one posture and one set of desired state. A commercial leg, attachment order or chain version creates no posture member or assignment. Rules come only from the acceptance receipts of the service subscription's own purchases. The commercial model checks supply compatibility; Entitlements does not read another leg's plan.

  Keys and authority use the ordering tenant for posture and the resource tenant for sources and adapter calls. Payer and seller axes remain evidence on the posture and receipt (ENT-D-15). They do not select resource authority, provider keys or adapter payloads. The catalog tenant in each accepted purchase reference separately qualifies the Pricing receipt read; it may equal the seller but grants no resource authority. Transferring a payer or account on a leg changes no assignment or desired state.

  `quota_controlled` funding needs a consumption profile. V1 has none, so Products cannot qualify that offer and setup or admission must refuse it, as the Accounts PRD requires.
- **Limits**: ENT-Q27 covers a future offer-to-quota mapping and capability profile. It adds no current consumption or accounting contract.
- **Propagated**: PRD.md §1.4, §2.2, §5.2, §13; DESIGN.md §3.3.1, §3.4; features/01-posture-intake.md

### ENT-D-06 Posture outcome per revision

- **Where**: [DESIGN.md](DESIGN.md) §3.3.1.
- **Problem**: Subscriptions reads one outcome for each posture it sent (SUB-D-106). It must not know which capability assignments a profile needs.
- **Decision**: Entitlements returns an outcome for `(orderingTenantId, subscriptionId, revision)`. It combines the [delivery states](features/06-reconciliation-recovery.md#delivery-state) of the required desired states: `failed` if any is failed, `applied` if all are applied, otherwise `pending`. Optional or inapplicable capabilities do not block the result.

  A pending assignment cannot hide another assignment's failure. Per-capability results and findings remain readable.
- **Limits**: ENT-D-12 defines the read. A failed posture outcome can change when a new application attempt starts under ENT-D-13.
- **Propagated**: DESIGN.md §3.3.1; features/06-reconciliation-recovery.md

### ENT-D-07 Assignment identity per preparation generation

- **Where**: [initial readiness](features/02-initial-readiness.md) and the documents listed under Propagated.
- **Problem**: An attempt can close before activation. Its late commands and cleanup must not affect a successor that prepares the same purchase.
- **Decision**: A preparation generation is an activation operation and preparation revision. Each generation has distinct assignment identities and source keys. For quotas, one assignment covers an accepted acquisition, logical binding and generation. Its metrics are members of that assignment (ENT-D-11).

  `closed_without_activation` closes that generation's sources permanently. A higher revision or late ready result cannot reopen them. Cleanup targets only that generation.

  Subscriptions owns a stable `fulfillmentRef` for each purchased component. Entitlements uses it to correlate reads across assignments and generations. It is not an assignment/source key and grants no access. Adapters and hooks do not receive it. [Feature 06](features/06-reconciliation-recovery.md#public-reads-and-operator-commands) defines the contract.

  Recovery within a generation retains the assignment and operation identities. It cannot create a second environment. Linking an existing customer account stores a target reference; cleanup cannot delete that account. Activation retains the assignments it prepared.
- **Limits**: ENT-Q10 requires each adapter to prove its identity mapping. Before a successor, ENT-D-08 requires confirmation that the prior effects are resolved or isolated and the target can be reused.
- **Propagated**: features/02-initial-readiness.md; features/03-quota-projection.md; features/04-license-grants.md; features/05-resource-execution.md

### ENT-D-08 Cleanup obligations and settlement after non-activation

- **Where**: [initial readiness](features/02-initial-readiness.md) and [resource execution](features/05-resource-execution.md).
- **Problem**: A closed preparation can leave provider effects. A successor must not collide with them. Ordinary retention deletion needs evidence that a never-activated subscription does not have.
- **Decision**: Entitlements keeps a cleanup obligation for each closed-generation assignment that may have effects. Its states are `open`, `requested`, then `settled` or `quarantined`. The closure resolution authorizes cleanup. Resource work uses `withdraw`; retention deletion remains `delete`. The quota source stays closed, but its application history and recovery remain available.

  Unknown cleanup remains open with a finding. Only correlated adapter evidence can confirm resolution or safe isolation of possible external effects. Operator acknowledgment or disposition changes only the finding; it cannot settle or quarantine the obligation.

  Withdrawal is accepted when the intake transaction commits. The closed generation's readiness read returns a separate closed result with the exact Subscriptions resolution and settlement: `open`, `settled` or `quarantined`. It has no current readiness status. For every assignment that could have external effects, settlement requires confirmed resolution or isolation and safe reuse from its pinned adapter. Fold all obligations, including optional assignments: any unresolved obligation gives `open`; all settled gives `settled`; otherwise return `quarantined` only when every remaining obligation has confirmed isolation and safe successor scope. A finding label cannot satisfy this fold.

  A generation can settle locally only after its terminal non-activation fence and durable dispatch history prove that no effect-capable call could have left the process. Commit a permanent dispatch-start marker before permitting such a call, under the same generation fence used by closure. Closure cancels unstarted work. Missing receipts, expired leases or absence of a manifest are not proof of no dispatch. Once a marker exists, require adapter evidence even if the worker may have crashed before the call.

  Entitlements tracks these obligations and confirmations. It does not inspect inventory, count allocations or calculate released quota. OSS owns those mechanisms.

  Subscriptions admits a successor only after `settled` or `quarantined`. Until then preparation remains in flight and another activation is refused as retryable (SUB-D-88). The successor's deadline starts only after admission.
- **Limits**: E-U06 adds settlement to the Subscriptions effect record. ENT-D-14 defines escalation and recovery of unknown cleanup.
- **Propagated**: DESIGN.md §3.1, §3.3.2; features/02-initial-readiness.md; features/05-resource-execution.md; UPSTREAM_REQS.md §2.5

### ENT-D-09 Failure class of preparation and the closure signal

- **Where**: [DESIGN.md](DESIGN.md) §3.3.2 and [initial readiness](features/02-initial-readiness.md).
- **Problem**: Subscriptions closes an attempt only on a definitive preparation failure (SUB-D-86). It needs a result even when the manifest cannot be built.
- **Decision**: Aggregate readiness uses the Subscriptions statuses: `pending`, `ready`, `failed_retryable`, `failed_definitive` and `blocked`. An adapter's failed readiness observation includes its definitive or retryable class. Application receipts also contribute: pending, partial and unknown remain pending; refused maps through the pinned capability profile's `readinessFailureClasses` to retryable or definitive failure. An unclassified refusal is retryable with a finding. Applied evidence satisfies only the application criterion, and only without an open fulfillment discrepancy; required outputs, observations and hooks still apply.

  Evaluate required assignments in this order: any definitive failure gives `failed_definitive`; an active emergency security block gives `blocked`; any retryable failure gives `failed_retryable`; all required adapter criteria and configured readiness hooks satisfied gives `ready`; otherwise return `pending`. Hook failures are retryable and never definitive (ENT-D-25). Pending work cannot hide a failure. Subscriptions waits on pending, blocked or retryable results and closes on definitive failure.

  If the posture or profile is unsupported or inconsistent, return `failed_definitive` with `manifest_unbuildable`, without a manifest binding. If a dependency is temporarily unavailable, return `pending` with the reason.

  The aggregate result and deadline evidence are the only preparation inputs to Subscriptions' closure decision. Entitlements sends no closure command. Each changed result schedules an Event Broker notification; the full-result read remains authoritative (ENT-D-12). Only Subscriptions evaluates the deadline.

  A producer's `withdrawn` observation means definitive failure with `producer_withdrawn`. Only Subscriptions' resolution can close the manifest. Once resolved, the manifest is `resolved_applied` or `resolved_closed`; later observations remain evidence. A generation closed without activation returns the closed variant with resolution and settlement under ENT-D-08, even if no manifest exists. It cannot return a current readiness status. Closure and later settlement results share the operation/preparation sequence. Late readiness cannot reopen the attempt. Feature 02 defines the SDK/GTS variants under E-U06.

  The pinned adapter reports a typed, assignment-scoped `security_block` observation. Active blocks on required assignments prevent ready, including with the `applied` criterion; optional blocks remain visible. Only an explicit higher-sequence clearance for that incident from the same authority can lift it, with audited evidence. Ordinary readiness, hooks and operator findings cannot clear it. Feature 02 defines incident identity, GTS reasons and recovery. Subscriptions retains activation authority.
- **Limits**: Each capability profile pins `readinessFailureClasses` by version and digest. It maps registered refusal reasons to `retryable` or `definitive`; qualification covers every supported reason. A readiness failure class does not change operation recovery or authorize cleanup. E-U06 defines the shared result fields. Quota application results remain the separate four-state contract in ENT-D-11.
- **Propagated**: DESIGN.md §3.3.2; features/02-initial-readiness.md

### ENT-D-10 One derivation path and the readiness profile

- **Where**: [initial readiness](features/02-initial-readiness.md), [resource execution](features/05-resource-execution.md) and [DESIGN.md](DESIGN.md) §3.3.1.
- **Problem**: Separate preparation and activation derivations could produce different identities at T. The readiness profile also needs one owner.
- **Decision**: Quota, license and resource flows derive and dispatch each revision, including `prepare_activation`. Preparation requires its explicit authority. The readiness component builds a manifest from these desired states and the profile, then combines observations. It dispatches no provider work itself.

  If the profile requires service fulfillment, preparation derives a `provision` intent. Activation retains the logical service assignment; OSS controls its resources. A license assignment covers one logical grant binding, capability and holder within its saved commercial source and preparation generation. It can represent multiple seats. Quantity changes, restriction and resume retain its identity; distinct rights have independent source fences. `DesiredGrantSet` is a local collection, not one assignment. A resource assignment covers a posture member, capability and logical target. A quota assignment covers a logical binding (ENT-D-07, ENT-D-11). Its saved `AdapterBinding` identifies the observation producer.

  Entitlements owns the versioned readiness profile. Each capability declares whether it is required, optional or not applicable, its readiness criterion, target rule and required outputs. Subscriptions records the profile reference and version on the activation attempt and sends them in preparation. For purchases covered by ENT-D-22, it copies the pre-purchase binding instead of selecting from live Settings; E-U06 extends SUB-D-85.

  The `entitlements` activation profile selects this gear. With `no_preparation`, Subscriptions sends no preparation posture (SUB-D-70). Entitlements accepts its applied resolution without a local manifest or held commitment, then applies the first lifecycle posture normally. V2 hooks are available only with the `entitlements` activation profile. Assignments created after activation inherit notice bindings from the locally retained profile of their source's applied preparation generation.

  A live route or new profile cannot replace saved bindings. Products qualification requires Entitlements preparation for hook-enabled services. Pricing freezes that requirement. Orders obtains a durable Subscriptions configuration binding before commitment/capture, then passes it to activation. Ordinary Settings changes cannot replace the saved mode or profile (ENT-D-22, ENT-D-25).
- **Limits**: ENT-D-21 defines the profile. E-U06 adds its reference to the Subscriptions activation contract.
- **Propagated**: DESIGN.md §3.1, §3.3.1; features/02-initial-readiness.md; features/05-resource-execution.md

### ENT-D-11 Quota application follows the Commercial Quotas specification

- **Where**: [quota projection](features/03-quota-projection.md) and the documents listed under Propagated; the [Commercial Quotas design, merged in PR #5973](../../docs/quotas/DESIGN.md).
- **Problem**: The quota path needs one contract from plan acceptance to adapter application. This package must use the same rules.
- **Decision**: Follow the [Commercial Quotas design, merged in PR #5973](../../docs/quotas/DESIGN.md). Check its accepted version before qualification.

  *Boundary.* Entitlements calls the BSS-selected quota application adapter. E-U01 defines that contract. The adapter design selects its OSS components, connections, downstream APIs and permissions. This package requires no particular OSS implementation.

  *Operations.* Apply with `apply_quota_state(operation_id, source_key, source_revision, desired_digest, complete_target, effective_boundary, authority)`. Read the source state and recover an operation by its saved identity and binding. The target retains exact metric, amount, unit, policy, scope and validity evidence. Incomplete input cannot remove missing members.

  *Results.* Status is `pending`, `applied`, `refused` or `unknown`. Known partial work stays `pending`; uncertain effects are `unknown`. Only confirmation of the complete set allows `applied`. Refusal requires no unresolved effects or recovery obligations. A timeout or exhausted retries cannot become refusal. The contract does not require one OSS transaction.

  *Ordering.* The adapter exposes its highest durably accepted revision and digest separately from the last fully applied revision and digest. Malformed, unauthorized or conflicting calls advance neither. After accepting a higher revision, older delayed or in-flight work cannot apply superseded state, even if the newer request is later refused. Acceptance alone proves no effective limit.

  *Replay.* An operation replay returns its recorded or recovered result, including a saved refusal. Changed content conflicts. A corrected attempt uses a new operation and higher source revision under the same purchase. Posture revision, source revision and observation version are separate sequences. Closure permanently blocks grants but preserves authorized reads and recovery.

  *Exclusivity.* Subscriptions secures the commercial scope before purchase commitment. Entitlements checks its claim against the saved scope and installs the claim with the assignment. A missing or conflicting claim blocks dispatch. A successor generation uses the same acquisition claim and needs safe-reuse confirmation (ENT-D-07, ENT-D-08). The adapter receives only the authority/domain/claim proof, without payer or purchase payloads.

  *Catalog and rules.* Products owns reviewed GTS metric, unit, accounting and scope definitions, historical evidence, qualification and sale availability. Pricing owns plan amounts and accepted price/quota terms. It checks the complete set against enabled qualification at publication and acceptance. Entitlements derives from the frozen rules and committed quantity. Charging terms do not change the quota amount. Entitlements offers no support query.

  *V1 scope.* Support initial installation and qualified increases. Numerical decreases, consumption quotas, pooling, partitions and scope migration are future scope. Regional or pool scopes are optional qualified profiles. A tenant-wide offer requires proof of its aggregate scope; no binding may copy the full allowance into independent scopes. Buying a limit alone requires no VM creation.

  *Reads and recovery.* Entitlements exposes purchased/desired limits, application status and discrepancies. OSS supplies effective limits and used, reserved and remaining values. Entitlements keeps no runtime counter projection or allocation/release ledger. A disabled qualification blocks new sales but preserves accepted terms and recovery obligations. Temporary outages remain pending or unknown; a fallback provider is not success.
- **Limits**: The quota gates G01 to G08 require implementation evidence. This design does not choose internal OSS components or reopen their architecture.
- **Propagated**: PRD.md §1.4, §5.1, §5.2, §6.3, §9, §13; DESIGN.md §3.1, §3.3.1, §3.4, §4.1; features/03-quota-projection.md; UPSTREAM_REQS.md §2.5, §4

### ENT-D-12 Posture-owner contract with Subscriptions

- **Where**: [DESIGN.md](DESIGN.md) §3.3.1 and [posture intake](features/01-posture-intake.md).
- **Problem**: Subscriptions sends posture and reads its outcome (SUB-D-87, SUB-D-88, SUB-D-106). Both gears need explicit intake results and a rule for digest conflicts.
- **Decision**: Entitlements provides five service-only SDK methods: `submit_commercial_posture(posture)`, `read_posture_receipt(orderingTenantId, subscriptionId, revision, digest, operationId)`, `read_preparation_result(orderingTenantId, operationId, postureRevision)`, `read_posture_application_result(orderingTenantId, subscriptionId, revision)` and `read_service_fulfillment(orderingTenantId, subscriptionId, postureRevision, cursor)`. The last read returns scoped, snapshot-paged service outcomes and confirmed effective times under feature 05, without OSS inventory.

  Intake returns `taken` when the new head and its work are committed, `replay` for the same revision and digest, `stale` for a lower revision, `conflict` for changed content at the same revision, or `refused` for authorization, validation or schema failure. A stale result includes the head revision; a conflict includes the stored digest; a refusal includes its reason.

  Transport failure leaves the outcome unknown. Subscriptions recovers the exact intake receipt or retries the original call. The receipt retains the decision separately from whether the accepted revision is still current. Replay is delivery metadata, not a new decision. Feature 01 defines the SDK read and scoped operator inspection; neither implies application.

  Subscriptions marks `taken` and `replay` as dispatched, and `stale` as superseded. It keeps conflict or refusal pending with the reason and follows its backoff and overdue-warning rules.

  `submit_commercial_posture` serves the posture apply and preparation prepare/withdraw ports. The readiness read serves the preparation port. Posture outcome is `applied`, `failed` or `pending` with at most 16 findings. Entitlements saves each changed readiness result with an outbox notification for Event Broker. Subscriptions consumes that notification and schedules its driver to read the full result. There is no readiness SDK callback. Notifications carry correlation and `statusSequence`, not readiness authority. Polling recovers lost notifications; duplicates and gaps never require replay of every intermediate result. Feature 02 defines the event contract.

  Input is the Subscriptions SDK's `CommercialPostureV1`. It has one document-wide `access`: `prepare`, `grant`, `restrict`, `revoke` or `withdrawn`. Component grants carry SKUs, quantity, acceptance references and lot identities. E-U06 must extend them with the effective validity, holder scope and per-member access supplied by Subscriptions. Pricing freezes the purchased definition, binding and policies. Entitlements checks these inputs and maps them to adapter requests; it does not infer member rights from document-wide access or current catalog values. The document also carries preparation context and activation resolution. Its digest is lowercase hexadecimal SHA-256 over canonical JSON with the digest member omitted.

  A conflict never rewrites an accepted revision. Subscriptions sends corrected content at a higher revision. An audited disposition closes the conflict finding.
- **Limits**: E-U06 records agreement on versions, field names and typed results. ENT-D-18 defines the other public reads.
- **Propagated**: DESIGN.md §3.3.1; features/01-posture-intake.md; DECOMPOSITION.md

### ENT-D-13 Recovery from a refusal

- **Where**: [quota projection](features/03-quota-projection.md), [reconciliation](features/06-reconciliation-recovery.md) and [DESIGN.md](DESIGN.md) §3.3.1.
- **Problem**: Subscriptions stops resending a posture once intake accepts it. Entitlements must recover its own work. Missing rule evidence must not prevent a valid restriction.
- **Decision**: A `Restricted` or `Revoked` posture applies to the whole source. Its command carries the source, posture and revision, so it needs no rule snapshot. Granting or widening access always requires complete derivation.

  Retry temporary incomplete derivation under the pinned core recovery policy of feature 06. Horizon exhaustion raises a finding and slows recovery without treating missing evidence as refusal. A definitive derivation refusal opens a finding, reports failed application and waits for an operator reconcile command or a higher posture revision. A required capability refusal makes the posture outcome failed even before an assignment exists. Temporary incomplete derivation stays pending.

  An adapter refusal is final for that operation. After remediation, a new attempt needs a higher source revision and new operation identity. Entitlements may start it automatically only for reasons marked remediable by the profile and within its attempt limit. Other retries need an operator reconcile command. A wrong posture is corrected only by Subscriptions through a higher revision.

  Pricing refuses known incompatible quota sets before purchase commitment using Products qualification. This proves compatibility, not live OSS capacity or availability. Later delivery failures keep the accepted obligation and enter application recovery. Entitlements adds no pre-commit support check.

  If the adapter's accepted head is higher than the local head, stop dispatch, overwrite nothing and open a finding. Feature 06 defines recovery from verified original history and the checks before dispatch resumes. Unverifiable history keeps the source blocked for data recovery; no revision bump or operator finding disposition bypasses it. Subscriptions sees failed delivery through the posture outcome and its overdue warning (SUB-D-106).
- **Limits**: Feature 06 defines the mandatory versioned recovery policy: call bounds, delivery backoff, result polling, escalation horizon/cadence and reason-specific replacement budget. The policy and recovery-chain counters are pinned to durable work; restarts and generated revisions cannot reset them. ENT-D-22 covers qualification for licenses and resources.
- **Propagated**: DESIGN.md §3.3.1; features/03-quota-projection.md; features/06-reconciliation-recovery.md; DECOMPOSITION.md

### ENT-D-14 Unknown outcomes and findings

- **Where**: [reconciliation](features/06-reconciliation-recovery.md).
- **Problem**: Unknown outcomes need escalation without being mistaken for refusal. An operator decision must not fabricate successful application.
- **Decision**: The capability profile sets the recovery horizon. An older unknown result opens `unknown_outcome` and escalates to the platform operator through a durable finding, queue, metric and event. Recovery continues at a lower rate under the original operation identity. Elapsed time cannot turn uncertainty into refusal.

  Findings have one immutable typed subject and owner: intake receipt or preparation generation in the ordering tenant; assignment or hook execution in the resource tenant. A missing assignment or manifest requires no placeholder. Reads, commands, audit and events follow that owner. Feature 06 defines the SDK/GTS variants and scope checks. Findings move from `open` to `acknowledged`, then to `resolved` by correlated evidence or `dispositioned` by an audited operator decision.

  A disposition closes only the finding. It cannot mark work applied, settle or quarantine a cleanup obligation, change OSS resource state or replace the adapter confirmation of ENT-D-08. Subscriptions still sees pending or failed delivery and decides commercial actions through its own contracts.
- **Limits**: The profile's numeric values remain qualification evidence. Feature 06 specifies their SDK/GTS fields, start times and persistence, plus the core policy used before capability selection. Exhausted fast delivery slows recovery without discarding the operation. ENT-D-18 defines operator reads and commands; ENT-D-20 defines their audit records.
- **Propagated**: features/06-reconciliation-recovery.md; DECOMPOSITION.md

### ENT-D-15 The posture carries the tenant axes of the subscription

- **Where**: [DESIGN.md](DESIGN.md) §3.3.1.
- **Problem**: One service may have several commercial legs, each with its own payer and seller. Provider work must not choose a leg.
- **Decision**: `CommercialPostureV1` carries the ordering, resource, payer and seller tenants fixed on the service subscription (SUB-D-113). A leg changes none of these axes, so changing a leg's payer publishes no posture.

  Entitlements uses the ordering tenant for posture identity and the resource tenant for source keys and adapter authority. Payer and seller remain evidence in the posture copy. They do not select provider routing or resource authority (ENT-D-05). Historical Pricing reads use the explicit catalog tenant and acceptance ID from purchase provenance, with a separate owner-side read decision. This does not change source identity.
- **Propagated**: PRD.md §6.1; DESIGN.md §3.3.1, §3.4; features/01-posture-intake.md; UPSTREAM_REQS.md §2.1

### ENT-D-16 Only a posture revision changes provider state

- **Where**: [quota projection](features/03-quota-projection.md), [license grants](features/04-license-grants.md), [resource execution](features/05-resource-execution.md) and [reconciliation](features/06-reconciliation-recovery.md).
- **Problem**: Lifecycle events must have a clear effect on desired state. Renewal or a scheduled change must not accidentally create new rights.
- **Decision**: Only a posture revision changes Entitlements' desired provider state. An event without a posture changes nothing here. Access is `grant` for active, `restrict` for suspended and `revoke` for cancelled.

  Renewal publishes no posture. It creates no new quota allowance; grants and resources continue. Scheduled cancellation and grace publish no posture until Subscriptions commits a status.

  Suspension restricts the source. It is an access control, not a numerical quota decrease. The adapter must block prohibited actions without creating a fresh allowance on resume. OSS owns how it preserves resource accounting. License restriction uses the frozen `suspend_access` policy; revocation uses `revoke_access`. Feature 04 defines their closed values, holder kinds and validity modes. Subscriptions supplies the actual member scope, interval and state; resource behavior comes from the resource execution contract.

  A quantity increase or new component arrives in a new revision. Existing quota bindings increase under ENT-D-11; new components get their own assignments. Numerical decreases and base-plan changes are outside v1.

  If Tenant Resolver reports the resource tenant inactive, workers send only restricting commands: restrict, revoke, withdraw and cleanup. Grants and increases stay pending with `tenant_inactive`. Subscriptions decides the commercial outcome.
- **Limits**: ENT-Q05 requires the quota adapter's restriction guarantee. Disposition fields need their Subscriptions contract before use.
- **Propagated**: features/03-quota-projection.md; features/04-license-grants.md; features/05-resource-execution.md; features/06-reconciliation-recovery.md

### ENT-D-17 Position toward neighbouring designs

- **Where**: [UPSTREAM_REQS.md](UPSTREAM_REQS.md) §2.5.
- **Problem**: Other owner designs must agree on who applies commercial decisions and what preparation may do before activation.
- **Decision**: This package defines the BSS boundary. Each provider design defines its own integration.

  *Quotas.* Entitlements calls the quota application adapter under ENT-D-11. Subscriptions secures exclusive commercial ownership before commitment. The adapter verifies the claim and protects other sources. Its OSS components, management roles and enforcement APIs belong to the adapter design. E-U01 requires the observable contract, not changes to a named OSS component.

  *Licenses.* Entitlements calls the selected license adapter. Any license-provider design that names Subscriptions as the issuer must adopt this application boundary under E-U02. Provider-specific grant verbs stay in the adapter design.

  *Orders.* Customer rights start under the accepted activation contract. Preparation before T uses one activation operation's bounded platform authority (ENT-D-02, SUB-D-57). It does not start the customer's service period. Orders alignment goes through Subscriptions S-U01.
- **Propagated**: DESIGN.md §3.1; DECOMPOSITION.md; UPSTREAM_REQS.md §2.5

### ENT-D-18 Public surface

- **Where**: [DESIGN.md](DESIGN.md) §3.3.8 and §4.3.
- **Problem**: Consumers and authorization owners need a concrete SDK, route and permission inventory.
- **Decision**: The gear exposes `EntitlementsClientV1` and routes under `/bss-entitlements/v1`. Subscriptions' five methods and adapter observation intake remain SDK-only (ENT-D-04, ENT-D-12). Users and operators read assignments and findings, request reconciliation and acknowledge or disposition findings through `PolicyEnforcer`. Operators with explicit ordering-tenant permission also inspect safe intake receipts; that permission grants no raw posture access. Public routes and adapter payloads carry no subscription ID.

  Types use `gts.cf.bss.entitlements.<type>.v1~`. Reason codes are registered instances of one reason type. Permissions derive from the platform permission base as `cf.bss.entitlements.<name>.v1`, with one action and resource type. Resources are posture intake, assignment, finding, fulfillment output, hook execution and audit record.

  Public lists use ToolKit OData and `Page<T>`, with declared filter/order fields and 50/200 limits. Feature 06 owns the collection contract; features 01 and 07 use it for receipt and hook inspection. The gear publishes readiness-change and finding notifications through Event Broker. Posture remains an SDK command. DESIGN §3.3.8 and §4.3 contain the inventory.
- **Limits**: Names become final with `entitlements-sdk`; they are not registered yet. ENT-D-19 to ENT-D-25 define plugins, audit, readiness profiles, qualification, outputs, held readiness and extension hooks.
- **Propagated**: DESIGN.md §3.3.8, §4.3; PRD.md §9

### ENT-D-19 License and resource plugin contracts

- **Where**: [DESIGN.md](DESIGN.md) §3.3.5 and §3.3.6.
- **Problem**: Lost results need a recovery operation for each capability. Shared guarantees must not depend on a provider's native API.
- **Decision**: Quota, license and resource plugins expose application, reads and recovery. The quota contract is ENT-D-11.

  *License plugin.* Operations are `apply_license_grant(operation_id, source_key, source_revision, desired_digest, complete_target, effective_boundary, authority)`, `read_license_grant(source_key)` and `read_operation_result(source_key, operation_id)`. The source identifies one logical right, not a whole plan or one seat. Its complete target contains definition, quantity, holder scope, validity, frozen restriction/revocation application policies, posture and preparation authority. Each assignment has its own operation, revision, digest and result; the local grant collection has no shared adapter fence.

  Results use `pending`, `applied`, `refused` or `unknown`. They include accepted and applied revisions and digests, effective time when the provider owns it, the assignment's provider reference and required output descriptors. Ordering follows ENT-D-11: saved operations replay their result, unseen lower revisions are stale, changed content conflicts, and terminal revocation closes the source. No older work can restore superseded rights.

  *Resource plugin.* Operations are `submit_service_operation(operation_id, source_key, assignment_generation, kind, boundary, evidence, authority)`, `read_service_operation(source_key, operation_id)`, `read_service_state(source_key, assignment_generation)` and `apply_service_hold(operation_id, source_key, assignment_generation, hold_policy_version, held)`.

  The `evidence` envelope carries required `source_revision` and `desired_digest` for the desired target. Several work orders, such as stop then export, may share that revision and digest while keeping distinct operation IDs and kinds. Bind each operation to its logical service assignment, generation and complete request content. The generation identifies BSS authority, not physical infrastructure. Native resource identities and execution stay in OSS and the adapter design. Changed content under one operation ID conflicts. A changed desired digest at the same source revision also conflicts.

  The adapter accepts the revision durably before effects and prevents older work from starting superseded effects. It cannot undo an irreversible action that already won, such as deletion. Retain its actual outcome and recover partial or uncertain effects. Check recorded operation identity and content before the stale-revision filter: a known older operation returns its saved or recovered result, while an unseen lower revision is stale.

  Work orders return `pending`, `applied`, `partial`, `refused` or `unknown` for the service intent, with actual effective time when confirmed. The state read returns assignment-level fulfillment, acknowledged hold policy version and retention/export state. It contains no resource roster. An adapter may use a tenant-level OSS operation without knowing individual resources. `apply_service_hold` returns `acknowledged`, `pending`, `refused` or `unknown`. Entitlements reports protection only after acknowledgment. The resource contract's `partial` status does not extend the quota result enum.

  *Outputs.* License and resource plugins expose `retrieve_output(source_key, output_id, output_version, retrieval_policy)` when the profile requires customer delivery.

  *Every call.* Use the saved target and namespace; the adapter rejects a mismatch. Send no subscription identifier. A retry returns the saved or recovered result of the same operation, not a new attempt. Final results remain stable; pending, partial or unknown work may progress through recovery. If the adapter has no operation record, repeat the original request and identity under the current ordering rules. A transport timeout means unknown. Observations enter through `record_assignment_observation`.
- **Limits**: These are proposed SDK operations. The feature contracts define fields, bounds, digest rules and result variants; `entitlements-sdk` implements them. E-U02 and E-U05 require adapter designs to map the operations to their providers.
- **Propagated**: DESIGN.md §3.3.5, §3.3.6; features/04-license-grants.md; features/05-resource-execution.md

### ENT-D-20 Audit record and retention

- **Where**: [DESIGN.md](DESIGN.md) §3.3.8 and §3.7.
- **Problem**: User and operator actions need durable attribution. Deleting stored fences could also let a delayed command reopen a closed source.
- **Decision**: Each applied or refused user/operator command writes one `gts.cf.bss.entitlements.audit.v1~` record in the outcome transaction. Commands include assignment reconciliation, finding acknowledgment or disposition, and protected output retrieval.

  Record the target tenant, resource and before/after revisions, operation ID and kind, asserting and executing principal references, subject type and home tenant, grant anchor, request ID, safe reason, outcome and refusal reason. Separate protected actor identity data from the immutable action/outcome core under feature 06. Audit records belong to the subject owner tenant. Ordinary callers cannot rewrite them; expiry and identity erasure follow the platform audit policy. Never include protected content, retrieval links or provider payloads.

  Service calls and worker actions use their journals, observations, receipts and resolutions as evidence. They create no separate audit record. A refusal that stores nothing creates none. Operators list one tenant's audit records by cursor with `list_audit_records`, `GET /audit` and `audit_read`.

  A versioned deployment policy sets retention for completed operational details and closed-finding details. Keep unresolved work, current state, complete results required by service reads and evidence needed by active rights or cleanup. Compaction preserves operation identity, request digest, final result and revision/closure fences. Audit has its own platform policy. Feature 06 defines eligibility, holds, replay retirement and actor-identity erasure. Delivered outbox rows may be removed once their journals own recovery. Entitlements stores no protected output content or retrieval link.

  If an observation changes neither status, reason nor evidence reference, advance its stream's high-water mark without another record. Storage grows with state changes, not polling frequency.
- **Limits**: No TTL deletes unresolved work or removes a fence while stale requests remain admissible. Finite fence retention requires a qualified replay bound and durable namespace-retirement proof at every receiving boundary; otherwise keep the fence. Linked financial evidence and retention holds may extend detail retention. A recovery horizon controls escalation, not deletion. See feature 06 for the policy owners and required fields.
- **Propagated**: DESIGN.md §3.1, §3.3.8, §3.7, §4.3; features/06-reconciliation-recovery.md; PRD.md §9.1

### ENT-D-21 Readiness profile schema

- **Where**: [DESIGN.md](DESIGN.md) §3.3.9.
- **Problem**: The manifest and Subscriptions attempt pin a readiness profile. Both owners must validate its identity and digest consistently.
- **Decision**: A profile is an immutable `gts.cf.bss.entitlements.readiness_profile.v1~` record identified by `(profileRef, version)`. Its digest is lowercase hexadecimal SHA-256 over canonical JSON without the digest member. Deployment configuration supplies profiles with routes. Entitlements loads them at startup and refuses to start if a stored identity has different content. Changes require a new version; no API edits a profile.

  Each profile may also contain the immutable `hooks` list of ENT-D-25. Absence means no hooks; digest verification preserves the original serialized content. Each profile has quota, license and resource entries. Each entry states `required`, `optional` or `not_applicable`, the adapter capability profile, readiness criterion, target rule and required outputs.

  Criterion `applied` needs a stored applied receipt for the desired revision. `observed_ready` also needs a ready observation from the saved producer for that revision. Target rule `resource_tenant` uses the resource tenant; `member_scope` uses the posture member's holder scope. Optional capabilities are dispatched and reported but do not block ready.

  Three `not_applicable` entries and no readiness hooks produce an empty readiness requirement set. An empty assignment roster with readiness hooks still waits for those checks. The profile identity and digest cover both capabilities and hooks.

  Subscriptions records `profileRef` and `version` on the activation attempt and includes both in preparation. Purchases covered by ENT-D-22 retain their pre-purchase selection through retries and successor attempts. Version follows SUB-D-85: 1 to 128 visible ASCII characters. An unknown profile or unsupported schema makes the manifest unbuildable (ENT-D-09). Preparation duration and deadline belong to Subscriptions, not this profile (ENT-D-24).
- **Limits**: Profile management APIs are outside v1. ENT-Q13 keeps the bounds on held preparation count and age open.
- **Propagated**: DESIGN.md §3.1, §3.3.9; features/02-initial-readiness.md

### ENT-D-22 Qualification of license and resource profiles before sale

- **Where**: [UPSTREAM_REQS.md](UPSTREAM_REQS.md) §2.5 and [DESIGN.md](DESIGN.md) §3.4.
- **Problem**: An offer must not be sold when no qualified adapter can serve its required license or resource profile. Pricing needs one source for that decision.
- **Decision**: Products owns license and resource profile qualification and sale availability, as it does for quotas. Qualification records pin adapter and contract versions, capability profile/version/digest and evidence references. Availability for new sales is separate and audited. Grant definitions reference a qualified license profile. For service fulfillment, Products qualifies the mapping from versioned SKUs to an adapter-owned service-type descriptor and exact profile. Pricing freezes that mapping and its resolved parameters/policies in an accepted service binding (feature 05). The descriptor supplies execution identity; Products adds no Service entity. The adapter maps it to native OSS offering and instance identities.

  Pricing checks enabled qualification at publication and purchase/change acceptance. It freezes the activation requirement in the receipt. Orders then asks Subscriptions to bind compatible effective activation Settings to that exact acquisition and acceptance before purchase commitment or capture. Subscriptions owns the durable mode/policy/profile snapshot; activation consumes it instead of reselecting from live Settings. The clock still starts at activation admission. Feature 07 defines the owner SDK methods, evidence and recovery. E-U03/E-U04/E-U06/E-U10 gate adoption. Entitlements provides no support-query API.

  Disabling qualification blocks new publication and sales. Accepted purchases keep their terms and obligations. Missing fulfillment remains pending or unknown with findings and recovery under ENT-D-13 and ENT-D-14.
- **Limits**: E-U03 and E-U04 require Products and Pricing adoption. Adapter owners supply evidence under E-U02 and E-U05.
- **Propagated**: DESIGN.md §3.4, §4.1; UPSTREAM_REQS.md §2.5

### ENT-D-23 Fulfillment outputs

**Release: v2 / p2.** Application receipts and provider-confirmed access remain v1; Entitlements output delivery is deferred.

- **Where**: [DESIGN.md](DESIGN.md) §3.3.7, §3.3.8 and §4.3.
- **Problem**: Portals need safe output metadata, recoverable content retrieval and a clear delivery state.
- **Decision**: *Kinds and metadata.* `gts.cf.bss.entitlements.fulfillment_output.v1~` has three kinds: license identifier, license document or key, and service endpoint. Metadata is closed for each kind. Identifier fields are `displayName`, `licenseId`, `validFrom` and `validUntil`. Document fields are `displayName`, `fileName`, `mediaType` and `sizeBytes`. Endpoint fields are `displayName`, `url`, `protocol` and `region`. Text fields have at most 255 characters. `licenseId` is display-safe, never a key. URLs contain no credentials or signed query. Unknown properties quarantine the output with a finding. A derived type may add descriptive properties, never secrets.

  *Content.* The provider behind the saved adapter holds protected content. Entitlements stores only an opaque reference and retrieves through that binding. If a provider issues content only once, the adapter retains it in a resource-tenant platform store: File Storage for documents and the credential store for keys. Entitlements reads neither store.

  *Retrieval.* The profile chooses `stream` or `link`. Streaming checks authority on every request. A link serves one output version for the configured 1 to 900 seconds. Issued links cannot be withdrawn early, so immediate-cutoff profiles use streaming. Only available outputs may issue links.

  *Delivery state.* A version starts `pending`. It becomes `available` when content is retrievable or `unavailable` when the adapter reports a loss. Ordered observations may move between these two states. Final states are `superseded` by a newer current version, `revoked` by source revocation or generation closure without activation, and `expired` at the end of validity. Restriction blocks retrieval unless the profile allows it; it does not change delivery state. Metadata state does not prove physical license revocation.

  *Surface.* `list_output_descriptors` and `get_output_descriptor` map to `GET /outputs` and `GET /outputs/{id}` under `output_read`. `retrieve_output` maps to `POST /outputs/{id}/retrieve` under the separate `output_retrieve` permission. The resource tenant owns the output. Each retrieval creates an audit record. Consumers read outputs. Configured hooks receive safe post-commit output notices under ENT-D-25; the gear emits no broker output event.
- **Limits**: E-U02 and E-U05 require recoverable retrieval, stable content per version and cutoff evidence. Authorization and security owners confirm permissions and link bounds under ENT-Q29.
- **Propagated**: DESIGN.md §3.3.7, §3.3.8, §4.3; features/04-license-grants.md; features/05-resource-execution.md; PRD.md §9.1

### ENT-D-24 Held readiness between Subscriptions and Entitlements

- **Where**: [DESIGN.md](DESIGN.md) §3.3.2 and [initial readiness](features/02-initial-readiness.md).
- **Problem**: Both gears must interpret held readiness and a lost activation resolution consistently. One owner and clock must control the attempt deadline.
- **Decision**: *Commitment.* Entitlements creates one commitment per generation in the transaction that first marks aggregate readiness ready. `readinessCommitment` carries its ID and binding reference. The binding covers ordering tenant, activation operation, preparation revision/digest, manifest identity/version/digest and readiness profile. A later pending or failed result does not end the commitment.

  *Resolution.* Subscriptions delivers `activationResolution` in an applied or withdrawal posture and retains the same immutable result for its authorized operation recovery read. It identifies the operation, generation, preparation revision/digest, decision version and outcome. `applied` adds T, manifest binding and commitment. `closed_without_activation` adds the reason and any bound manifest. Its reason is diagnostic: preserve an unfamiliar owner-valid code and process the verified closure. Unknown outcomes, invalid authority and mismatched bindings remain rejected. The reason cannot add cleanup authority or prove settlement. Later lifecycle documents identify the applied activation through `resolutionRef`.

  Store one resolution per ordering tenant, activation operation and preparation revision. Identical content is a replay. Conflicting content applies nothing and opens a finding.

  *Recovery.* A newer lifecycle revision may supersede an undelivered resolution posture (SUB-D-106). Entitlements uses `resolutionRef` or its saved preparation identity to read the exact operation result from Subscriptions. It retries unresolved reads with backoff under its service identity and the ordering tenant scope. Resolution has its own generation fence: a valid historical result can settle that generation even when its posture is stale.

  It cannot replace the desired head or restore access after a later restriction or cancellation. Missing, pending or unreachable results leave the obligation open. Silence, subscription status and timers do not prove resolution. Before deadline escalation, read fresh authorized operation progress. If it confirms the attempt is unpaused and its current deadline plus the saved resolution recovery horizon has passed, open `resolution_overdue`.

  The horizon is the maximum of the core policy and applicable capability horizons under feature 06. Paused or unknown progress cannot prove overdue status. Recovery and held-age monitoring continue. Close an obsolete finding after a verified pause, updated deadline or confirmed resolution. A finding never authorizes release.

  *Deadline.* Subscriptions owns the preparation duration and deadline: 1 second to 30 days, frozen on the attempt (SUB-D-85). Posture carries the start and deadline as evidence. A pause stops Subscriptions' clock (SUB-D-59). Entitlements uses its clock only for the overdue finding. The readiness profile contains no duration.
- **Limits**: ENT-Q11 requires each adapter to prove retained preparation, emergency behavior and races with late preparation. ENT-Q12 covers cleanup mapping and E-AC35/E-AC36. ENT-Q13 covers limits on held preparation count and age.
- **Propagated**: DESIGN.md §3.3.2; features/02-initial-readiness.md; UPSTREAM_REQS.md §2.5; PRD.md §15

### ENT-D-25 Typed hooks through ToolKit plugins

**Release: v2 / p2.** V1 profiles do not depend on hooks.

- **Where**: [DESIGN §3.3.10](DESIGN.md#3310-extension-hooks), [ADR 0002](ADR/0002-hook-plugins.md) and [feature 07](features/07-extension-hooks.md).
- **Problem**: Services need custom readiness checks and post-delivery actions without adding service-specific code to the Entitlements host.
- **Decision**: Add `readiness_check`, `after_assignment_outcome` and `after_output_state_changed`. Entitlements owns their GTS inputs/results and invocation conditions. Deployment-selected ToolKit hook plugins implement the typed SDK. The host reuses the plugin registration, scoped-client and outbox pattern from Subscriptions, with its own payloads and authority rules. Configured hooks do not require Serverless; services without bindings need no hook plugin.

  Freeze every binding in the profile and manifest or assignment before dispatch. Hooks require the `entitlements` activation profile; later assignments inherit the applied generation's saved profile (ENT-D-10). Pin the plugin instance, exact implementation version, code/configuration digest and approved execution policy. New code requires a new qualified profile version. Existing work cannot follow `latest`, a version range or a changed registry target.

  Plugins use their own restricted credentials for downstream calls. In-process code is trusted and qualified; this interface is not a customer-code sandbox. Readiness checks are read-only and return `passed` or `not_ready`. Their evidence is separate from adapter observations and must match the current prerequisite epoch and digest. All configured checks are required. Remote binding rejection or unavailability stays pending with a finding, never a definitive manifest failure. They cannot hide adapter failures, cancel a purchase or lift a security block. Subscriptions owns activation, its deadline and cleanup authority. Checks never run on restriction, revocation or cleanup.

  Save post-commit notices with the source change. Their handlers may perform approved external actions. They cannot provide access required for initial readiness. Monitor completion and expose failed or unknown execution separately from source state. Include only safe metadata and scoped correlation, never protected content or caller credentials.

  The hook plugin owns execution retries within an admitted invocation. Entitlements owns separately bounded start delivery, result reads and readiness rechecks. Check budgets count admitted invocations, including failures and cancellation after admission. Proven non-admission consumes no check. Reserve a slot before dispatch; confirmed admission consumes it once, and proven `not_started` releases it once. Unknown starts keep the reservation, original key and binding. `checkNumber` identifies attempts rather than consumed checks. No replacement execution hides uncertainty. Consumers must deduplicate notice effects. Exactly-once effects are not promised.
- **Limits**: E-U09 requires pinned implementations, limited identity, deadlines and recoverable results from the selected plugin. No best-effort fallback enables hooks before that evidence exists. Tenant-owned code, definitive hook vetoes, binding migration and manual replay remain outside the v2 hook scope.
- **Propagated**: PRD.md §6.7; DESIGN.md §3.3.10; DECOMPOSITION.md; features/01-posture-intake.md; features/02-initial-readiness.md; features/04-license-grants.md; features/05-resource-execution.md; features/06-reconciliation-recovery.md; features/07-extension-hooks.md; UPSTREAM_REQS.md §2.6; ADR/0002-hook-plugins.md

## Open questions

The v1 and v2 BSS contract details are specified in the [decomposition contract index](DECOMPOSITION.md#contract-specification-ownership). ENT-Q01 to ENT-Q15 require owner adoption, concrete profile values or runtime evidence. ENT-Q29 requires platform/security approval and scale proof. ENT-Q30 tracks implementation and proof of the typed hook plugin contract. ENT-Q27 is future scope. These entries do not reopen accepted architecture or postpone SDK/result definitions.

| ID | Question | Owners | Blocks |
| --- | --- | --- | --- |
| ENT-Q01 | Products and Pricing adoption of the registered catalog, exact units, subject types, immutable policies and frozen rule evidence in the Commercial Quotas specification. | Pricing, Products, Entitlements | quota profile |
| ENT-Q02 | Pricing and Orders adoption of fixed/per-plan-unit rules, exact quantity mapping and supported increase contracts. V1 has no independent component-unit or numerical-decrease profile. | Pricing, Orders, Entitlements | quota profile |
| ENT-Q03 | Adapter evidence that independent restrictions are reported as discrepancies without changing accepted caps. Confirm scope, observation ordering and emergency/held-readiness behavior. BSS stores no live usage projection. | Quota adapter, Products, Entitlements, Subscriptions | quota profile |
| ENT-Q04 | Adapter application/read/recovery contract, source authority and permissions. Prove accepted/applied heads, stale-write protection, refusal replay and recovery of partial/unknown work. A single OSS transaction is not required. | Quota adapter, Entitlements | quota profile |
| ENT-Q05 | Adapter evidence for restriction, revocation and resume without restoring prohibited access or granting a second allowance. OSS accounting stays behind the adapter. Numerical decreases are outside v1. | Quota adapter, Products, Entitlements | quota profile |
| ENT-Q06 | Adapter mapping of accepted validity, effective boundaries and authoritative time. Renewal must not create a new allowance. Counter/window algorithms belong to OSS; consumption profiles are future scope. | Quota adapter, Entitlements | quota profile |
| ENT-Q07 | Owner adoption of supported continuation and lot rules, expiry handling, terminal source protection and recovery. Incompatible lot combinations and numerical decreases are refused in v1. | Pricing, Quota adapter, Entitlements | quota profile |
| ENT-Q08 | Versioned adapter/profile qualification, executed evidence and Products availability. Metadata or an approved design cannot substitute for implementation proof. Regional/pool profiles need their own scope evidence. | Quota adapter, Products, Entitlements | quota profile |
| ENT-Q09 | Adapter readiness feed and recovery after missed events, restart or retention gaps. Prove authoritative assignment state and sequence through events, polling or another reliable feed. | Entitlements, selected adapter | initial readiness profile |
| ENT-Q10 | Adapter mapping to `scopeId`, `assignmentId` and `desiredRevision`. Distinct environments must stay distinct, and retired identities must not be reused. | Entitlements, selected adapter | initial readiness profile |
| ENT-Q11 | Prove retained preparation while activation is undecided, emergency security behavior and races between resolution and late preparation (SUB-D-57, ENT-D-24). This is not an uptime guarantee. | Entitlements, selected adapter | initial readiness profile |
| ENT-Q12 | Prove cleanup after closure and safe predecessor effects before a successor (ENT-D-08, ENT-D-24). Orders must define remediation of the retained purchase/payment. Execute E-AC35 and E-AC36. | Entitlements, Subscriptions, Orders, selected adapter | initial readiness profile |
| ENT-Q13 | Authorized pre-T or never-activated preparation is platform-borne (SUB-D-57). Confirm time, capability, usage and admission bounds, financial authority and usage-to-T accounting. Include held-preparation count/age limits during pauses and the Orders-authorized action at a limit (S-U04). | Entitlements, Subscriptions, Rating, Billing | initial readiness profile |
| ENT-Q15 | Adapter confirmation of applied quota limits during authorized preparation and continuation of the same assignment at activation. Resource readiness and safe reuse remain separate adapter confirmations. OSS owns allocation, counters and releases; Entitlements receives no per-resource quota events. | Entitlements, Subscriptions, quota and resource adapters | allocation-dependent initial readiness |
| ENT-Q27 | Future `quota_controlled` funding: offer-to-quota identity, block/degrade behavior and acceptable lag/overshoot. Accounts and the selected OSS owners must agree on the consumption contract. V1 provides none and cannot qualify these offers. | Entitlements, Accounts, Subscriptions, OSS owners, Products | sponsored usage under `quota_controlled`; future consumption profile |
| ENT-Q29 | Authorization owner confirms the permission inventory. Security owner confirms output retrieval and link lifetime. Verify operation at 10,001 tenants toward the 100,000-tenant target (ENT-D-04, ENT-D-18, ENT-D-23). | Entitlements, platform authorization, security | every command, worker and read |
| ENT-Q30 | Hook plugin and security owners qualify pinned implementations, restricted downstream credentials, bounded admission/execution, stable replay and typed result recovery without Serverless. Prove E-AC50 to E-AC58 and E-U09 for each enabled hook profile. | Hook plugin owners, platform security, Entitlements | configured hooks only |

ENT-Q01 to ENT-Q08 track adoption and implementation evidence for the chosen quota contracts. They do not reopen the ownership or v1 scope decisions. Quota gates G01 to G08 hold that evidence (ENT-D-11). ENT-Q09 to ENT-Q13 and ENT-Q15 close per adapter profile. ENT-D-24 defines the Subscriptions handoff; a cached readiness notification cannot replace its commitment and resolution contract.

The alignment questions ENT-Q27 and ENT-Q29 hold what ENT-D-04 and ENT-D-05 leave open against Accounts and the commercial model.
