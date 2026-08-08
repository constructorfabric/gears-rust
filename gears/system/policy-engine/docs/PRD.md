# PRD — Policy Engine


<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [Relationship to existing policy subsystems](#relationship-to-existing-policy-subsystems)
  - [Relationship to ADR-0001](#relationship-to-adr-0001)
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
  - [5.1 Policy Content Model](#51-policy-content-model)
  - [5.2 Bundle Lifecycle](#52-bundle-lifecycle)
  - [5.3 Assignment and Inheritance](#53-assignment-and-inheritance)
  - [5.4 Policy Matching](#54-policy-matching)
  - [5.5 Decision Semantics](#55-decision-semantics)
  - [5.6 Decision Outputs](#56-decision-outputs)
  - [5.7 Evaluation Backend](#57-evaluation-backend)
  - [5.8 Multi-Tenancy](#58-multi-tenancy)
  - [5.9 Observability](#59-observability)
  - [5.10 Configuration](#510-configuration)
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

The Policy Engine is the Gears platform gear that owns authorization and governance policy: it stores policy content, manages its lifecycle, evaluates it against operations and resources, and returns decisions together with the row-level constraints that Policy Enforcement Points compile into database queries. It is the platform's Policy Decision Point implementation, and the single place where policy content is authored, versioned, and audited.

The gear follows the pattern [CredStore](../../../credstore/docs/PRD.md) established for credential storage: the gear owns the domain model, lifecycle, and contracts, while a pluggable backend performs the actual policy evaluation. This keeps Gears free of a mandated policy language — the evaluation backend is replaceable — while giving the platform a first-class home for policy content that no single consumer owns.

### 1.2 Background / Problem Statement

Gears authorization runs a PEP to PDP to `AccessScope` pipeline described in [ARCHITECTURE_MANIFEST §10.3](../../../../docs/ARCHITECTURE_MANIFEST.md) and [arch/authorization/DESIGN.md](../../../../docs/arch/authorization/DESIGN.md). The enforcement half of that pipeline is complete: `authz-resolver` abstracts the PDP, `PolicyEnforcer` wraps it for domain gears, and the secure database layer compiles returned constraints into scoped queries. The decision half is not. The two plugins in the tree are a development stub whose own documentation says it must not be used in production, and a tenant-scoping plugin that implements a fixed set of hierarchy access rules — it emits real constraints and fails closed correctly, but it has no role or permission model and does not differentiate by action, reading the requested action only for logging. Four documents record the same gap: the `authz-resolver` README plans a production PDP plugin, the stub plugin promises its own replacement, the authorization integration test plan states that a real plugin must resolve access from an external policy source, and [PERMISSION_GTS_TYPE.md](../../../../docs/arch/authorization/PERMISSION_GTS_TYPE.md) lists both the evaluating plugin and an AuthZ management gear owning grants and role bindings as future work.

The platform already defines the permission vocabulary those documents point at. `AuthzPermissionV1` gives permissions a canonical type, declared at compile time. What does not exist is anywhere to store which subject holds which permission, under what conditions, or to version and audit that content as it changes. Authorization policy in Gears today is either hard-coded in a plugin or absent.

### Relationship to existing policy subsystems

Two gears have already met a narrower version of this need, and this gear does not displace either. `quota-enforcement` defines `QuotaResolutionEngineV1`, a capability-based contract with pluggable arbitration engines and no mandated engine technology. `event-broker` defines a `FilterEngine` plugin trait over a GTS-typed registry with a built-in expression engine. Both are accepted specifications, both own the state their policy governs, and both are the right shape for their domain.

The pattern they share — a GTS-typed registry of pluggable evaluation engines — is the pattern this gear adopts, which is evidence it is the platform's idiom rather than an import. The difference is ownership: those registries are local to a gear and evaluate expressions against state that gear holds. Authorization has no such owner, because the state it governs is spread across every gear that enforces. First release therefore addresses authorization only. Whether governance policy consolidates here later is a question for the consumers to raise when they have a need their own registry cannot meet, not one this document should answer on their behalf.

### Relationship to ADR-0001

[ADR-0001](../../../../docs/arch/authorization/ADR/0001-pdp-pep-authorization-model.md) is accepted and states two positions this gear contradicts: that the Policy Administration Point is entirely vendor-controlled and Gears never sees or stores policies, and that the PDP is the vendor's authorization service reached through `authz-resolver`. This gear is both a PAP and a PDP inside the platform.

That is a deliberate departure, and it is narrower than it looks. ADR-0001's reasoning is that Gears must integrate with a vendor's existing authorization infrastructure without demanding resource synchronisation or a policy format. That reasoning holds, and this gear does not weaken it: `authz-resolver` remains the abstraction, a vendor PDP remains selectable, and a deployment that already has a policy manager can ignore this gear entirely. What changes is that a deployment *without* one is no longer required to build a plugin before it can authorise anything.

A superseding ADR recording that change is a prerequisite deliverable for this gear, not an artefact of it. Until it exists, this document's positioning is provisional.

### 1.3 Goals (Business Outcomes)

Milestones below refer to this document's own priority tiers — "p1 complete" means every `p1` requirement in Sections 5 and 6 is met and verified. The repository has no separate release-maturity vocabulary.

| Outcome | Baseline | Target | By |
|---|---|---|---|
| A deployment can authorise in production without writing a plugin | Zero production-capable decision paths; the only general-purpose plugin is a stub marked "do not use in production" | One supported decision path, exercised end to end by the platform example server and the authorization integration suite | p1 complete |
| Policy changes are versioned, attributable, and reviewable | Authorization policy is hard-coded in plugin source; changes require a release | 100 percent of policy changes carry an author, a version, and a content digest, and take effect without a deployment | p1 complete |
| The evaluation backend is replaceable | No backend contract exists | Two structurally different backends satisfy the contract with no change to consuming gears | p2 complete |
| Deployments with an existing Policy Decision Point are unaffected | Not applicable — no gear to conflict with | The authorization integration suite passes with this gear absent and a vendor plugin selected | p1 complete |
| Decisions are reconstructable after the fact | No decision records | Any decision in the retention window can be traced to the subject, the action, and the policy version that determined it | p1 complete |

### 1.4 Glossary

| Term | Definition |
|------|------------|
| PDP | Policy Decision Point. The component that evaluates policy and returns a decision. This gear. |
| PEP | Policy Enforcement Point. The component that enforces a decision at the point of resource access. Domain gears, via `PolicyEnforcer`. |
| PAP | Policy Administration Point. The surface through which policy is authored and managed. Provided by this gear. |
| PIP | Policy Information Point. A source of additional attributes used during evaluation. `tenant-resolver` and `resource-group` serve this role. |
| Policy Bundle | A versioned, deployable collection of policy documents, assigned to a tenant. |
| Policy Document | A single unit of policy content within a bundle, carrying a kind that determines how it is evaluated. |
| Policy Target | The binding that determines when a policy document is evaluated, by trigger type, phase, resource type, and filters. |
| Constraint | A set of predicates, combined with AND, that a PEP compiles into query conditions. Multiple constraints combine with OR. |
| Decision | The outcome of evaluating policy for a subject, action, and resource. |
| Obligation | An action a PEP must perform when enforcing an allow decision. |
| Assignment | The binding of an active bundle to a tenant, determining which resources the bundle governs. |
| Policy Priority | The ordering value on an assignment, used to break ties between assignments at the same tenant. Unrelated to plugin priority. |
| Plugin Priority | The platform-wide value used to select among candidate plugin instances during backend discovery. Unrelated to policy priority, and the two follow opposite conventions — see [Backend Discovery](#57-evaluation-backend). |
| Evaluation Backend | The pluggable component that performs policy evaluation for a given policy language. |
| GTS | Global Type System. Provides the identifiers used for resource types, plugin instances, permissions, and error codes. |

## 2. Actors

### 2.1 Human Actors

#### Policy Author

**ID**: `cpt-cf-policy-engine-actor-policy-author`

- **Role**: Authors and revises policy content, submits it for validation, and promotes it through the bundle lifecycle.
- **Needs**: Authoring and validation feedback before activation; deterministic evaluation semantics; the ability to review what a policy will do before it takes effect.

#### Platform Operator

**ID**: `cpt-cf-policy-engine-actor-platform-operator`

- **Role**: Configures and operates the gear, selects the evaluation backend, sets limits, and responds to availability and latency incidents.
- **Needs**: Configuration with safe defaults; visibility into decision latency, backend health, and cache behaviour; predictable failure semantics.

#### Tenant Policy Administrator

**ID**: `cpt-cf-policy-engine-actor-tenant-policy-admin`

- **Role**: Manages policy for a tenant subtree within the bounds set by ancestor tenants.
- **Needs**: Policy management confined to their own subtree; visibility into which inherited policies constrain them.

#### Security Auditor

**ID**: `cpt-cf-policy-engine-actor-security-auditor`

- **Role**: Reviews authorization and governance decisions after the fact for compliance and incident investigation.
- **Needs**: A complete, tamper-evident decision record with enough context to reconstruct why a decision was reached.

### 2.2 System Actors

#### AuthZ Resolver

**ID**: `cpt-cf-policy-engine-actor-authz-resolver`

- **Role**: The authorization gateway gear. Reaches this gear through the `pe-authz-plugin` bridge, which implements the `AuthZResolverPluginClient` contract and translates between the authorization evaluation model and this gear's decision API. The primary consumer at first release.

#### Policy Enforcement Point

**ID**: `cpt-cf-policy-engine-actor-pep`

- **Role**: Any domain gear enforcing a decision through `PolicyEnforcer`. Consumes constraints indirectly, by way of the AuthZ Resolver, and declares which resource properties it can compile.

#### Governance Consumer

**ID**: `cpt-cf-policy-engine-actor-governance-consumer`

- **Role**: A gear that evaluates policy for a purpose other than authorization — quota and budget enforcement, per-tenant runtime governance, event subscription filtering, or approval routing. Consumes decisions and obligations directly rather than through the AuthZ Resolver.

#### Evaluation Backend Plugin

**ID**: `cpt-cf-policy-engine-actor-backend-plugin`

- **Role**: The pluggable component that evaluates policy content in a specific policy language and returns decisions, permitted-scope information, and obligations. Discovered through the types registry and reached in-process.

#### Hierarchy Provider

**ID**: `cpt-cf-policy-engine-actor-hierarchy-provider`

- **Role**: `tenant-resolver` supplies tenant ancestry, descendants, and barrier state; `resource-group` supplies group hierarchy and membership. Both act as Policy Information Points during assignment resolution and constraint generation.

#### Types Registry

**ID**: `cpt-cf-policy-engine-actor-types-registry`

- **Role**: Receives this gear's GTS registrations and provides discovery of evaluation backend plugin instances.

#### Decision Record Sink

**ID**: `cpt-cf-policy-engine-actor-audit-sink`

- **Role**: Receives the decision records this gear emits, for retention, export, and analysis.

## 3. Operational Concept & Environment

> Project-wide runtime, operating system, architecture, lifecycle policy, and integration patterns are defined at the repository level in [ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md) and the foundational [guidelines/](../../../../guidelines/). This gear has no parent gear PRD. Only gear-specific constraints appear below.

### 3.1 Gear-Specific Environment Constraints

- The gear sits on the authorization hot path. Every enforced operation in every gear that uses `PolicyEnforcer` results in an evaluation, so the gear's availability and latency bound the platform's.
- The gear is a decision authority, not a data owner for the resources it governs. It never holds copies of the resources policy applies to, and it never requires consuming gears to synchronise resource state into it.
- [GEARS.md](../../../../docs/GEARS.md) lists Policy Manager under Core Platform Services, the category of authoritative services that may exist outside Gears. This gear implements that capability inside the platform while leaving the external-service option intact, the same position CredStore occupies for credential storage. The catalog entry needs to reflect that once this gear exists.
- The evaluation backend is a compile-time dependency of the deployment, not of the gear. A deployment that selects no backend has no decision path and must not start with authorization enabled.

## 4. Scope

### 4.1 In Scope

- Policy content model: bundles, versions, documents, targets, and filters, with their relationships and constraints.
- Bundle lifecycle: draft, activation, deprecation, immutability after activation, content integrity, and optimistic concurrency.
- Policy assignment to tenants, with inheritance, precedence, and barrier handling across the platform's tenant tree.
- Evaluation of a subject, action, and resource against the applicable policy set, returning a decision.
- Generation of row-level constraints for collection operations, so that enforcement happens in the query rather than after it.
- Trigger-based evaluation: matching policy documents to operations and events by type, phase, resource type, and filters.
- Obligations attached to allow decisions.
- A pluggable evaluation backend contract, and discovery of backend instances.
- Multi-tenant isolation of policy content and decisions.
- Decision records covering every evaluation.
- Configuration, observability, and operational limits.

### 4.2 Out of Scope

- The identity of subjects and the validation of their credentials, which belong to `authn-resolver`.
- Enforcement itself, including compilation of constraints into SQL, which belongs to the PEP and the secure database layer.
- Tenant and resource group hierarchy ownership, which belongs to `tenant-resolver` and `resource-group`. This gear reads hierarchy and does not maintain it.
- Allowance state and its consumption, which belongs to `quota-enforcement`. This gear evaluates policy that governs quota and budget — the rules, limits, and conditions — and does not hold counters, reserve allowance, or track consumption.
- The `pe-authz-plugin` bridge into `authz-resolver`, which is specified separately.
- Any specific policy language. Language-specific authoring, syntax, and validation rules belong to the evaluation backend that implements them.
- Billing and metering as systems of record. The gear may evaluate budget policy but does not own usage or billing state.
- A policy authoring user interface.
- Migration of policy content from external policy managers.

## 5. Functional Requirements

> **Testing strategy**: All requirements are verified via automated tests targeting 90 percent or greater code coverage unless otherwise specified. Verification method is documented only where a non-test approach applies.

### 5.1 Policy Content Model

#### Bundle Composition

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-bundle-composition`

The system **MUST** represent policy content as bundles that contain policy documents, where each document carries a kind that determines how it is evaluated and each document carries one or more targets that determine when it is evaluated. A bundle is the unit of versioning, assignment, and integrity.

- **Rationale**: Policy that is versioned per document cannot be reasoned about as a whole, and policy that is versioned per tenant cannot be shared. The bundle is the granularity at which authors think about a coherent set of rules and at which operators activate and withdraw them.
- **Actors**: `cpt-cf-policy-engine-actor-policy-author`

#### Document Kinds

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-document-kinds`

The system **MUST** distinguish policy documents by kind, and **MUST** route each kind to the evaluation path appropriate to it. The kind set is closed and versioned: access control, governance guardrail, quota, budget, routing, and an extension kind for content the platform does not interpret. Adding a kind is a versioned change to the set, not a configuration option. A kind classifies what a policy governs; it does not imply that this gear owns the state that policy refers to.

- **Rationale**: A quota rule and an access rule answer different questions and combine differently. Without a declared kind, consumers cannot request only the policy relevant to them, and the gear cannot apply the right combination semantics. Kinds whose subject matter is owned elsewhere — quota and budget limits in particular — are evaluated here and enforced by the gear that holds the corresponding state.
- **Actors**: `cpt-cf-policy-engine-actor-policy-author`, `cpt-cf-policy-engine-actor-governance-consumer`

#### Target Binding

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-target-binding`

The system **MUST** allow each policy document to declare when it applies, by trigger type, evaluation phase, resource type, and attribute filters. Filters combine conjunctively. Resource type matching **MUST** support exact type identifiers and type patterns.

Attribute filters compare a named attribute against a value using a closed operator set: equal, not equal, member of, not member of, present, and not present. Resource type patterns are the pattern forms the platform's type system already defines — a concrete identifier, a wildcard over a namespace, and an attribute predicate — so this gear introduces no pattern syntax of its own.

The trigger and phase vocabularies are likewise closed and versioned. Trigger types are: an operation on a resource, a system or domain event, a schedule, an external signal, and continuous state observation. Phases are: before the operation, where the outcome can block it; after the operation, where it cannot; and continuous, which is bound to no single operation. Adding to either set is a versioned change, not a configuration option. First release implements the operation trigger in the before phase; the remainder are declared so that targets authored now remain valid when the others land, and their implementation is tracked separately.

- **Rationale**: Evaluating every document on every operation does not scale and makes policy behaviour opaque. Declared targets let the gear compute a small applicable set and let authors state intent precisely. The vocabularies are enumerated here because two other requirements match on them, and a matching rule over an undefined set cannot be verified.
- **Actors**: `cpt-cf-policy-engine-actor-policy-author`

#### Content Validation

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-content-validation`

The system **MUST** validate policy content before it can be activated, and **MUST** report validation failures against the specific documents that caused them without activating any part of the bundle.

- **Rationale**: Invalid content discovered at evaluation time fails closed and denies real traffic. Validation at authoring time moves that failure to a person who can fix it.
- **Actors**: `cpt-cf-policy-engine-actor-policy-author`

### 5.2 Bundle Lifecycle

#### Lifecycle States

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-lifecycle-states`

The system **MUST** move bundles through draft, active, and deprecated states, **MUST** permit modification only in draft, and **MUST** reject modification of an active or deprecated bundle with a distinguishable conflict result.

- **Rationale**: An activated bundle is a decision authority. If it can change underneath, no decision record can be reproduced and no reviewer can be sure what was in force at a given moment.
- **Actors**: `cpt-cf-policy-engine-actor-policy-author`, `cpt-cf-policy-engine-actor-platform-operator`

#### Content Integrity

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-content-integrity`

The system **MUST** record a content digest for each activated bundle version and **MUST** refuse to evaluate content whose digest does not match the recorded value.

- **Rationale**: Policy is a security control. Undetected modification, whether from storage corruption or tampering, silently changes who can do what.
- **Actors**: `cpt-cf-policy-engine-actor-security-auditor`, `cpt-cf-policy-engine-actor-platform-operator`

#### Optimistic Concurrency

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-optimistic-concurrency`

The system **MUST** require a precondition on every modifying operation against existing policy content and **MUST** reject the operation when the content has changed since it was read.

- **Rationale**: Multiple administrators editing one tenant's policy is normal. Last-write-wins on a security control silently discards the other author's change.
- **Actors**: `cpt-cf-policy-engine-actor-policy-author`, `cpt-cf-policy-engine-actor-tenant-policy-admin`

#### Version History

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-version-history`

The system **MUST** retain previous versions of a bundle after a new version is activated, **MUST** make each retained version identifiable by the decision records that reference it, and **MUST** support returning a bundle to a previously retained version.

- **Rationale**: Auditing a past decision requires the content that produced it, not the content in force today. The same retained versions are what make recovery from a bad activation a single operation rather than a reconstruction from memory.
- **Actors**: `cpt-cf-policy-engine-actor-security-auditor`, `cpt-cf-policy-engine-actor-platform-operator`

#### Effective Windows

- [ ] `p3` - **ID**: `cpt-cf-policy-engine-fr-effective-windows`

The system **MUST** allow an assignment to carry a start and an end time, and **MUST** include the assignment in evaluation only within that window.

- **Rationale**: Time-bounded access — a maintenance window, a temporary elevation, a contractual period — is otherwise implemented as a manual revocation that someone forgets.
- **Actors**: `cpt-cf-policy-engine-actor-tenant-policy-admin`, `cpt-cf-policy-engine-actor-platform-operator`

#### Deprecation

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-deprecation`

The system **MUST** allow an active bundle to be deprecated, **MUST** stop including deprecated bundles in evaluation within the activation propagation window, and **MUST** retain the bundle and its history for audit.

- **Rationale**: Withdrawing policy must be as controlled as activating it, and must not destroy the record of what was in force.
- **Actors**: `cpt-cf-policy-engine-actor-platform-operator`

### 5.3 Assignment and Inheritance

> Policy is assigned to tenants in the platform's single-root tenant tree, and inherited down that tree. The gear introduces no hierarchy of its own. Resource groups narrow which resources within a tenant a policy applies to, and appear as a targeting dimension rather than as an assignment level.

#### Tenant Assignment

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-tenant-assignment`

The system **MUST** allow an active bundle to be assigned to a tenant with an explicit policy priority, and **MUST** apply that bundle to the tenant and to its descendants in the tenant tree.

- **Rationale**: Policy that must be attached to every tenant individually is unmaintainable at any real tenant count, and drifts the moment a tenant is created. Assigning to the platform's existing tree means policy inherits the same structure everything else in the platform already uses.
- **Actors**: `cpt-cf-policy-engine-actor-tenant-policy-admin`, `cpt-cf-policy-engine-actor-platform-operator`

#### Nearest Tenant Precedence

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-nearest-tenant`

Where assignments at more than one tenant in an ancestry chain apply to the same evaluation, the system **MUST** order them by proximity to the resource, nearest first, breaking ties among assignments on the same tenant by policy priority, highest first. This ordering governs **permitting** outcomes only: a prohibition from any assignment in the chain denies, regardless of its position, per `cpt-cf-policy-engine-fr-denial-precedence`.

- **Rationale**: Delegation requires that a tenant can refine what an ancestor permitted, so proximity has to win among permits or delegation is impossible. It must not win over a prohibition, or a descendant could delete a constraint its ancestor is accountable for by simply asserting a nearer permit. Resolving the two rules this way makes ancestor guardrails a consequence of denial precedence rather than a separate mechanism, which is why no separate guardrail requirement appears in this document.
- **Actors**: `cpt-cf-policy-engine-actor-tenant-policy-admin`, `cpt-cf-policy-engine-actor-platform-operator`

#### Barrier Handling in Inheritance

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-inheritance-barriers`

The system **MUST** resolve the subtree an assignment reaches using the barrier handling supplied by the caller on the evaluation request, and **MUST NOT** apply a barrier default of its own. Where an assignment declares that it reaches through barriers, that declaration **MUST** widen the resolved subtree only for evaluations whose caller requested barrier handling that permits it; it **MUST NOT** override a caller that requested barriers be respected.

- **Rationale**: The platform's position is that barriers are context-dependent and the caller decides per resource type — business data respects them, billing does not. A stored barrier setting that overrode the caller would reverse that. The assignment-level declaration exists only so an operator guardrail can be marked as one that may reach through when the caller already permits it; composition is caller-first, and a gear-level default would make one of the two cases silently wrong.
- **Actors**: `cpt-cf-policy-engine-actor-platform-operator`, `cpt-cf-policy-engine-actor-tenant-policy-admin`

### 5.4 Policy Matching

#### Applicable Set Determination

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-applicable-set`

For each evaluation the system **MUST** determine the applicable set of policy documents by matching trigger type, resource type, evaluation phase, and attribute filters, restricted to active assignments visible from the requesting tenant context.

- **Rationale**: The applicable set is what makes evaluation cost independent of total policy volume. It is also the explanation an author needs when a policy does not fire.
- **Actors**: `cpt-cf-policy-engine-actor-authz-resolver`, `cpt-cf-policy-engine-actor-governance-consumer`

#### Deterministic Ordering

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-deterministic-ordering`

The system **MUST** evaluate the applicable set in a total order, such that the same inputs always produce the same decision. Proximity to the resource orders first, policy priority second, and a stable property of the assignment itself resolves the remainder — so that two assignments on the same tenant sharing a policy priority still evaluate in a fixed order.

- **Rationale**: Non-deterministic evaluation order makes decisions irreproducible, which defeats both audit and debugging, and turns intermittent authorization failures into unfixable ones. Proximity and priority alone do not produce a total order, because nothing prevents two assignments on one tenant from carrying the same priority; without a third key the outcome depends on retrieval order, and short-circuiting makes that observable in the decision.
- **Actors**: `cpt-cf-policy-engine-actor-security-auditor`

#### Evaluation Phases

- [ ] `p3` - **ID**: `cpt-cf-policy-engine-fr-evaluation-phases`

The system **MUST** evaluate documents in the after-the-operation phase without allowing their outcome to affect whether the operation proceeded, and **MUST** record those outcomes as it does any other.

- **Rationale**: Governance frequently needs to observe before it enforces. Without a phase whose outcome cannot block, every new rule is a production risk and authors cannot measure impact before turning it on. The phase vocabulary itself is defined by `cpt-cf-policy-engine-fr-target-binding`; this requirement covers only the non-blocking behaviour, which is why it is not needed for a first release that ships the blocking phase alone.
- **Actors**: `cpt-cf-policy-engine-actor-governance-consumer`, `cpt-cf-policy-engine-actor-policy-author`

### 5.5 Decision Semantics

#### Deny by Default

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-deny-by-default`

Where no policy in the applicable set permits the requested operation, the system **MUST** deny, and **MUST** state that the denial resulted from an absence of permitting policy rather than from an explicit prohibition.

- **Rationale**: The alternative permits access whenever policy is missing, misconfigured, or not yet loaded. Distinguishing the two denial causes is what lets an operator tell a misconfiguration from a working control.
- **Actors**: `cpt-cf-policy-engine-actor-authz-resolver`

#### Denial Precedence

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-denial-precedence`

Where the applicable set yields both permitting and prohibiting outcomes, the system **MUST** deny.

- **Rationale**: A prohibition is a stated intent to prevent something. Allowing a permission elsewhere to override it would make prohibitions unreliable and therefore unusable as a control.
- **Actors**: `cpt-cf-policy-engine-actor-policy-author`

#### Outcome Combination

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-outcome-combination`

A policy document yields exactly one outcome per evaluation, drawn from a closed, versioned set: **permit**, **prohibit**, **defer** to human review, and **observe**, which records without influencing the result.

The system **MUST** combine the outcomes of the applicable set into exactly one result, by these rules: any prohibit yields a prohibition; otherwise any defer yields a deferral; otherwise at least one permit yields a permission; otherwise the result is a prohibition by default. An observe outcome never changes the result.

- **Rationale**: Consumers need one answer, not a list. Stating the rules here rather than deferring them makes the requirement testable and makes the precedence between prohibition, deferral, and permission a product decision rather than an implementation accident.
- **Actors**: `cpt-cf-policy-engine-actor-authz-resolver`, `cpt-cf-policy-engine-actor-governance-consumer`

#### Short-Circuit Evaluation

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-short-circuit`

The system **MUST** stop evaluating the applicable set once no remaining outcome can change the result, and **MUST** record how many documents matched alongside how many were evaluated.

- **Rationale**: Short-circuiting is what bounds evaluation cost on the hot path, and a prohibition can be reached without examining the rest of the set. Recording both counts keeps that visible — otherwise an auditor cannot tell whether a policy was consulted and permitted, or never reached.
- **Actors**: `cpt-cf-policy-engine-actor-authz-resolver`, `cpt-cf-policy-engine-actor-security-auditor`

#### Deferral Outcome

- [ ] `p3` - **ID**: `cpt-cf-policy-engine-fr-deferral-outcome`

The system **MUST** support an outcome that neither permits nor prohibits but defers the operation for human review, and **MUST** return it distinguishably from both a permission and a prohibition.

- **Rationale**: A deferral is not a denial — the operation may still proceed once someone approves it. Conflating them either blocks legitimate work or bypasses the review entirely. This is p2 because the authorization path cannot currently represent it, so first-release value is limited to consumers calling this gear directly.
- **Actors**: `cpt-cf-policy-engine-actor-governance-consumer`

#### Denial Reason

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-denial-reason`

Every denial **MUST** carry a machine-readable reason code drawn from the platform's error type system, and **MUST** permit an accompanying human-readable detail that is recorded but not returned to end users.

- **Rationale**: Callers must branch on the reason without parsing prose. Detail aids diagnosis but describes the policy posture, and returning it to an end user leaks how the control is configured.
- **Actors**: `cpt-cf-policy-engine-actor-authz-resolver`, `cpt-cf-policy-engine-actor-pep`

#### Denial and Failure Are Distinct

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-denial-versus-failure`

The system **MUST** report an infrastructure failure distinctly from a policy denial, and **MUST NOT** represent a backend outage, a hierarchy provider outage, or an internal error as a negative decision.

- **Rationale**: Both outcomes block the operation, but they demand different responses: a denial is correct behaviour, an outage is an incident. Conflating them hides outages from alerting and tells users their permissions changed when they did not.
- **Actors**: `cpt-cf-policy-engine-actor-authz-resolver`, `cpt-cf-policy-engine-actor-platform-operator`

#### Emergency Access

- [ ] `p3` - **ID**: `cpt-cf-policy-engine-fr-emergency-access`

The system **MUST** permit an operation that policy would otherwise prohibit when all of the following hold: the request explicitly asserts emergency access, the subject holds the emergency entitlement, and the entitlement is resolvable without consulting the evaluation backend. Every decision reached this way **MUST** be marked as such in its decision record and **MUST** increment a distinct metric.

- **Rationale**: Incidents occur where correct policy blocks necessary recovery. The entitlement must be resolvable without the backend, because the incident being recovered from may be the backend itself — an override that depends on the thing that is down is not an override. An unmarked one is indistinguishable from an attack, so both the record mark and the metric are part of the requirement rather than consequences of it.
- **Actors**: `cpt-cf-policy-engine-actor-platform-operator`, `cpt-cf-policy-engine-actor-security-auditor`

### 5.6 Decision Outputs

#### Constraints for Collection Operations

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-collection-constraints`

Where a caller requests constraints, the system **MUST** express the subject's permitted scope as predicates the caller can apply to its query, and **MUST NOT** require the caller to retrieve resources in order to determine access to them.

- **Rationale**: This is the requirement the platform's authorization model is built on. Evaluating per resource makes collection operations scale with data volume rather than with policy, which the accepted architecture decision rejected explicitly.
- **Actors**: `cpt-cf-policy-engine-actor-authz-resolver`, `cpt-cf-policy-engine-actor-pep`

#### Respect Caller Capability

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-caller-capability`

The system **MUST** produce constraints only over properties and predicate forms the caller has declared it supports, and where the permitted scope cannot be expressed within those limits, **MUST** either express it in a form the caller does support or deny.

- **Rationale**: A constraint the caller cannot compile is silently dropped, and a dropped constraint returns data the subject is not entitled to. Denying is the only safe outcome when the scope cannot be expressed.
- **Actors**: `cpt-cf-policy-engine-actor-pep`

#### Hierarchy-Aware Constraints

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-hierarchy-constraints`

Where the permitted scope spans a tenant subtree, the system **MUST** express it in a form whose cost does not grow with the size of that subtree, provided the caller supports such a form. Where the scope spans a resource group hierarchy, the system **MUST** use the equivalent constant-cost form when the caller can evaluate group membership, and **MUST** otherwise degrade to enumerated identifiers.

- **Rationale**: Enumerating every identifier in a subtree produces predicates that grow without bound and defeats the constant-cost property the authorization model requires. Tenant subtrees can always take the constant-cost path because the closure is projectable. Group hierarchies usually cannot, because most domain gears do not carry group membership — so for groups, enumeration is the common case and the constant-cost form is the exception, not the reverse.
- **Actors**: `cpt-cf-policy-engine-actor-pep`

#### Barrier and Status Awareness

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-barrier-awareness`

When resolving a tenant subtree for scope or constraints, the system **MUST** honour the requested barrier handling and tenant status filtering, and **MUST** deny rather than widen the resolved set when either cannot be applied.

- **Rationale**: Barriers mark administrative boundaries that some resource families must respect and others must cross. Getting this wrong exposes one tenant's data to another, and defaulting to the wider set on error makes the failure invisible.
- **Actors**: `cpt-cf-policy-engine-actor-hierarchy-provider`, `cpt-cf-policy-engine-actor-pep`

#### Obligations

- [ ] `p3` - **ID**: `cpt-cf-policy-engine-fr-obligations`

The system **MUST** allow a permitting decision to carry obligations the caller is required to honour. The obligation set is open rather than closed — consumers introduce obligations their domain needs — so each **MUST** carry a stable identifier from the platform's type system, and a caller that does not recognise an identifier **MUST** treat the decision as prohibiting rather than ignoring the obligation.

- **Rationale**: Some policy permits an action only under conditions the decision point cannot enforce itself. An obligation the caller does not understand must be rejected rather than ignored, so obligations must be identifiable rather than free-form.
- **Actors**: `cpt-cf-policy-engine-actor-governance-consumer`

### 5.7 Evaluation Backend

#### Backend Discovery

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-backend-discovery`

The system **MUST** discover its evaluation backend through the types registry and **MUST** select deterministically when more than one candidate is present, following the platform's existing plugin priority convention.

- **Rationale**: Binding the backend at compile time would make the language choice a property of the platform build rather than of the deployment, which is what the pluggable contract exists to avoid. Backend selection uses plugin priority, which is a different value from the policy priority on an assignment and follows the opposite convention. The two never apply to the same decision, and neither is derived from the other; the glossary defines both.
- **Actors**: `cpt-cf-policy-engine-actor-types-registry`, `cpt-cf-policy-engine-actor-backend-plugin`

#### Absent Backend

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-absent-backend`

Where no evaluation backend can be resolved, the system **MUST** deny every evaluation, **MUST** report the condition distinctly from a policy denial, and **MUST** surface it as a readiness failure rather than only as a per-request error.

- **Rationale**: A deployment with no decision path is misconfigured, not restrictive. Denying is correct; reporting it as a policy outcome would send an operator looking for a policy that does not exist. Backends resolve lazily on first use, so a misconfigured deployment otherwise starts cleanly and fails only when traffic arrives — surfacing it as unreadiness moves the discovery to deployment time without requiring resolution to become eager.
- **Actors**: `cpt-cf-policy-engine-actor-platform-operator`

#### Dependency Timeouts

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-dependency-timeouts`

The system **MUST** bound the time it will wait on the evaluation backend and on each hierarchy provider, and **MUST** deny when a bound is exceeded rather than waiting indefinitely.

- **Rationale**: The fail-closed requirement enumerates backend and hierarchy timeouts as conditions that must deny, which presupposes a timeout exists to be exceeded. Nothing on the current authorization path enforces one, so a hung dependency stalls the request rather than denying it — turning a bounded outage into an unbounded one.
- **Actors**: `cpt-cf-policy-engine-actor-platform-operator`, `cpt-cf-policy-engine-actor-hierarchy-provider`

#### Backend Responsibility Boundary

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-backend-boundary`

The system **MUST** own policy content, lifecycle, assignment resolution, hierarchy access, and decision recording, and **MUST** delegate to the backend only the interpretation of policy content.

- **Rationale**: If backends own lifecycle or hierarchy, every backend reimplements them and they diverge. Confining the backend to language semantics is what makes a second backend a bounded piece of work.
- **Actors**: `cpt-cf-policy-engine-actor-backend-plugin`

### 5.8 Multi-Tenancy

#### Policy Content Isolation

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-content-isolation`

The system **MUST** confine policy content reads and writes to the requesting tenant and the tenants it is entitled to manage, and **MUST** make content belonging to other tenants indistinguishable from content that does not exist.

- **Rationale**: Policy content describes a tenant's security posture. A response that distinguishes forbidden from absent confirms existence and leaks structure.
- **Actors**: `cpt-cf-policy-engine-actor-tenant-policy-admin`

#### Cross-Tenant Evaluation

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-cross-tenant`

The system **MUST** deny an evaluation whose resource tenant lies outside the subtree the subject's context is entitled to reach under the requested tenant mode, barrier handling, and status filtering, and **MUST** identify that denial cause distinctly from an ordinary absence of permitting policy.

- **Rationale**: A resource in a descendant tenant is the normal case in a hierarchy, and the platform's default tenant mode is subtree — so "different tenant" is not the test. The test is whether the resource lies within the reachable subtree once barriers and tenant status are applied. Access outside that boundary is the highest-consequence failure in a multi-tenant platform, so it needs its own denial cause rather than being reported as no policy matched.
- **Actors**: `cpt-cf-policy-engine-actor-authz-resolver`, `cpt-cf-policy-engine-actor-security-auditor`

#### Administration Authorization

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-admin-authorization`

The system **MUST** authorise every management operation against the caller's own security context, distinguishing at minimum the ability to read policy content, to author and modify drafts, to activate and withdraw bundles, and to assign bundles to a tenant. A caller **MUST NOT** be able to grant itself, or any subject, an entitlement it does not itself hold.

- **Rationale**: This gear decides who may do what, which makes its own administration surface the highest-value target in the platform. Undefined self-authorization means the first implementation invents it, and a privilege-escalation path through policy authoring defeats every policy the gear enforces. Separating read from author from activate from assign matters because the four have different blast radii and are held by different people.
- **Actors**: `cpt-cf-policy-engine-actor-tenant-policy-admin`, `cpt-cf-policy-engine-actor-platform-operator`, `cpt-cf-policy-engine-actor-policy-author`

#### Bootstrap

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-bootstrap`

The system **MUST** provide a path by which the first policy content can be created in a deployment that has none, without that path depending on a decision the gear cannot yet make.

- **Rationale**: Denial by default applies to the management surface too, so a deployment with no policy denies the operations needed to create policy. Without a defined bootstrap the gear is unusable from a cold start, and implementations reach for an undocumented backdoor — which then survives into production.
- **Actors**: `cpt-cf-policy-engine-actor-platform-operator`

### 5.9 Observability

#### Decision Records

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-decision-records`

The system **MUST** emit a record for every evaluation. This requirement owns the normative field set; no other requirement restates it. Every record **MUST** carry:

- a correlation identifier
- the subject and the subject's tenant
- the action
- the resource type, and the resource identifier where the evaluation named one
- the decision
- the denial cause, where the decision is negative
- the identity and version of the policy content that determined the outcome
- the count of documents that matched and the count actually evaluated, which differ when evaluation short-circuits
- the elapsed time

- **Rationale**: This is the evidence base for compliance and incident review. Recording only denials loses the allow that mattered; omitting the policy version makes the record unreproducible; omitting the matched-versus-evaluated counts hides that short-circuiting left policy unexamined. Field sets that appear in more than one place drift, so they appear here only.
- **Actors**: `cpt-cf-policy-engine-actor-security-auditor`, `cpt-cf-policy-engine-actor-audit-sink`

#### Record Confidentiality

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-fr-record-confidentiality`

Decision records **MUST NOT** contain bearer tokens or other subject credentials, and **MUST NOT** reproduce constraint contents in a form that discloses resource identifiers to record consumers not entitled to them.

- **Rationale**: Audit records are widely readable by design. A record that carries a credential turns the audit trail into a credential store, and one that enumerates identifiers turns it into a data leak.
- **Actors**: `cpt-cf-policy-engine-actor-audit-sink`

#### Decision Record Retention

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-record-retention`

The system **MUST** apply a configurable retention period to decision records, **MUST** remove records beyond it, and **MUST** keep the retention period and the current record volume observable.

- **Rationale**: Decision records accumulate at request rate and carry subject identifiers, so unbounded retention is both a storage problem and a data-protection one. A retention period that is configurable but unobservable is indistinguishable from none, since nobody notices it was never applied.
- **Actors**: `cpt-cf-policy-engine-actor-platform-operator`, `cpt-cf-policy-engine-actor-security-auditor`

#### Operational Metrics

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-metrics`

The system **MUST** expose metrics covering decision latency, decision outcomes by cause, backend and hierarchy provider latency and failures, cache effectiveness, and every fail-closed denial by cause.

- **Rationale**: The gear fails closed, so its failures present as user-visible denials rather than errors. Without a fail-closed counter separated by cause, an outage is indistinguishable from a policy change.
- **Actors**: `cpt-cf-policy-engine-actor-platform-operator`

#### Evaluation Explanation

- [ ] `p3` - **ID**: `cpt-cf-policy-engine-fr-explanation`

The system **MUST** support explaining a decision on request, reporting which documents were applicable, which were evaluated, and which determined the outcome, without changing the decision itself.

- **Rationale**: The most common question about a policy engine is why it did what it did. Answering it by reading policy content does not scale past trivial configurations.
- **Actors**: `cpt-cf-policy-engine-actor-policy-author`, `cpt-cf-policy-engine-actor-security-auditor`

#### Evaluation Without Enforcement

- [ ] `p3` - **ID**: `cpt-cf-policy-engine-fr-dry-run`

The system **MUST** support evaluating a draft bundle against a supplied request without activating the bundle and without the result affecting any live decision.

- **Rationale**: Activation is the only way to learn what a policy does otherwise, which makes every change a production experiment. Dry-run turns the blast radius of a new rule into something an author can measure beforehand.
- **Actors**: `cpt-cf-policy-engine-actor-policy-author`

### 5.10 Configuration

#### Operational Limits

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-operational-limits`

The system **MUST** enforce configurable bounds on policy content size, on the applicable set per evaluation, and on the cost of any single evaluation, and **MUST** fail an operation that exceeds a bound rather than degrading decision latency.

- **Rationale**: Without bounds, one tenant's policy growth becomes every tenant's latency problem, and the latency target becomes unenforceable.
- **Actors**: `cpt-cf-policy-engine-actor-platform-operator`

#### Configuration Validation

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-fr-configuration-validation`

The system **MUST** reject unknown configuration keys at startup and **MUST** apply documented defaults to every optional setting.

- **Rationale**: A misspelled key that is silently ignored leaves an operator believing a limit or a backend selection is in force when it is not.
- **Actors**: `cpt-cf-policy-engine-actor-platform-operator`

## 6. Non-Functional Requirements

> Project-wide baselines for performance, security, reliability, and scalability are defined at the repository level in [ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md) and the foundational [guidelines/](../../../../guidelines/). This gear has no parent gear PRD. Only gear-specific NFRs appear below.

### 6.1 Gear-Specific NFRs

#### Decision Latency

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-nfr-decision-latency`

The system **MUST** return a decision within a bounded time at steady-state load, measured at the gear boundary and including policy matching, evaluation, and constraint generation.

- **Threshold**: p95 within 5 ms and p99 within 10 ms, measured at the gear boundary, with warm caches, at a sustained 10,000 requests per second. Decision record emission is excluded, being asynchronous. A hierarchy cache miss is excluded and governed separately.
- **Rationale**: Every enforced operation in every gear using `PolicyEnforcer` incurs one evaluation, so this latency is added to the platform's request path, and the budget matches what the platform's other hot-path components already hold. The exclusions are what make it affordable rather than aspirational: the tenant resolver's own budget is p95 within 5 ms **per call** and authorization may need more than one, so a decision that missed cache and resolved hierarchy synchronously could consume the entire budget in a dependency. Holding the target therefore depends on the cache hit rate below, not on the evaluation being fast.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation.

#### Hierarchy Resolution Latency

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-nfr-hierarchy-latency`

The system **MUST** resolve tenant and resource group hierarchy within a bounded time and **MUST** sustain a cache hit rate high enough to keep hierarchy resolution off the critical path for most evaluations.

- **Threshold**: p95 within 2 ms on a cache hit; cache hit rate at or above 90 percent after warm-up under steady-state load.
- **Rationale**: Assignment resolution and constraint generation both require hierarchy, and both sit inside the decision latency budget rather than beside it. A cache hit is an in-process lookup, so it must consume a fraction of that budget; without caching, each evaluation would add a Policy Information Point round trip and the decision target would be unreachable.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation.

#### Fail-Closed Determinism

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-nfr-fail-closed`

The system **MUST** deny on every error path, and **MUST NOT** expose any configuration that converts an evaluation failure into an allow decision.

- **Threshold**: Zero allow decisions across the complete set of injected failure conditions — backend unavailable, backend timeout, hierarchy provider unavailable, malformed policy content, unresolvable tenant context, and internal error.
- **Rationale**: The gear is the platform's authorization authority. A permissive failure mode converts any gear outage into an authorization bypass. This requirement makes the property testable rather than incidental.
- **Verification Method**: Fault injection across the enumerated failure conditions.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation.

#### Availability

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-nfr-availability`

The system **MUST** meet an availability target consistent with its position on the request path of every enforced operation.

- **Threshold**: 99.99 percent availability measured monthly, excluding planned maintenance.
- **Rationale**: Because the gear fails closed, unavailability presents to users as platform-wide denial of service across every gear that enforces policy, not as a degraded feature. The target therefore matches the strictest budget held by any component on the same request path rather than the looser one held by components beside it. At 99.95 percent the gear would be permitted roughly 22 minutes of total platform denial per month, which is not a service level anyone would accept once stated that way.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation.

#### Tenant Isolation

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-nfr-tenant-isolation`

The system **MUST** enforce the content visibility and management boundary defined by `cpt-cf-policy-engine-fr-content-isolation` and `cpt-cf-policy-engine-fr-admin-authorization`, and **MUST NOT** allow a decision for one tenant to be influenced by policy content the requesting context is not entitled to.

- **Threshold**: Zero cross-tenant reads, writes, or decision influences across the isolation test suite, including hierarchy traversal at barrier boundaries.
- **Rationale**: Policy content reveals the security posture of its owner. Leakage across tenants is a confidentiality breach independent of whether any decision changes.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation.

#### Decision Record Completeness

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-nfr-decision-record`

The system **MUST** produce a decision record for every evaluation, on both allow and deny paths, carrying enough context to reconstruct the decision without access to the original request.

- **Threshold**: One record per evaluation, on both allow and deny paths, with no sampling, at the sustained throughput in the scalability requirement. Record content is defined by `cpt-cf-policy-engine-fr-decision-records` and is not restated here. Emission **MUST NOT** extend decision latency, so records are durable asynchronously; at most 5 seconds of records may be lost on abrupt process termination, and that window **MUST** be observable.
- **Rationale**: Auditors must be able to answer why a specific decision was reached months later, and sampling loses exactly the rare decisions most likely to be investigated. Durability cannot be synchronous, because a durable write inside a 5 ms p95 budget at this throughput would dominate it — so the honest requirement is a bounded, measured loss window rather than a guarantee the latency target contradicts.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation.

#### Decision Cache Safety

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-nfr-cache-safety`

Where the system caches anything that contributes to a decision, the cached entry **MUST NOT** outlive the authority it was derived from.

- **Threshold**: Every cache key includes the tenant context, the token scopes, and the version of the policy content the entry was derived from; no entry's lifetime exceeds either the credential expiry of the request that populated it or the activation propagation window; no error result is cached.
- **Rationale**: A cache keyed without scope permits privilege escalation by replaying a broader entry against a narrower request; a cache outliving credential expiry extends access past revocation. These are the caching constraints stated in [arch/authorization/DESIGN.md](../../../../docs/arch/authorization/DESIGN.md).
- **Architecture Allocation**: See DESIGN.md § NFR Allocation.

#### Activation Propagation

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-nfr-activation-propagation`

The system **MUST** make an activated or deprecated bundle effective within a bounded and documented window across all evaluation paths.

- **Threshold**: A bundle state change is reflected in decisions within 60 seconds; the window is documented and observable.
- **Rationale**: Operators revoking access during an incident need a known upper bound on when the change takes effect. An unbounded window makes revocation unverifiable.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation.

#### Scalability

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-nfr-scalability`

The system **MUST** absorb load growth and concurrency without violating the decision latency target or failing requests through internal contention.

- **Threshold**: A tenfold increase over baseline load stays within the decision latency target; 1,000 concurrent evaluations complete with zero failures attributable to contention.
- **Rationale**: Policy evaluation load scales with total platform traffic across all gears, not with any single gear's usage.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation.

#### Content Durability

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-nfr-durability`

The system **MUST** be recoverable to a recent consistent state after storage loss, for policy content and its version history.

- **Threshold**: Recovery point objective within 1 hour and recovery time objective within 15 minutes for policy content, versions, and assignments, verified by a restore exercise rather than by configuration review.
- **Rationale**: Loss of policy content does not degrade the platform, it stops it: with no content, denial by default denies every enforced operation everywhere. The gear is also the only holder of the version history that audit depends on, and that history cannot be reconstructed from any other system. Decision records are covered by their own retention requirement and are not in scope here.
- **Verification Method**: Restore exercise from backup into a clean deployment.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation.

### 6.2 NFR Exclusions

- Data residency and geographic partitioning: policy content follows the deployment's existing residency posture; the gear introduces no separate residency surface.
- Offline and air-gapped operation: not required at first release. The gear and its backend are in-process, so the gear has no independent connectivity requirement beyond that of the platform.
- Functional safety and hazard analysis: not applicable. The gear is an information system with no physical actuation and no safety-critical control path.
- Accessibility, internationalisation, and device support: not applicable at first release. The gear exposes no end-user interface; a policy authoring interface is out of scope, and the administration API is consumed by operators and other gears.
- Support tiering and diagnostic SLAs: not specified here. The gear inherits the platform's support model; the availability requirement above is the only gear-specific service level.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Policy Decision Client

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-interface-decision-client`

- **Type**: Rust trait, asynchronous, registered in ClientHub without scope
- **Stability**: stable
- **Description**: The decision surface consumed by the `pe-authz-plugin` bridge and by governance consumers. Accepts a subject, an action, a resource, and an evaluation context; returns a decision, any generated constraints, any obligations, and a denial reason when the decision is negative. This surface is wider than what the authorization path can currently relay — see the Authorization Evaluation Contract below — so governance consumers reach outcomes that authorization consumers do not.
- **Breaking Change Policy**: Major version bump required.

#### Policy Management Client

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-interface-management-client`

- **Type**: Rust trait, asynchronous, registered in ClientHub without scope
- **Stability**: stable
- **Description**: The administration surface for policy content. Covers bundle, version, document, and target lifecycle, tenant assignment, and validation of content before activation. Separated from the decision surface so that consumers on the hot path do not depend on the administration contract.
- **Breaking Change Policy**: Major version bump required.

#### Evaluation Backend Plugin Contract

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-interface-backend-plugin`

- **Type**: Rust trait, asynchronous, registered in ClientHub under a GTS instance scope
- **Stability**: unstable
- **Description**: The service provider interface a policy language backend implements. Receives validated policy content and an evaluation input, and returns a decision with permitted-scope information and obligations. The gear owns content, lifecycle, assignment resolution, and hierarchy; the backend owns language semantics only.
- **Breaking Change Policy**: Minor version bump while unstable.

#### Policy Administration REST API

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-interface-rest-api`

- **Type**: REST API, versioned, served beneath the platform API prefix
- **Stability**: stable
- **Description**: External administration surface for policy content, mirroring the management client. Uses the canonical problem error envelope and precondition headers for concurrency control.
- **Breaking Change Policy**: Backward compatible within a major version.

### 7.2 External Integration Contracts

#### GTS Registration

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-contract-gts`

- **Direction**: provided to the types registry
- **Protocol/Format**: GTS link-time inventory. Registers the evaluation backend plugin specification, the policy resource types the gear itself exposes for enforcement, and the gear's error type family.
- **Compatibility**: Type identifiers are stable; new versions are new identifiers.

#### Hierarchy Read Contract

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-contract-hierarchy-read`

- **Direction**: required from `tenant-resolver` and `resource-group`
- **Protocol/Format**: In-process client traits via ClientHub. Requires tenant ancestry, descendants with barrier and status handling, resource group hierarchy, and group membership.
- **Compatibility**: The gear depends on the read surface only and tolerates additive change.

#### Decision Record Contract

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-contract-decision-record`

- **Direction**: provided to downstream consumers
- **Protocol/Format**: Structured records with a stable field set, emitted per evaluation.
- **Compatibility**: Additive field changes only within a major version; consumers must tolerate unknown fields.

#### Authorization Evaluation Contract

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-contract-authz-evaluation`

- **Direction**: provided to `authz-resolver` via the `pe-authz-plugin` bridge
- **Protocol/Format**: The bridge implements the `AuthZResolverPluginClient` contract and maps it onto this gear's decision surface, including the capability negotiation and supported-property rules defined in [arch/authorization/DESIGN.md](../../../../docs/arch/authorization/DESIGN.md).
- **Representational limit**: The authorization response carries a binary decision, constraints, and a reason accompanying a denial. It has no representation for an outcome deferred to human review, and none for an obligation attached to a permit. A deferral is at least expressible as a distinguishable denial; an obligation on a permit has no equivalent, because a permitting response carries no reason field. Whether the contract grows to represent these outcomes, or they remain available only to consumers calling this gear directly, is an open decision recorded in Section 13. The mechanism, if it grows, belongs to DESIGN.
- **Compatibility**: Governed by the `authz-resolver` SDK version; the bridge absorbs contract drift.

## 8. Use Cases

#### Author and Activate a Policy Bundle

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-usecase-activate-bundle`

**Actor**: `cpt-cf-policy-engine-actor-policy-author`

**Preconditions**:
- An evaluation backend is configured and available.
- The author is entitled to manage policy at the target tenant.

**Main Flow**:
1. The author creates a draft bundle and adds policy documents and targets to it.
2. The author submits the draft for validation.
3. The system validates content against the backend and reports any errors without activating.
4. The author activates the bundle and assigns it to a tenant.
5. The system records content integrity, freezes the bundle against further modification, and begins including it in evaluation.

**Postconditions**:
- The bundle is active, immutable, and effective at the assigned tenant and its descendants within the activation propagation window.

**Alternative Flows**:
- **Validation fails**: The bundle remains in draft; errors identify the offending documents; nothing changes for evaluation.
- **Concurrent modification**: The activation is rejected on the precondition check and the author re-reads before retrying.

#### Authorize a Single-Resource Operation

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-usecase-authorize-resource`

**Actor**: `cpt-cf-policy-engine-actor-authz-resolver`

**Preconditions**:
- At least one active bundle is assigned to a tenant in the resource's ancestry chain.

**Main Flow**:
1. The bridge submits a subject, action, resource type, resource identifier, and evaluation context.
2. The system resolves the applicable policy set for the resource's tenant, honouring inheritance and precedence.
3. The system evaluates the matching documents through the backend.
4. The system returns the decision, with a denial reason when negative and any obligations when positive.
5. The system emits a decision record.

**Postconditions**:
- The caller holds a decision it can enforce, and the evaluation is recorded.

**Alternative Flows**:
- **No applicable policy**: The system denies by default and states that no policy matched.
- **Backend unavailable**: The system reports the failure as an infrastructure error distinct from a denial, so that callers can surface unavailability rather than a false denial.

#### Authorize a Collection Operation

- [ ] `p1` - **ID**: `cpt-cf-policy-engine-usecase-authorize-collection`

**Actor**: `cpt-cf-policy-engine-actor-authz-resolver`

**Preconditions**:
- The caller has declared which resource properties it can compile and which hierarchy capabilities it supports.

**Main Flow**:
1. The bridge submits a collection action with a request for constraints.
2. The system determines the subject's permitted scope from the applicable policy set.
3. The system expresses that scope as constraints over properties the caller declared it can compile, degrading to a form the caller supports where necessary.
4. The system returns the decision together with the constraints.

**Postconditions**:
- The caller holds constraints sufficient to restrict the query, and no resource outside the permitted scope can be returned.

**Alternative Flows**:
- **Constraints required but none can be produced**: The system denies rather than allowing an unconstrained query.
- **Permitted scope exceeds what the caller can express**: The system denies rather than returning constraints the caller would silently drop.

#### Evaluate a Governance Policy

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-usecase-governance-check`

**Actor**: `cpt-cf-policy-engine-actor-governance-consumer`

**Preconditions**:
- Policy documents of a governance kind are active for the tenant.

**Main Flow**:
1. The consumer submits the operation it intends to perform, with the relevant context.
2. The system matches documents by trigger type, phase, resource type, and filters.
3. The system evaluates the matched documents and combines their outcomes.
4. The system returns the combined outcome with any obligations the consumer must honour.

**Postconditions**:
- The consumer proceeds, proceeds with obligations, or abandons the operation.

**Alternative Flows**:
- **An outcome requires human review**: The system returns that outcome distinctly, so the consumer can route the operation for approval rather than treating it as a denial.

#### Withdraw an Active Bundle

- [ ] `p2` - **ID**: `cpt-cf-policy-engine-usecase-withdraw-bundle`

**Actor**: `cpt-cf-policy-engine-actor-platform-operator`

**Preconditions**:
- A bundle is active and an operator has determined it must stop taking effect.

**Main Flow**:
1. The operator deprecates the bundle or removes its tenant assignment.
2. The system stops including it in evaluation within the activation propagation window.
3. The system records the change against the operator.

**Postconditions**:
- Decisions no longer reflect the withdrawn bundle; the bundle and its history remain available for audit.

**Alternative Flows**:
- **Withdrawal would leave no applicable policy**: Evaluation falls back to deny by default, and the system makes that consequence visible before the change is applied.

## 9. Acceptance Criteria

- [ ] A Gears deployment can run authorization end to end against this gear with no development stub plugin present.
- [ ] Collection operations are enforced in the query: no configuration produces a decision that requires filtering results after retrieval.
- [ ] A permitted tenant subtree of 10,000 tenants produces a constraint whose size does not grow with the subtree.
- [ ] Every enumerated failure condition produces a denial, and no configuration produces an allow on failure.
- [ ] A hung backend or hierarchy provider produces a denial within the stated timeout rather than stalling the request.
- [ ] Policy content can be authored, validated, activated, and withdrawn without a deployment restart, and every state change is attributable to an actor.
- [ ] An activated or withdrawn bundle takes effect within the activation propagation window, verified by measurement rather than by inspection.
- [ ] An auditor can reconstruct any past decision from its record, including which policy version determined it and how many documents were matched versus evaluated.
- [ ] Policy content and its version history survive a restore exercise into a clean deployment within the stated recovery objectives.
- [ ] A second evaluation backend can be substituted without changes to consuming gears.
- [ ] A deployment that already has an external Policy Decision Point can continue to use it without deploying this gear.
- [ ] Policy content and decisions are confined to their owning tenant across the isolation test suite, including at barrier boundaries.
- [ ] No management operation allows a caller to grant an entitlement it does not itself hold.
- [ ] A deployment with no policy content can create its first bundle without an undocumented path.
- [ ] A denial caused by an unreachable dependency is distinguishable, in both the response and the metrics, from a denial caused by policy.

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| `types-registry` | GTS registration and discovery of evaluation backend plugin instances | p1 |
| Evaluation backend plugin | Performs policy evaluation for a specific policy language; no decision path exists without one | p1 |
| `tenant-resolver` | Tenant ancestry, descendants, barrier and status handling, used in assignment resolution and constraint generation | p1 |
| `toolkit-db` | Persistence for policy content, with scoped access through the secure data layer | p1 |
| `resource-group` | Resource group hierarchy and membership, used for group-scoped constraints | p2 |
| `authz-resolver` | Consumes decisions through the `pe-authz-plugin` bridge; the gear's first consumer | p2 |
| Decision record sink | Retention and export of decision records; structured logging suffices at first release | p3 |

## 11. Assumptions

- Subjects arrive already authenticated, with identity and token scopes established upstream by `authn-resolver`.
- Tenant hierarchy is a single-root tree with barrier semantics as defined in the authorization tenant model; the gear reads it and does not reinterpret it.
- The evaluation backend runs in the same process as the gear at first release. This bounds transport failure modes but does not remove them: an in-process backend can still hang or exhaust resources, so the timeout and fail-closed requirements apply to it regardless of co-location, and the hierarchy providers are separate gears in every deployment.
- Consuming gears enforce the decisions they receive. The gear has no mechanism to detect a caller that requests a decision and ignores it.
- Policy content volume per tenant is small relative to resource volume, so policy retrieval is cacheable and does not dominate evaluation cost.
- A backend's policy language and its language-level evaluation semantics are properties of that backend, not constraints on the gear's contracts. The gear's contracts are specified so that a second backend with a different language remains possible.

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| The evaluation backend cannot express a subject's permitted scope as query constraints | Collection operations degrade to denial or to post-retrieval filtering, defeating the platform's enforcement model | Treat constraint generation as an acceptance gate for any backend; settle the mechanism in DESIGN before committing to a backend |
| A pluggable backend is nominally neutral but the first backend's language shapes the contracts | The platform acquires a de facto policy language, which an accepted architecture decision explicitly rejected | Validate the backend contract against a second, structurally different backend before declaring it stable |
| Gear latency or unavailability propagates to every enforced operation | Platform-wide denial of service, since the gear fails closed by design | Hold the latency and availability targets as release gates; keep hierarchy caching on the critical path; measure at the gear boundary |
| Policy inheritance behaves wrongly at self-managed tenant boundaries | An ancestor guardrail silently stops applying, or a delegated tenant's policy reaches beyond its boundary; both are invisible until a decision is questioned | Make barrier behaviour explicit per assignment rather than a global default; treat crossings as a test surface, not a configuration detail |
| Policy content growth makes evaluation cost scale with tenant count | Latency target becomes unreachable at large deployments | Bound bundle size and applicable-set size; measure evaluation cost against tenant and document count, not only against request rate |
| The gear accumulates governance responsibilities faster than consumers adopt it | Specified surface with no users, and requirements shaped by speculation rather than integration | Keep authorization as the only first-release consumer; admit further consumers only with a concrete integration |
| Decision semantics are settled during implementation rather than specified | Behaviour at the edges — the terminal decision set, whether a deferral outcome exists, how precedence rules interact — is discovered only when someone questions a decision, and cannot be changed once relied upon | Fix the decision set, the deferral outcome, and the precedence interaction in DESIGN as inputs to implementation, not outputs of it |
| Obligations and structured reason codes exceed what an available backend can carry out of an evaluation | Scope is understated and surfaces late as rework, or the requirements are quietly dropped | Verify both against a candidate backend before DESIGN; the affected requirements are prioritised p2 so the first release is not blocked on them |
| The choice to grow or not grow the authorization contract is deferred indefinitely | Deferral and obligations read as satisfied while being unreachable for the gear's primary consumer, and domain gears bypass the enforcement abstraction to reach them | Force the choice before the consumer bridge is specified; see Section 13 |

## 13. Open Questions

Each question carries an owner role and the point by which it must be answered. A question unanswered past its point blocks the artefact named beside it.

| Question | Owner | Needed by |
|---|---|---|
| Does the superseding ADR reconciling this gear with ADR-0001 get written, and does it hold? The gear's positioning is provisional until it exists. | Steering committee | Before DESIGN |
| Does the authorization contract grow to carry deferral and obligations, or are they confined to consumers calling this gear directly? Growing it changes a shared SDK with existing plugins behind it; confining them leaves the primary consumer unable to express two outcomes and invites gears to route around the enforcement abstraction. | Authorization owner | Before the consumer bridge is specified |
| Does a pluggable backend satisfy the intent of the decision against policy-language lock-in, or does that decision need revisiting now that a policy engine is a platform gear rather than a vendor plugin? | Steering committee | Before DESIGN |
| Should an assignment be inheritable across a self-managed tenant barrier, and under what caller conditions? The platform treats barriers as context-dependent, so a single default is wrong for either operator guardrails or delegated policy. | Platform architecture | Before DESIGN |
| Can a bundle be assigned to a resource group rather than only to a tenant? Groups are a targeting dimension today, which keeps assignment aligned with the tenant tree. | Gear owner | Before first release |
| What are the bounds on bundle size, document count per bundle, and applicable-set size per evaluation? These are user-visible limits that shape how policy can be organised. | Gear owner | Before first release |
| Where do deferred outcomes terminate? The approval service is the natural destination, but nothing connects them and that gear is currently a stub. | Gear owner | Before the deferral outcome ships |
| Does this gear evaluate the platform's existing permission instances, or does a separate backend or gear own that? | Authorization owner | Before DESIGN |
| Does first release specify the full trigger type and phase surface while implementing only the operation-triggered blocking path, or narrow the specification to what ships? | Gear owner | Before DESIGN |

## 14. Traceability

Downstream artifacts for this gear are not yet written. When they are, they belong at `DESIGN.md`, `ADR/`, and `features/` alongside this document.

- **Authorization model**: [arch/authorization/DESIGN.md](../../../../docs/arch/authorization/DESIGN.md), [ADR-0001](../../../../docs/arch/authorization/ADR/0001-pdp-pep-authorization-model.md)
- **Enforcement contract**: [authz-resolver](../../authz-resolver/), consumed through the `pe-authz-plugin` bridge specified separately
- **Platform architecture**: [ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md), [GEARS.md](../../../../docs/GEARS.md)
- **Comparable gear**: [CredStore](../../../credstore/docs/PRD.md), for the local-implementation-plus-plugin pattern this gear follows
- **Adjacent future work**: [PERMISSION_GTS_TYPE.md](../../../../docs/arch/authorization/PERMISSION_GTS_TYPE.md) names an AuthZ management gear owning grants, role types, and bindings — the closest existing pointer to the capability this gear provides
- **Existing policy subsystems**: [quota-enforcement](../../quota-enforcement/docs/PRD.md) and [event-broker](../../event-broker/docs/PRD.md), whose pluggable evaluation registries this gear follows rather than replaces
- **Standards lineage**: the PDP, PEP, PAP, and PIP vocabulary and the constraint-bearing evaluation model derive from NIST SP 800-162 and OpenID AuthZEN 1.0 by way of ADR-0001


