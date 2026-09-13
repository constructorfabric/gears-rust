---
cf: true
type: requirement
name: Rust PR Review Guidelines
version: 1.1
purpose: Idiomatic, engineering-grade checklist for reviewing Rust pull requests
---

# Rust PR Review Guidelines

## Overview

Use this guideline to review Rust pull requests for correctness, idiomatic style, maintainability, safety, and operational quality.

This is a **PR review checklist**, not a language tutorial and not a generic architecture manifesto.
Focus on **real merge risk**, **idiomatic Rust**, and **actionable findings**.

The checklist has two tiers:

1. **Architecture review** — run once at the PR level before reading any file. Catches structural and design-level problems that span multiple files or are invisible inside a single hunk.
2. **Code review** — run per-file. Catches implementation-level problems in the diff.

### Bullet markers

Some criteria carry an inline marker. Both change whether you may post a finding.

- `Requires Rust >= X.Y` — the rule depends on an API or compiler behavior newer than the
  baseline. Check `rust-toolchain.toml` (currently `1.97.0`) and the workspace
  `rust-version` (currently `1.95.0`) before flagging. Never flag code for failing to use an API
  that is newer than the pinned toolchain, and never flag code for failing to use an API newer
  than the declared MSRV in a crate that must honor it.
- `Requires Clippy >= X.Y` — only the **lint coverage** is newer, not the rule. The underlying
  problem is a finding on any toolchain; the marker just tells you whether CI catches it for you.
  Never skip one of these because the toolchain is older, which is the opposite of how
  `Requires Rust >= X.Y` works.
- `Enforcement: clippy <lint> (deny)` — the rule is already denied in `Cargo.toml`
  `[workspace.lints]`, so a violation fails the build on its own. Keep it in mind while reading
  the code, but **do not post it as a finding**: it falls under the existing rule that
  clippy-catchable issues are dropped. `Enforcement: review-only` means no lint covers it and it
  is yours to catch.

## Review Goals

Review the PR as a senior Rust engineer. Prioritize:

1. Architecture and structural correctness (PR-level pass first)
2. Correctness and invariant preservation
3. Idiomatic Rust usage
4. Error handling and panic safety
5. Async and concurrency correctness
6. API and type design
7. Security and data handling
8. Performance footguns
9. Test adequacy
10. Observability and operability

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

Comment wording is defined in one place only: `docs/pr-review/comment-style.md`. Read it before
writing findings. The rules below are about *what* to report, not how to word it.

- Report only problems, not praise
- Do not invent issues without evidence
- Do not complain about style that `rustfmt` should handle
- Do not report anything marked `Enforcement: clippy ... (deny)` — the build already rejects it
- Do not demand speculative abstractions
- Prefer concrete Rust-specific guidance over generic OO theory
- If something cannot be verified from the diff or context, do not state it as fact
- Prefer fewer, higher-signal findings over many weak ones
- Refer to `guidelines/GTS.md` when the PR touches GTS identifiers, type schemas, well-known instances, discriminator/const-enum-like values, `x-gts-traits` / `x-gts-traits-schema`, type registries, or type-driven authorization/extension behavior

---

## Severity Dictionary

- **CRITICAL**: can cause data corruption, security issue, undefined behavior, deadlock, production outage, or incorrect behavior in core paths
- **HIGH**: significant correctness, maintainability, or operability risk; should usually be fixed before merge
- **MEDIUM**: meaningful improvement; fix if practical in this PR
- **LOW**: minor issue or polish

---

# ARCHITECTURE REVIEW (PR-level pass — run before reading individual files)

Assess the PR as a whole before examining any file. Read the title, body, full file list, and diff structure. For each statement below, investigate the diff and relevant code, then flag any violations as PR-level comments with a severity and a concrete fix.

- Long-running work (retries, external I/O, waits) should not block process startup or prevent clean shutdown.
- Known safety limitations documented only in source comments are invisible to operators — they need a runtime signal.
- Layer boundaries must be respected: infrastructure must not encode business rules, domain must not import persistence types.
- Multi-step writes must define a recovery path for every partial-failure arm — no silent half-committed state.
- The same decision (classification, validity check, traversal) should be computed in one place and consumed elsewhere, not re-implemented independently.
- Degraded-mode paths, skipped steps, and background failures must surface at `warn` or higher — not swallowed or logged at `info`.
- Error variants must be semantically distinct; reusing a generic variant for domain-specific conditions breaks pattern-matching by callers.
- New background tasks and periodic loops must have lifecycle control (cancellation, bounded retries, failure signaling).
- Shared mutable state and lock scope must be justified; prefer ownership transfer and message passing.
- If the PR bundles multiple independently shippable changes, note it in the summary (do not post as a PR comment).

---

# MUST HAVE

## RUST-API-001: Idiomatic Public API Design
**Severity**: HIGH

Check that public APIs follow idiomatic Rust conventions.

- Function, type, trait, and gear names are clear and conventional
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
- Accept `impl Into<String>` for owned string parameters instead of forcing callers to pre-convert
- Flag more than 2-3 boolean parameters on one function — use an options struct, enum, or builder instead
- Validating constructors should return `Result`, not panic or silently clamp invalid input
- Flag `Deref`/`DerefMut` impls used to simulate inheritance on a wrapper type — this leaks the inner type's full API and blurs ownership; prefer explicit delegation methods or a trait
- Flag third-party types in public signatures (`reqwest::Error`, `sqlx::Row`, `hyper::Uri`) — wrap them so the dependency can be swapped without a breaking change
- Public config and value types should implement both `Default` (so `unwrap_or_default` and generic containers work) and an inherent `new()`
- Library value types should keep fields private behind a validated constructor; `pub` fields make every invariant a caller responsibility and are a semver commitment
- Flag a hand-rolled `match` on a kind/tag field that dispatches behavior — take a trait object or an `Fn` strategy parameter instead
- When the orphan rule blocks `impl ForeignTrait for ForeignType`, use a newtype wrapper, not a local extension trait or a free helper
- New public types should implement `Debug`, and new `pub` items in a library crate need `///` docs. `Enforcement: clippy missing_errors_doc, missing_panics_doc (deny)` for the error/panic sections; the type-level `Debug` impl and the item doc itself are review-only

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
- Prefer exhaustive `match` over a wildcard `_ =>` catch-all on crate-owned enums, so a new variant forces a compile error at call sites instead of silently falling through
- Use `#[non_exhaustive]` on public enums/structs expected to grow variants or fields later
- Use `#[must_use]` on results that are easy to drop by accident (builders, "must be applied" configs, guards)
- No bare `as` for numeric conversion: widening uses `u64::from(x)`, narrowing uses `TryFrom` with a real error path, and pointer casts use `.cast::<T>()` so constness is preserved. `Enforcement: clippy cast_possible_truncation, cast_possible_wrap, cast_precision_loss, cast_sign_loss (deny)` for the lossy cases; lossless widening (`cast_lossless`) and pointer casts (`ptr_as_ptr`) are review-only
- Comparison traits are all-manual or all-derived over the same field set — `a == b` must hold exactly when `cmp` returns `Equal`. A hand-written `PartialEq` next to a derived `Ord` (or the reverse) is a latent inconsistency. `Requires Rust >= 1.98` for the derive fast path that exposes it
- A type with a manual `PartialEq` cannot be used in a constant pattern. `Requires Rust >= 1.98`
- Do not apply `#[repr(transparent)]` over a `#[non_exhaustive]`, `repr(C)`, or private-field type; keep it only for real ABI or `transmute` needs. `Requires Rust >= 1.98`
- Parse non-zero domain values with `NonZero::<u32>::from_str_radix(s, 10)?` instead of `parse()` followed by `NonZero::new(..).ok_or(..)`, so zero is a parse error rather than a second failure mode. `Requires Rust >= 1.98`
- Manual `PartialEq`/`Hash`/`Debug`/`Ord` impls should destructure `Self { .. }` and name every ignored field, so adding a field breaks the build instead of silently changing semantics. `Enforcement: clippy missing_fields_in_debug (deny)` for `Debug` only; the other impls are review-only
- In destructuring patterns prefer a named ignore (`has_fuel: _`) over a bare `_` or `..`, so the reader sees which fields were deliberately unused
- Flag `..Default::default()` in a struct literal for a crate-owned config or request type — a field added later is silently defaulted at every construction site instead of forcing a compile error. List the fields explicitly
- Note when reviewing exhaustiveness: `if let` guards do not participate in exhaustiveness checking, so a `match` that looks total may not be. `Requires Rust >= 1.95`

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
- Use `TryFrom`, not `From`, for conversions that can fail — a `From` impl that panics or silently coerces on bad input is a correctness bug
- Prefer `bool::ok_or` / `bool::ok_or_else` over a stack of `if cond { return Err(..) }` guards: the receiver is the **`bool`** condition that must hold, as in `user.is_active().ok_or(Error::Inactive)?`. This is the method on `bool` (unstable feature `bool_to_result`), not the long-standing `Option::ok_or`, which needs no version gate. `Requires Rust >= 1.98`
- Box large error payloads, and never return `Result<_, ()>` from an async signature — the `clippy::result_large_err` and `clippy::result_unit_err` lints now fire on `async fn` too. The underlying rule holds on any toolchain; only the lint coverage on `async fn` is new. `Requires Clippy >= 1.98`

---

## RUST-PANIC-001: Panic Safety
**Severity**: HIGH

- No `unwrap()`, `expect()`, or `panic!()` in production paths without strong justification
- `unreachable!()` is only used where the invariant is truly guaranteed
- Assertions are not used as normal runtime validation in production code
- Panics are reserved for impossible states, tests, examples, or process-fatal initialization where justified

**Review guidance**:
- In library code, panic is usually a much stronger smell
- In service code, `expect()` may be acceptable only for startup invariants, and only when the call carries `#[expect(clippy::expect_used, reason = "...")]` plus a very clear message. `expect_used` is denied workspace-wide, so an unannotated `expect()` does not build; the annotation is what makes the carve-out real
- Every `unwrap`/`expect` exception must state why the value is guaranteed `Some`/`Ok`. A `const` initializer is the only broadly defensible case. `Enforcement: clippy unwrap_used, expect_used (deny)` for the bare call; the missing justification is review-only
- Flag panic-prone code in request paths, workers, retries, background tasks, and data pipelines
- When a missing value has a sensible default, the fix for `unwrap()` is `unwrap_or`/`unwrap_or_else`/`unwrap_or_default`, not a justification comment. `Enforcement: clippy unwrap_used (deny)`
- Flag `push`/`insert` followed by `last_mut().unwrap()` or `.expect()` — `Vec::push_mut`, `Vec::insert_mut`, and `VecDeque::push_{front,back}_mut` hand back `&mut T` with no unwrap at all. `Requires Rust >= 1.95`
- Flag an `is_empty()`/`len()` check decoupled from a later `v[0]`/`v[i]` access — match on `v.as_slice()` (`[]`, `[one]`, `[first, rest @ ..]`) so the compiler proves the access instead of the reviewer
- Flag `v[i]` or `&s[a..b]` where the index derives from external input — use `.get()` or a pattern match
- Byte-range slicing a `str` (`&s[..8]`) with externally derived bounds panics inside a multi-byte UTF-8 sequence — use `s.get(..8)` or `chars().take(n)`
- Flag an unbounded integer or char range loop (`for i in 250u8..`) — it wraps, panics, or never terminates. The bug is real on any toolchain; only the `clippy::for_unbounded_range` lint is new. `Requires Clippy >= 1.98`

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
- Prefer borrowed parameter types over owned (`&str` not `&String`, `&[T]` not `&Vec<T>`, `&T` not `&Box<T>`)
- Use `mem::take`/`mem::replace` to move a field out of `&mut self` or an enum variant instead of cloning it
- Fallible functions that take ownership of an argument should return that value inside the error so the caller can retry without re-cloning
- Flag a single lifetime parameter used to bound both an input argument and a stored/returned reference — it over-constrains callers; use separate lifetimes
- Prefer RAII guard types (custom `Drop`) over manual acquire/release call pairs
- Flag a clone, `RefCell`, or `Arc` introduced only to borrow two fields of the same struct — decompose the struct into independently borrowable parts instead
- `move` closures and spawned tasks should capture the narrowest value: rebind in a scope block (`let db = Arc::clone(&db);`) rather than capturing `self` or a whole context. Spell a refcount bump `Arc::clone(&x)`, never `x.clone()`, so it is distinguishable from a deep clone
- Prefer confining `mut` to an initialization block (`let x = { let mut x = ..; x.sort(); x };`) over a binding that stays mutable long after setup

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
- Functions holding partial or shared state across `.await` points should be auditable for cancel-safety — consider what happens if the future is dropped mid-await via `select!` or a timeout
- `Drop` cannot `.await` — audit `Drop` impls on async-held resources (transactions, connections, guards) for cleanup that actually needs an async call; it must be handled explicitly, not assumed to run via `Drop`
- CPU-bound async loops should yield periodically (`tokio::task::yield_now()`) so they do not starve other tasks on the same executor thread
- Every async fn reachable from `select!`, `timeout`, or an abortable task should carry a `// cancel-safe: <reason>` or `// NOT cancel-safe: <reason>` comment. "All awaits are idempotent" is not a valid reason, and per-call semantics differ: `read` is cancel-safe, `read_exact` is not
- `await_holding_lock` and `await_holding_refcell_ref` are necessary but not sufficient — hand-review every `Mutex` import in an async module. A guard returned from a helper, stored in a struct field, or produced by `MutexGuard::map` escapes the lint entirely. `Enforcement: clippy await_holding_lock, await_holding_refcell_ref (deny)` for the direct shape only; the escaping shapes are review-only

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
- Prefer `Atomic*::update` / `try_update` over a hand-rolled `compare_exchange` retry loop. `Requires Rust >= 1.95`

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
- Never leak internal details (stack traces, file paths, SQL text, dependency versions) in error responses returned to external callers
- Security-sensitive randomness (tokens, session IDs, nonces) must use a CSPRNG — `OsRng` or `getrandom`, never `rand::thread_rng()` or a seeded PRNG
- Outbound requests built from user-supplied URLs or hosts must guard against SSRF (validate/allowlist the destination, block internal/link-local ranges) before the request is issued
- Flag hardcoded secrets, API keys, passwords, or tokens committed literally in the diff
- Flag disabled or weakened TLS certificate validation, and require a TLS floor of 1.2 or higher. For mTLS, the client chain must be validated *and* the CN/SAN checked — an accepted chain with no name check is not authentication
- Every string input needs an explicit maximum length enforced before it is processed
- Validation patterns must be allowlists, not denylists
- Never `format!`-interpolate an unvalidated identifier into an outbound API path or URL; validate the charset first
- Strip matching delimiters atomically with `str::strip_circumfix`. Chained `strip_prefix`/`strip_suffix` with `unwrap_or(raw)` silently accepts half-delimited input, which matters for quoted header values and bracketed IPv6 in `Forwarded`/RFC 7239 parsing that feeds rate limiting or allowlists. `Requires Rust >= 1.98`
- Decode UTF-16 with `String::from_utf16le`/`from_utf16be` (endianness stated in the name, no intermediate `Vec<u16>`), and use the fallible form rather than `_lossy` on security-relevant input — U+FFFD substitution collapses distinct malformed inputs and defeats allowlist comparison. `Requires Rust >= 1.98`
- Secrets held in memory must be wrapped (`secrecy::Secret<String>`) so they zeroize on drop and redact in `Debug`/`Display`. A plain `String` field in a config struct leaks through any `{:?}` log line

---

## RUST-SEC-002: HTTP Response Security Headers and Fingerprint Suppression
**Severity**: HIGH

Applies to a PR that adds or changes a router, server bootstrap, or response middleware.

- The OWASP Secure Headers set is applied once, from a single tower layer, not per handler
- `Strict-Transport-Security: max-age=63072000; includeSubDomains`
- `X-Content-Type-Options: nosniff`
- `X-Frame-Options: deny`
- `Content-Security-Policy: default-src 'self'; object-src 'none'; frame-ancestors 'none'`
- `Referrer-Policy: no-referrer`
- `Permissions-Policy` present and restrictive
- COOP, COEP, and CORP present for browser-facing responses
- `Cache-Control: no-store` on API responses that carry per-user data
- No `Server` or `X-Powered-By` header, and no `X-*` header carrying a build hash, internal hostname, or tracing ID

**Review guidance**:
- Flag a new router that mounts no header layer at all, once, at the router; do not repeat the finding per route
- Flag a header set applied per handler instead of as a layer, since it will be forgotten on the next route
- Flag a permissive CSP (`unsafe-inline`, `unsafe-eval`, `*`) added without a stated reason

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
- Flag `n != 0` when decoding a strictly-0/1 wire or storage flag — `bool::try_from(n).map_err(...)` surfaces out-of-contract bytes as a parse error instead of silently reading them as `true`. `Requires Rust >= 1.95`
- The `f32`/`f64` `algebraic_add`/`sub`/`mul`/`div`/`rem` methods permit reassociation, so results vary across call sites, optimization levels, and targets. Never use them on a value that is compared, sorted, hashed, serialized, persisted, asserted on, or used for billing, quota, or alert thresholds. `Requires Rust >= 1.98`

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
- Flag `.collect::<Vec<_>>()` immediately consumed by another loop or iteration — chain iterators instead of materializing an intermediate collection
- Flag `Box::new([0; N])` for large buffers — allocate via `vec![0; n]` to avoid building an oversized value on the stack before the move
- Flag `map.get(&key.to_string())` or a `.clone()` at a lookup site — a `HashMap<String, V>` can be queried with `&str`
- Box oversized enum variants; collapse double indirection (`Box<Vec<T>>` to `Vec<T>`, `Box<String>` to `String`, `Arc<String>`/`Rc<String>` to `Arc<str>`/`Rc<str>`). `Enforcement: clippy rc_buffer (deny)` for the `Rc`/`Arc` shapes; `large_enum_variant` and the `Box` shapes are review-only
- Flag `String::from("lit")` or `format!("lit")` with no interpolation — use the `&'static str` literal where the receiver accepts it. `Enforcement: clippy manual_string_new (deny)`
- Prefer one `format!` over a chain of `push_str` for mixed literal and dynamic text; in hot paths use `String::with_capacity(n)` plus `push_str` instead of repeated `+`/`format!` reallocation. `Enforcement: clippy format_push_string (deny)`
- `&str.to_string()` should be `.to_owned()`; `Vec::with_capacity(0)`/`String::with_capacity(0)` should be `new()`. `Enforcement: clippy str_to_string (deny)`
- Large stack frames and large stack arrays in tasks with small stacks. `Enforcement: clippy large_stack_arrays (deny)` plus the `stack-size-threshold` in `clippy.toml`
- Prefer std bit-manipulation methods (`bit_width`, `isolate_highest_one`, `isolate_lowest_one`, `highest_one`, `lowest_one`) over hand-rolled shift and mask arithmetic, which usually mishandles the zero case. `Requires Rust >= 1.97`
- In hot integer-formatting loops, `core::fmt::NumBuffer<T>` plus `n.format_into(&mut buf)` reuses one stack buffer instead of allocating per call, and removes the need for an `itoa`-style dependency. `Requires Rust >= 1.98`
- `core::hint::cold_path()` may mark a genuinely unlikely branch, but it is a hint only and must never be load-bearing for correctness. `Requires Rust >= 1.95`

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
- Authentication attempts, both success and failure and with the source IP, and authorization denials, with identity, requested resource, and reason, must be logged through structured `tracing`. Never log request or response bodies that may carry credentials or PII

---

## RUST-OBS-002: No Debug Artifacts in Production Output
**Severity**: MEDIUM

- No `dbg!()` in committed code
- No `println!()`, `eprintln!()`, or direct writes to stdout/stderr in library or service code
- Diagnostic output goes through `tracing::{trace,debug,info,warn,error}` so it is structured, level-gated, and correlated

**Review guidance**:
- `Enforcement: clippy dbg_macro, use_debug (deny)` covers `dbg!` and `{:?}` formatting in output. `println!`/`eprintln!` are **not** denied in this workspace, so those are review-only and postable
- A CLI binary writing intended program output to stdout is not a finding; a service or library doing it is

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
- New parser, validator, or serde-roundtrip code should carry at least one property-based test (`proptest`/`quickcheck`): no panic on arbitrary input, and roundtrip identity
- A test that needs live services or network access must not land in the unit tier — place it under `tests/integration` or `tests/e2e` and state the tier, so the default suite stays runnable without external dependencies

---

## RUST-MOD-001: Gear Boundaries and Code Organization Are Clean
**Severity**: HIGH

- Responsibilities are separated clearly
- Business logic is not tangled with transport, persistence, or framework glue
- Helpers are not used to hide poor structure
- Gears are cohesive
- Visibility and dependency direction are intentional
- The PR does not introduce avoidable architectural drift

**Review guidance**:
- Flag "god gears"
- Flag handlers/controllers doing domain work directly
- Flag infrastructure details leaking into domain logic without need
- New platform-gated code should use std `cfg_select!` rather than the `cfg-if` crate. Do not ask for existing `cfg_if!` usages to be migrated. `Requires Rust >= 1.95`
- A function past the pinned complexity or length thresholds should be decomposed, not given another branch. `Enforcement: clippy cognitive_complexity, too_many_lines (deny)` at the thresholds in `clippy.toml` (cognitive complexity 20, 200 lines)
- Source-level blanket lint escalation is covered by RUST-LINT-001

---

## RUST-LINT-001: In-Source Lint Suppression Hygiene
**Severity**: MEDIUM

- Every in-source `#[allow(...)]` and `#[expect(...)]` carries `reason = "..."`
- `#[expect]` is preferred over `#[allow]` so the suppression fails once it becomes unnecessary
- Suppressions are as narrow as possible: per item or per expression, never crate-level
- No crate-level group `allow` (`clippy::all`, `clippy::pedantic`, `clippy::nursery`) added in source
- No `#![deny(warnings)]` or equivalent blanket escalation in source

**Review guidance**:
- A blanket source-level `deny(warnings)` silently escalates any future lint, including new compiler lints, into a hard build break for downstream consumers. Deny specific lints by name, or gate it in the build: prefer `build.warnings = "deny"` in `.cargo/config.toml` or `CARGO_BUILD_WARNINGS=deny` over `RUSTFLAGS="-D warnings"`, since it applies to local packages only and does not bust the build cache. `Requires Rust >= 1.97` for the `build.warnings` form
- Where a foreign trait mandates `async fn`, silence `unused_async_trait_impl` per impl with `#[expect(..., reason = "...")]`, never crate-wide
- `Enforcement: clippy ignore_without_reason (deny)` covers `#[ignore]` on tests only. The general `reason` requirement on `#[allow]`/`#[expect]` is not linted in this workspace and is review-only

---

## RUST-DEP-001: Dependency and Advisory Manifest Hygiene
**Severity**: HIGH

Applies **only** when the diff touches a manifest or lint/advisory config: `Cargo.toml`,
`Cargo.lock`, `deny.toml`, `.cargo/audit.toml`, `.cargo/config.toml`, `clippy.toml`, or
`rust-toolchain.toml`. Skip this rule entirely when the diff contains none of them.

- No dependency added with a `git = ...` source or a non-crates.io `registry = ...` source; both are supply-chain surface with no advisory or vet coverage
- No `*` or open-ended version specification on a newly added dependency
- A RUSTSEC advisory added to `ignore = [...]` in `.cargo/audit.toml` or `deny.toml` carries a comment stating why it does not apply to this code
- `.cargo/audit.toml` and `deny.toml` do not drift apart: an advisory accepted in one is reflected in the other
- A `[lints.*]` group entry (`all`, `pedantic`, `nursery`) declares `priority = -1`; without it Cargo rejects the manifest as soon as any per-lint override exists
- No lint declared that no longer exists (`string_to_string`, `from_iter_instead_of_collect` now emit `unknown_lints`)
- `unsafe_code` is not downgraded from `forbid` to `deny` without a justified per-item `#[allow(unsafe_code)]` accompanying the change
- The `deny.toml` license allowlist is not widened, and `[sources]` is not loosened, without a stated reason

**Review guidance**:
- `Enforcement: clippy wildcard_dependencies (deny)` covers the wildcard version case; everything else here is review-only
- An undocumented `ignore = ["RUSTSEC-...."]` entry is the highest-signal finding in this rule: it converts a known vulnerability into a silent one
- A toolchain pin change in `rust-toolchain.toml` shifts which `Requires Rust >= X.Y` criteria are live across the whole catalog; call it out

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
- No `iter.by_ref().peekable().peek()` — it silently consumes and discards an item. Bind the `Peekable` or call `.next()`. The bug is real on any toolchain; only the `clippy::by_ref_peekable_peek` lint is new. `Requires Clippy >= 1.98`
- No `mem::forget` on a type with a `Drop` impl; it is almost always a leak bug rather than an intentional leak

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

`Enforcement: rustc unsafe_code (forbid)` workspace-wide, and `forbid` cannot be overridden
locally. The criteria below therefore apply only to a crate that deliberately opts out of the
workspace lint block. Do not post them against a crate that inherits `forbid`.

**Review guidance**:
- Recover a sub-slice offset with `str::substr_range` / `[T]::subslice_range`, never `sub.as_ptr().offset_from(parent.as_ptr())`. Both return `Option` and need no `unsafe`; note `subslice_range` panics for zero-sized element types. `Requires Rust >= 1.98`
- Use `Atomic::from_mut` / `from_mut_slice` / `get_mut_slice` instead of transmuting or `slice::from_raw_parts_mut`-casting a plain buffer to `[AtomicU32]`; `&mut` already proves exclusivity, so no `unsafe` is needed. `Requires Rust >= 1.98`
- For a multi-byte read out of a `&[u8]`, use `u16::from_le_bytes(buf.get(a..b).ok_or(..)?.try_into().map_err(..)?)`, or `core::ptr::read_unaligned` with a `// SAFETY:` comment. Never `*(p as *const u16)` or `ptr::read::<u16>` on a pointer derived from a byte slice: both require T-alignment and are UB or a hardware trap on non-x86 targets. Never `try_into().unwrap()`
- `#[unsafe(no_mangle)]`, `#[unsafe(link_section)]`, `#[unsafe(export_name)]`, and `#[unsafe(naked)]` now trip `unsafe_code`. A crate that needs them cannot stay on `forbid` — move it to `deny` plus a justified per-item `#[allow(unsafe_code)]` with a `// SAFETY:` note. `Requires Rust >= 1.98`
- Do not define symbols the Rust runtime reserves (`memcmp`, `memset`, `strlen`); `invalid_runtime_symbol_definitions` is deny-by-default and sits outside the `warnings` group. Do not return `core::ffi::c_void` from an `extern "C"` shim. `Requires Rust >= 1.98`
- Flag pointer casts that change alignment requirements, and raw-pointer arguments dereferenced in a safe fn, as prose findings even where the lints are silent
- Negative guardrail: do not demand Miri coverage on a crate with `unsafe_code = "forbid"`, on an FFI-heavy crate, or on a bare-metal target. Use `cargo-geiger` for transitive `unsafe` instead

---

## RUST-NO-007: No Contract Drift by Accident
**Severity**: HIGH

- No accidental API breakage
- No accidental serde/wire/schema changes
- No accidental visibility expansion
- No accidental behavior changes hidden inside refactoring
- No new blanket trait impls (`impl<T: Bound> Trait for T`) in a public API without deliberate justification — they are a semver/coherence hazard for downstream crates. The fix is to seal the trait behind a private supertrait so only this crate can implement it, or to write per-type impls
- No `..Default::default()` in a crate-owned struct literal where a later field addition would be silently defaulted instead of breaking the build (see RUST-TYPE-001)

---

# Review Heuristics

## Prefer This

- Small, explicit types
- Meaningful enums and newtypes
- `Result` with preserved context
- Narrow visibility
- Clear gear boundaries
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
* Drop anything marked `Enforcement: clippy ... (deny)` — the build already rejects it, and posting it costs a slot that a real finding needs
* Check the toolchain before flagging a `Requires Rust >= X.Y` criterion
* Word every comment per `docs/pr-review/comment-style.md`, which is the only authority on phrasing
