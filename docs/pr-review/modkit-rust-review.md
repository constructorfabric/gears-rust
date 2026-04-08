---
cypilot: true
type: requirement
name: Rust PR Review Guidelines
version: 1.0
purpose: Idiomatic, engineering-grade checklist for reviewing Rust pull requests
---

# Rust PR Review Guidelines

## Overview

Use this guideline to review Rust pull requests for correctness, idiomatic style, maintainability, safety, and operational quality.

This is a **PR review checklist**, not a language tutorial and not a generic architecture manifesto.  
Focus on **real merge risk**, **idiomatic Rust**, and **actionable findings**.

## Review Goals

Review the PR as a senior Rust engineer. Prioritize:

1. Correctness and invariant preservation
2. Idiomatic Rust usage
3. Error handling and panic safety
4. Async and concurrency correctness
5. API and type design
6. Security and data handling
7. Performance footguns
8. Test adequacy
9. Observability and operability

---

## Output Contract

Produce an **issues-only** report.

For each issue include:

- **Checklist ID**
- **Severity**: CRITICAL | HIGH | MEDIUM | LOW
- **Location**: file path and line(s)
- **Issue**: what is wrong
- **Why it matters**: concrete impact
- **Fix**: specific recommendation

### Rules for reporting

- Report only problems, not praise
- Do not invent issues without evidence
- Do not complain about style that `rustfmt` should handle
- Do not demand speculative abstractions
- Prefer concrete Rust-specific guidance over generic OO theory
- If something cannot be verified from the diff or context, do not state it as fact
- Prefer fewer, higher-signal findings over many weak ones

---

## Severity Dictionary

- **CRITICAL**: can cause data corruption, security issue, undefined behavior, deadlock, production outage, or incorrect behavior in core paths
- **HIGH**: significant correctness, maintainability, or operability risk; should usually be fixed before merge
- **MEDIUM**: meaningful improvement; fix if practical in this PR
- **LOW**: minor issue or polish

---

# MUST HAVE

## RUST-API-001: Idiomatic Public API Design
**Severity**: HIGH

Check that public APIs follow idiomatic Rust conventions.

- Function, type, trait, and module names are clear and conventional
- Types express meaning better than raw `bool`, `String`, or loosely structured maps
- Arguments are hard to misuse
- Builders are used where construction is complex
- Trait boundaries are purposeful and not overly broad
- Public APIs expose the minimum necessary surface
- Return types are ergonomic and predictable
- Visibility is minimal (`pub` only where necessary)

**Review guidance**:
- Prefer domain types over primitive obsession
- Prefer explicit enums/newtypes where they encode invariants
- Avoid exposing implementation details in public signatures

---

## RUST-TYPE-001: Type Safety and Invariants
**Severity**: HIGH

- Important invariants are enforced by types where practical
- Invalid states are made unrepresentable when reasonable
- `Option` and `Result` are used intentionally, not as vague escape hatches
- Newtypes are used when they improve safety or readability
- Distinct concepts are not mixed through aliases of the same primitive type
- Lifetimes and ownership model are used to prevent misuse, not bypassed with clones or shared mutability

**Review guidance**:
- Prefer compile-time guarantees over comments
- Flag APIs that rely on caller discipline when the type system could help

---

## RUST-ERR-001: Error Handling Is Explicit and Useful
**Severity**: CRITICAL

- Fallible operations return `Result` where failure is expected
- Error context is preserved
- Errors are not swallowed or silently downgraded
- Error messages are actionable
- Domain errors are distinguishable where that matters
- The code does not rely on logs alone instead of returning errors

**Review guidance**:
- Flag `map_err(|_| ...)` if it destroys useful context
- Flag generic error wrapping that hides root cause without reason
- Prefer propagation with context over ad hoc stringification

---

## RUST-PANIC-001: Panic Safety
**Severity**: HIGH

- No `unwrap()`, `expect()`, or `panic!()` in production paths without strong justification
- `unreachable!()` is only used where the invariant is truly guaranteed
- Assertions are not used as normal runtime validation in production code
- Panics are reserved for impossible states, tests, examples, or process-fatal initialization where justified

**Review guidance**:
- In library code, panic is usually a much stronger smell
- In service code, `expect()` may be acceptable only for startup invariants with a very clear message
- Flag panic-prone code in request paths, workers, retries, background tasks, and data pipelines

---

## RUST-OWN-001: Ownership and Borrowing Are Used Idiomatically
**Severity**: MEDIUM

- The code does not clone data unnecessarily
- References are preferred when ownership transfer is not needed
- `Arc`, `Rc`, `Mutex`, `RwLock`, and interior mutability are used only when justified
- Large values are not copied or moved unnecessarily
- Borrowing structure keeps APIs ergonomic and efficient

**Review guidance**:
- Flag defensive cloning without evidence
- Flag ownership patterns that make APIs awkward or expensive
- Flag unnecessary heap allocation or conversion churn

---

## RUST-ASYNC-001: Async Code Is Runtime-Safe
**Severity**: CRITICAL

- No blocking I/O or long CPU-bound work on async executor threads without proper offloading
- `.await` is not performed while holding a lock unless the design explicitly requires and justifies it
- Task cancellation is handled where required
- Timeouts are present where the operation can hang indefinitely
- Retries are bounded and observable
- Background tasks have lifecycle control and error handling

**Review guidance**:
- Flag blocking calls inside async paths
- Flag `.await` inside critical sections
- Flag detached tasks with no supervision or shutdown behavior
- Flag loops that can retry forever without jitter, cap, or logging

---

## RUST-CONC-001: Shared State and Concurrency Are Well Designed
**Severity**: HIGH

- Shared mutable state is minimized
- Lock scope is small and intentional
- The chosen primitive matches the workload: channels, atomics, `Mutex`, `RwLock`, etc.
- There is no obvious deadlock or starvation risk
- Concurrency assumptions are visible in the code
- Synchronization is not broader than necessary

**Review guidance**:
- Prefer ownership transfer and message passing over pervasive shared state
- Flag `Arc<Mutex<_>>` used as a default design habit
- Flag nested locking or lock ordering hazards

---

## RUST-SEC-001: Security and Boundary Validation
**Severity**: CRITICAL

- External input is validated at boundaries
- Authorization and tenant/resource scoping are enforced where applicable
- Secrets, tokens, and sensitive identifiers are not logged
- Path, command, SQL, serialization, and deserialization boundaries are treated as hostile
- Dangerous defaults are not silently accepted

**Review guidance**:
- Flag missing validation on request or config boundaries
- Flag security checks implemented too deep or too late
- Flag implicit trust in upstream data without validation

---

## RUST-DATA-001: Serialization and Data Contracts Are Stable
**Severity**: HIGH

- `serde` attributes are intentional and correct
- Field renames, defaults, enum formats, and optionality are safe for the intended contract
- Backward/forward compatibility is considered where relevant
- Deserialization failures remain diagnosable
- Time, UUID, and numeric formats are handled consistently

**Review guidance**:
- Flag accidental wire-format changes
- Flag fragile enum/string handling
- Flag implicit defaults that can hide contract bugs

---

## RUST-PERF-001: No Obvious Performance Footguns
**Severity**: MEDIUM

- No obvious N+1 queries or repeated expensive work in hot paths
- Allocations are not excessive without reason
- Data structures fit the access pattern
- Work is not repeated unnecessarily
- Expensive formatting/logging is not done eagerly in hot paths

**Review guidance**:
- Do not micro-optimize blindly
- Report only clear, likely-relevant footguns
- Prefer evidence-based performance comments

---

## RUST-OBS-001: Logging, Tracing, and Metrics Are Operationally Useful
**Severity**: MEDIUM

- Important failures are logged at the right boundary
- Logs contain enough context to debug production issues
- Sensitive data is not emitted
- Request/task/job identifiers are propagated where relevant
- Metrics or tracing exist for critical operational paths when the service is long-running

**Review guidance**:
- Flag duplicate logging of the same error at multiple layers unless intentional
- Flag logs with no identifiers or context
- Flag missing observability in background workers, retries, queue processing, and external calls

---

## RUST-TEST-001: Tests Cover Behavior, Not Just Syntax
**Severity**: HIGH

- New behavior is covered by tests
- Core happy path is tested
- Important error paths are tested
- Edge cases and regressions are tested where risk justifies it
- Tests verify observable behavior, not internal implementation details
- Tests are deterministic and readable

**Review guidance**:
- Do not require exhaustive testing for trivial refactors
- Do flag missing tests for bug fixes, parsing, state transitions, retries, concurrency-sensitive code, and boundary conditions

---

## RUST-MOD-001: Module Boundaries and Code Organization Are Clean
**Severity**: HIGH

- Responsibilities are separated clearly
- Business logic is not tangled with transport, persistence, or framework glue
- Helpers are not used to hide poor structure
- Modules are cohesive
- Visibility and dependency direction are intentional
- The PR does not introduce avoidable architectural drift

**Review guidance**:
- Flag "god modules"
- Flag handlers/controllers doing domain work directly
- Flag infrastructure details leaking into domain logic without need

---

# MUST NOT HAVE

## RUST-NO-001: No Placeholder Production Logic
**Severity**: CRITICAL

- No `todo!()`, `unimplemented!()`, stub returns, fake success, or empty implementations in production paths
- No placeholder branches that silently discard work
- No fake adapters presented as complete behavior unless clearly test-only

---

## RUST-NO-002: No Silent Failure
**Severity**: CRITICAL

- No ignored `Result` for fallible operations without justification
- No `let _ = ...` on meaningful failures unless explicitly intentional and documented
- No empty error handlers
- No failure paths that only log and continue when correctness requires propagation or state change

---

## RUST-NO-003: No Panic-Driven Control Flow
**Severity**: HIGH

- No `unwrap()` / `expect()` used as ordinary control flow
- No panic used instead of validation or typed error handling
- No assumptions about "this can never fail" unless invariant is obvious and local

---

## RUST-NO-004: No Async Blocking Footguns
**Severity**: CRITICAL

- No blocking file, network, database, sleep, or CPU-heavy work directly inside async tasks without appropriate handling
- No `.await` while holding broad or long-lived locks unless explicitly justified
- No unbounded fan-out of tasks without backpressure

---

## RUST-NO-005: No Unjustified Shared Mutability
**Severity**: HIGH

- No `Arc<Mutex<_>>` as a default convenience pattern
- No pervasive interior mutability where plain ownership would work
- No overly broad lock-protected state blobs

---

## RUST-NO-006: No Unsafe Without Tight Justification
**Severity**: CRITICAL

- No `unsafe` unless it is necessary
- Unsafe blocks must have local justification and clear invariants
- No casual assumptions around aliasing, lifetimes, initialization, or FFI contracts
- No undocumented transmute-like behavior

---

## RUST-NO-007: No Contract Drift by Accident
**Severity**: HIGH

- No accidental API breakage
- No accidental serde/wire/schema changes
- No accidental visibility expansion
- No accidental behavior changes hidden inside refactoring

---

# Review Heuristics

## Prefer This

- Small, explicit types
- Meaningful enums and newtypes
- `Result` with preserved context
- Narrow visibility
- Clear module boundaries
- Structured async flows
- Bounded retries and timeouts
- Tests for behavior and regressions
- Standard library and ecosystem conventions
- Simplicity over abstraction

## Be Suspicious Of

- Generic abstractions with no current need
- Excessive trait layering
- Broad `pub` exposure
- Clone-heavy code
- `Arc<Mutex<HashMap<...>>>` growing into a hidden subsystem
- Lossy error conversion
- Detached background tasks
- Hidden wire-format changes
- Logging without identifiers
- Refactors mixed with behavioral change and no tests

---

# What "Idiomatic Rust" Means in Review

Treat code as more idiomatic when it is:

- Clear without being verbose
- Safe by construction
- Explicit about ownership and failure
- Conservative with shared mutability
- Consistent with standard Rust ecosystem conventions
- Easy to test
- Hard to misuse
- Minimal in API surface
- Honest about runtime behavior

Do **not** equate "idiomatic" with:
- maximum cleverness
- maximum abstraction
- macro-heavy design by default
- avoiding all cloning at any cost
- forcing functional style where it hurts readability

---

# Reporting Format

## Compact Format

```markdown
## Rust PR Review

| # | ID | Sev | Location | Issue | Why it matters | Fix |
|---|----|-----|----------|-------|----------------|-----|
| 1 | RUST-ERR-001 | CRITICAL | src/service.rs:84-96 | Error context is discarded by `map_err(|_| ...)` | Production failures become undiagnosable without the original cause | Preserve source error and add context |
| 2 | RUST-ASYNC-001 | CRITICAL | src/worker.rs:41-58 | Blocking operation in async task | Starves the async runtime and causes latency spikes | Move to `spawn_blocking` or dedicated worker |
````

## Full Format

```markdown
### 1. Error context is lost

**Checklist ID**: `RUST-ERR-001`  
**Severity**: CRITICAL
**Location**: `src/service.rs:84-96`

**Issue**  
The code converts a specific repository error into a generic string/error variant and drops the original cause.

**Why it matters**  
This makes production failures harder to diagnose and may prevent correct retry or classification logic.

**Fix**  
Preserve the original error as source/context and map only at the service boundary if needed.
```

---

# Final Review Discipline

Before finalizing the review:

* Report only real, evidence-based issues
* Prefer Rust-specific findings over generic OO criticism
* Do not request speculative abstractions
* Do not nitpick formatting that tooling should handle
* Escalate correctness, panic, async, concurrency, contract, and security problems first
* Keep the report concise and engineering-focused
