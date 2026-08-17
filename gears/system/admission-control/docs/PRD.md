# PRD — Admission Control

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
  - [3.1 Gear-Specific Environment Constraints](#31-gear-specific-environment-constraints)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 The Admission Decision](#51-the-admission-decision)
  - [5.2 Built-in Policies](#52-built-in-policies)
  - [5.3 Engine Selection](#53-engine-selection)
  - [5.4 Failure Semantics](#54-failure-semantics)
  - [5.5 Observability](#55-observability)
  - [5.6 Configuration](#56-configuration)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

Admission Control is the platform's gate on management operations. A gear about to perform an operation asks it one question — may this proceed — and receives one answer. The gate answers by applying the platform's own rules and by consulting the policy engine the deployment has selected, and it never modifies the operation it is asked about.

The gear is deliberately small. It owns no policy content, offers no authoring surface, and holds no lifecycle: policy content and its management belong to the policy engine behind it, which is replaceable. What the gate owns is the question, the answer, the failure semantics, and the built-in policies that hold regardless of which engine is selected.

### 1.2 Background / Problem Statement

Management gears each decide for themselves whether an operation may proceed. [Infrastructure Resource Manager](../../../infrastructure-resource-manager/docs/PRD.md) states the consequence in its own requirements: enforcement across the estate was inconsistent and left audit gaps, which is why it now demands that every operation be policy-gated, and why it declares an abstract Policy Decision Service actor rather than a concrete one.

Two things are missing between that demand and anything that could satisfy it. There is no single interception point a gear can call — so every gear that wants gating invents its own, and the platform cannot say what is gated or how. And there is nowhere to put rules the platform enforces for everyone: rules that must hold whichever policy engine a deployment selected, and that no tenant administrator can withdraw.

The [Policy Engine](../../policy-engine/docs/PRD.md) supplies decisions but is not that interception point. It is one selectable engine, it evaluates tenant-authored content, and a deployment may replace it. A built-in policy stored inside it leaves with it. This gear is the seam that stays.

### 1.3 Goals (Business Outcomes)

Milestones refer to this document's own priority tiers — "p1 complete" means every `p1` requirement in Sections 5 and 6 is met and verified.

| Outcome | Baseline | Target | By |
|---|---|---|---|
| A gear reaches policy through one platform interface rather than its own check | Each gear invents its own policy check, or omits one | One interface, exercised for the policy question on every operation an enforcing gear declares as gated | p1 complete |
| The platform can state rules that hold whichever engine is selected | Built-in policies can only live inside a replaceable engine | Built-in policies are applied on every gated operation and survive substitution of the engine | p1 complete |
| An operation is never admitted because a check could not run | No defined behaviour; each gear improvises | Zero admissions across the complete set of injected failure conditions | p1 complete |
| What the platform gates is knowable | No record of which operations were gated or how they were decided | Every admission decision is recorded with its cause and the identity of what decided it | p1 complete |
| Replacing the policy engine does not disturb the gears that call the gate | Not applicable — no gate, no engine contract | The calling interface is unchanged by substitution of the engine behind it | p2 complete |

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Admission | The act of permitting or refusing a management operation before it takes effect. |
| Gate | This gear. The single point at which the platform decides whether a management operation may proceed. It issues the verdict; it does not carry it out. |
| Enforcing Gear | A management gear that calls the gate before performing an operation and enforces the answer. The policy enforcement point: the only component that can actually stop the operation. Infrastructure Resource Manager first among them. |
| Policy Engine | The pluggable component that evaluates tenant-authored policy and returns a decision. Exactly one is selected per deployment. |
| Built-in Policy | A rule this gear applies itself, supplied by deployment configuration, enforced for every tenant, and independent of which policy engine is selected. |
| Verdict | The gate's answer to an enforcing gear: admitted or refused, with a third value reserved for a deferral by `cpt-cf-admission-control-fr-deferral-relay` and served once `cpt-cf-admission-control-fr-deferral-verdict` ships. |
| Deferral | An engine outcome that neither permits nor prohibits, but holds the operation for human approval. The gate relays it; it never routes it to an approver and never resolves one. |
| Governed / Ungoverned | The cause the policy engine attaches to a permission, stating whether policy considered the operation or was silent about it. The gate records it and does not branch on it. |
| PDP / PEP / PAP | Policy Decision Point, Policy Enforcement Point, Policy Administration Point. This gear is a decision point: it decides its built-in policies itself and delegates every tenant-authored question to the selected engine, which is the decision and administration point for that content. The **enforcing gear is the PEP** — it is the only component able to stop the operation, and Section 11 records that this gear cannot detect one that asks and then proceeds regardless. The gear's name follows the established term for this component in comparable systems and is not a claim to be the enforcement point. |
| GTS | Global Type System. Provides the identifiers used for resource types, plugin instances, and error codes. |

## 2. Actors

> **Note**: Stakeholder needs are managed at project/task level by steering committee. Documented below are the actors that interact with this gear.

### 2.1 Human Actors

#### Platform Operator

**ID**: `cpt-cf-admission-control-actor-platform-operator`

- **Role**: Configures the gate: which policy engine is selected, what built-in policies apply, and what bounds the gate enforces on the engine. Responds to admission latency and availability incidents.
- **Needs**: Configuration validated at startup rather than discovered at first traffic; visibility into what is being refused and by what; predictable behaviour when the engine is unavailable.

#### Security Auditor

**ID**: `cpt-cf-admission-control-actor-security-auditor`

- **Role**: Reviews after the fact which operations were gated, which were refused, and whether the gate was ever bypassed or degraded.
- **Needs**: A record for every gated operation, including those refused because a check could not run, distinguishable from those refused by a rule.

### 2.2 System Actors

#### Enforcing Gear

**ID**: `cpt-cf-admission-control-actor-enforcing-gear`

- **Role**: A management gear that calls the gate before an operation and enforces the verdict. Supplies the operation context; receives admitted or refused. Does not reach the policy engine directly.

#### Policy Engine

**ID**: `cpt-cf-admission-control-actor-policy-engine`

- **Role**: The selected engine. Receives an evaluation request from the gate, returns a permission or a prohibition, and is the only component that evaluates tenant-authored policy. Exactly one is active per deployment, and it is replaceable.

#### Types Registry

**ID**: `cpt-cf-admission-control-actor-types-registry`

- **Role**: Resolves the GTS identifier naming the selected policy engine, and the resource-type identifiers built-in policies and requests refer to.

#### Admission Record Sink

**ID**: `cpt-cf-admission-control-actor-audit-sink`

- **Role**: Receives the admission records this gear emits, for retention, export, and analysis. The sink is `event-broker`: records are published to a platform-scoped audit topic whose bound storage backend owns retention, compaction, and deletion. `policy-engine` publishes its decision records to the same topic, so one stream carries both what was gated and how policy decided it. Because this gear owns no database, the topic is where its records become durable rather than a copy of state it already holds — a refusal by built-in policy and a could-not-run refusal exist nowhere else.

## 3. Operational Concept & Environment

> Project-wide runtime, operating system, architecture, lifecycle policy, and integration patterns are defined at the repository level in [ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md) and the foundational [guidelines/](../../../../guidelines/). This gear has no parent gear PRD. Only gear-specific constraints appear below.

### 3.1 Gear-Specific Environment Constraints

- The gate is on the path of every operation an enforcing gear declares as gated. Its latency is added to that gear's, and because it fails closed, its unavailability presents as refusal of those operations rather than as a degraded feature.
- The gate composes in-process, the platform's default execution mode: it, its enforcing gears, and the selected policy engine share one runtime and communicate through typed clients. Every `p1` requirement here is written for that shape.
- Running the gate as its own service over the platform's remote transport is a separate capability, required at `p3` by `cpt-cf-admission-control-fr-remote-decision-surface`. The contracts do not change, because they are transport-agnostic; what changes is that the decision surface acquires a network projection, the latency figures in Section 6 stop applying, and unreadiness becomes a safe signal because removing the gate from rotation no longer removes the gears it gates.
- The operational surface of `cpt-cf-admission-control-fr-operational-surface` is present regardless of which of the two the deployment composes.
- The gate holds no persistent state of its own beyond its records. Built-in policies arrive as deployment configuration and are versioned with the deployment, not through an API.
- The gate is one step inside an enforcing gear's own admission sequence, not a replacement for it. Infrastructure Resource Manager's pipeline may enrich a request with defaults before or after calling the gate; the gate itself never modifies anything.

## 4. Scope

### 4.1 In Scope

- A single admission interface: one operation in, admitted or refused out.
- Selection of exactly one policy engine per deployment, and the contract that engine implements.
- Built-in policies: their form, their application, and their precedence over the engine's answer.
- Failure semantics: what happens when the engine is unavailable, times out, or errors.
- Batch admission, where an enforcing gear needs one verdict over a change touching several resource types.
- Admission records covering every gated operation, and the metrics over them.
- Bounds the gate places on the engine, and back-off behaviour when the engine asks for it.
- Configuration and its validation.

### 4.2 Out of Scope

- Policy content, its authoring, its lifecycle, its versioning, and its assignment to tenants. These belong to the policy engine, and this gear neither stores nor manages them.
- Evaluation of tenant-authored policy. The gate evaluates its own built-in policies and delegates every tenant-authored question to the engine.
- Modification of the operation being admitted. The gate is a judgement, never a rewrite: it produces no defaults, no injected fields, and no substitutions. Generating and mutating policy, in the sense Kyverno uses those terms, is excluded at every priority tier rather than deferred to one.
- The enforcing gear's own admission sequence, including any enrichment it performs. The gate is one step within it.
- Authorization: whether a subject may act on a resource at all. That is a different question, owned by `authz-resolver`, and reaching the gate does not answer it.
- Quota and allowance state, owned by `quota-enforcement`, together with the advisory output that arises there. Consumption diagnostics come from that gear's own read surfaces, and threshold-crossing warnings are events it emits to sinks registered with it rather than values returned on a verdict. Neither travels with a policy decision, and this gate does not sit on the quota path at all: an enforcing gear calls quota itself, and Infrastructure Resource Manager orders that call before policy. Obligations differ: they arise from the policy evaluation this gate does mediate, and `cpt-cf-admission-control-fr-admission-interface` requires them relayed.
- Combining verdicts from several policy engines. Exactly one engine is selected per deployment.
- Approval workflow for a deferred operation: requesting an approval, notifying an approver, storing a pending operation, resolving it, or expiring it. The gate relays a deferral per `cpt-cf-admission-control-fr-deferral-relay` and nothing more. Where a deferral terminates is an open question in Section 13 and in the engine's own requirements, and it belongs to an approval service that does not exist.

## 5. Functional Requirements

> **Testing strategy**: All requirements are verified via automated tests targeting 90 percent or greater code coverage unless otherwise specified. Verification method is documented only where a non-test approach applies.

### 5.1 The Admission Decision

#### Single Admission Interface

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-admission-interface`

The system **MUST** expose one operation by which an enforcing gear submits an intended operation and receives a verdict of admitted or refused. The verdict **MUST** be the whole of the answer: the system **MUST NOT** return a modified request or a partial result. An admitted verdict **MUST** carry, unaltered, any obligations the selected engine attached to its permission; the system **MUST NOT** interpret, validate, add to, or drop them.

- **Rationale**: One interface is the point of the gear. A gate that answers in more than one shape becomes a component each caller integrates differently, which is the condition the platform is trying to leave. Refusing to return modifications is what keeps the caller's own sequence in the caller's hands. Obligations are the one thing that travels through rather than stopping here: they are the engine's instruction to the enforcing gear, not the gate's, and Infrastructure Resource Manager requires them delivered unaltered. A gate that dropped them would silently convert a conditional permission into an unconditional one.
- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`

#### Decision Order

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-decision-order`

The system **MUST** apply built-in policies before consulting the policy engine, and **MUST** refuse without consulting the engine where a built-in policy refuses. Where no built-in policy refuses, the system **MUST** consult the engine and **MUST** admit only where the engine permits.

- **Rationale**: Built-in policies are not overridable by tenant policy, so an engine that permits cannot rescue an operation a built-in policy refuses — and consulting it anyway spends the latency budget on an answer that cannot change the verdict. Ordering the cheap, local, non-overridable check first is both correct and the only ordering that saves work.
- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`, `cpt-cf-admission-control-actor-policy-engine`

#### Refusal Cause

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-refusal-cause`

Every refusal **MUST** carry a machine-readable cause drawn from the platform's error type system, distinguishing at minimum: refused by a built-in policy, refused by policy, refused because the operation awaits approval per `cpt-cf-admission-control-fr-deferral-relay`, and refused because a check could not run. A refusal by a built-in policy **MUST** identify the rule; a refusal by policy **MUST** carry the reason the engine supplied.

- **Rationale**: The causes demand different responses. A platform-rule refusal is permanent until configuration changes, a policy refusal is a tenant's own rule and may be corrected by editing content, an awaiting-approval refusal is neither wrong nor broken and clears when a person acts, and a could-not-run refusal is an incident and is retryable. A caller that cannot tell them apart either retries what will never succeed or gives up on what would.
- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`, `cpt-cf-admission-control-actor-platform-operator`

#### Deferral Relay

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-deferral-relay`

The policy engine plugin contract **MUST** carry a deferral alongside a permission and a prohibition from its first version, whether or not any selected engine emits one, and the verdict **MUST** reserve the corresponding third value unpopulated.

Until `cpt-cf-admission-control-fr-deferral-verdict` ships, the system **MUST** map an engine deferral to a refusal carrying the awaiting-approval cause of `cpt-cf-admission-control-fr-refusal-cause`. It **MUST NOT** map a deferral to the could-not-run cause, to a refusal by policy, or to an admission. A deferral is a mapped engine result, so it is not the unmappable result enumerated in the threshold of `cpt-cf-admission-control-nfr-fail-closed`, and the fault-injection set **MUST NOT** treat it as one.

- **Rationale**: The engine can already produce a third outcome — `cpt-cf-policy-engine-fr-deferral-outcome` makes the result three-valued — while this gear's verdict is two-valued, and the gate's own rule for a result it cannot map is to refuse with the could-not-run cause. Composed, those two turn "a person must approve this" into "the system is broken, retry", which is the precise conflation the engine's own requirement exists to prevent: a caller reading could-not-run retries a decision that no retry can change, and the approval it is waiting for is never requested because nothing told the caller one was needed. Carrying the deferral in the plugin contract from the first version is the same discipline the obligation collection already follows in `cpt-cf-admission-control-interface-engine-plugin` — a shape every engine implements is one that cannot be widened later without breaking all of them — and it is cheaper here than there, because the engine that will emit deferrals is specified today. Mapping it to a distinguishable refusal in the meantime is the honest interim: the operation genuinely does not proceed, so refusing is correct, and a caller that only understands refusals is still safe. What the distinct cause buys is that the refusal is not mistaken for an outage in the metrics of `cpt-cf-admission-control-fr-metrics`, and that a caller able to raise an approval knows to.
- **Actors**: `cpt-cf-admission-control-actor-policy-engine`, `cpt-cf-admission-control-actor-enforcing-gear`, `cpt-cf-admission-control-actor-platform-operator`

#### Batch Admission

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-batch-admission`

The system **MUST** accept several intended operations as one batch and **MUST** return one verdict for the batch alongside the individual verdicts, such that a refusal of any member refuses the batch. The response **MUST** identify every refused member rather than only the first. Where a member defers, `cpt-cf-admission-control-fr-deferral-verdict` governs how it combines; until that requirement ships a deferral is a refusal and combines as one.

- **Rationale**: An enforcing gear admitting a plan needs one answer for the plan, and needs it to match what a preview of the same plan returned. Per-member calls multiply the fixed cost of an admission across the plan and leave the caller to combine partial answers, which is where two callers begin combining them differently.
- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`

#### Three-Valued Verdict

- [ ] `p3` - **ID**: `cpt-cf-admission-control-fr-deferral-verdict`

The system **MUST** serve the third verdict value reserved by `cpt-cf-admission-control-fr-deferral-relay`, returning a deferral distinguishably from both an admission and a refusal, and **MUST NOT** admit an operation it defers. In a batch, a deferral **MUST** combine as follows: any refused member refuses the batch; otherwise any deferred member defers it; otherwise the batch is admitted. The system **MUST NOT** route the deferral to an approver, request an approval, or hold the operation open awaiting one.

Serving this value changes what a caller can receive from a stable interface, so it **MUST** be a major version of `cpt-cf-admission-control-interface-admission-client`, and the tier of this requirement **MUST** track `cpt-cf-policy-engine-fr-deferral-outcome` — there is nothing to serve while no engine emits a deferral.

- **Rationale**: A deferral is not a refusal: the operation proceeds once someone approves it, and a caller told "refused" has no reason to wait for that. Serving it as its own value is what lets an enforcing gear hold the request rather than abandon it. The combination rule mirrors the engine's, so a batch cannot be admitted whole while one member waits on a person, and prohibition still absorbs — a deferral cannot rescue an operation policy prohibits. Withholding the routing is the boundary: an approval flow needs a store, a lifetime, a notification path, and an actor who answers, none of which this gear has, and a gate that held operations open would acquire per-request state it is built without. This is `p3` for the same reason the engine's outcome is — nothing in the tree routes a deferral to an approver, so the value would be served to no one.
- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`, `cpt-cf-admission-control-actor-policy-engine`

#### Remote Decision Surface

- [ ] `p3` - **ID**: `cpt-cf-admission-control-fr-remote-decision-surface`

The system **MUST** be able to serve its admission decision over the platform's remote transport, so that a deployment can run the gate as a service separate from the gears it gates. The semantics **MUST NOT** differ from the in-process surface: the same verdicts, the same causes, the same failure behaviour, and the same refusal to modify a request.

- **Rationale**: A deployment may want the gate isolated for scaling, for separate securing, or for reach from something that cannot link it. Nothing in the design prevents this — the contracts carry no in-process assumption — but a capability nothing requires is a capability nobody verifies, and the difference between "possible" and "supported" is a conformance test. It is `p3` because no consumer has asked for it and because the latency question it raises has no answer yet: two transport round trips, one into the gate and one onward to the engine, against a consumer budget that reserves nothing for either.
- **Actors**: `cpt-cf-admission-control-actor-platform-operator`, `cpt-cf-admission-control-actor-enforcing-gear`

### 5.2 Built-in Policies

#### Built-in Policy Form

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-builtin-policy-form`

The system **MUST** apply built-in policies supplied as deployment configuration, and **MUST** evaluate them itself through the platform's shared evaluation facility rather than delegating them to the selected policy engine. Their content is policy content in the language a backend of that facility accepts; this gear defines no language of its own.

A built-in policy **MUST** yield either a prohibition or nothing. It **MUST NOT** yield a permission: an outcome that is not a prohibition leaves the decision to the selected engine, per `cpt-cf-admission-control-fr-decision-order`.

The system **MUST NOT** offer an API by which built-in policies are created, modified, or withdrawn.

- **Rationale**: Built-in policies are the platform's own, and they need the same expressive power as anything a tenant could write — a platform invariant that cannot state a condition is not much of an invariant. Evaluating them here rather than in the engine is what makes them survive substitution of that engine, which is the whole reason they are not simply content assigned at the root tenant. Confining them to prohibition keeps the precedence story sound: a built-in that could permit would let deployment configuration overrule a tenant's own rules in the permissive direction, which is the one direction a platform guardrail should never travel. Withholding a management API is not an omission but the boundary itself: the moment built-in policies become authorable at runtime, this gear is a second policy engine.
- **Actors**: `cpt-cf-admission-control-actor-platform-operator`

#### Built-in Policies Are Not Overridable

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-builtin-policy-precedence`

A refusal by a built-in policy **MUST NOT** be overridable by policy content, by a tenant administrator, or by any request parameter. Built-in policies **MUST** apply to every tenant and to every enforcing gear identically.

- **Rationale**: A rule a tenant can withdraw is a tenant's rule. The reason for keeping these outside the policy engine is that the engine is replaceable and its content is administrable; a built-in policy that inherited either property would not be a built-in policy. This is also the requirement that makes the gear's existence load-bearing rather than a convenience: without it, everything here could live in the engine.
- **Actors**: `cpt-cf-admission-control-actor-platform-operator`, `cpt-cf-admission-control-actor-security-auditor`

#### Built-in Policies Survive Engine Substitution

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-builtin-policy-independence`

The system **MUST** apply built-in policies identically whichever policy engine is selected, and **MUST** continue to apply them when no engine is selected at all.

- **Rationale**: This is what "built-in" means, and it is the one property that could not be obtained by assigning a bundle at the root tenant of the policy engine. A deployment that substitutes a vendor engine, or that runs before any engine is configured, still gets the platform's own rules.
- **Actors**: `cpt-cf-admission-control-actor-platform-operator`

#### Built-in Evaluation Bounds

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-builtin-evaluation-bounds`

The system **MUST NOT** pass any capability into the evaluation of a built-in policy: the backend receives the request context, the policy content, and an evaluation timestamp the gate supplies, and nothing else. The system **MUST** bound the wall-clock time spent evaluating a single built-in policy and the time spent on all of them for one admission, **MUST** make both bounds operator-configurable, and **MUST** treat an exceeded bound as a failure that refuses per `cpt-cf-admission-control-fr-fail-closed`. Where the backend's bound is checked cooperatively between units of work rather than preemptively, the configured figure is an upper bound plus one such unit, and `cpt-cf-admission-control-nfr-overhead` is measured against the achieved figure rather than the configured one.

A built-in policy **MUST NOT** reference a non-deterministic builtin — a clock reader, a random number or identifier generator — and `cpt-cf-admission-control-fr-configuration-validation` **MUST** reject one that does. The gate supplies the evaluation timestamp precisely so that content need not read a clock, and a built-in policy whose verdict varies between two identical requests is a platform guardrail nobody can reproduce, review, or reason about.

- **Rationale**: Once the gate evaluates rather than matches, it acquires the exposure the policy engine already carries: content that could reach the network would make configuration a remote-execution surface, and content whose cost is unbounded would make one badly written built-in a platform-wide latency problem on the path of every gated operation. The bounds are separate from the engine's because they are spent from a different budget — the gate's own overhead, before the engine is called at all.
- **Actors**: `cpt-cf-admission-control-actor-platform-operator`

### 5.3 Engine Selection

#### Single Engine Selection

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-engine-selection`

The system **MUST** select exactly one policy engine, discovered through the types registry by its GTS identifier, and **MUST** select deterministically where more than one candidate is registered, following the platform's plugin priority convention. The system **MUST NOT** consult more than one engine for a decision.

- **Rationale**: One engine at a time is how this platform composes plugins everywhere else, and it is what keeps the gate free of a verdict-combination model, an ordering model, and per-engine failure policy — none of which any consumer has asked for. Deterministic selection matters because a gate that resolves a different engine on two instances of the same deployment produces different verdicts for the same operation.
- **Actors**: `cpt-cf-admission-control-actor-types-registry`, `cpt-cf-admission-control-actor-policy-engine`

#### Absent Engine

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-absent-engine`

Where no policy engine can be selected, the system **MUST** apply built-in policies and **MUST** refuse every operation those rules do not already refuse, carrying the could-not-run cause. The system **MUST** report the condition on its operational surface as degraded, and **MUST NOT** report it as an unreadiness that removes its host from rotation.

Where a configured engine identifier cannot be resolved at startup, the system **MUST** fail to start. The two conditions are distinct: no engine configured is a deployment that has not finished being set up, and an engine configured but unresolvable is a deployment that is wrong.

- **Rationale**: A deployment with no engine is misconfigured, not permissive, so refusing is correct. Reporting it as unreadiness is not: in the in-process shape the gate shares a host with the gears it gates, so failing readiness would evict their serving surfaces from rotation over a dependency only this gear needs. The platform's guidance on that method says the same — report a dependency outage as degraded and reserve unreadiness for what the gear itself cannot serve through, which this is not, because built-in policies still apply. Separating the unresolvable-identifier case is what keeps a typo from becoming a silently engine-less deployment: applying built-in policies regardless is what `cpt-cf-admission-control-fr-builtin-policy-independence` requires, and it is not licence to run without the engine someone asked for.
- **Actors**: `cpt-cf-admission-control-actor-platform-operator`

#### Engine Result Interpretation

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-engine-result`

The system **MUST** admit where the engine returns a permission and **MUST** refuse where it returns a prohibition, without regard to the cause the engine attaches to a permission. A deferral is neither, and is governed by `cpt-cf-admission-control-fr-deferral-relay`. The system **MUST** record that cause and **MUST NOT** treat a permission whose cause is ungoverned differently from one whose cause is governed.

- **Rationale**: The engine's answer is already the verdict; re-deciding it here would put policy semantics in a gear that owns no policy. The cause is worth recording because it is how an operator learns what share of the estate policy is silent about, but branching on it would mean the gate deciding that silence is refusal — a policy judgement, made in the wrong component, and one that would refuse every operation in a deployment that has authored nothing.
- **Actors**: `cpt-cf-admission-control-actor-policy-engine`, `cpt-cf-admission-control-actor-enforcing-gear`

### 5.4 Failure Semantics

#### Fail Closed on Engine Failure

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-fail-closed`

Where the selected engine is unreachable, exceeds its bound, or returns an error, the system **MUST** refuse the operation with the could-not-run cause. The system **MUST NOT** admit an operation it was unable to have evaluated, and **MUST NOT** expose configuration that converts an engine failure into an admission.

- **Rationale**: An operation admitted because a check failed is an operation nobody checked, and the failure is invisible precisely when it matters. Refusing makes an engine outage a loud, bounded, retryable condition rather than a silent lapse in enforcement. Withholding a bypass switch is deliberate: a documented way to turn enforcement off is a way that gets left on.
- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`, `cpt-cf-admission-control-actor-platform-operator`

#### Engine Call Bound

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-engine-bound`

The system **MUST** bound the time it waits on the engine, **MUST** make the bound operator-configurable with a documented default, and **MUST** treat an exceeded bound as an engine failure.

- **Rationale**: The fail-closed requirement presupposes a bound to exceed. Without one a hung engine stalls the calling gear's request rather than refusing it, turning a bounded outage into an unbounded one and consuming the caller's own budget while it waits.
- **Actors**: `cpt-cf-admission-control-actor-platform-operator`

#### Honour Engine Back-Off

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-engine-backoff`

Where the engine signals that it is refusing transiently and asks callers to back off, the system **MUST** reduce the rate at which it calls that engine for the stated interval, and **MUST** continue to refuse operations during it rather than admitting them.

- **Rationale**: The engine refuses transiently when it cannot record what it decides, and a gate that retried at full rate would amplify load on the storage already failing. Continuing to refuse while backing off is what keeps the back-off from becoming an admission path.
- **Actors**: `cpt-cf-admission-control-actor-policy-engine`

### 5.5 Observability

#### Admission Records

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-admission-records`

The system **MUST** emit a record for every admission decision. Within this gear, this requirement owns the normative field set and no other requirement here restates it; the engine's own decision record is governed by `policy-engine` and is neither restated nor constrained here. Every record **MUST** carry:

- a correlation identifier the system **MUST** mint per gated operation, and the batch identifier where the decision was part of a batch. The system **MUST** supply both to the engine on the evaluation request, and **MUST NOT** derive either from a value the calling gear or its client controls
- the calling enforcing gear
- the subject and the subject's tenant
- the action, the resource type, and the resource identifier where the request named one
- the verdict
- the cause, and for a refusal by built-in policy the rule's identity, for a refusal by policy the engine's reason, and for a could-not-run refusal the failure condition
- for an admission, the permission cause the engine attached, and the identifiers of any obligations it carried
- the identity of the selected engine
- the elapsed time

- **Rationale**: This is how the platform answers what was gated and how it was decided, which is the fourth goal of this document. Recording the engine's identity matters because a deployment can substitute it, and a record that does not say which engine decided cannot be compared across a substitution.

  This gear mints the correlation identifier because it is the only component that sees a whole gated operation: the engine sees one evaluation within it, and the calling gear does not know an evaluation happened. Minting it here and forwarding it is what lets the two records reach the shared audit topic joinable, and a batch joins per member on the correlation identifier and as a group on the batch identifier. It is deliberately minted here rather than taken from a tracing identifier: an audit key has to be present on every record and outside the reach of whoever is being audited, and a value a caller can repeat, omit, or choose is not one an audit trail can be joined on.

  Four fields here also appear on the engine's record — subject, action, resource, and the identifiers above. The duplication is deliberate, not drift: a refusal by built-in policy never reaches the engine, and a could-not-run refusal is by definition one the engine could not record, so this record has to be readable with no counterpart to join to. Every other field is one only this gear holds.
- **Actors**: `cpt-cf-admission-control-actor-security-auditor`, `cpt-cf-admission-control-actor-audit-sink`

#### Operational Surface

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-operational-surface`

The system **MUST** expose a read-only operational surface reporting its readiness and its effective configuration: which policy engine is selected, which built-in policies are loaded with their identities, and the match count for each. The surface **MUST** be read-only — it **MUST NOT** create, modify, or withdraw a built-in policy, or change the selected engine.

The surface **MUST** additionally report the refusals this gear reached without an evaluation — those produced by a built-in policy, and those produced because the engine could not be reached — as counts over a bounded recent window, broken down by built-in policy identity and by failure condition respectively. These **MUST** be reported separately from refusals the engine decided.

- **Rationale**: Every other requirement here produces a decision an operator cannot see the inputs to. A rule that never fires, an engine that resolved to something unexpected, a deployment sitting in the degraded state of `cpt-cf-admission-control-fr-absent-engine` — none is discoverable from the outside without this, and the first two are the failure modes an operator is least able to detect because nothing happens. Keeping it read-only is what stops the surface becoming the rule-management API that `cpt-cf-admission-control-fr-builtin-policy-form` withholds.

  The refusal counts exist because the engine's own refusal projection cannot carry these two classes and says so: a built-in refusal never reaches the engine, and a could-not-run refusal happens because the engine was unreachable. Without a surface here, the two refusal classes the platform most needs to see during an incident are the two that appear nowhere, and an operator reading an empty projection would take an outage for quiet. A match count alone does not answer this — it says a policy fired, not how often it refused or what it refused. Counts rather than a queryable history is the deliberate limit: the durable per-decision evidence is the admission record of `cpt-cf-admission-control-fr-admission-records` on the shared audit topic, and duplicating it into a queryable store here would give this gear the persistent state it is built without.
- **Actors**: `cpt-cf-admission-control-actor-platform-operator`

#### Record Confidentiality

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-record-confidentiality`

Admission records **MUST NOT** contain bearer tokens or other subject credentials, and **MUST NOT** contain the values of caller-supplied operation context. Where a record refers to that context it **MUST** carry only the names of the properties supplied.

- **Rationale**: Admission records are widely readable by design, and the gate has no schema for caller-supplied context — it forwards it rather than interpreting it. A component that cannot classify a value cannot decide whether keeping it is safe, so it keeps the names and not the values.
- **Actors**: `cpt-cf-admission-control-actor-audit-sink`

#### Operational Metrics

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-metrics`

The system **MUST** expose metrics covering admission latency, verdicts by cause, engine latency and failures, back-off periods entered, and the share of admissions whose permission cause was ungoverned.

- **Rationale**: The gate fails closed, so its failures present as refusals rather than as errors, and without a could-not-run counter separated from policy refusals an engine outage is indistinguishable from a tightened rule. The ungoverned share is the platform's measure of how much of the estate policy is silent about, and this gear is where every operation passes, so it is the only place that number is complete.
- **Actors**: `cpt-cf-admission-control-actor-platform-operator`

### 5.6 Configuration

#### Configuration Validation

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-configuration-validation`

The system **MUST** validate its configuration at startup — the engine identifier, every built-in policy, and every bound — **MUST** reject unknown configuration keys, and **MUST** fail to start rather than run with configuration it could not validate. Built-in policies naming a resource type that does not resolve **MUST** fail validation, as **MUST** those referencing a builtin denylisted by `cpt-cf-admission-control-fr-builtin-evaluation-bounds`. Both checks run against the parsed form of the content rather than its source text.

- **Rationale**: Every setting here changes what the platform refuses. A misspelled engine identifier that resolves to nothing, or a built-in policy naming a type that does not exist, is a rule that silently never fires — and a guardrail that silently never fires is the failure an operator is least able to detect, because nothing happens.
- **Actors**: `cpt-cf-admission-control-actor-platform-operator`

## 6. Non-Functional Requirements

> Project-wide baselines for performance, security, reliability, and scalability are defined at the repository level in [ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md) and the foundational [guidelines/](../../../../guidelines/). This gear has no parent gear PRD. Only gear-specific NFRs appear below.

### 6.1 Gear-Specific NFRs

#### Gate Overhead

- [ ] `p1` - **ID**: `cpt-cf-admission-control-nfr-overhead`

The system **MUST** add a bounded overhead to the operation it gates, measured at the gate boundary and excluding the time the engine spends.

- **Threshold**: In the in-process shape, p95 within 5 ms and p99 within 10 ms for a single admission, excluding engine time and excluding record emission, which is asynchronous. Evaluation of built-in policies is inside this figure and is the dominant term in it, which is why `cpt-cf-admission-control-fr-builtin-evaluation-bounds` requires a configurable cap: the figure holds only while the built-in set stays small and its per-policy bound stays well inside the total. This figure does not cover the remote shape of `cpt-cf-admission-control-fr-remote-decision-surface`, which adds a transport round trip in each direction that no budget here accounts for; Section 13 records the question that shape has to answer before it is deployed. Built-in policy matching is inside this figure. A batch of up to the configured batch bound adds no more than 10 ms at p95 beyond the sum of its members' engine time.
- **Rationale**: The gate's cost is spent from the enforcing gear's budget, not its own. Infrastructure Resource Manager holds 500 ms at p95 for a mutation acknowledgment and the selected engine holds 25 ms; the gate's own share has to be small enough that inserting it does not change the consumer's arithmetic. Excluding engine time makes the figure attributable — a slow admission is then either the gate's fault or the engine's, and the metrics say which.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation.

#### Fail-Closed Determinism

- [ ] `p1` - **ID**: `cpt-cf-admission-control-nfr-fail-closed`

The system **MUST** refuse on every error path, and **MUST NOT** expose any configuration that converts a failure into an admission.

- **Threshold**: Zero admissions across the complete set of injected failure conditions — engine unreachable, engine timeout, engine error, engine returning an unmappable result, no engine selected, types registry unavailable during rule matching, and internal error.
- **Rationale**: The gate is the platform's decision point on the admission path; the enforcing gear is what enforces, per Section 1.4. That division is exactly why a permissive failure mode here is so costly. Enforcing gears enforce the answer they are given, so a failure that produced an admission would be honoured across every gear that calls the gate, and the bypass would be silent — nothing downstream is positioned to notice that the answer was not really a decision. This requirement makes the property testable rather than incidental.
- **Verification Method**: Fault injection across the enumerated failure conditions.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation.

#### Non-Modification

- [ ] `p1` - **ID**: `cpt-cf-admission-control-nfr-non-modification`

The request an enforcing gear submits **MUST** be unchanged by the gate.

- **Threshold**: For every admitted operation across the conformance suite, the request the caller holds after the call is byte-identical to the one it submitted, and no field of it is readable back through the response.
- **Rationale**: The gate's contract with its callers is that it judges and returns; a gate that could modify would make every caller's subsequent validation conditional on what the gate did. Stating it as a measurable property rather than as an omission is what stops a later mutating capability arriving without a decision.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation.

#### Availability

- [ ] `p1` - **ID**: `cpt-cf-admission-control-nfr-availability`

The system **MUST NOT** reduce the availability of the process it runs in, and where it runs as a separate process **MUST** meet an availability target better than that of the gears whose operations it gates.

- **Threshold**: Conditioned on deployment shape. **In-process**, the platform's default and the shape of first release: the gate's availability is the host's, and the measurable requirement is that no failure originating in the gate terminates, stalls, or exhausts its host — verified as zero host-fatal outcomes across the fault-injection set above. **Out-of-process**, which applies only where `cpt-cf-admission-control-fr-remote-decision-surface` is met: 99.95 percent measured monthly, with planned maintenance excluded only when announced in advance and capped at 30 minutes per month, since the gate fails closed and its maintenance is an outage of every gated operation.
- **Rationale**: Because the gate fails closed, unavailability presents as refusal of every gated operation across every calling gear. An independent target is only meaningful where the gate can fail independently of its callers, which in-process composition does not provide.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation.

#### Record Completeness

- [ ] `p1` - **ID**: `cpt-cf-admission-control-nfr-record-completeness`

The system **MUST** produce a record for every admission decision, on both verdicts, carrying enough context to reconstruct the decision without access to the original request.

- **Threshold**: One record per decision, on both verdicts, with no sampling. Record content is defined by `cpt-cf-admission-control-fr-admission-records` and is not restated here. Emission **MUST NOT** extend gate overhead, so records are durable asynchronously; at most 5 seconds of records may be awaiting durability at any moment, and that window **MUST** be observable.
- **Rationale**: The record is how the platform answers what it gated. Sampling loses the rare decisions most likely to be investigated, and recording only refusals loses the admission that mattered. Durability cannot be synchronous without spending the overhead budget on it, so the honest requirement is a bounded, measured window.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation.

### 6.2 NFR Exclusions

- Data residency and geographic partitioning: the gate stores nothing in a location its deployment does not already determine.
- Functional safety and hazard analysis: not applicable. The gear is an information system with no physical actuation.
- Accessibility, internationalisation, and device support: not applicable. The gear exposes no end-user interface; its operational surface is consumed by operators and tooling.
- Regulatory certification: not gear-specific. The gate holds no payment, health, or financial-reporting data; personal data is confined to the subject identifiers in its records and follows the platform's retention model.
- Support tiering and diagnostic service levels: inherited from the platform support model; the availability requirement above is the only gear-specific service level.
- Scalability as a separate requirement: the gate holds no shared mutable state on the decision path, so its throughput is bounded by the engine behind it rather than by the gate. The overhead requirement governs the gate's own contribution at any load.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Admission Client

- [ ] `p1` - **ID**: `cpt-cf-admission-control-interface-admission-client`

- **Type**: Rust trait, asynchronous, registered in ClientHub without scope
- **Stability**: stable
- **Description**: The interface enforcing gears call, and the gear's principal surface. Accepts an intended operation — subject, action, resource type and optional identifier, tenant context, and operation properties — singly or as a batch, and returns admitted or refused with a cause. An admitted verdict carries the obligations the engine attached, relayed unaltered; the collection is empty until the selected engine emits any. The verdict reserves a third value for a deferral, unpopulated: while `cpt-cf-admission-control-fr-deferral-verdict` is unimplemented a deferral arrives as a refusal carrying the awaiting-approval cause, so a caller that handles two values is conformant, and serving the third value later is the major version that requirement calls for.
- **Conformance expectations on the caller**: two, neither observable by this gear. Any error result is a refusal. And an obligation the caller does not recognise makes the decision refused — the gate cannot apply that rule itself, because it recognises no obligation identifier and would therefore refuse every decision carrying one; it belongs to the gear that enforces.
- **Breaking Change Policy**: Major version bump required.

#### Operational REST API

- [ ] `p1` - **ID**: `cpt-cf-admission-control-interface-operational-api`

- **Type**: REST API, versioned, served beneath the platform API prefix
- **Stability**: stable
- **Description**: The read-only surface of `cpt-cf-admission-control-fr-operational-surface`: readiness, the selected engine's identity, the loaded built-in policies and their match counts. Uses the canonical problem error envelope. This surface exists in both deployment shapes and is the carrier for the readiness the gear reports; it exposes no admission decision and accepts no mutation.
- **Breaking Change Policy**: Backward compatible within a major version.

#### Policy Engine Plugin Contract

- [ ] `p1` - **ID**: `cpt-cf-admission-control-interface-engine-plugin`

- **Type**: Rust trait, asynchronous, registered in ClientHub under a GTS instance scope
- **Stability**: unstable
- **Description**: The contract a policy engine implements to be selectable by the gate. Receives an evaluation request and returns a permission with its cause and any obligations, a prohibition with a reason, or a deferral, and may signal that callers should back off. The obligation collection and the deferral both exist from the first version even though no engine populates either yet: adding them later would be a breaking change to a contract every engine implements, and the engine that will emit deferrals is already specified. This gear owns the contract; engines conform to it.
- **Breaking Change Policy**: Minor version bump while unstable.

### 7.2 External Integration Contracts

#### GTS Registration

- [ ] `p1` - **ID**: `cpt-cf-admission-control-contract-gts`

- **Direction**: provided to and required from the types registry
- **Protocol/Format**: GTS link-time inventory in one direction, resolution in the other. The gate registers the policy engine plugin specification and its own error family; it resolves the selected engine's identifier and the resource types built-in policies and requests name.
- **Compatibility**: Type identifiers are stable; new versions are new identifiers.

#### Admission Record Contract

- [ ] `p1` - **ID**: `cpt-cf-admission-control-contract-admission-record`

- **Direction**: provided to downstream consumers
- **Protocol/Format**: Structured records with the stable field set of `cpt-cf-admission-control-fr-admission-records`, emitted per decision.
- **Compatibility**: Additive field changes only within a major version; consumers must tolerate unknown fields.

## 8. Use Cases

#### Gate a Resource Operation

- [ ] `p1` - **ID**: `cpt-cf-admission-control-usecase-gate-operation`

**Actor**: `cpt-cf-admission-control-actor-enforcing-gear`

**Preconditions**:
- A policy engine is selected and reachable.
- The enforcing gear holds the operation it intends to perform and the security context of the initiating caller.

**Main Flow**:
1. The enforcing gear submits the intended operation to the gate.
2. The gate matches the operation against built-in policies and finds none that refuses.
3. The gate submits an evaluation request to the selected engine.
4. The engine returns a permission with its cause.
5. The gate returns admitted, and emits a record carrying the verdict, the cause, and the engine's identity.

**Postconditions**:
- The enforcing gear proceeds with an unmodified request, and the decision is recorded.

**Alternative Flows**:
- **A built-in policy refuses**: The gate refuses without consulting the engine, naming the rule.
- **The engine prohibits**: The gate refuses, carrying the engine's reason.
- **The engine is unreachable or exceeds its bound**: The gate refuses with the could-not-run cause, which the caller can retry as transient.

#### Gate a Multi-Type Change

- [ ] `p1` - **ID**: `cpt-cf-admission-control-usecase-gate-batch`

**Actor**: `cpt-cf-admission-control-actor-enforcing-gear`

**Preconditions**:
- The enforcing gear has classified a change touching several resource types and needs one verdict before dispatching work.

**Main Flow**:
1. The enforcing gear submits one batch containing an intended operation per resource type the change touches.
2. The gate applies built-in policies to every member.
3. The gate submits the surviving members to the engine.
4. The gate combines the member verdicts, refusing the batch if any member is refused.
5. The gate returns the batch verdict with every refused member identified, and records each member.

**Postconditions**:
- The enforcing gear dispatches the whole change or refuses it whole, able to report every fault at once.

**Alternative Flows**:
- **Batch exceeds the configured bound**: The gate refuses the request against the limit rather than admitting a subset.

#### Substitute the Policy Engine

- [ ] `p2` - **ID**: `cpt-cf-admission-control-usecase-substitute-engine`

**Actor**: `cpt-cf-admission-control-actor-platform-operator`

**Preconditions**:
- A second engine implementing the plugin contract is registered.

**Main Flow**:
1. The operator changes the configured engine identifier and restarts the deployment.
2. The gate validates the new identifier at startup and resolves the engine.
3. Built-in policies continue to apply unchanged.
4. Subsequent records name the new engine.

**Postconditions**:
- Enforcing gears observe no change to the interface they call, and records distinguish decisions made before and after the substitution.

**Alternative Flows**:
- **The new identifier does not resolve**: The gate fails to start rather than running without an engine.

## 9. Acceptance Criteria

- [ ] An enforcing gear reaches the policy question through this interface on every operation it declares as gated, with no policy logic of its own beyond enforcing the verdict. Quota, licence, and authorization remain the enforcing gear's own calls, in the order its requirements set.
- [ ] The request an enforcing gear submits is byte-identical after the call, on both verdicts.
- [ ] A built-in policy refuses an operation with no policy engine configured at all.
- [ ] A built-in policy refuses an operation that the selected engine would have permitted, and no configuration or policy content overturns it.
- [ ] Substituting the policy engine changes no behaviour observable through the admission interface except the engine identity in records.
- [ ] Every enumerated failure condition produces a refusal carrying the could-not-run cause, and no configuration produces an admission on failure.
- [ ] A hung engine produces a refusal within the configured bound rather than stalling the calling gear.
- [ ] The refusal causes are distinguishable by a caller without parsing prose, and a could-not-run refusal is distinguishable in the metrics from a policy refusal.
- [ ] An engine deferral produces a refusal carrying the awaiting-approval cause, distinguishable from a could-not-run refusal in both the response and the metrics, and never an admission.
- [ ] A batch refuses whole and names every refused member, and a preview of the same change returns the same verdict as its apply, given unchanged policy between the two. The gate holds no state, so nothing pins a verdict between a preview and an apply: a policy change, an assignment change, or a configuration change in the interval may legitimately produce a different answer, and no reservation, lease, or decision token is offered against that.
- [ ] A batch answers for a set of operations and not for an ordered sequence: every member is judged against the state in force when the batch is submitted, so a member that only becomes admissible after an earlier member takes effect is refused. A caller sequencing dependent operations submits them as it performs them rather than as one batch.
- [ ] Configuration naming an unresolvable engine or an unresolvable resource type fails startup.
- [ ] Every decision is recorded on both verdicts, and no record contains a credential or a caller-supplied property value.

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| Policy engine | The selected engine, reached through the plugin contract; without one the gate refuses every operation built-in policies do not already refuse | p1 |
| Policy evaluation facility | Compiles and evaluates built-in policy content in the language its backends accept; without it the gate has no way to apply its own policies. Does not exist yet, in any form, anywhere in the repository | p1 |
| `types-registry` | Resolution of the selected engine's identifier and of the resource types built-in policies and requests name; GTS registration of the plugin specification | p1 |
| `toolkit-canonical-errors` | The gear's error family and its refusal causes | p1 |
| `toolkit-security` | Security context propagation from the enforcing gear | p1 |
| `event-broker` | Transport, retention, and export of admission records, published to the platform-scoped audit topic shared with `policy-engine`. On the durability path, unlike for that gear: this gate owns no database, so the topic is the only place an admission record becomes durable, and `cpt-cf-admission-control-nfr-record-completeness` is met against it. A refusal by built-in policy and a could-not-run refusal are recorded nowhere else, and a could-not-run refusal is by definition one the engine could not record either | p1 |

## 11. Assumptions

- Subjects arrive already authenticated, with identity and tenant context established upstream, and the gate forwards that context rather than establishing it.
- Enforcing gears enforce the verdicts they receive, including any obligations attached to an admission: they honour what they recognise and refuse rather than proceed on what they do not. The gate has no mechanism to detect a caller that asks and then proceeds regardless, and no requirement here depends on detecting one.
- Enforcing gears supply complete operation context. A property the gate forwards is a property the caller chose to send; the gate has no schema for it and cannot tell an omission from an absence. This includes facts the caller relays rather than owns — a tenant's subscription plan or service tier, where policy is written to decide on one. The gate neither validates such a value nor knows which properties carry one, so a relayed entitlement is exactly as trustworthy as the gear that sent it; `cpt-cf-admission-control-fr-record-confidentiality` also keeps the value out of the admission record, which carries the property's name alone.
- The selected engine returns a permission with a cause, a prohibition with a reason, or a deferral, per the contract in Section 7.1. An engine that cannot express that shape is not selectable. An engine that never emits a deferral is entirely conformant — the variant is carried so that the gate can map one, not because any engine must produce one.
- Built-in policies are few and change with the deployment. Nothing here is designed for a built-in policy set that grows at tenant scale or changes at runtime; that is what the policy engine is for.
- The evaluation facility's backends provide two properties this gear depends on and cannot supply itself: syntax validation independent of evaluation, so that `cpt-cf-admission-control-fr-configuration-validation` can reject a malformed built-in at startup, and acceptance of an externally imposed per-policy cost bound, so that `cpt-cf-admission-control-fr-builtin-evaluation-bounds` has an implementation. Both are confirmed present in the first candidate audited. What that audit also established is that a backend declares no sandbox posture of its own, that its determinism depends on which builtins its build registers, and that a builtin can be neither removed nor shadowed — so the denylist this gear enforces at startup is the only mechanism available, and it is a property of a specific backend build rather than of the backend.
- Infrastructure Resource Manager, the first enforcing gear, exists as a specification and not yet as software, and its own requirements describe it calling a decision service directly. Routing it through this gate is a change to that gear's integration which nobody has yet agreed.

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| The gate becomes a second policy engine by increments — built-in policies gain an authoring API, then a lifecycle, then tenant scoping | The split this gear exists to create collapses, and the platform has two components managing policy content with different models | The boundary is management, not expressiveness: built-in policies may say anything the facility can express, and arrive only as deployment configuration. Treat any request to author one at runtime as a request for content in the engine |
| The evaluation facility does not land | The gate cannot apply built-in policies at all, and two p1 requirements have no implementation path | Nothing this gear can mitigate; Section 10 records the dependency as a prerequisite |
| A backend build registers a non-deterministic builtin the denylist does not name | A platform guardrail decides differently on two identical requests, and because built-in refusals are the ones an operator trusts most, the inconsistency is attributed to anything but the rule | Bind the denylist to the audited build and re-audit on upgrade; enumerate the backend's registered builtins in a test rather than trusting a hand-maintained list. The policy engine carries the same exposure, so it is one audit serving two gears |
| No enforcing gear adopts the gate | A gate with no callers, and requirements shaped by speculation rather than integration | Validate the interface against Infrastructure Resource Manager's admission requirements before implementation; admit a second caller only with a concrete integration |
| Gate overhead is measured but engine time is not attributed | A slow admission is blamed on the gate or the engine by argument rather than by measurement, and neither improves | Exclude engine time from the gate's own threshold and export both separately from first release |
| Fail-closed behaviour makes every engine outage a platform-wide stoppage | An engine incident stops all gated operations, and pressure builds for a bypass switch | Hold the engine bound tight enough that an outage is detected quickly; keep the could-not-run cause retryable and distinguishable so callers degrade rather than fail permanently |
| A deferral is reported as an outage | An operation waiting on a person looks like a broken dependency: the caller retries what no retry can resolve, the approval is never requested, and the metrics show an engine incident that is not happening | `cpt-cf-admission-control-fr-deferral-relay` carries the deferral in the plugin contract from the first version and gives it its own refusal cause, so the two are distinguishable before any engine emits one |
| Built-in policies and policy content express overlapping intent | An operator cannot tell which component refused, or writes the same rule twice with different semantics | Name the responsible rule in every platform-rule refusal; keep the two vocabularies deliberately different, so a rule that can be expressed here obviously cannot be expressed there |

## 13. Open Questions

Each question carries an owner role and the point by which it must be answered.

| Question | Owner | Needed by |
|---|---|---|
| What is the first built-in policy? The mechanism is specified, but none has been named, and an empty set makes `cpt-cf-admission-control-fr-builtin-policy-independence` unfalsifiable in practice — there is nothing whose survival across an engine substitution can be observed. | Platform architecture | Before first release |
| Does Infrastructure Resource Manager agree to reach policy through this gate rather than calling a decision service directly, and to keep its request enrichment inside its own pipeline? | Infrastructure Resource Manager owner | Before implementation |
| What is the default engine call bound, and does it fit inside the enforcing gear's budget alongside the engine's own latency target? | Gear owner | Before implementation |
| What is the bound on batch size, and is it the same bound the engine enforces or a separate one? | Gear owner | Before first release |
| What latency budget applies where `cpt-cf-admission-control-fr-remote-decision-surface` is met, and does any consumer's own budget survive two transport round trips — one into the gate and one onward to the engine? Neither this document nor Infrastructure Resource Manager reserves an allowance for either. | Platform architecture | Before that requirement is implemented |
| Should built-in policies be able to require that policy has governed an operation — refusing an admission whose permission cause is ungoverned — or is that a policy question that belongs in the engine? | Platform architecture | Before first release |
| Where does a deferral terminate, and who raises the approval — the enforcing gear that received the deferred verdict, or a service the gate does not know about? The engine's own requirements carry the same question, and no approval service exists. Answering it decides whether `cpt-cf-admission-control-fr-deferral-verdict` is enough or whether the gate needs a correlation the approval can be resolved against. | Platform architecture | Before the deferral verdict ships |

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
- **Features**: [features/](./features/)
- **Policy engine**: [Policy Engine](../../policy-engine/docs/PRD.md), the first implementation of the plugin contract in Section 7.1
- **First enforcing gear**: [Infrastructure Resource Manager](../../../infrastructure-resource-manager/docs/PRD.md), whose admission pipeline, cascade admission, and policy gating requirements this gate serves
- **Platform architecture**: [ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md), [GEARS.md](../../../../docs/GEARS.md)
- **Comparable plugin selection**: [quota-enforcement](../../quota-enforcement/docs/PRD.md) and `authz-resolver`, whose one-plugin-at-a-time selection model this gear follows
- **Standards lineage**: the PDP, PEP, and PAP vocabulary in Section 1.4 derives from NIST SP 800-162, as it does in the policy engine beside this gear
