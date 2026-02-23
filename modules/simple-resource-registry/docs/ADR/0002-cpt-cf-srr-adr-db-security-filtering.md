---
status: accepted
date: 2026-02-24
---
# ADR-0002: DB-Level Security Filtering for Resource Access Control

**ID**: `cpt-cf-srr-adr-db-security-filtering`

## Context and Problem Statement

Every resource operation (GET, PUT, DELETE, LIST) must enforce three orthogonal access control dimensions: tenant isolation, owner scoping (when `is_per_owner_resource=true`), and GTS type scope (from token permissions). The system needs a strategy for enforcing these checks that is both secure and does not leak information about the existence of resources the caller cannot access. Should access control be checked at the application level (fetch first, then check) or at the database query level (inject predicates into every query)?

## Decision Drivers

* Must not leak resource existence to unauthorized callers (no 403 revealing "resource exists but you lack access")
* Must align with CyberFabric's SecureORM pattern for tenant isolation
* Must produce consistent behavior across GET, PUT, DELETE, and LIST operations
* Must avoid double-query patterns (fetch → check → re-fetch) for performance
* Must work across all storage backends (not just relational databases)

## Considered Options

* Application-level ACL checks (fetch then check)
* DB-level query filtering (inject predicates)

## Decision Outcome

Chosen option: "DB-level query filtering", because it provides zero information leakage (unauthorized resources return 404, not 403), aligns with SecureORM, uses a single query path, and produces consistent behavior across all CRUD operations.

**Exception**: POST (create) and LIST (type-scope-only) use pre-query checks because there is no existing resource to filter — 403 is returned when the requested type is not in the caller's token scope.

### Consequences

* Good, because no information leakage — callers cannot distinguish "does not exist" from "not authorized" for individual resources
* Good, because single query path — tenant, owner, and type filters are WHERE predicates, no double-fetch
* Good, because aligns with SecureORM's established pattern for tenant scoping
* Good, because consistent behavior across GET/PUT/DELETE/LIST — all use the same filter mechanism
* Bad, because debugging access issues is harder — callers see 404 and cannot tell if it's a permission problem
* Bad, because storage backends must support predicate-based filtering (trivial for SQL, may be less natural for non-relational stores)

### Confirmation

* Integration tests verify that GET/PUT/DELETE on a resource belonging to another tenant or owner returns 404 (not 403)
* Integration tests verify that POST with out-of-scope type returns 403
* Integration tests verify that LIST with no-intersection type scope returns 403
* Code review confirms all storage backend queries include tenant_id, owner_id, and type scope predicates

## Pros and Cons of the Options

### Application-level ACL checks (fetch then check)

Fetch the resource first, then check tenant/owner/type scope in application code. Return 403 if access is denied.

* Good, because clear error reporting — callers know exactly why access was denied
* Good, because straightforward implementation — no complex query construction
* Bad, because leaks resource existence — 403 confirms the resource exists but caller lacks permission
* Bad, because double-query pattern — must fetch to check, then re-fetch or use the cached result
* Bad, because inconsistent with SecureORM's query-level tenant scoping
* Bad, because each storage backend would need both a raw fetch and a filtered fetch path

### DB-level query filtering (inject predicates)

Inject `tenant_id = ctx.subject_tenant_id`, `type IN (permitted_types)`, and `owner_id = ctx.subject_id` (when applicable) as WHERE clause predicates on every query. If no rows match, return 404.

* Good, because zero information leakage — 404 for both "not found" and "not authorized"
* Good, because single query — all filters applied in one database round-trip
* Good, because aligns with SecureORM tenant scoping pattern
* Good, because consistent across all operations — same predicate mechanism for GET/PUT/DELETE/LIST
* Bad, because harder to debug access issues for callers
* Bad, because storage backends must support predicate injection

## More Information

The 403 response is reserved for two pre-query scenarios where no resource lookup is involved:

1. **POST create**: The requested resource `type` is not in the caller's token GTS scope → 403 `gts-type-not-in-scope`
2. **LIST no-intersection**: The requested type filter has zero intersection with the caller's permitted GTS types → 403 `gts-type-not-in-scope`

In both cases, 403 is appropriate because no resource-level information is being leaked — the denial is based purely on the type, not on any specific resource's existence.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-srr-fr-gts-access-control` — GTS type-based access control enforcement strategy
* `cpt-cf-srr-fr-read-resource` — GET returns 404 when filtered out by security predicates
* `cpt-cf-srr-fr-update-resource` — PUT returns 404 when filtered out
* `cpt-cf-srr-fr-delete-resource` — DELETE returns 404 when filtered out
* `cpt-cf-srr-fr-list-resources` — LIST applies DB filters; empty result set is valid (not error)
* `cpt-cf-srr-principle-plugin-isolation` — Plugins receive already-authorized predicates from the domain layer
