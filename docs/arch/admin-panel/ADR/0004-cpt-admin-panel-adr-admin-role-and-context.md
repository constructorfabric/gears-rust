---
status: accepted
date: 2026-06-25
decision-makers: gears-rust admin-panel working group
---

# Admin mode via a static role stub plus an admin-context endpoint


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A: Static role stub + admin-context endpoint](#option-a-static-role-stub--admin-context-endpoint)
  - [Option B: Real admin-mode in SecurityContext/authz now](#option-b-real-admin-mode-in-securitycontextauthz-now)
  - [Option C: Client-side mode heuristics from `/me`](#option-c-client-side-mode-heuristics-from-me)
  - [Option D: Dedicated admin-context endpoint](#option-d-dedicated-admin-context-endpoint)
  - [Option E: Extend `GET /me`](#option-e-extend-get-me)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-admin-panel-adr-admin-role-and-context`

## Context and Problem Statement

The panel must distinguish platform admin from tenant admin and fetch the caller's admin context (principal, tenant, admin mode, enabled gears, capabilities) at startup, while keeping the backend the final authority. Today the security model carries only identity and home tenant (`SecurityContext`: subject id/type/tenant, token scopes, bearer); there is no admin-mode concept, the authorization resolver answers only yes/no per request (capabilities are not enumerable), the only identity endpoint is `GET /me` (subject id/type/home tenant), and there is no context/capabilities endpoint. How do we represent admin mode and expose admin context for v0 without committing the production authorization model?

## Decision Drivers

- **Backend authority** — mode and capabilities must come from the backend, not from a user-toggleable UI control.
- **No production-model commitment** — v0 must not lock in how admin roles are represented in the real authorization system.
- **Unblock v0** — a workable mode + context path is needed now; the static plugins are the platform's existing dev seam.
- **Clearly non-production** — demo/static auth must be unmistakably marked as such.
- **Forward compatibility** — the admin-context contract should survive a later move to a real role model.
- **Capability-driven UI** — navigation and write gating need a capabilities list the panel can consume.

## Considered Options

For representing admin mode:

- **Option A**: Stub two roles in the static (non-production) auth plugins — map distinct dev tokens to identities carrying a platform-admin vs tenant-admin marker (via `subject_type` / token scopes) — and add an admin-context endpoint that derives mode and capabilities from the security context.
- **Option B**: Add a real admin-mode concept to `SecurityContext` and the authorization model now.
- **Option C**: Compute admin mode purely client-side from `GET /me` plus heuristics.

For exposing context:

- **Option D**: Add a dedicated admin-context endpoint.
- **Option E**: Extend `GET /me` to return mode, enabled gears, and capabilities.

## Decision Outcome

Chosen options: **Option A** (static role stub) **+ Option D** (dedicated admin-context endpoint).

For v0, two clearly-marked, non-production roles are stubbed in the static auth plugins: distinct dev tokens resolve to identities marked platform-admin or tenant-admin (using `subject_type` and/or token scopes, which the static plugins already support). A dedicated admin-context endpoint returns the principal (subject id/type), resolved home tenant, admin mode, and a capabilities list, derived server-side from the security context and the static role marker. Enabled gears are **not** part of this contract — the panel reads them separately from the gear orchestrator (`GET /gear-orchestrator/v1/gears`), keeping admin-context a thin identity/authorization projection. The panel calls it at startup to drive mode selection, capability-gated navigation, and tenant scope, with the backend remaining the final authority on every action.

A dedicated endpoint (D) is chosen over extending `/me` (E) because `/me` is a minimal, non-tenant-scoped identity reflection used broadly; admin context is a richer, admin-specific concern that should not bloat or change `/me`'s contract.

The production representation of admin mode (real authorization action, token scope claim, or identity field) is explicitly deferred and recorded as an open question; the static stub and the admin-context contract are designed so that the production model can replace the stub without changing the frontend.

### Consequences

- The static auth/authz plugins gain two dev identities/tokens marked as platform-admin and tenant-admin; these and the role stub must be labeled non-production in UI and docs (`cpt-admin-panel-nfr-demo-marking`).
- A new admin-context endpoint must be implemented; its host gear (account-management, API Gateway, or a thin admin gear) and exact shape are settled in DESIGN (open question).
- The endpoint must compute a capabilities list and an enabled-gears summary; for v0 capabilities may be derived from the role marker and the enabled gears, not from per-request authorization enumeration.
- Tenant isolation continues to be enforced server-side (Secure ORM tenant-subtree predicates and authorization decisions); the panel must not implement isolation.
- When a real role model is introduced, the static stub is removed and the admin-context endpoint is re-backed by it without a frontend contract change.
- `GET /me` is unchanged.

### Confirmation

Confirmed by design review of DESIGN.md (admin-context endpoint shape and host gear, static role stub), by the panel selecting mode and capability-gated navigation from the endpoint, by tenant admin being unable to reach cross-tenant data, and by e2e tests covering both modes.

## Pros and Cons of the Options

### Option A: Static role stub + admin-context endpoint

Mark two roles in the static plugins; derive mode/capabilities server-side.

- Good, because it reuses the platform's existing static-plugin dev seam (token-to-identity mapping with `subject_type`/scopes).
- Good, because mode stays backend-derived, not a UI toggle.
- Good, because it unblocks v0 without committing the production authorization model.
- Good, because it is forward-compatible — the stub can be swapped for a real model behind the same endpoint.
- Bad, because it is explicitly non-production and must be clearly marked to avoid misuse.

### Option B: Real admin-mode in SecurityContext/authz now

Introduce a first-class admin-mode in the core security model immediately.

- Good, because it would be the production-correct representation.
- Bad, because it is a cross-cutting change to the security model that blocks v0.
- Bad, because the right representation (action vs scope vs claim) is an unresolved design question.

### Option C: Client-side mode heuristics from `/me`

Infer mode in the browser from identity plus guesses (e.g. home tenant is root).

- Good, because no backend change.
- Bad, because mode would be UI-derived and spoofable, violating backend-authority.
- Bad, because capabilities and enabled gears are not available from `/me`.

### Option D: Dedicated admin-context endpoint

A purpose-built endpoint returning principal, tenant, mode, enabled gears, capabilities.

- Good, because it isolates admin concerns from the minimal `/me` contract.
- Good, because it can aggregate enabled gears and capabilities in one startup call.
- Neutral, because it is one new endpoint to own and version.

### Option E: Extend `GET /me`

Add mode/gears/capabilities to the existing identity endpoint.

- Good, because one fewer endpoint.
- Bad, because it overloads a minimal, broadly-used, non-tenant-scoped identity reflection with admin-specific, heavier data.
- Bad, because it couples `/me` consumers to admin-context changes.

## More Information

- Issue: constructorfabric/gears-rust#4144
- `SecurityContext` carries subject id/type, home tenant, token scopes (`["*"]` = unrestricted), and bearer token.
- The static auth plugins support `accept_all` and `static_tokens` modes (token-to-identity mapping with `subject_type` and scopes).
- `GET /me` returns subject id/type and home tenant only.
- Tenant isolation is enforced at the database layer via tenant-subtree predicates.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

- `cpt-admin-panel-fr-admin-context` — defines the startup admin-context fetch.
- `cpt-admin-panel-fr-role-stub` — defines the v0 static platform/tenant role stub.
- `cpt-admin-panel-fr-admin-modes` — mode is backend-derived from the admin context.
- `cpt-admin-panel-interface-admin-context` — fixes the admin-context endpoint as the contract.
- `cpt-admin-panel-nfr-demo-marking` — requires marking the stub as non-production.
- `cpt-admin-panel-nfr-backend-authority` — backend remains the authority for mode and actions.
