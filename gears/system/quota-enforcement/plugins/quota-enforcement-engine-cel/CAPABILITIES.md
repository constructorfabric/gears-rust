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

## Evaluation cost

`evaluation_cost_limit` counts units: one per evaluated node and comprehension
iteration, plus one per 64 bytes copied or created. Binding the inputs charges
their size once. Reads copy, so a variable, field or index read charges the value
it returns; reads borrow along the chain, so `q.tier` charges the field, not `q`.
String literals, string concatenation and function results charge their size.
Values that are only moved charge their node alone: list and map literals, other
operators, ternaries, `cel.bind`, and comprehension results. `map` and `filter`
append to their result in place, so the result is never copied while it grows; an
expression that reads the generated accumulator `__result__` is evaluated step by
step instead, with the same result.

Bounds are checked on every produced value, whatever it is charged. A list may
hold 1 MiB in total, such as the quota set or a `filter` over it. A map or a string
may hold 64 KiB, and so may each bound quota. Every list or map holds at most 1024
items, and nesting stops at depth 48.

## Environment and decision record

The documents fix the input `request` (ADR-0013), the config shape `{ expr: <CEL string> }` (ADR-0005, PRD §5.9) and the
`Decision { result, debit_plan, diagnostics }` output. How the applicable-Quotas set
is bound is engine-internal (ADR-0005, "Concrete realisation"). This engine binds:

| Variable   | Type                                    |
|------------|-----------------------------------------|
| `request`  | the `metadata` schema of the metric type |
| `amount`   | `int`                                   |
| `metric_id`| `string`: the `id` of the request's metric object, `""` when it has none |
| `quotas`   | `list<{ id, subject_type, tier, cap, consumed, remaining }>` |

`cap` and `remaining` are nullable integers: integer operators accept them and
evaluation refuses an actual `null`. A field the profile cannot type — a `number`
(there is no float), a `$ref`, a `not`, a nullable scalar other than an integer, or
a member of a schema object without declared properties — stays reachable so `has()`
can test it, and every operator, comparison, call or record field that uses it is
refused at save time with a position. A `weight: number` is therefore visible but
unusable until the metric type declares it as `integer`.

A Quota carries no metadata: QE core has already left out the Quotas whose
conditions do not hold on the request (ADR-0013). `tier` is `"tenant"` for the tenant subject type and `"subject"` for every
other. `cap`/`remaining` are `null` for an unbounded Quota. No principal, attribution,
clock or randomness is bound.

## Bounds before the parser

`cel-core`'s parser is recursive descent without a depth limit; 500 nested
parentheses (a kilobyte) overflowed a 2 MiB stack. Before it runs, bracket nesting
is refused past 32 levels (string literals excluded), and the parser itself runs on
a 64 MiB stack so operator chains the bracket count cannot see (`!!!x`,
`a ? b : c ? d : ...`) fail as parse errors rather than aborting the process. The
source is bounded at 8 KiB and the expanded AST at 1024 nodes / 48 levels.

## Decision record, on every path

The record returned is exactly one of
`{ "debit_plan": [ [ { "id", "amount" }, ... ], ... ] }` (`Allowed`, one inner list per
chain, primary pool first; a plan with no entry is `Denied` with `NO_QUOTA_SELECTED`)
or `{ "deny": { "reason", "violated_quota_ids"? } }`. A flat list of entries is not a
plan. Amounts are decoded with checked conversions; a negative amount or a non-UUID id
is a type error before any plan is formed. The chain rules (each chain adds up to the
requested amount, no Quota twice) are checked by QE after evaluation.

At save time the record is checked on every path — each branch of a conditional
and the body of a `cel.bind` on its own — so one valid branch cannot vouch for
another. A literal record must carry exactly one of the two keys; `debit_plan` must be
a list of lists, and every entry needs exactly `id: string` and `amount: int` (a nullable
integer counts, and evaluation refuses the `null`); `deny` needs `reason: string`
and may carry `violated_quota_ids: list<string>`. A record built from computed
keys can only be decoded at evaluation and is decoded strictly there.

Each compiled policy reports whether it reads `request`.

A policy is type-checked against the metric type schema of **every** metric in its
persisted snapshot. Evaluation for a metric outside that set fails with
`InvalidConfig`; it never falls back to a looser environment or another engine. A
snapshot with no schema (a global policy, or one that reads no `request`) leaves
`request` untyped, and the policy evaluates for any metric. An untyped `request` is not
checked at save: a read of a property that a request does not carry fails the
evaluation, and the request is refused with `POLICY_EVALUATION_FAILED`. Such a policy
guards the read with `has()`; a wrong property name is the operator's error.
