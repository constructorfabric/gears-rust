<!-- Created: 2026-07-30 by Constructor Tech -->

# Defect Class → Control Map


<!-- toc -->

- [How to use it](#how-to-use-it)
- [Tenant isolation bypass](#tenant-isolation-bypass)
- [Missing authorization gate](#missing-authorization-gate)
- [Globally scoped tables](#globally-scoped-tables)
- [Wire ↔ storage type mismatch](#wire--storage-type-mismatch)
- [Error classification](#error-classification)
- [Information disclosure](#information-disclosure)
- [In-process bypass](#in-process-bypass)
- [N+1 and query-count regressions](#n1-and-query-count-regressions)
- [Concurrency and write-skew](#concurrency-and-write-skew)
- [API completeness](#api-completeness)
- [Coverage that proves nothing](#coverage-that-proves-nothing)
- [Related documents](#related-documents)

<!-- /toc -->

Every other document in this folder answers *how to build a thing*: how to write a unit test, an E2E
test, a repository, a REST operation. This one answers the inverse question, which none of them
does:

> Given a class of defect, which control is supposed to catch it — and is that control actually
> present on my gear?

The distinction matters because defects do not escape from laziness nearly as often as they escape
from **nobody's job**. A tenant-scope bypass that ships is rarely one someone declined to test; it
is usually one that no existing control was obliged to catch, so no reviewer had grounds to block
it. That is what this map is for: before merging, walk the classes that apply to your gear and
confirm each has a control, not a good intention.

## How to use it

- **Writing a gear:** treat each applicable class as a requirement on your test suite, not a
  suggestion. If a class applies and you have no control for it, say so in `DESIGN.md` with a reason.
- **Reviewing:** for any defect you find, locate its class below. If the class has a control and the
  gear lacks it, that is the finding — the specific bug is a symptom.
- **Finding a defect no class covers:** add the class. A defect that fits nowhere here means the map
  has a hole, and the hole will let the next one through too.

Controls are ordered within each class from cheapest to most thorough. Cheap ones are not weaker —
a lint that makes a mistake unrepresentable beats a test that catches it after the fact.

## Tenant isolation bypass

An operation that reads or writes rows belonging to a tenant the caller cannot reach.

- **Lint: an `AccessScope` obtained and then discarded.** Binding a scope to `_` or dropping it is
  almost always a bypass, and it is statically detectable — the value exists precisely to be
  threaded into the query. This is the single highest-yield control for this class, because the
  mistake is invisible in review (the call to the enforcer *is* there) and invisible in tests that
  use one tenant.
- **Lint or review gate: `AccessScope::allow_all()` on a production path** without a written
  justification at the call site. Legitimate uses exist — an in-process PDP that cannot re-enter
  itself, a migration — and each should have to say so.
- **Two-tenant harness.** For every route, an object created under tenant A must be unreachable and
  unmodifiable under a context for tenant B. A single-tenant suite cannot fail on this class no
  matter how thorough it is, which is why coverage numbers say nothing here.

## Missing authorization gate

A route or service method reachable without a policy decision.

- **Structural check: a service method reachable from a REST handler takes `&SecurityContext`.** A
  method that cannot see the caller cannot be gated; making the parameter mandatory turns an
  omission into a compile error.
- **Deny-all harness.** Under a `PolicyEnforcer` that denies everything, no authenticated route may
  answer 2xx. This catches the whole class at once instead of one route at a time, and it keeps
  catching newly added routes for free.
- **Machine-checked permission matrix.** Route → resource → action, verified against the code rather
  than written in prose. A prose matrix drifts silently, and the drift is invisible until an
  operator writes a policy from it and the gear denies a permitted caller.

## Globally scoped tables

Tables with no tenant column — type registries, plugin catalogues, platform-wide settings.

- **Registry of such entities plus a mandatory gate on every access.** An entity declared with no
  tenant/resource/owner columns cannot be filtered, so the *only* protection is the permission
  check. Enumerate these entities explicitly; the danger is that they look scoped because every
  neighbouring table is.
- Note the failure mode peculiar to this class: since there is nothing to filter, a resource
  descriptor that advertises no properties makes a PDP's baseline constraint fail to *compile*, and
  a permitted caller receives 500 rather than a result. Fail-closed here looks like an outage, not
  like a denial.

## Wire ↔ storage type mismatch

A value whose representation on the wire differs from its column type — a string on the wire against
a numeric surrogate key, a UUID against text.

- **An executing test on the real database engine** for every predicate and filter that becomes SQL.
  This class is invisible on a permissive engine: a loosely typed backend silently returns an empty
  page where the production engine raises `operator does not exist`. Engine parity is not optional
  for this class.
- **No assertions on `format!("{:?}")` of a request struct.** A debug-format assertion passes while
  the value never reaches the database, so it proves the request was built, not that the query
  works. Defects of this class survive precisely under such tests.

## Error classification

Wrong status code, or an infrastructure failure reported as a client error and vice versa.

- **A full problem-shape assertion in every negative test**, not a status-code check. Status alone
  passes while the body is a bare string, the wrong problem type, or missing the field that lets a
  client dispatch.
- **No blanket `map_err` of a foreign error into a database/internal variant.** This single pattern
  converts every client mistake in a library — a bad filter, an unknown sort field, a stale cursor —
  into a 500. It is the most common source of misclassification and the easiest to lint for.
- **A storage-error → canonical-code table** that is part of the contract rather than folklore.
  Unique-violation, foreign-key violation, serialization failure and deadlock each have one correct
  answer; deciding them per call site guarantees divergence.

## Information disclosure

A response that reveals the existence, name or identifier of something the caller cannot access.

- **Assert the absence of foreign identifiers in response bodies** — not only in the obvious
  payloads but in error messages, which is where they usually leak, because a helpful message
  interpolates the value it just rejected.
- **Rule: "no access" and "does not exist" return the same answer** wherever the difference would be
  an existence oracle. This applies especially to resources a gear does not own: a gear that cannot
  legitimately enumerate foreign tenants must not disclose which ones exist, so an unreachable
  target has to be indistinguishable from an absent one.
- Watch for the asymmetric case: a uniqueness check that must run unscoped in order to be correct,
  and therefore has a foreign identifier in hand at the moment it fails.

## In-process bypass

The same operation gated over HTTP and ungated when called in-process.

- **Rule: the local client adapter goes through the same gates as the REST handlers.** Both are
  peers over the domain layer, and a bypass granted to a shared client is granted to every consumer
  of it, not to the one that needed it.
- **Beware identical signatures with different authorization.** Where a narrow read trait duplicates
  method names from a gated client trait, nothing at the call site distinguishes them, and resolving
  the wrong one from the client hub silently removes authorization. Name the ungated surface so the
  bypass is visible where it is used.

## N+1 and query-count regressions

Statement count that grows with data size rather than with request size.

- **A test asserting the exact number of statements per operation**, run at two data sizes so the
  slope is what is asserted, not the absolute count. Absolute counts rot on every refactor; slopes
  do not.
- This control has a blind spot worth stating: statement counting cannot see isolation levels or the
  cost of a single statement, so a lowered isolation level and a ten-thousand-item `IN` list both
  pass every rule in it.

## Concurrency and write-skew

Two correct-looking transactions that together violate an invariant.

- **A real-engine concurrency suite with a post-state invariant check** in every scenario, since the
  failure is in the *resulting state*, not in either transaction's return value.
- Enumerate the **pairs**, and re-enumerate when adding a write path: a suite covers the pairs it
  names and nothing else. A new operation means a new pair, and a lowered isolation level means the
  old pairs no longer prove what they used to.
- **Watch write sets, not just isolation.** An operation that rewrites a column it never intended to
  change — because it read the row and passed every field back — loses a concurrent writer's update
  regardless of how carefully isolation was chosen. Disjoint write sets remove the hazard instead of
  guarding it.

## API completeness

Partial update impossible, atomic operations expressed as field edits, counters absent from error
bodies, request and response shapes disagreeing.

- **A baseline checklist of what a gear must provide by default**, walked before merge. This class
  has no natural test: nothing fails, a client simply cannot express what it needs, and the gap is
  only visible against an explicit expectation.
- Pay attention to fields that are **operations wearing the costume of attributes**. A parent
  reference whose change triggers validation and a projection rebuild is not an ordinary column, and
  putting it in a replacement payload makes "do not change this" inexpressible.

## Coverage that proves nothing

Not a defect class in the product — a defect class in the suite, and the reason several of the above
survive.

- **A stub that answers "not found" to everything** makes every validation path that tolerates
  absence unreachable. The tests pass, the branch is never executed, and coverage counts it.
- **A test whose named subject has no production caller** measures nothing. Check that the function
  under test is reachable from a request or a lifecycle hook.
- **Shared mutable state across tests** — a fixture that mutates rows other tests read — makes
  results order-dependent, so a failure appears and disappears without a code change.
- **Identifiers in a test plan that no test carries.** If the plan is the contract, an entry with no
  test is a claim with no evidence; either the test exists or the entry says it does not.

## Related documents

- `12_unit_testing.md`, `13_e2e_testing.md` — how to write the controls named above.
- `14_security_review_checklist.md` — the walk-before-merge form of the tenant-scope, global-table,
  disclosure and in-process classes.
- `15_gear_api_baseline.md` — the walk-before-merge form of the API-completeness class.
- `11_database_patterns.md` — transactions, isolation and retry, for the concurrency class.
