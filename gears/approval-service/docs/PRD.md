---
description: "Product requirements for the Approval Service gear: synchronous approval verdicts for gear operations on registered resource projections, governed by layered firewall-style rules, approval policies with approvers, auto-approval rules, and decisions across the tenant hierarchy."
---

# PRD — Approval Service

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
  - [1.5 Coexistence with BSS Approval Library](#15-coexistence-with-bss-approval-library)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
  - [4.3 Future Scope](#43-future-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Registration](#51-registration)
  - [5.2 Governance](#52-governance)
  - [5.3 Decisions and Requests](#53-decisions-and-requests)
  - [5.4 Verification](#54-verification)
  - [5.5 Administration and Audit](#55-administration-and-audit)
  - [5.6 Extended Governance (p2, p3)](#56-extended-governance-p2-p3)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [Gear Gates an Operation and the User Asks for Approval](#gear-gates-an-operation-and-the-user-asks-for-approval)
  - [Purchase Order Release](#purchase-order-release)
  - [Pull Request Merge](#pull-request-merge)
  - [Model Access Approved by the Model Owner](#model-access-approved-by-the-model-owner)
  - [Repository Read and Write Access](#repository-read-and-write-access)
  - [Task Waits for Approval](#task-waits-for-approval)
  - [Order Approval with Escalation](#order-approval-with-escalation)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

The Approval Service answers one question synchronously for any gear: may this subject perform this operation on this resource now? Gears call it on every protected request, through an SDK with a client-side cache, and receive a verdict. The service owns approval governance: the catalog of approvable resources, the rules that decide whether approval is required, the approval policies that say who must approve, the auto-approval rules that approve without human review, the decisions approvers record, the pending approval requests, and the audit of all of it.

The service never executes, orchestrates, or waits for the protected operation. When a gear needs to wait for an approval, it handles the waiting itself, by polling the verdict or by reacting to the service's change events.

### 1.2 Background / Problem Statement

Gears need approval gates for high-impact actions such as using an AI model, task execution approval, releasing a purchase order, publishing a price change in Pricing, approving an order before fulfillment in Orders Workflow, etc. Without a shared service, each gear implements its own approval state, approver routing, administration, hierarchy handling, and audit.

Approval is not authorization. The platform PDP decides who may call which operation. Approval is an additional governance layer: an approver can prohibit a specific model even when authorization allows all models. Gears need both checks, from two independent sources.

Platform vendors assemble gears into products and must decide which gear operations require approval in their product through configuration file without changing gear code. Tenant administrators must then manage approvals, approvers, and automation at runtime, within the limits their ancestors set.

### 1.3 Goals (Business Outcomes)

- A gear adds an approval gate with one SDK call per protected request and no approval logic of its own.
- A platform vendor sets approval defaults for a product through approval service configuration file only, with no gear code changes.
- A tenant administrator defines who approves what, and which requests approve automatically, without redeploying anything.
- Every verdict is explainable: it names the rule, policy, auto-approval rule, or decision that produced it.
- Every governance change is auditable: who changed what, when, and why.

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Approval Opportunity | A gear operation that may require approval, registered by the gear as a GTS type. It declares the accepted resource projection types, subject types, optional context attributes, its decision authority, whether standing approvals are allowed, and a mandatory default action. |
| Decision Authority | Where approve decisions for an opportunity may be recorded: at the subject tenant or its ancestors (default), or only at the resource tenant or its ancestors (owner-only). |
| Resource Projection | A presentation of an external resource with a stable contract specifically for the Approval Service. It doesn't contain internal resource fields the Approval Service doesn't need to have access to. It has the following attributes: Resource Projection Type (GTS), resource tenant, resource ID, and version (string, empty by default), display name.
| Resource Tenant | The tenant that owns the resource. Part of the projection identity. |
| Subject | Who needs the approval: subject type (GTS), subject tenant, and subject ID. Examples: a user, a service account, a tenant. |
| Requester | The authenticated identity on whose behalf the client gear made the verification call that opened a request. It can differ from the subject, for example when the subject is a tenant. |
| Subject Tenant | The tenant of the subject. Verdict resolution walks the tenant hierarchy from the subject tenant upward. |
| Scope Tenant | The tenant at which a rule, policy, auto-approval rule, or decision is recorded. Rules, policies, and auto-approval rules apply to that tenant and its descendants; a decision applies to its subject. |
| Context Attributes | Values the gear passes on a verification call, as declared by the opportunity, for example an order amount or a list of changed paths. Used in conditions and shown to approvers. |
| Context Fingerprint | A digest of the context attributes of a verification call. A request and the approve decisions recorded against it are bound to the fingerprint of the request. |
| Rule | A firewall-style statement that maps an opportunity pattern and a resource selector to an action: `deny`, `require_approval`, or `not_required`. |
| Rule Layer | The source of a rule: approval opportunity defined default value, approval service configuration file, or runtime rule stored in the service. |
| Approval Policy | A statement of what approval must consist of: a minimum number of approvals, mandatory approvers, and optionally a narrowed list of approvers, for an opportunity pattern and resource selector, optionally with a condition. |
| Approver | Any user whom the PDP grants the approve permission for the opportunity in the scope tenant. A policy can narrow which approvers count toward it. |
| Check Permission | The PDP permission to ask for a verdict on an opportunity. It never allows deciding. |
| Approve Permission | The PDP permission to record approve and deny decisions on an opportunity. It is separate from the check permission. |
| Auto-Approval Rule | A rule that approves a request without human review when its condition holds. |
| Decision | An `approve` or `deny` for one opportunity, one resource projection, and one subject, with a mandatory reason. It may reference an open approval request. |
| Standing Approval | An approve decision recorded without referencing a request. It is not bound to a context fingerprint and is allowed only when the opportunity allows standing approvals. |
| Approval Request | A record that a subject asked for approval of an opportunity on a resource projection and was told approval is required. |
| Verdict | The result of a verification call: an outcome, a reason code, `mayProceed`, and what produced it. |
| Approval Lease | The time during which a caller may rely on an `approved` or `not_required` verdict without verifying again. |
| Request Reconciliation | Re-evaluation of open requests after a committed change, closing those whose verdict is no longer `pending`. |

### 1.5 Coexistence with BSS Approval Library

BSS gears (Pricing, Products, Orders Workflow) use the embedded library `cf-gears-bss-approval` (`gears/bss/libs/approval`). The Approval Service **MUST** coexist with it: the two share no data and do not call each other. New gears that need approval gates use the Approval Service. Replacing the library is future scope (§4.3).

## 2. Actors

### 2.1 Human Actors

#### Gear Vendor

**ID**: `cpt-cf-approval-service-actor-gear-vendor`

- **Role**: Builds gears and registers the approval opportunities, projection types, subject types, and context attributes of their gear.
- **Needs**: Gate operations with one call, ship a sensible default action per opportunity, and keep no approval logic in the gear.

#### Platform Vendor

**ID**: `cpt-cf-approval-service-actor-platform-vendor`

- **Role**: Assembles gears into a product and sets product-wide rules and policies in the approval service configuration file.
- **Needs**: Override gear defaults per product without changing gear code.

#### Target Platform Administrator

**ID**: `cpt-cf-approval-service-actor-admin`

- **Role**: Operates a product and manages approval governance for a tenant at runtime: rules, policies, auto-approval rules, and standing decisions.
- **Needs**: Browse approvable resources and requests, define who approves and what approves automatically, approve or deny individually or in bulk, see why a verdict was given, and review the audit history.

#### Approver

**ID**: `cpt-cf-approval-service-actor-approver`

- **Role**: Holds the approve permission for an opportunity and decides on its requests. Can be a business user, such as a finance manager, not only an administrator.
- **Needs**: See the requests they may decide, with their context attributes, and approve or deny with a reason.

#### End User or API Client

**ID**: `cpt-cf-approval-service-actor-requester`

- **Role**: Attempts a protected operation through a gear. The gear asks for the verdict on the user's behalf.
- **Needs**: Learn whether the operation may proceed, and that an approval request has been raised when approval is required.

### 2.2 System Actors

#### Integrated Gear

**ID**: `cpt-cf-approval-service-actor-integrated-gear`

- **Role**: Registers opportunities and resource projections, and asks for a verdict before each protected operation.
- **Needs**: A low-latency, fail-closed verdict that cannot be bypassed by stale cache entries beyond a bounded delay.

#### Group Source

**ID**: `cpt-cf-approval-service-actor-group-source`

- **Role**: The Resource Group gear, which may be backed by identity provider synchronization. Provides group membership for group approvers and subject groups (p2).

## 3. Operational Concept & Environment

Platform runtime, architecture, and baselines are defined in [ARCHITECTURE_MANIFEST.md](../../../docs/ARCHITECTURE_MANIFEST.md), [guidelines](../../../guidelines/README.md), and the [GTS guideline](../../../guidelines/GTS.md). Gear-specific concept:

1. A gear registers its approval opportunities, each with its accepted projection types, subject types, context attributes, and default action.
2. A platform vendor and approval service operator adds configuration rules and policies in the approval service config file. Tenant administrators add runtime rules, policies, and auto-approval rules.
3. On every protected request, the gear asks for a verdict for (opportunity, subject, resource projection, context attributes), optionally asking the service to register the projection and raise an approval request.
4. The service resolves the effective rule and, when approval is required, the deny decisions, auto-approval rules, applicable policies, and approve decisions, and returns the verdict.
5. Approvers see the requests they may decide and record decisions. The next verdict reflects them, subject to the bounded SDK cache delay.

The service **MUST NOT** perform, orchestrate, or wait for the protected operation, and **MUST NOT** act as an authorization data source.

## 4. Scope

### 4.1 In Scope

- **p1**: opportunity registration with mandatory default action, mandatory standing approval setting, context attributes, and decision authority; resource projections; subjects; layered firewall-style rules; separate check and approve permissions; live approval policies with minimum approvals, mandatory approvers, and narrowed approver lists; decisions with mandatory reason and separation of duties; approval requests raised by verification, bound to context fingerprints; request reconciliation; synchronous explainable verification and batch verification; correlation ID and decision reasons in verdicts; approval leases; SDK with bounded cache; tenant hierarchy; administrator browsing and approver inbox; bulk decisions; audit trail; durable change and request events.
- **p2**: auto-approval rules; conditional policies; staged policies; group approvers; policy snapshot per request; subject-type selectors on rules; request escalation; decisions on behalf of an approver; subject group decisions; decision expiry; access review report.
- **p3**: periodic recertification campaigns; delegable (overridable) ancestor rules.

### 4.2 Out of Scope

- Executing, orchestrating, or tracking the protected operation.
- Waiting or parking on behalf of a gear.
- Authorization: the PDP **MUST NOT** use approvals as a data source, and approvals never grant beyond what the PDP allows.
- Storing internal resource fields beyond the declared context attributes of a request.
- Managing groups; the Group Source owns membership.
- Notification delivery, reminders, and escalation timers.
- Delegation and proxy approvals.
- Electronic signatures on decisions.

### 4.3 Future Scope

Candidates without a committed priority:

- Replacing `cf-gears-bss-approval` (`cpt-cf-approval-service-upreq-bss-library-parity`), after assessing multi-item approval units, exclusion of all item authors, and the BSS Approvals Inbox feed.
- A decision covering all versions of a resource.
- Rule hygiene: warnings for shadowed or redundant rules, and simulation of a proposed rule, policy, or auto-approval change.
- Break-glass: time-limited, audited emergency bypass with alerting.
- Audit export to a SIEM.
- Per-tenant quotas on projections and requests.
- Resources shared across tenant branches, where the resource tenant is not on the subject tenant's ancestor path. Owner-only decisions apply only along that path.

## 5. Functional Requirements

### 5.1 Registration

#### Register Approval Opportunity

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-register-req-type`

The system **MUST** let a gear register approval opportunities as GTS types. Each opportunity **MUST** declare its accepted projection types, its accepted subject types, its context attributes (possibly none), its decision authority (subject tenant by default, or resource tenant), whether standing approvals are `allowed` or `prohibited`, and exactly one default action (`deny`, `require_approval`, or `not_required`). Registration without a default action or without the standing approval setting **MUST** be rejected. When an update changes the setting to `prohibited`, existing standing approvals for the opportunity **MUST** stop applying.

The system **MUST** let only the owning gear's service identity register or update its opportunities.

- **Rationale**: A mandatory default makes every verdict resolvable without any hard-coded fallback. A mandatory standing approval setting makes gear vendors decide whether an approval may outlive the facts of a request: acceptable for `repo.read`, not for `po.release`.
- **Actors**: `cpt-cf-approval-service-actor-gear-vendor`, `cpt-cf-approval-service-actor-integrated-gear`
- **Covers**: `cpt-cf-approval-service-upreq-register-resource`

#### Resource Projection

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-resource-projection`

The system **MUST** store resource projections identified by projection type (GTS), resource tenant, resource ID (string), and version (string, empty when the gear does not version the resource). Each projection **MUST** carry a display name.

The system **MUST NOT** store internal resource fields in a projection.

A projection **MUST** be registered either by an explicit registration call or by a verification call with the register option. Re-registration **MUST** update the display name. A gear **MUST** register projections only for projection types it owns.

The system **MUST** let a gear deregister a projection. Decisions and requests for a deregistered projection **MUST** be archived and **MUST NOT** become active again if the same identity is registered later.

- **Rationale**: Administrators need a catalog of approvable resources without the service duplicating gear data. A different version, such as a new revision, commit SHA, or content hash, is a different projection, so an approval of one version never covers another.
- **Actors**: `cpt-cf-approval-service-actor-integrated-gear`, `cpt-cf-approval-service-actor-admin`
- **Covers**: `cpt-cf-approval-service-upreq-register-resource`

#### Subject

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-subject`

The system **MUST** identify the subject of every verdict, request, and decision by subject type (GTS), subject tenant, and subject ID. The gear chooses the granularity by the subject it sends, for example a user or a tenant.

Subject records **MUST** hold identifiers only.

- **Rationale**: Approvals are about who may do what. Identifiers only keep personal data out of the service.
- **Actors**: `cpt-cf-approval-service-actor-integrated-gear`

#### Context Attributes

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-context-attributes`

The system **MUST** accept context attributes on a verification call and **MUST** validate them against the opportunity's declaration; a mismatch returns `invalid`. Context attributes **MUST** be stored only with the approval request they open, shown to its approvers, and recorded in audit. They **MUST NOT** be stored on the projection.

The system **MUST** bind every request, and every approve decision recorded against it, to the request's context fingerprint; deny decisions and standing approvals are unbound. A request or bound decision whose fingerprint differs from the call's **MUST** be inapplicable, and the verdict follows normal precedence (`cpt-cf-approval-service-fr-verdict-resolution`); if approval is still required and unsatisfied, the outcome is `approval_required` with reason `context_changed`. With the register option, the mismatched request **MUST** close with `context_changed`, and a new one opens only if approval is still required and unsatisfied.

- **Rationale**: Approvers need the facts of a request, such as an order amount, and conditions need values to evaluate, without the service mirroring gear data.
- **Actors**: `cpt-cf-approval-service-actor-integrated-gear`, `cpt-cf-approval-service-actor-approver`

### 5.2 Governance

#### Approval Rules

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-approval-rules`

The system **MUST** support rules, each with a scope tenant, an opportunity pattern, a resource selector (a projection type pattern or one specific projection), and an action: `deny`, `require_approval`, or `not_required`. Patterns **MUST** follow GTS wildcard and version-matching semantics.

Administrators **MUST** be able to create, change, and delete runtime rules. Every runtime rule change **MUST** record a reason.

- **Rationale**: Firewall-style rules let administrators state broad defaults and narrow exceptions, for example `gts.*` requires approval while one type does not.
- **Actors**: `cpt-cf-approval-service-actor-admin`, `cpt-cf-approval-service-actor-platform-vendor`

#### Rule Layers

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-rule-layers`

The root tenant has three rule layers: runtime rules, approval service configuration file rules, and gear defaults (from opportunity registration). Descendant tenants **MUST** have runtime rules only.

The system **MUST** apply the layers one by one, in this order:

1. Runtime rules (existing in approval service database). If at least one matches, the most specific matching runtime rule decides, and the next layers are not checked.
2. Configuration file rules, only if no runtime rule matches. If at least one matches, the most specific one decides.
3. The gear default, only if no rule of the previous layers matches. It matches every projection type the opportunity accepts, so a decision always exists.

Layers **MUST** be read at verification time and **MUST NOT** be copied into one another. Deleting a runtime rule **MUST** restore the effect of the configuration and gear layers.

- **Rationale**: Gear upgrades and product configuration changes take effect without data migration, and runtime changes are reversible.
- **Actors**: `cpt-cf-approval-service-actor-gear-vendor`, `cpt-cf-approval-service-actor-platform-vendor`, `cpt-cf-approval-service-actor-admin`

#### Approval Policies

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-approval-policies`

The system **MUST** support approval policies, each with a scope tenant, an opportunity pattern, a resource selector, a minimum number of approvals, a list of mandatory approvers (users), an optional list of approvers (users), and an optional maximum approval lease (`cpt-cf-approval-service-fr-approval-lease`). Without an approver list, every holder of the approve permission counts toward the policy; with one, only the listed approvers count.

A policy **MUST** be satisfied only when approve decisions from at least the minimum number of distinct counting approvers exist, and every mandatory approver has approved.

Policies **MUST** be definable in the approval service configuration file and at runtime. Every applicable policy of the subject tenant and its ancestors **MUST** be satisfied (logical AND); descendants **MUST** be able to add policies and **MUST NOT** be able to remove or weaken ancestor policies. Every runtime policy change **MUST** record a reason.

In p1, policies are live: verification and open requests **MUST** use the currently applicable policies. A policy change **MUST NOT** revoke or re-request decisions; recorded approvals keep counting toward the current policies. An open request whose verdict is no longer `pending`, for example after a relaxation, **MUST** be closed by request reconciliation (`cpt-cf-approval-service-fr-request-reconciliation`), with the policy change named in audit. Policy snapshots (`cpt-cf-approval-service-fr-policy-snapshot`, p2) replace this behavior.

- **Rationale**: Purchase orders, pull requests, and production changes need four-eyes approval and mandatory sign-off, such as a finance lead or a repository owner. Live policies keep p1 simple: an open request gets the same verdict as a new one.
- **Actors**: `cpt-cf-approval-service-actor-admin`, `cpt-cf-approval-service-actor-platform-vendor`
- **Covers**: `cpt-cf-approval-service-upreq-multi-party-gates`

#### Group Approvers

- [ ] `p2` - **ID**: `cpt-cf-approval-service-fr-group-approvers`

The system **MUST** let approver lists and mandatory approvers of a policy name groups from the Group Source. A mandatory group **MUST** be satisfied by an approval from any one of its members.

- **Rationale**: Code owners, finance teams, and security teams change membership often; policies reference the team, not its members.
- **Actors**: `cpt-cf-approval-service-actor-admin`, `cpt-cf-approval-service-actor-group-source`

#### Policy Snapshot per Request

- [ ] `p2` - **ID**: `cpt-cf-approval-service-fr-policy-snapshot`

When a request opens, the system **MUST** fix the applicable policies and their versions for that request. Later policy changes **MUST NOT** change what that request must satisfy, and verification of its target **MUST** use the fixed policies. This replaces the p1 live policy behavior for open requests (`cpt-cf-approval-service-fr-approval-policies`). Rules and deny decisions **MUST** still apply immediately.

- **Rationale**: Reviewers of an open purchase order or pull request must not see the required approvals change under them, and a loosened policy must not silently approve requests in flight.
- **Actors**: `cpt-cf-approval-service-actor-admin`, `cpt-cf-approval-service-actor-approver`

#### Subject-Type Selectors

- [ ] `p2` - **ID**: `cpt-cf-approval-service-fr-subject-type-selector`

The system **MUST** let rules, policies, and auto-approval rules carry an optional subject type pattern, which takes part in matching and specificity after the resource selector and the opportunity pattern.

- **Rationale**: Some operations need approval for users but not for service accounts or automation identities, or the reverse.
- **Actors**: `cpt-cf-approval-service-actor-admin`, `cpt-cf-approval-service-actor-platform-vendor`

#### Auto-Approval Rules

- [ ] `p2` - **ID**: `cpt-cf-approval-service-fr-auto-approval`

The system **MUST** let administrators create, change, enable, disable, and delete auto-approval rules, each with a scope tenant, an opportunity pattern, a resource selector, and an optional condition. A rule without a condition matches unconditionally. Every change **MUST** record a reason.

Conditions **MUST** be expressions in a sandboxed expression language (see DESIGN) over the opportunity, subject, projection, and context attributes. A condition that fails to evaluate **MUST** be treated as not matching.

Auto-approval **MUST** apply only where approval is required and **MUST NOT** override a `deny` rule or a deny decision. A matching auto-approval rule is an authorized exception that satisfies only policies whose scope tenant is the rule's scope tenant or one of its descendants; a policy of an ancestor of the rule's scope tenant **MUST** still be satisfied by its own approvals or by an auto-approval rule of its own tenant or above. A policy **MUST** be able to forbid auto-approval entirely (human approval only). An ancestor **MUST** be able to disable auto-approval for its descendants.

An auto-approved verdict **MUST** name the auto-approval rule. When the verification call has the register option, the system **MUST** record the automatic approval in audit.

- **Rationale**: Manual approval of every request does not scale. Administrators build automatic policies such as "approve purchase orders below 1,000" or "approve read access to non-production repositories" while staying auditable and within ancestor limits.
- **Actors**: `cpt-cf-approval-service-actor-admin`
- **Covers**: `cpt-cf-approval-service-upreq-auto-approval`, `cpt-cf-approval-service-upreq-threshold-evaluation`

#### Conditional Policies

- [ ] `p2` - **ID**: `cpt-cf-approval-service-fr-conditional-policies`

The system **MUST** let an approval policy carry an optional condition, in the same expression language and over the same inputs as auto-approval rules. A policy applies only when its condition holds.

- **Rationale**: Approvers often depend on facts of the request, for example finance approval above an order amount, or a security team when infrastructure paths change.
- **Actors**: `cpt-cf-approval-service-actor-admin`
- **Covers**: `cpt-cf-approval-service-upreq-threshold-evaluation`

#### Staged Policies

- [ ] `p2` - **ID**: `cpt-cf-approval-service-fr-staged-policies`

The system **MUST** let a policy define ordered stages, each with its own minimum approvals, mandatory approvers, and optional approver list. The policy is satisfied when every stage is satisfied in order: an approve decision counts toward a stage only if recorded after the previous stage was satisfied. A pending verdict and the approver inbox **MUST** show only the current stage's requirements to its approvers.

- **Rationale**: Some gates must be ordered, such as legal review after finance approval of an order.
- **Actors**: `cpt-cf-approval-service-actor-admin`, `cpt-cf-approval-service-actor-approver`
- **Covers**: `cpt-cf-approval-service-upreq-staged-gates`

### 5.3 Decisions and Requests

#### Approval Requests

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-create-request`

When a verification call with the register option results in approval being required, the system **MUST** register the projection and open an approval request for (opportunity, projection, subject), recording the requester, the context attributes, and an optional justification supplied by the caller. At most one open request **MUST** exist per (opportunity, projection, subject); repeated calls **MUST** return the existing request and publish no second request-opened event, so this key is the request's idempotency key.

A request **MUST** close when its verdict is no longer `pending`, when the requester, the gear that opened it, or an administrator withdraws it, or when its projection is deregistered. The verdict of a request **MUST** be evaluated with its own subject, projection, and stored context attributes. Closed requests **MUST NOT** appear in approver inboxes. The requester and the gear that opened a request **MUST** be able to read it and its current outcome by request ID.

- **Rationale**: Asking for approval is a regular verdict call, so end users can request access through any gear without a separate flow. A gear that waits on request events, such as a Task Engine worker that receives only the event header, reads the outcome by ID; a gear closes the requests of a superseded version, such as an amended order.
- **Actors**: `cpt-cf-approval-service-actor-requester`, `cpt-cf-approval-service-actor-integrated-gear`
- **Covers**: `cpt-cf-approval-service-upreq-lifecycle`, `cpt-cf-approval-service-upreq-approval-request`, `cpt-cf-approval-service-upreq-request-read`, `cpt-cf-approval-service-upreq-gate-cancel`

#### Request Reconciliation

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-request-reconciliation`

After every committed change that can alter verdicts (decision, rule, policy, auto-approval rule, opportunity update, projection deregistration), the system **MUST** close each affected open request whose verdict is no longer `pending`, with the outcome, cause, audit record, and request event.

- The request of a decision's own (opportunity, projection, subject) **MUST** be re-evaluated in the decision's transaction; other affected requests after commit, within `cpt-cf-approval-service-nfr-reconciliation-delay`.
- Reconciliation **MUST** be idempotent and resumable, and close each request at most once.
- Verification without the register option **MUST** stay read-only and return the live verdict, reconciled or not.

- **Rationale**: One broad change can affect thousands of open requests; asynchronous reconciliation keeps changes small and verification read-only.
- **Actors**: `cpt-cf-approval-service-actor-admin`, `cpt-cf-approval-service-actor-integrated-gear`
- **Covers**: `cpt-cf-approval-service-upreq-lifecycle`, `cpt-cf-approval-service-upreq-status-changed-event`

#### Record Decision

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-submit-decision`

The system **MUST** let any user who holds the approve permission for the opportunity in the scope tenant, as decided by the PDP for that opportunity and projection, record an `approve` or `deny` decision for (opportunity, projection, subject). A decision **MAY** reference the open request for that (opportunity, projection, subject), which binds an approve to the request's context fingerprint. An approve without a request (a standing approval) **MUST** be rejected unless the opportunity allows standing approvals; a deny never needs a request. An approve decision counts toward every applicable policy that has no approver list or lists its actor.

The scope tenant of a decision **MUST** follow the opportunity's decision authority:

- **Subject tenant** (default): the subject tenant or one of its ancestors.
- **Resource tenant** (owner-only): for approve decisions, the resource tenant or one of its ancestors only, and only when the resource tenant is the subject tenant or one of its ancestors; otherwise approval cannot be granted (cross-branch sharing is future scope). Deny decisions remain possible at the subject tenant or its ancestors, because restricting is always allowed within a tenant's own scope.

Every decision **MUST** record the actor, the scope tenant, the time, and a mandatory reason.

Decisions **MUST** follow this lifecycle:

| From | To | Trigger |
|------|----|---------|
| — | `active` | An approver records `approve` or `deny`. |
| `active` | `superseded` | The same approver records a new decision for the same (opportunity, projection, subject, scope tenant); the new decision becomes `active`. |
| `active` | `revoked` | The author withdraws it, or a holder of the administer permission revokes it, with a reason. |
| `active` | `expired` | Its expiry time passes (p2). |
| `active`, `superseded`, `revoked`, `expired` | `archived` | The projection is deregistered. |

Only `active` decisions apply. At most one `active` decision **MUST** exist per approver and (opportunity, projection, subject, scope tenant). An approver **MUST** only supersede their own decision; another approver's deny **MUST** be revoked before it stops applying. Between approvers, any applicable active deny wins over any number of approvals. Every transition **MUST** trigger reconciliation of affected open requests (`cpt-cf-approval-service-fr-request-reconciliation`); a later verification with the register option opens a new request.

The system **MUST** reject a decision whose actor is the subject or the requester.

- **Rationale**: Who may approve is an authorization question, answered by the PDP per opportunity, so business approvers need no special role in this service. Owner-only authority lets a resource owner, such as the tenant that owns a shared model, keep approval of inherited resources to itself.
- **Actors**: `cpt-cf-approval-service-actor-approver`, `cpt-cf-approval-service-actor-admin`
- **Covers**: `cpt-cf-approval-service-upreq-lifecycle`

#### Decisions on Behalf of an Approver

- [ ] `p2` - **ID**: `cpt-cf-approval-service-fr-on-behalf-decision`

The system **MUST** let a gear's service identity record or revoke a decision on behalf of a named user, when the PDP permits the gear to do so and the named user holds the approve permission. The decision **MUST** record both identities.

- **Rationale**: Reviews made in an external system, such as GitHub pull request reviews, are mirrored by a gear as decisions of the actual reviewer. A gear that keeps its own approval screens, such as Model Registry, routes its administrators' approve, reject, and revoke actions here.
- **Actors**: `cpt-cf-approval-service-actor-integrated-gear`
- **Covers**: `cpt-cf-approval-service-upreq-on-behalf-decision`

#### Bulk Decisions

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-bulk-approve`

The system **MUST** let a holder of the approve permission record decisions for a bounded list of (projection, subject) pairs for one opportunity in one atomic operation: all are recorded or none. Each item **MAY** reference an open request; when the opportunity prohibits standing approvals, every approve item **MUST** reference one. On failure the response **MUST** list each item with its reason. Each item **MUST** produce its own audit record and change event.

- **Rationale**: Tenant onboarding approves a predefined set of resources at once.
- **Actors**: `cpt-cf-approval-service-actor-admin`
- **Covers**: `cpt-cf-approval-service-upreq-bulk-approve`

### 5.4 Verification

#### Verify Approval

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-verify-approval`

The system **MUST** expose a synchronous verification call taking an opportunity, a subject, a resource projection, context attributes, and an optional register option. Without the register option the call **MUST** be read-only.

Verification **MUST** return one of these outcomes, each with a machine-readable reason code:

| Outcome | `mayProceed` | Meaning |
|---------|--------------|---------|
| `not_required` | `true` | The effective rule is `not_required`. |
| `approved` | `true` | Approval is required and is satisfied by decisions or an auto-approval rule. |
| `approval_required` | `false` | Approval is required, not satisfied, and no request with the call's context fingerprint is open. |
| `pending` | `false` | Approval is required, not satisfied, and a request with the call's context fingerprint is open. |
| `denied` | `false` | A `deny` rule or an applicable deny decision. |
| `invalid` | `false` | Unknown opportunity, an unaccepted projection or subject type, or invalid context attributes. |
| `unavailable` | `false` | The verdict cannot be established reliably (fail closed). |

Every verdict **MUST** identify what produced it: the rule (with layer and scope tenant), the auto-approval rule, or the decisions (with actor and time), and for `pending` the open request and its unmet policy requirements. Every `approved` and `not_required` verdict **MUST** carry an approval lease (`cpt-cf-approval-service-fr-approval-lease`). Every verdict **MUST** carry a version that increases monotonically whenever a change affecting it is committed, so callers can reject results older than one they have seen.

- **Rationale**: Verification is the only runtime integration point for gears and must be explainable to administrators, approvers, and users.
- **Actors**: `cpt-cf-approval-service-actor-integrated-gear`, `cpt-cf-approval-service-actor-requester`
- **Covers**: `cpt-cf-approval-service-upreq-query-status`, `cpt-cf-approval-service-upreq-requirement-verdict`

#### Batch Verification

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-batch-verify`

The system **MUST** verify a bounded list of resource projections, each with its context attributes, for one opportunity and one subject in a single read-only call, returning one verdict per item as if verified separately. The list limit **MUST** be set in configuration.

- **Rationale**: Gears that list approvable resources, such as Model Registry listing only the models that would resolve, filter a page in one call.
- **Actors**: `cpt-cf-approval-service-actor-integrated-gear`
- **Covers**: `cpt-cf-approval-service-upreq-batch-status`

#### Correlation and Decision Reason

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-correlation`

The system **MUST** accept a caller correlation ID on a verification call, store it with the request it opens, and echo it in verdicts and request events for that request. Verdicts produced by decisions **MUST** include the reasons of those decisions.

- **Rationale**: Process gears, such as Orders Workflow, correlate approvals with their own process instances and record why an approval was granted or denied.
- **Actors**: `cpt-cf-approval-service-actor-integrated-gear`
- **Covers**: `cpt-cf-approval-service-upreq-approval-request`, `cpt-cf-approval-service-upreq-decision-outcome`

#### Approval Lease

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-approval-lease`

The configuration **MUST** define a default and a maximum lease. An approver **MUST** be able to set a lease on an approve decision (the default otherwise), and a policy **MAY** set a maximum lease; both **MUST NOT** exceed the configured maximum.

The effective lease **MUST** be the shortest of: the configured maximum; every applicable policy maximum; and the time until the verdict would stop being satisfied, where each approve decision counts until its lease or (p2) expiry ends, and an auto-approval or `not_required` rule until the configured default ends. For example, mandatory approvers with 60 s and 10 min give 60 s; two required approvals out of 60 s, 10 min, and 10 min give 10 min.

A lease is not a grant: changes take effect in the service when committed, and only a new verification renews a lease. A gear **MUST** verify again before a protected operation once its lease has ended.

- **Rationale**: Bounds revocation delay by every approver's and policy's limit, while ignoring short leases of approvals the verdict does not depend on.
- **Actors**: `cpt-cf-approval-service-actor-integrated-gear`, `cpt-cf-approval-service-actor-approver`, `cpt-cf-approval-service-actor-admin`
- **Covers**: `cpt-cf-approval-service-upreq-status-changed-event`

#### Verdict Resolution

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-verdict-resolution`

The system **MUST** resolve a verdict as follows:

1. **Validate**: an unknown opportunity, an unaccepted type, or invalid context attributes return `invalid`.
2. **Effective action**: for each tenant from the subject tenant up to the root, select one rule at that level, checking layers one by one (`cpt-cf-approval-service-fr-rule-layers`): runtime rules first; at the root, configuration rules only if no runtime rule matches, then the gear default. A descendant level without a matching runtime rule selects nothing. Within a layer, the most specific matching rule wins: the resource selector is compared first, then the opportunity pattern; a specific projection is more specific than any type pattern, and a narrower GTS pattern is more specific than a wider one. On a tie, the stricter action wins. Across tenant levels, the strictest selected action wins: `deny`, then `require_approval`, then `not_required`.
3. **Action**: `deny` returns `denied`; `not_required` returns `not_required`.
4. For `require_approval`, using the applicable decisions (those on the subject itself and those on any tenant subject on the subject tenant's path to the root):
   1. Any applicable active deny decision returns `denied`.
   2. An approve decision bound to a request counts only with that request's context fingerprint; a standing approval counts only if the opportunity allows standing approvals.
   3. When policies apply: `approved` when every applicable policy is satisfied, by applicable approve decisions or by an auto-approval rule allowed to satisfy it (§5.2). When no policy applies: `approved` when an applicable active approve decision or a matching auto-approval rule exists.
   4. Otherwise `pending` when an open request has the call's fingerprint, else `approval_required` (reason `context_changed` if the open request has another fingerprint).

A context mismatch **MUST NOT** change this order. An approval recorded at the subject tenant **MUST** be able to satisfy a `require_approval` rule set by an ancestor. Decisions and auto-approval **MUST NOT** override a `deny` rule.

- **Rationale**: One deterministic order makes verdicts predictable and explainable. Within a tenant, specific exceptions work as in a firewall; across tenants, descendants can only restrict.
- **Actors**: `cpt-cf-approval-service-actor-integrated-gear`, `cpt-cf-approval-service-actor-admin`
- **Covers**: `cpt-cf-approval-service-upreq-tenant-hierarchy`, `cpt-cf-approval-service-upreq-query-status`

#### Tenant Hierarchy

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-tenant-hierarchy`

The system **MUST** apply the tenant hierarchy on every verification: ancestor rules, policies, auto-approval restrictions, and deny decisions bind descendants, and a decision on a tenant subject applies to every subject in that tenant and its descendants. The system **MUST** also work in a flat deployment with a single tenant level.

- **Rationale**: Multi-tenant platforms govern centrally while allowing local approvals within ancestor limits.
- **Actors**: `cpt-cf-approval-service-actor-platform-vendor`, `cpt-cf-approval-service-actor-admin`
- **Covers**: `cpt-cf-approval-service-upreq-tenant-hierarchy`

### 5.5 Administration and Audit

#### Administrator Browsing and Approver Inbox

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-administration`

The system **MUST** let authorized administrators list, within the tenants they manage: opportunities; projections by projection type and resource tenant; open requests by opportunity, projection, and subject; decisions; rules of all layers; policies; and auto-approval rules. The system **MUST** let an administrator see why a given verdict would be returned without side effects.

The system **MUST** give each approver a list of the open requests they hold the approve permission for, with context attributes, justification, and unmet policy requirements.

- **Rationale**: Administrators govern and approvers act; both need to understand verdicts.
- **Actors**: `cpt-cf-approval-service-actor-admin`, `cpt-cf-approval-service-actor-approver`

#### Authorization of Service Operations

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-validate-eligibility`

The system **MUST** authorize every operation through the platform PDP, with distinct permissions per opportunity GTS type:

- **check**: ask for a verdict;
- **request**: open an approval request through the register option;
- **approve**: record approve and deny decisions;
- **administer**: manage rules, policies, and auto-approval rules, and revoke decisions.

The check permission **MUST NOT** imply the approve permission. Registration, request withdrawal, and reads **MUST** also be authorized. Administration **MUST** be limited to the tenants an administrator manages.

- **Rationale**: Approval governance is meaningful only when its own operations are authorized. Separating check from approve lets every gear ask for verdicts while only designated users decide.
- **Actors**: `cpt-cf-approval-service-actor-admin`, `cpt-cf-approval-service-actor-approver`, `cpt-cf-approval-service-actor-integrated-gear`

#### Audit Trail

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-audit-trail`

The system **MUST** record every registration, rule, policy, and auto-approval change, decision, revocation, automatic approval, and request change in an append-only audit trail with actor, scope tenant, previous and new state, reason where required, and time.

- **Rationale**: Compliance and support need a complete history of who governed what and why.
- **Actors**: `cpt-cf-approval-service-actor-admin`
- **Covers**: `cpt-cf-approval-service-upreq-audit-trail`

#### Change Events

- [ ] `p1` - **ID**: `cpt-cf-approval-service-fr-event-notifications`

The system **MUST** publish an event for every change that can alter a verdict: decision recorded or revoked; rule, policy, or auto-approval change; request opened or closed; projection deregistered. Events **MUST** carry the affected opportunity, projection or pattern, subject when applicable, scope tenant, verdict version, and a deduplication key. Events **MUST** be recorded durably with their change (`cpt-cf-approval-service-nfr-atomicity`) and delivered to the Event Broker at least once after commit; consumers deduplicate by the key.

The system **MUST** publish request events, of GTS event types, when a request opens, receives a decision, is escalated (p2), and closes, whatever the cause of closing. Each request event **MUST** be published in the subject tenant and, when different, in the requester's tenant, with the opportunity GTS type as event subject type and the request ID as event subject ID, and **MUST** carry the resulting outcome, the closing reason, and the correlation ID. A gear holding the request ID can therefore wait for its approval by event type, subject type, and subject ID, for example with a Task Engine event wait, and read the outcome by request ID.

Events **MUST** only signal changes. They **MUST NOT** command or orchestrate gears.

- **Rationale**: Gears invalidate cached verdicts and stop waiting without polling. Events reach the tenant where the waiting gear runs, even when the decision was recorded at an ancestor, and a request never closes without an event, so no waiter is left hanging. Stakeholder notification delivery is built by consumers on these events.
- **Actors**: `cpt-cf-approval-service-actor-integrated-gear`
- **Covers**: `cpt-cf-approval-service-upreq-status-changed-event`, `cpt-cf-approval-service-upreq-stakeholder-notifications`, `cpt-cf-approval-service-upreq-request-event-wait`, `cpt-cf-approval-service-upreq-decision-outcome`

### 5.6 Extended Governance (p2, p3)

#### Request Escalation

- [ ] `p2` - **ID**: `cpt-cf-approval-service-fr-request-escalation`

The system **MUST** let the requester or the gear that opened a request escalate it while it is open, with a reason. A policy **MAY** name escalation recipients (users; groups with `cpt-cf-approval-service-fr-group-approvers`). Each escalation **MUST** increase the request's escalation level, be audited, publish a request-escalated event naming the recipients of the unmet policies, and mark the request as escalated in inboxes. Escalation **MUST NOT** change the verdict, close or reject the request, or grant the approve permission to recipients.

- **Rationale**: Process gears, such as Orders Workflow, own durable escalation timers; this service owns who is escalated to. Notification delivery is built on the event.
- **Actors**: `cpt-cf-approval-service-actor-integrated-gear`, `cpt-cf-approval-service-actor-admin`
- **Covers**: `cpt-cf-approval-service-upreq-escalation`

#### Subject Group Decisions

- [ ] `p2` - **ID**: `cpt-cf-approval-service-fr-subject-groups`

The system **MUST** let holders of the approve permission record decisions on a subject group from the Group Source. Gears **MUST** keep sending individual subjects and **MUST NOT** pass group information in verification calls. Verification **MUST** treat a decision on any group containing the subject as applicable. An applicable deny for the subject or any of its groups **MUST** win over any approve.

The separation-of-duties check **MUST** reject a group decision whose actor is a member of the group.

- **Rationale**: Administrators approve teams at once while gears stay unaware of groups.
- **Actors**: `cpt-cf-approval-service-actor-admin`, `cpt-cf-approval-service-actor-group-source`

#### Decision Expiry

- [ ] `p2` - **ID**: `cpt-cf-approval-service-fr-decision-expiry`

The system **MUST** let an approver or administrator set an expiry time on a decision. An expired decision **MUST** stop applying at verification time.

- **Rationale**: Time-limited access is a common governance need and supports access reviews.
- **Actors**: `cpt-cf-approval-service-actor-admin`, `cpt-cf-approval-service-actor-approver`

#### Access Review Report

- [ ] `p2` - **ID**: `cpt-cf-approval-service-fr-access-review`

The system **MUST** report the active approve decisions within a tenant scope with their age, actor, reason, and expiry.

- **Rationale**: SOX, ISO 27001, and SOC 2 expect standing access to be reviewed periodically.
- **Actors**: `cpt-cf-approval-service-actor-admin`

#### Recertification Campaigns

- [ ] `p3` - **ID**: `cpt-cf-approval-service-fr-recertification`

The system **MUST** let administrators run campaigns in which reviewers confirm or revoke active approvals in a scope by a deadline.

- **Rationale**: Formal recertification closes the access review loop.
- **Actors**: `cpt-cf-approval-service-actor-admin`

#### Delegable Ancestor Rules

- [ ] `p3` - **ID**: `cpt-cf-approval-service-fr-delegable-rules`

The system **MUST** let an ancestor mark a rule as overridable, so that descendant runtime rules can relax it.

- **Rationale**: Some platforms delegate governance to child tenants.
- **Actors**: `cpt-cf-approval-service-actor-admin`

## 6. Non-Functional Requirements

Project baselines apply ([ARCHITECTURE_MANIFEST.md](../../../docs/ARCHITECTURE_MANIFEST.md), [guidelines](../../../guidelines/README.md)). Gear-specific NFRs:

### 6.1 Gear-Specific NFRs

#### Verification Latency

- [ ] `p1` - **ID**: `cpt-cf-approval-service-nfr-latency`

The system **MUST** answer verification calls within 50 ms at p95 under nominal load, including policy, condition, and group membership evaluation. An SDK cache hit **MUST** make no network call.

- **Threshold**: 50 ms p95, service side, nominal load as defined in DESIGN.
- **Rationale**: Verification sits on the hot path of every protected request.
- **Covers**: `cpt-cf-approval-service-upreq-query-status`
- **Architecture Allocation**: See DESIGN.md § NFR Allocation

#### Verification Throughput

- [ ] `p1` - **ID**: `cpt-cf-approval-service-nfr-throughput`

The system **MUST** sustain 1,000 verification calls per second on average, served by the service without SDK caching, from 8 parallel test clients, on an average machine with its database holding 1,000,000 approval decisions, with both PostgreSQL and MariaDB.

- **Threshold**: 1,000 calls/s average, 8 parallel clients, 1,000,000 approval decisions stored, reference machine defined in DESIGN, PostgreSQL and MariaDB.
- **Rationale**: Verification runs on every protected request of every integrated gear and must not degrade as decisions accumulate.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation

#### Bounded Revocation Delay

- [ ] `p1` - **ID**: `cpt-cf-approval-service-nfr-revocation-delay`

The SDK **MUST** cache only `approved` and `not_required` verdicts, keyed by every verification input including the context fingerprint, and only until the verdict's approval lease ends. Calls with the register option **MUST NOT** be served from the cache. A revoked decision, a new deny, a stricter rule or policy, or a disabled auto-approval rule **MUST** therefore take effect for SDK callers no later than the end of the lease. Callers **MUST** be able to bypass the cache for a single call.

- **Threshold**: At most the approval lease; default set in configuration.
- **Rationale**: Caching must never let a revoked approval live on indefinitely.
- **Covers**: `cpt-cf-approval-service-upreq-status-changed-event`
- **Architecture Allocation**: See DESIGN.md § NFR Allocation

#### Change Atomicity

- [ ] `p1` - **ID**: `cpt-cf-approval-service-nfr-atomicity`

A decision, governance change, or request change **MUST** be committed in one transaction with its audit record and a durable event record, or not at all. Delivery to the Event Broker **MUST** happen after commit, at least once, outside that transaction.

- **Threshold**: Zero changes without an audit or durable event record under fault injection; every durable event record delivered once the Event Broker recovers.
- **Rationale**: Partial writes produce wrong verdicts and incomplete audit; durable event records avoid a distributed transaction with the broker.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation

#### Reconciliation Delay

- [ ] `p1` - **ID**: `cpt-cf-approval-service-nfr-reconciliation-delay`

Request reconciliation (`cpt-cf-approval-service-fr-request-reconciliation`) **MUST** close every affected open request whose verdict is no longer `pending` within a configured bound after the triggering change commits, under nominal load.

- **Threshold**: Configured bound; target value is an open question (§13).
- **Rationale**: Approver inboxes and waiting gears must learn soon after a change that a request is settled.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation

#### Fail-Closed Availability

- [ ] `p1` - **ID**: `cpt-cf-approval-service-nfr-availability`

Verification **MUST** return `unavailable` with `mayProceed = false` whenever the verdict cannot be established reliably, including when the Group Source cannot answer. The SDK **MUST** treat any error as refusal, except that a cached verdict within its approval lease **MUST** remain usable. The availability target is an open question (§13).

- **Threshold**: Zero `mayProceed = true` results on dependency failure other than cached verdicts within their lease.
- **Rationale**: An approval gate that fails open is no gate.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation

#### Request Data Classification

- [ ] `p1` - **ID**: `cpt-cf-approval-service-nfr-data-classification`

Context attributes and justifications **MUST** be tenant-scoped, readable only by the requester, holders of the approve permission for the opportunity, and administrators of the scope, and retained no longer than the audit retention period. Opportunities **MUST NOT** declare secrets or payment card data as context attributes.

- **Threshold**: Zero reads of request data outside those roles in authorization tests.
- **Rationale**: Requests carry business facts, such as amounts and vendors, that other tenants and users must not see.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### REST API

- [ ] `p1` - **ID**: `cpt-cf-approval-service-interface-rest-api`

- **Type**: REST API
- **Stability**: stable
- **Description**: Registration, verification, batch verification, requests (read, withdraw, escalate), decisions, bulk decisions, rules, policies, auto-approval rules, browsing, approver inbox, and audit reads.
- **Breaking Change Policy**: Major API version bump required.

#### Client SDK

- [ ] `p1` - **ID**: `cpt-cf-approval-service-interface-sdk`

- **Type**: Rust gear client (ClientHub)
- **Stability**: stable
- **Description**: One call returns the verdict and `mayProceed`, with context attributes, the register option, a correlation ID, batch verification, a client-side cache bounded by the approval lease, a per-call cache bypass, and fail-closed error handling. Gears keep no approval logic.
- **Breaking Change Policy**: Major version bump required.

### 7.2 External Integration Contracts

#### Approval Verification Contract

- [ ] `p1` - **ID**: `cpt-cf-approval-service-contract-verification`

- **Direction**: provided by library
- **Protocol/Format**: Verification request and verdict as defined in `cpt-cf-approval-service-fr-verify-approval`.
- **Compatibility**: New outcomes only in a new major version; callers treat unknown outcomes as refusal.

#### Change Event Contract

- [ ] `p1` - **ID**: `cpt-cf-approval-service-contract-events`

- **Direction**: provided by library
- **Protocol/Format**: Event Broker gear, GTS event types.
- **Compatibility**: Additive fields only within a major version.

#### Group Membership Contract

- [ ] `p2` - **ID**: `cpt-cf-approval-service-contract-group-source`

- **Direction**: required from client (Resource Group gear)
- **Protocol/Format**: Lookup of the groups containing a user or subject, and of the members of a group.
- **Compatibility**: Follows the Resource Group SDK versioning.

## 8. Use Cases

### Gear Gates an Operation and the User Asks for Approval

- [ ] `p1` - **ID**: `cpt-cf-approval-service-usecase-register-and-approve`

**Actor**: `cpt-cf-approval-service-actor-requester`

**Preconditions**:
- The gear registered the opportunity with default action `require_approval`; no policy applies.

**Main Flow**:
1. The user attempts the operation; the gear verifies with the register option.
2. The service registers the projection, opens a request, publishes a request event, and returns `pending`.
3. The gear refuses the operation and tells the user that approval was requested, or waits for the request event.
4. A holder of the approve permission sees the request in their inbox and approves with a reason.
5. The user retries, or the waiting gear wakes on the request-closed event; the gear verifies and receives `approved`.

**Postconditions**:
- The request is closed; the decision and request history are audited.

**Alternative Flows**:
- **Deny**: the approver records a deny decision; later verifications return `denied`.

### Purchase Order Release

- [ ] `p2` - **ID**: `cpt-cf-approval-service-usecase-admin-override`

**Actor**: `cpt-cf-approval-service-actor-admin`

**Preconditions**:
- The procurement gear registered opportunity `po.release` with default `require_approval`, standing approvals prohibited, projection = purchase order with the revision as version, subject = buyer, and context attributes amount, currency, vendor, and cost center.
- Finance managers hold the approve permission for `po.release`.

**Main Flow**:
1. The administrator creates an auto-approval rule "amount below 1,000" and a conditional policy "amount at least 10,000: two approvals, finance lead mandatory", each with a reason.
2. A purchase order of 500 is verified and returns `approved`, naming the auto-approval rule.
3. A purchase order of 5,000 opens a request; one finance manager approves; the verdict becomes `approved`.
4. A purchase order of 20,000 opens a request; it stays `pending` until two approvals including the finance lead exist.

**Postconditions**:
- Approvers saw the amount, vendor, and cost center; every approval is audited.

**Alternative Flows**:
- **Revision**: the buyer changes the order; the new revision is a new projection and needs approval again.
- **Amount changed without a new revision**: the context fingerprint changes; the open request closes with `context_changed`, approvals bound to it stop applying, and a new request opens unless the new amount is auto-approved.
- **Standing approval attempted**: a finance manager tries to approve without a request; rejected because `po.release` prohibits standing approvals.
- **Buyer approves own order**: rejected by separation of duties.
- **Policy tightened during review**: the open request keeps its fixed policies.

### Pull Request Merge

- [ ] `p1` - **ID**: `cpt-cf-approval-service-usecase-policy-approval`

**Actor**: `cpt-cf-approval-service-actor-approver`

**Preconditions**:
- The source control gear registered opportunity `pr.merge` with standing approvals prohibited, projection = pull request with the head commit SHA as version, subject = author.
- A policy for `pr.merge` on repository pull requests requires two approvals with the repository owner mandatory. Repository reviewers hold the approve permission for `pr.merge` on that repository.

**Main Flow**:
1. The gear verifies the merge with the register option; the service opens a request and returns `pending` with the unmet requirements.
2. Two reviewers approve; the verdict stays `pending` because the repository owner has not approved.
3. The repository owner approves; the next verification returns `approved`.

**Postconditions**:
- The verdict names the satisfied policy and the decisions.

**Alternative Flows**:
- **New commit**: the gear verifies with a new SHA; it is a new projection, so approval is required again.
- **Author approves**: rejected by separation of duties.
- **Policy relaxed during review**: an administrator drops the mandatory repository owner; the two recorded approvals still count, so reconciliation closes the request as `approved`, and the audit names the policy change.
- **Code owners team** (p2): the mandatory approver is a group; a condition on changed paths adds the security team.
- **Reviews in GitHub** (p2): a sync gear records each GitHub review on behalf of the reviewer.
- **Bot authors** (p2): a rule with a subject-type selector makes merges by automation identities `not_required`.

### Model Access Approved by the Model Owner

- [ ] `p1` - **ID**: `cpt-cf-approval-service-usecase-tenant-approval`

**Actor**: `cpt-cf-approval-service-actor-admin`

**Preconditions**:
- Model Registry registered opportunity `model.use` with decision authority resource tenant and standing approvals allowed; projection = model (empty version), resource tenant = owning tenant; subject = consuming tenant.
- A rule requires approval for the model projection type; no policy applies.

**Main Flow**:
1. The administrator of the owning tenant browses models and selects one.
2. The administrator approves it for the owning tenant subject, with a reason.
3. Every descendant tenant that consumes the model receives `approved`.

**Postconditions**:
- A change event is published; SDK caches converge within the bounded delay.

**Alternative Flows**:
- **Descendant tries to approve**: an approve decision by a descendant tenant administrator is rejected, because only the owner may approve.
- **Descendant excludes the model**: a descendant administrator records a deny; that descendant and its own descendants receive `denied`.
- **Bulk onboarding**: the owner approves a list of models in one atomic operation.
- **Model list**: Model Registry batch-verifies the candidate models of a page and lists only the approved ones.
- **Approval from the registry** (p2): the administrator approves or revokes in Model Registry, which records the decision on their behalf.

### Repository Read and Write Access

- [ ] `p1` - **ID**: `cpt-cf-approval-service-usecase-repo-access`

**Actor**: `cpt-cf-approval-service-actor-approver`

**Preconditions**:
- The source control gear registered opportunities `repo.read` and `repo.write` with standing approvals allowed, projection = repository (empty version), subject = user.
- Repository maintainers hold the approve permission for both; team leads hold it for `repo.read` only.

**Main Flow**:
1. A user opens the repository; the gear verifies `repo.read` with the register option and receives `pending`.
2. A team lead approves read access; the user can read.
3. The user pushes; the gear verifies `repo.write` and receives `pending`.
4. A team lead cannot approve write access; a maintainer approves it.

**Postconditions**:
- Read and write approvals are separate decisions with separate audit.

**Alternative Flows**:
- **Temporary write access** (p2): the maintainer sets an expiry on the decision.
- **Whole team** (p2): the maintainer approves a subject group.

### Task Waits for Approval

- [ ] `p2` - **ID**: `cpt-cf-approval-service-usecase-task-wait`

**Actor**: `cpt-cf-approval-service-actor-integrated-gear`

**Preconditions**:
- A gear registered opportunity `task.execute`; its worker runs tasks in Task Engine and holds the Event Broker `consume` permission on request events for that opportunity.

**Main Flow**:
1. The worker verifies with the register option and receives `pending` with the request ID.
2. The worker arms a Task Engine event wait on the request event type, with the opportunity type as subject type and the request ID as subject ID, and parks the task.
3. An approver decides; the request closes and its closed event is published in the task's tenant.
4. Task Engine resumes the task with the event header; the worker reads the request outcome by ID and continues or stops.

**Postconditions**:
- Task Engine holds no approval state; the decision and request history are audited here.

**Alternative Flows**:
- **Context changed or withdrawn**: the request closes with that reason; the worker verifies again and waits on the new request.
- **Wait times out**: the worker verifies again and re-arms the wait.

### Order Approval with Escalation

- [ ] `p2` - **ID**: `cpt-cf-approval-service-usecase-order-approval`

**Actor**: `cpt-cf-approval-service-actor-integrated-gear`

**Preconditions**:
- Orders Workflow registered opportunity `order.approve` with standing approvals prohibited, projection = order with the order version as version, context attribute TCV.
- A staged policy requires finance, then legal, and names a sales director as escalation recipient.

**Main Flow**:
1. On order submission, Workflow verifies with the register option and its correlation ID; it reflects `approved` or, on `pending`, `pending_approval` into Orders Lifecycle.
2. Finance approves; the verdict stays `pending` with legal as the current stage.
3. Workflow's 72-hour timer fires; Workflow escalates the request, and the request-escalated event names the sales director.
4. Legal approves; the request closes and its event carries the outcome, the decision reasons, and the correlation ID.
5. Workflow reflects `approved` into Orders Lifecycle and starts fulfillment.

**Postconditions**:
- Every decision and the escalation are audited; Workflow kept only its timers and process state.

**Alternative Flows**:
- **Order amended**: Workflow deregisters the prior order version, which closes its request, and verifies the new version.
- **Rejected**: an approver denies; the closed event carries `denied` and the reason.
- **Service unavailable**: verification returns `unavailable`; Workflow parks with the order in `submitted`.

## 9. Acceptance Criteria

- [ ] A gear gates an operation with one SDK call and no approval logic.
- [ ] Registering an opportunity without a default action or without a standing approval setting is rejected.
- [ ] For an opportunity that prohibits standing approvals, an approve decision or bulk approve item without an open request is rejected, and existing standing approvals stop applying when the setting changes to prohibited.
- [ ] A gear cannot register opportunities or projections for types it does not own.
- [ ] A verification with the register option opens at most one request per (opportunity, projection, subject) and records the requester and context attributes.
- [ ] A verification without the register option changes nothing.
- [ ] Unknown opportunities, unaccepted types, and invalid context attributes return `invalid` with distinct reason codes.
- [ ] Within one tenant level, layers are checked one by one (runtime, configuration, gear default) and the first layer with a matching rule decides, by its most specific rule; across levels, the strictest action wins.
- [ ] Deleting a runtime rule restores the configuration or gear default result.
- [ ] The check permission does not allow deciding; any holder of the approve permission for the opportunity can decide.
- [ ] A policy is satisfied only with the minimum number of distinct counting approvals and every mandatory approver.
- [ ] Every applicable policy of the subject tenant and its ancestors must be satisfied.
- [ ] For an owner-only opportunity, approve decisions outside the resource tenant and its ancestors are rejected.
- [ ] A decision of a subject tenant approver satisfies an ancestor's `require_approval` rule and never overrides a `deny` rule.
- [ ] An ancestor deny decision wins over a descendant approve decision.
- [ ] A decision on a tenant subject applies to all subjects in that tenant and its descendants.
- [ ] An approval of one projection version does not apply to another version.
- [ ] Every verdict names what produced it.
- [ ] Batch verification returns, for each item, the verdict a separate verification would return.
- [ ] Every request closure, whatever its cause, publishes a request event in the subject tenant and the requester's tenant, and the requester and the opening gear can read the outcome by request ID.
- [ ] A caller correlation ID is echoed in verdicts and request events, and decision-produced verdicts carry the decision reasons.
- [ ] Decisions and runtime governance changes without a reason are rejected.
- [ ] A decision whose actor is the subject or the requester is rejected.
- [ ] A request closes when its verdict is no longer `pending`.
- [ ] After a broad rule or policy change, every affected open request whose verdict is no longer `pending` is closed within the reconciliation bound, while verification without the register option writes nothing.
- [ ] After a policy change, recorded approvals stay active and count toward the current policies; a relaxation closes affected open requests as `approved`.
- [ ] Request events are published in the subject tenant with the opportunity GTS type as subject type and the request ID as subject ID.
- [ ] Decisions and requests of a deregistered projection stay archived after re-registration.
- [ ] A bulk decision with one failing item records nothing and reports each item's reason.
- [ ] A revocation takes effect for SDK callers no later than the end of the approval lease.
- [ ] A verdict lease never exceeds the configured maximum or an applicable policy's maximum; with two mandatory approvers who set 60 seconds and 10 minutes, the lease is 60 seconds.
- [ ] A changed context fingerprint closes the open request with `context_changed`, and approvals bound to the old fingerprint stop applying.
- [ ] With a changed context fingerprint, `deny` and `not_required` rules still win; `context_changed` is returned only when approval is still required and unsatisfied.
- [ ] An approver's new decision supersedes their own previous one; another approver's deny stays until revoked.
- [ ] Verification sustains 1,000 calls/s from 8 parallel clients with 1,000,000 approval decisions stored, on PostgreSQL and on MariaDB.
- [ ] Verification returns `unavailable` with `mayProceed = false` when a dependency fails.
- [ ] Every governance change commits atomically with its audit record and durable event record, and its event reaches the Event Broker at least once after commit.
- [ ] (p2) An auto-approval never overrides a `deny` rule or a deny decision, and never satisfies a policy of an ancestor of its scope tenant.
- [ ] (p2) Policy changes do not alter the policies fixed for an open request.
- [ ] (p2) In a staged policy, an approval counts toward a stage only after the previous stage is satisfied.
- [ ] (p2) Escalating a request publishes an event naming the escalation recipients and changes neither the verdict nor the request state.

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| Platform AuthN and AuthZ (PDP) | Authentication and authorization of all service operations | p1 |
| Types Registry (GTS) | Opportunity, projection, subject, and event types; pattern matching | p1 |
| Tenant Resolver | Tenant ancestry for verdict resolution; optional in flat deployments | p1 |
| Resource Group gear | Group approver and subject group membership | p2 |
| Platform audit infrastructure | Durable, append-only audit records | p1 |
| Event Broker gear | Change event delivery under the service's publishing identity | p1 |

## 11. Assumptions

- Gears enforce both the PDP decision and the approval verdict before a protected operation.
- Gears send stable resource IDs and change the version whenever an approval must not carry over for reasons not captured in context attributes.
- The PDP can evaluate the approve permission per opportunity and projection, so per-resource approvers, such as repository reviewers, are modelled in authorization.
- The Resource Group gear can serve membership lookups within the verification latency budget, with caching (p2).

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| The service is on the hot path of every protected request | HIGH — an outage blocks all gated operations | Fail closed, SDK cache, availability target (§13) |
| Rule, policy, and auto-approval precedence is misunderstood | MEDIUM — unexpected denials or approvals | Explainable verdicts naming what produced them; side-effect-free verdict preview |
| A broad auto-approval rule approves too much | HIGH — governance bypassed | Ancestor restriction, reasons and audit on every change, immediate disable, explained verdicts |
| A restrictive product default blocks gears after installation | MEDIUM — operations refused until administrators act | Gear defaults chosen by gear vendors; platform configuration reviewed per product |
| Approve permissions are granted too broadly | HIGH — unintended users approve | Separate check and approve permissions per opportunity; audit names every approver; access review report (p2) |
| A broad governance change affects many open requests | MEDIUM — inboxes and waiting gears see settled requests until reconciled | Live verdicts at verification; idempotent, resumable reconciliation within a configured bound |
| Group membership changes are not propagated as events | MEDIUM — a removed member keeps a group approval until caches expire | Bounded membership cache lifetime; request membership change events from Resource Group |

## 13. Open Questions

- What availability target must verification meet? (Owner: Approval Service owner; Resolve by: before DESIGN approval)
- What default and maximum approval leases balance load and revocation delay? (Owner: Approval Service owner; Resolve by: before DESIGN approval)
- What reconciliation bound must request reconciliation meet after a governance change? (Owner: Approval Service owner; Resolve by: before DESIGN approval)
- Will Model Registry move its model approval state to this service? Its PRD currently keeps approval locally. (Owner: Model Registry team; Resolve by: before p1 implementation)
- Can the Resource Group gear publish membership change events and accept GTS subject types as resource types? (Owner: Resource Group team; Resolve by: before p2 planning)
- Will Orders Workflow replace its stand-in with this service, and does [UPSTREAM_REQS.md](./UPSTREAM_REQS.md) §2.3 replace its §9.2 expectations contract as the normative home? (Owner: BSS team; Resolve by: before p2 planning)

## 14. Traceability

- **UPSTREAM_REQS**: [UPSTREAM_REQS.md](./UPSTREAM_REQS.md)
- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: None yet; approved decisions will be recorded in `docs/ADR/`.
- **Features**: None yet; implementation specifications will be recorded in `docs/features/`.
