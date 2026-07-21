Created:  2026-07-08 by Constructor Tech
Updated:  2026-07-08 by Constructor Tech
# ADR-0028: Full PEP (PDP/PEP) + SecureORM with Denormalized Owner Columns


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Full PEP + SecureORM with denormalized, immutable owner columns](#option-1-full-pep--secureorm-with-denormalized-immutable-owner-columns)
  - [Option 2: JOIN to `sessions` for scoping](#option-2-join-to-sessions-for-scoping)
  - [Option 3: Service-gate ownership without SecureORM](#option-3-service-gate-ownership-without-secureorm)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-07-08

**Status**: accepted

**Review**: Revisit when a zero-downtime / expand-contract migration is required in production (see OQ2), or if cross-tenant session transfer enters scope (OQ3).

**ID**: `cpt-cf-chat-engine-adr-authz-pep-secureorm`

## Context and Problem Statement

Authentication (AuthN) is already wired: the API Gateway injects a `SecurityContext` into every `.authenticated()` request. Authorization, however, was performed by manual `(tenant_id, user_id)` ownership filters with `AccessScope::allow_all()` in every repository — there was no PDP decision, no query-level authorization, and no fail-closed guarantee. Chat Engine needs to adopt the platform PDP/PEP + SecureORM model (`docs/arch/authorization/DESIGN.md`, `docs/toolkit_unified_system/06_authn_authz_secure_orm.md`) so that every sensitive database access is gated by a PDP decision compiled into an `AccessScope` and enforced by `SecureConn` at the SQL layer.

The design detail (ownership boundaries, PEP call surface, per-operation flow, fail-closed error mapping, session-type permission model, bypass registry, shared-read boundary, and migration) is defined in `gears/chat-engine/docs/DESIGN.md` §3.5. This ADR records the decision and its rationale; the DESIGN is the authoritative "what".

## Decision Drivers

* Query-level authorization (SQL `WHERE`) for LIST correctness and point-op existence-hiding, not point-in-time checks
* Fail-closed: deny, missing constraints, and PDP unavailability must never leak or grant access
* Minimal per-query overhead and correct pagination (owner-pair predicates compiled before `LIMIT`)
* Immutable, denormalized owner columns so `SecureConn` scopes on the row's own columns (no joins, no service-side gating)
* Auditable, enumerated bypasses for trusted-internal pipeline writes, scheduled system ops, and the subject-less share-token read
* Safe forward migration from the current `String` tenant/user columns to `UUID` owner columns

## Considered Options

* **Option 1 (chosen): Full PEP + SecureORM with denormalized, immutable owner columns.** Each scoped table carries `(owner_tenant_id, owner_id)` copied from the session at insert; `PolicyEnforcer` gates every operation; internal writes bypass via an enumerated registry.
* **Option 2: JOIN to `sessions` for scoping.** Rejected — per-query joins, harder SecureORM integration, and worse list-pagination performance.
* **Option 3: Service-gate ownership without SecureORM.** Rejected — keeps manual filters and `allow_all()`, no fail-closed guarantee, no query-level enforcement.

## Decision Outcome

Chosen: **Option 1**. See `cpt-cf-chat-engine-design-auth-model` (§3.5) for the full model, `cpt-cf-chat-engine-principle-owner-denorm-invariant` for the denormalization invariant, `cpt-cf-chat-engine-constraint-fail-closed-authz` and `cpt-cf-chat-engine-constraint-no-allow-all-outside-registry` for the enforced constraints, and `cpt-cf-chat-engine-dbtable-authz-owner-columns` for the migration.

### Consequences

- Every sensitive DB access is PDP-gated and SQL-enforced; `allow_all()` is confined to the auditable bypass registry.
- A single forward breaking migration casts `String`→`UUID` and backfills owner columns; zero-downtime is deferred (OQ2).
- PDP unavailability maps to 403 (fail-closed), not 503/500, so availability does not leak.

### Confirmation

Verified by PDP-deny tests (mock `DenyAllAuthZResolver`), scoped-allow fixtures (`ctx_allow_tenants`), a denormalization-invariant test, and per-bypass-site tests (including that system cross-tenant ops are not HTTP-exposed).

## Pros and Cons of the Options

### Option 1: Full PEP + SecureORM with denormalized, immutable owner columns

- Good, because every sensitive DB access is PDP-gated and SQL-enforced with correct pagination.
- Good, because scoping is on the row's own columns — no joins and no service-side gating.
- Good, because immutable owner columns make point-op prefetch TOCTOU-safe.
- Bad, because it requires a breaking forward migration and owner-column denormalization/backfill.

### Option 2: JOIN to `sessions` for scoping

- Good, because it avoids denormalized columns.
- Bad, because per-query joins complicate SecureORM integration and degrade list-pagination performance.

### Option 3: Service-gate ownership without SecureORM

- Good, because it is the smallest change from the current manual filters.
- Bad, because it keeps `allow_all()` everywhere, gives no fail-closed guarantee, and provides no query-level enforcement.

## Related Design Elements

- `cpt-cf-chat-engine-design-auth-model`
- `cpt-cf-chat-engine-principle-owner-denorm-invariant`
- `cpt-cf-chat-engine-constraint-fail-closed-authz`
- `cpt-cf-chat-engine-constraint-no-allow-all-outside-registry`
- `cpt-cf-chat-engine-component-policy-enforcer`
- `cpt-cf-chat-engine-interface-pep`
- `cpt-cf-chat-engine-design-authz-bypass-registry`
- `cpt-cf-chat-engine-dbtable-authz-owner-columns`
- `cpt-cf-chat-engine-seq-authz-list`
- `cpt-cf-chat-engine-seq-authz-point-op`
- `cpt-cf-chat-engine-seq-authz-shared-read`
- `cpt-cf-chat-engine-seq-authz-internal-write`
- `cpt-cf-chat-engine-nfr-authentication`
