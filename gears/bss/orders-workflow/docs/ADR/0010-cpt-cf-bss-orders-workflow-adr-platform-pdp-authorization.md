---
status: accepted
date: 2026-09-24
decision-makers: BSS Orders team (Architecture)
---
# ADR-0010: Authorization Is Delegated To The Platform PDP Through The Shared PolicyEnforcer Adapter


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Platform PDP through the shared PolicyEnforcer adapter (chosen)](#platform-pdp-through-the-shared-policyenforcer-adapter-chosen)
  - [Gear-local permission evaluator over a declaration table and issuer-materialised claims](#gear-local-permission-evaluator-over-a-declaration-table-and-issuer-materialised-claims)
  - [Hybrid: PDP for tenant scope, local evaluator for assignment and service-principal scope](#hybrid-pdp-for-tenant-scope-local-evaluator-for-assignment-and-service-principal-scope)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-orders-workflow-adr-platform-pdp-authorization`
## Context and Problem Statement

The first revision of `design/09-read-and-authz.md` authorized every operation with a
gear-local **permission evaluator**: a `owf_permission_declaration` table materialised from the
routing table at startup, evaluated against a `SecurityContext` this design invented — eleven
claims including `actor_class`, `seller_scope`, `approval_assignments`, `service_principal` and
`delegation_proof`, to be issued by the platform auth gateway and an approver-assignment directory
(`UPSTREAM_REQS.md` `AUTH-O1`). System actors arriving over the event transport were to be
authenticated by a **signed envelope** carrying a publisher principal, verified against a platform
key set, plus a topic ACL declared as gear configuration.

None of that exists on the platform. The real `SecurityContext`
([`libs/toolkit-security/src/context.rs`](../../../../../libs/toolkit-security/src/context.rs))
carries five fields — `subject_id`, `subject_type`, `subject_tenant_id`, `token_scopes` and
`bearer_token` — and no role, scope-set or assignment claim. The unified-system rule
([`docs/toolkit_unified_system/06_authn_authz_secure_orm.md`](../../../../../docs/toolkit_unified_system/06_authn_authz_secure_orm.md))
is that every authorization decision goes through `PolicyEnforcer` from `authz-resolver-sdk`,
every sensitive database access is covered by a PDP decision compiled to an `AccessScope` and
applied by `SecureConn`, and a denied, unreachable or constraint-less PDP fails closed. The broker
envelope ([`event-broker-sdk/src/typed_event.rs`](../../../../system/event-broker/event-broker-sdk/src/typed_event.rs)
`EnvelopedEvent`) carries no producer principal and no signature. The sibling Orders Lifecycle
design already replaced its own local evaluator with the shared PolicyEnforcer adapter
([`08-read-and-authz.md` §3.5](../../../orders-lifecycle/docs/design/08-read-and-authz.md#35-external-dependencies),
D-111, D-114, D-141), following Pricing's integration
([`05-governance.md` *AuthZ Resource and Action Catalog*](../../../pricing/docs/design/05-governance.md#authz-resource-and-action-catalog-normative)).

How does Orders Workflow authorize its seventeen REST routes and twelve event handlers against the
platform that exists, without giving up the properties the local evaluator was written to secure:
one decision path for every operation, an explicit scope on every grant, an approver confined to
the gates assigned to them, and no operation shipped unguarded?

## Decision Drivers

* The unified-system rule: `PolicyEnforcer` for all authorization decisions, a PDP decision covering every sensitive database access, `SecureConn` applying the compiled `AccessScope`, fail closed on denial, outage or missing constraints, and no `AccessScope` constructed manually in production code.
* The real `SecurityContext` carries five fields and no role, scope-set or assignment claim; a design whose predicates read `ctx.seller_scope` or `ctx.approval_assignments` is not implementable against it and cannot become implementable without a platform change nobody has asked for.
* Consistency with Orders Lifecycle `08 §3.5` and `§4.3` and with Pricing: the two Orders gears must sit on one authorization contract, one denial-mapping rule and one verification plan, or a reviewer of either inherits two.
* The approver-assignment scope (PRD §6.7 "MUST NOT act on approval requests for orders outside their assigned scope") must survive: it is the one scope the platform's tenant axes do not express.
* The broker provides produce grants per topic and platform-root tenancy for inter-service streams (Lifecycle D-95); it provides no signed envelope and no consumer-visible producer principal, so a control written against those cannot be built.
* The exhaustiveness guarantee — no route or handler ships without an arm — was the local evaluator's real value and must be kept in a form the platform supports.

## Considered Options

* **Platform PDP through the shared PolicyEnforcer adapter** — a registered resource/action catalogue, one `PolicyEnforcer`, PDP constraints compiled to `AccessScope` and applied inside the mutating statement, approver assignment as a gate-row property the PDP constrains on
* **Gear-local permission evaluator over a declaration table and issuer-materialised claims** — as previously written
* **Hybrid** — PDP for tenant-axis scope, a local evaluator for approver assignment and service-principal scope

## Decision Outcome

Chosen option: "Platform PDP through the shared PolicyEnforcer adapter", because it is the only
option that is implementable against the platform as it exists, it is the rule the platform
documentation states rather than an exception to it, and it is the option the sibling gear has
already taken, so the two Orders gears share one authorization contract. Concretely:

* **Resource types and actions are a registered catalogue.** Workflow registers GTS labels
  `gts.cf.bss.orders_workflow.<noun>.v1~` for `process_instance`, `fulfillment_task`,
  `manual_task`, `dead_letter`, `approval_gate` and `progress`, with the closed action set
  `start`, `read`, `cancel`, `retry_step`, `resolve`, `override`, `assign`, `escalate`,
  `redrive`, `discard`, `approve`, `read_inbox` (`09 §3.1`). Every REST route and event handler
  maps to exactly one `(resource, action)` pair (`09 §3.2`). Registration declares permissions;
  it issues no grants — role provisioning is the platform policy owner's, tracked under
  `cpt-cf-bss-orders-workflow-upreq-pdp-policy-integration`.
* **Scopes are PDP constraints, enforced in the statement.** Seller scope is a PDP constraint on
  the row's `seller_tenant_id` (an `Eq`, `In` or `InTenantSubtree` predicate — the PDP chooses);
  tenant isolation is the constraint on `resource_tenant_id`; the resource restriction on a
  targeted call is the standard resource-id property. The adapter compiles the returned
  constraints to an `AccessScope` with `access_scope_with` and requires constraints; `SecureConn`
  applies the scope inside the mutating `UPDATE`, so the ownership check is the statement's own
  `WHERE` clause and there is no read-then-compare-then-write (`09 §4.4`).
* **Approver assignment is a property of the gate row.** `owf_approval_gate.assigned_principal`
  is populated at gate-open from the routing configuration (`03 §3.2`, `§3.7`) and supplied to the
  PDP as a resource property; the approver's grant is an "own resource" `Eq` constraint
  `assigned_principal = subject_id`. There is no token claim, no assignment-directory inverse
  query and no gateway ask: `AUTH-O1` is withdrawn.
* **Service principals are `subject_type` plus `token_scopes` naming this gear.** A system actor
  on the REST surface is authorized when `subject_type` is the platform service-subject type and
  `token_scopes` names this gear's scope, and then only for the `(resource, action)` pair its arm
  declares; the PDP evaluates the same request. Event handlers are authorized by the broker's
  produce grant on the topic and platform-root tenancy (Lifecycle D-95) — the consumer sees no
  producer principal — and every handler additionally performs the PDP-authorized Lifecycle
  `order × read` before acting (`02 §2.1`). The signed-envelope mechanism and the
  `requires_signed_envelope` control are deleted.
* **A bounded worker exception with configured system authority, exactly as Lifecycle `08 §3.5`
  states it.** The five Workflow-owned workers of `01 §3.8` run under configured system authority
  without a per-row PDP decision; a bounded discovery scan may use `AccessScope::allow_all()`, every
  write narrows to its persisted target, the authority is never selected by a caller-supplied
  value, and the exception never extends to REST, SDK or event-handler paths.
* **The matrix survives as a startup conformance test.** `09 §4.1` is the expected-decision table.
  A CI test with a recording PDP double asserts, for every registered route and handler, the
  `(resource, action)` pair and properties the adapter asks for; a startup assertion fails the
  service if any registered route or handler maps to no catalogue pair, or any pair is not
  registered. There is no `owf_permission_declaration` table.
* **404 over 403 for an out-of-scope target is a declared deviation.** The unified-system rule
  maps a PDP denial to 403. Workflow answers a **targeted** denial with `not-found` (404) unless a
  follow-up `read` decision on the same target allows, in which case it answers `not-authorized`
  (403); an untargeted denial (list, start) is 403. This is Lifecycle D-114 and D-141 applied
  unchanged: a 403 on a row the caller cannot read is an existence oracle over other sellers'
  orders, and the follow-up read discloses nothing the caller could not learn by reading. It is
  recorded as this gear's single declared deviation from the platform default (`DESIGN.md` §2.2).
* **PDP outage fails closed; workers continue.** A timeout or unavailable PDP on a request path
  returns a sanitized 503, performs no mutation, settles no idempotency key and never falls back
  to a local decision; the workers continue because their authority is configured, not obtained
  from the PDP. The apply-time re-check of a long-running command (`09 §4.4`) re-runs the same PDP
  decision on the same target; a refusal there routes the command to a manual task with reason
  `authority-withdrawn` and the process left in its current phase — never to `parked`, which is
  the verdict park of ADR-0007.

### Consequences

* `authz-resolver-sdk` is an internal dependency and `authz-resolver` a mandatory external one; missing wiring is a startup failure, not a switch to local authorization.
* `design/09-read-and-authz.md` loses the `SecurityContext` and `PermissionDeclaration` entities, the `owf_permission_declaration` table, the signed-envelope mechanism and every predicate written against invented claims; it gains the catalogue, the endpoint mapping and the conformance test.
* `owf_approval_gate.assigned_principal` becomes an authorization input, so its population at gate-open is a security-relevant write and a gate carrying none is listed to nobody (`03 §3.2`).
* The decision endpoint's out-of-assignment refusal becomes 404 (`03 §3.3`), aligning with the targeted-denial rule; the separation-of-duties refusal stays a distinct 403 (`submitter-barred`, D-56).
* Delegation proof is forwarded to the PDP as request context and never validated locally (Lifecycle D-111 by reference); the `delegation_proof` claim is deleted.
* Provisioning of roles, the approver grant and the service-principal grants, and their verification against the deployed provider, are release prerequisites owned by the platform policy owner (`UPSTREAM_REQS.md` §2.8).
* Two Workflow controls the local evaluator carried are kept as local guards, because the PDP has no input for them: separation of duties at the decision endpoint (D-56) and the rule that no service principal drives an order state transition (the R1 seam, enforced by the catalogue: no `(resource, action)` pair a service principal holds writes order state).

### Confirmation

Verified by: a startup test asserting the service refuses to start without a resolvable
`dyn AuthZResolverApi`; the CI conformance test with a recording PDP double covering all
twenty-nine registered routes and handlers against the `09 §4.1` matrix, failing on an added or
mis-mapped route; a test that a denied decision leaves business state, the producer queue and the
idempotency registry unchanged; a test that a PDP timeout on a control operation returns 503 with
no mutation and no key settlement, and that the workers continue; a PostgreSQL integration test
racing a mutating operation against a `seller_tenant_id` change and asserting zero rows affected;
a test that an approver whose `subject_id` is not the gate's `assigned_principal` receives 404 on
the decision endpoint and an empty inbox page; and a test that the apply-time re-check refusal
raises one `authority-withdrawn` manual task and leaves `owf_process_instance.phase` unchanged.

## Pros and Cons of the Options

### Platform PDP through the shared PolicyEnforcer adapter (chosen)

* Good, because it is implementable against the five-field `SecurityContext` and the SDK that exists, with no upstream ask beyond policy provisioning.
* Good, because it is the platform rule rather than an exception to it, and it is Lifecycle's and Pricing's pattern, so one review covers both Orders gears.
* Good, because scope enforcement is the compiled `AccessScope` inside the statement, which is TOCTOU-safe by construction.
* Neutral, because the exhaustiveness guarantee moves from a table check to a conformance test and startup assertion — the same property, held by tests rather than by rows.
* Bad, because the deployed provider's role and relationship policy is not this gear's to verify; an in-process PDP double proves the question asked, not the answer given (Lifecycle `08 §3.5` *Provider capability must be verified separately*).

### Gear-local permission evaluator over a declaration table and issuer-materialised claims

* Good, because every arm is visible in one table and the two-way startup check is easy to state.
* Bad, because it evaluates claims the platform does not issue — `seller_scope`, `approval_assignments`, `service_principal`, `delegation_proof` — so nothing in it can run.
* Bad, because it violates the unified-system rule that authorization decisions go through `PolicyEnforcer`, and the review that found Lifecycle's local evaluator would find this one.
* Bad, because a hand-built table of grants is a second policy engine that the platform policy owner cannot see or provision.

### Hybrid: PDP for tenant scope, local evaluator for assignment and service-principal scope

* Good, because it keeps the approver-assignment scope under local control where the tenant axes cannot express it.
* Bad, because two evaluators are exactly the drift the one-evaluator principle forbids, and the assignment scope is expressible to the PDP as an `Eq` on a row property without any local logic.
* Bad, because the service-principal half still needs a claim the platform does not issue; `subject_type` and `token_scopes` are PDP inputs already.

## More Information

This decision amends D-37 (the service-principal wording) and is carried by D-63; D-64 records
the companion change to the reason catalogue. It builds on ADR-0007 (the verdict park is the only
meaning of `parked`), ADR-0008 (event handlers trust the broker's produce grant, not an envelope
signature) and ADR-0009 (an apply-time refusal is a manual task, never a dead letter). Lifecycle
D-111 (delegation proof is evaluated by the PDP), D-114 and D-141 (the targeted-denial mapping)
and D-95 (platform-root event tenancy) apply by reference.

## Traceability

- **PRD**: [PRD.md](../PRD.md) — §6.7 authorization, §9.1 operations, §12 acceptance criterion 21
- **DESIGN**: [DESIGN.md](../DESIGN.md) §2.2, §3.4, §3.5, §4.2, §4.8;
  [`design/09-read-and-authz.md`](../design/09-read-and-authz.md) §2, §3.1, §3.2, §3.4, §3.5,
  §3.6, §3.7, §4.1, §4.2, §4.4; [`design/03-approval-execution.md`](../design/03-approval-execution.md)
  §3.2, §3.3; [`design/07-manual-tasks.md`](../design/07-manual-tasks.md) §3.2, §3.3
- **Decisions register**: [`DECISIONS.md`](../DECISIONS.md) — D-37 (amended), D-56, D-63, D-64
- **Upstream asks**: [`UPSTREAM_REQS.md §2.8`](../UPSTREAM_REQS.md#28-platform-authorization-policy)
- **Precedents**: Orders Lifecycle [`08-read-and-authz.md`](../../../orders-lifecycle/docs/design/08-read-and-authz.md)
  §3.5, §4.3; Pricing [`05-governance.md`](../../../pricing/docs/design/05-governance.md)
  *AuthZ Resource and Action Catalog*

This decision directly addresses the following requirements or design elements:

* `cpt-cf-bss-orders-workflow-fr-owf-authorization` — the per-actor guard is the platform PDP decision on a registered `(resource, action)` pair, enforced as a compiled scope inside the statement; a system actor cannot drive order state because no pair it holds writes order state
* `cpt-cf-bss-orders-workflow-nfr-owf-api-latency` — one PDP decision per control operation, with the prefetch-first pattern on point reads; the PDP deadline is bounded so a degraded PDP fails the request closed rather than stalling it
* `cpt-cf-bss-orders-workflow-component-read-and-authz` (**authorization adapter**, the component `design/09-read-and-authz.md` §3.2 declares under its unchanged permission-evaluator identifier) — the single adapter over the shared `PolicyEnforcer`; owns the catalogue constants and the conformance test
* `cpt-cf-bss-orders-workflow-component-read-and-authz` (**control operation gateway**) — the single call site that invokes the adapter before dispatch
* `cpt-cf-bss-orders-workflow-component-approval-execution` (**approver inbox projection**) — the inbox filter is the PDP constraint `assigned_principal = subject_id`, compiled and applied by `SecureConn`
* `cpt-cf-bss-orders-workflow-adr-fail-closed-verdict-park` — `parked` keeps its one meaning; an authorization refusal at apply time is an `authority-withdrawn` manual task
