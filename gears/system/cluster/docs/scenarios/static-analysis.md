# Static Analysis — Scenario Details (`SC-LINT-*`)

> Detail for §9 of the [scenario catalog](./README.md). Template: *Intent / Steps /
> Expected / Done-when*. These are **L1** dylint `ui_test` fixtures, not runtime tests —
> they verify the compile-time rule that enforces `nfr-bounded-critical-section`
> (no remote I/O inside a lock critical section; ADR-002). Without them the rule can
> rot into aspirational documentation.

**SC-LINT-001 — positive fixture: rule fires** · L1 · ☐
- *Intent:* a remote call between `try_lock`/`lock` and `release` must be flagged at compile time.
- *Steps:* a `ui_test` fixture holding a `LockGuard` and issuing a cluster remote call (e.g. `cache.get(...)`) inside the critical section.
- *Expected:* the dylint rule emits a diagnostic at the offending call, with the expected message captured in the fixture's `.stderr`.
- *Done-when:* the `ui_test` passes with the diagnostic at the correct span.

**SC-LINT-002 — negative fixture: no false positive** · L1 · ☐
- *Intent:* compliant code (remote effects *before* acquire or *after* release; local-only critical section) must not be flagged.
- *Steps:* a `ui_test` fixture with a lock whose critical section does only local work, with remote calls outside the guard's scope.
- *Expected:* no diagnostic is emitted.
- *Done-when:* the `ui_test` compiles clean with an empty `.stderr`.

> **Scope note:** the rule is initially scoped to the four cluster backend traits within
> `try_lock`/`release` scopes; DB-transaction enforcement is a follow-up rule extension
> (DESIGN §2.2). Fixtures should be added for that scope when it lands.
