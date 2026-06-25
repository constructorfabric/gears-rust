---
name: go-module-docs
description: Write or review the documentation for a Go package under internal/ to the kitsoki standard. Use when adding a new package, when a package's doc.go / exported-symbol comments / runnable examples need writing or auditing, or when the user asks "is this package documented well", "bring this up to the doc standard", or "review the docs for <pkg>". Covers the eight-point rubric (doc.go, sections, non-goals, per-symbol "why", contracts, named constants, Example tests, live references), the paragon to copy, and the verification commands.
---

# Go module documentation standard

Every package under `internal/` documents itself to one bar. The
source of truth for the standard is
[`internal/README.md`](../../../internal/README.md); this skill is the
operational checklist for writing or auditing a package against it.

The **paragon** — the package to read first and mimic — is
[`internal/semroute`](../../../internal/semroute). Run
`go doc -all ./internal/semroute` and skim `doc.go`,
`verdict.go`, and `example_test.go` before writing your own. The
**`clock`** package is the second reference: it shows the same
discipline on an interface, plus the smallest possible runnable
examples (`internal/clock/example_test.go`).

## The eight-point rubric

```
□ S1  doc.go exists; first sentence says what it is + where it sits
□ S2  # sections, reader-first order, with a concrete Worked example
□ S3  Non-goals section, each with its reason
□ S4  every exported symbol: doc starts with the symbol name, explains WHY
□ S5  contracts stated: zero value, nil receiver, concurrency, error conditions
□ S6  magic numbers → documented named constants
□ S7  Example* funcs with checked // Output: blocks
□ S8  references resolve to LIVING docs (no dangling proposal §refs)
```

### S1 — `doc.go` with a "what + where it sits" opener

A dedicated `doc.go` holds the package comment. The **first sentence**
must name what the package is and how it relates to its neighbours —
that sentence is all `go doc <pkg>` and pkg.go.dev list views show.

> Package semroute implements the semantic-routing tier. It sits in the
> orchestrator between `TryDeterministic` and `Turn`: when the
> deterministic match misses, the orchestrator consults the per-app
> `Matcher` before calling the LLM harness.

### S2 — structured `#` sections, reader-first, with a worked example

godoc renders `# Heading` lines as sections. Proven order:
**Algorithm → key invariants → Worked example → Lifecycle → Non-goals.**
The worked example earns its place — a concrete input→output trace
teaches faster than any prose:

```
in:  "wade across the river"
tok: wade, acros, river       (stopwords let, the dropped)
bag: {wade, acros, river}
verdict: { Intent:"ford", Confidence:0.90, MatchReason:"synonym:wade" }
```

### S3 — a Non-goals section, each with its reason

State what the package deliberately does **not** do, and why. This is
what stops a future reader from "fixing" an intentional omission.

> - No alternation, regex, or optional segments in templates — the
>   design commits to "more templates over more DSL features."
> - No learned ranking. The bands are constants.

### S4 — every exported symbol documented in "why over what" voice

Doc comment starts with the symbol's name (godoc convention). Don't
restate the signature — state the contract and the reasoning.

> Match never errors on "no match" — the zero Verdict is the signal.
> An error is returned only for context cancellation; the function
> performs only one `ctx.Err()` check at entry to avoid scattering
> cancellation checks through what is otherwise a hot path.

### S5 — spell out the hard contracts

Zero value, nil receiver, concurrency safety, and error conditions —
the things that cause production bugs when left implicit.

> Matcher is the per-app compiled synonym index. Safe for concurrent
> use… The zero value is NOT useful — always go through Compile. A nil
> Matcher does, however, behave like an empty matcher.

`clock` shows the same on a tricky internal invariant, documenting the
*failure mode* right where a maintainer would trip on it:

> fireExpired runs entirely under f.mu. If a fired timer's channel
> receiver calls back into f.After… from the same goroutine, it will
> deadlock on f.mu. Handlers must not re-enter the clock synchronously.

### S6 — named constants over magic literals

Lift magic numbers to documented constants — including ones the
package deliberately never emits, documented as such so callers don't
repeat the literal (`semroute.ConfidenceExact`).

### S7 — runnable `Example*` functions with `// Output:` blocks

Put `ExampleXxx` functions in a `_test.go` (package `<pkg>_test`).
They're compiled, executed, and verified by `go test`, so the docs
**cannot** drift from behaviour, and they render as canonical usage on
pkg.go.dev. Mirror the worked example from `doc.go` in code. See
`internal/clock/example_test.go` for the minimal shape and
`internal/semroute/example_test.go` for a domain round-trip.

### S8 — references must resolve to living docs

A reference you can't follow is worse than none. **Proposals are
deleted once implemented** (see the root `CLAUDE.md`), so a package
comment citing `docs/proposals/<x>.md §N.N` becomes a dangling pointer
the moment the work ships. When a proposal lands:

1. Its durable content moves into a living narrative doc under
   `docs/architecture/` (or `docs/stories/`, etc.).
2. The package's `# Reference` section repoints there.
3. Bare `§N.N` proposal-section numbers are dropped or rephrased —
   the living doc renumbers, so the old numbers mean nothing.

Audit with: `grep -rn 'proposals/.*\.md\|proposal §' internal/<pkg>/`.

## Authoring / audit loop

1. `go doc -all ./internal/<pkg>` — read the package as a consumer sees it.
2. Walk the rubric S1–S8; fix gaps.
3. `go build ./internal/<pkg>/... && go vet ./internal/<pkg>/...`
4. `go test -run '^Example' ./internal/<pkg>/...` — examples must pass.
5. `grep -rn 'proposal §\|proposals/.*\.md' internal/<pkg>/` — must be empty.
