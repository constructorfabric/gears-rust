---
status: accepted
date: 2026-02-28
---

# Use 16 Canonical Error Categories Instead of Full HTTP Status Set

**ID**: `cpt-cf-errors-adr-canonical-error-categories`

## Context and Problem Statement

CyberFabric needs a finite error taxonomy that all modules use. The taxonomy must be transport-agnostic (usable for REST, gRPC, SSE) while providing enough granularity for consumers to write reliable error-handling code. How many error categories should the platform define, and which ones?

## Decision Drivers

* Transport agnosticism — categories must map cleanly to both HTTP status codes and gRPC status codes
* Consumer simplicity — a smaller set is easier to learn, document, and handle exhaustively
* Sufficient granularity — categories must distinguish meaningfully different failure modes (e.g., "not found" vs "already exists" vs "permission denied")
* Industry alignment — prefer a proven taxonomy over inventing one
* Compile-time exhaustiveness — the set must be small enough to enumerate in a Rust `match` without fatigue

## Considered Options

* **Option A**: Full HTTP status code set (~70 codes from RFC 9110)
* **Option B**: Google's 16 canonical error codes (from `google.rpc.Code`)
* **Option C**: Custom reduced set (5-10 hand-picked categories)

## Decision Outcome

Chosen option: **Option B — Google's 16 canonical error codes**, because it is the only option that meets all decision drivers: proven at massive scale, maps 1:1 to gRPC status codes, small enough for exhaustive matching, and granular enough to distinguish all common failure modes.

### Consequences

* All 16 categories must be defined in the error library with their context types, HTTP status mappings, and GTS identifiers
* The Google category name `unavailable` is represented in CyberFabric as `service_unavailable` (same semantics, clearer platform naming)
* Every module must migrate its existing ad-hoc error types to one of the 16 canonical categories — no module-specific error categories are allowed
* HTTP status codes that fall outside the 16 categories (e.g., 301, 413) must be mapped to the closest canonical category; the mapping rules must be documented in DESIGN
* Adding a new category in the future is a breaking change (new enum variant) — extensibility rules must be defined separately
* Modules that need finer granularity within a category must use the structured context payload, not a new category
* Developer onboarding must include the canonical category vocabulary as a prerequisite

### Confirmation

Design review confirms all 16 categories are documented with HTTP status mappings. Every module error can be classified into exactly one category.

## Pros and Cons of the Options

### Option A: Full HTTP Status Code Set

Use all ~70 HTTP status codes (1xx–5xx) as the error taxonomy.

* Good, because HTTP developers already know the codes
* Good, because no mapping needed for REST responses
* Bad, because HTTP status codes are transport-specific — they have no meaning in gRPC, SSE, or event-driven contexts
* Bad, because many codes are irrelevant to application errors (1xx informational, 3xx redirects)
* Bad, because the set is too large for exhaustive `match` — developers will use wildcard arms, defeating the purpose
* Bad, because some codes overlap semantically (400 vs 422, 401 vs 403) leading to inconsistent usage across modules

### Option B: Google's 16 Canonical Error Codes

Adopt the 16 categories from `google.rpc.Code`: `cancelled`, `unknown`, `invalid_argument`, `deadline_exceeded`, `not_found`, `already_exists`, `permission_denied`, `resource_exhausted`, `failed_precondition`, `aborted`, `out_of_range`, `unimplemented`, `internal`, `unavailable`, `data_loss`, `unauthenticated`.

* Good, because proven at Google/Cloud scale across hundreds of services
* Good, because maps 1:1 to gRPC status codes (same origin)
* Good, because 16 categories are practical for exhaustive `match`
* Good, because well-documented with clear usage guidance per category
* Neutral, because requires an HTTP status mapping table (one-time effort)
* Bad, because some categories are rarely used in CyberFabric's domain (`data_loss`, `cancelled`)

### Option C: Custom Reduced Set (5-10 Categories)

Define a small custom set (e.g., `client_error`, `server_error`, `not_found`, `unauthorized`, `conflict`).

* Good, because minimal learning curve
* Bad, because insufficient granularity — `client_error` conflates validation failures, permission issues, and precondition violations
* Bad, because no industry precedent — consumers cannot reuse existing knowledge
* Bad, because likely to grow over time as edge cases are discovered, losing the simplicity benefit

## More Information

The 16 categories and their HTTP/context type mappings are defined in [DESIGN.md](../DESIGN.md) § Category Reference.

Google's canonical error codes documentation: https://cloud.google.com/apis/design/errors#handling_errors

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements:

* `cpt-cf-errors-fr-finite-vocabulary` — Defines the 16-category finite error vocabulary
* `cpt-cf-errors-fr-transport-agnostic` — Categories map to both HTTP and gRPC, enabling transport-agnostic error representation
