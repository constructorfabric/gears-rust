# CEL dependency and execution decision

`cel-core` 0.5.1 supplies the Rust CEL parser and macro expansion with source spans.
Its `Program::eval` and the inspected `cel` 0.14.5 interpreter do not expose an
internal deadline/cost hook. QE must not call either unmetered interpreter on
operator expressions while holding database locks. A Tokio timeout is insufficient.

The QE adapter therefore uses a scoped AST evaluator with checks before every
node and comprehension iteration, checked integer arithmetic, bounded values,
and a fixed whitelist of bounded built-ins. Save-time structural schema checking
rejects unsupported nodes/functions and unavailable properties. No dynamic function
registration, clock, randomness, regex, networking, or filesystem capability is
exposed. The parser is separately bounded by source length and lexical nesting;
expanded AST depth and node count are checked before validation or evaluation.

This is a deliberately restricted CEL execution profile, not a claim that
`cel-core` provides a cost-limit API. Capability and adversarial tests must pass
before wiring this engine into the registry. Retained policies keep source and
schemas, never evaluator binaries.

## Environment and decision record

The documents fix the typed input triple `{request, resource, arbitration}`
(ADR-0007), the config shape `{ expr: <CEL string> }` (ADR-0005, PRD §5.9) and the
`Decision { result, debit_plan, diagnostics }` output. How the applicable-Quotas set
is bound is engine-internal (ADR-0005, "Concrete realisation"). This engine binds:

| Variable   | Type                                    |
|------------|-----------------------------------------|
| `request`  | the metric's request metadata schema    |
| `resource` | the resource projection schema, or null |
| `amount`   | `int`                                   |
| `quotas`   | `list<{ id, tier, cap, consumed, remaining, arbitration }>` |

`cap` and `remaining` are nullable integers: integer operators accept them and
evaluation refuses an actual `null`. A field the profile cannot type — a `number`
(there is no float), a `$ref`, a `not`, a nullable scalar other than an integer, or
a member of a schema object without declared properties — stays reachable so `has()`
can test it, and every operator, comparison, call or record field that uses it is
refused at save time with a position. The ADR-0007 example's `weight: number` is
therefore visible but unusable until the contract declares it as `integer`.

`quotas[i].arbitration` carries the constraint-contract metadata schema, so the
ADR-0007 pair checks apply to `request.region in q.arbitration.regions` inside a
comprehension. `cap`/`remaining` are `null` for an unbounded Quota. No principal,
attribution, clock or randomness is bound.

## Bounds before the parser

`cel-core`'s parser is recursive descent without a depth limit; 500 nested
parentheses (a kilobyte) overflowed a 2 MiB stack. Before it runs, bracket nesting
is refused past 32 levels (string literals excluded), and the parser itself runs on
a 64 MiB stack so operator chains the bracket count cannot see (`!!!x`,
`a ? b : c ? d : ...`) fail as parse errors rather than aborting the process. The
source is bounded at 8 KiB and the expanded AST at 1024 nodes / 48 levels.

## Decision record, on every path

The record returned is exactly one of `{ "debit_plan": [ { "id", "amount" } ] }`
(`Allowed`; an empty list is `Denied` with `NO_QUOTA_SELECTED`) or
`{ "deny": { "reason", "violated_quota_ids"? } }`. Amounts are decoded with checked
conversions; a negative amount, a non-UUID id or a duplicate id is a type error
before any plan is formed.

At save time the record is checked on every path — each branch of a conditional
and the body of a `cel.bind` on its own — so one valid branch cannot vouch for
another. A literal record must carry exactly one of the two keys; every
`debit_plan` element needs exactly `id: string` and `amount: int` (a nullable
integer counts, and evaluation refuses the `null`); `deny` needs `reason: string`
and may carry `violated_quota_ids: list<string>`. A record built from computed
keys can only be decoded at evaluation and is decoded strictly there.

Each compiled policy reports which inputs it reads (`request`, `resource`,
`arbitration`). Only those contracts are persisted with the version, and a later
activation is judged against those alone, so adding a resource projection cannot
strand a policy that never reads `resource`.

A policy is type-checked against the environment of **every** metric in its
persisted snapshot. Evaluation for a metric outside that set fails with
`InvalidConfig`; it never falls back to a looser environment or another engine.
