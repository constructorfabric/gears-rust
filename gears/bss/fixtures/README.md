# BSS joint golden conformance fixtures

The shared, version-controlled corpus that pins the arithmetic on the seam between the
pricing catalog — which publishes structure and never computes a charge — and
Rating/Tariffs, which compute. Pricing's design set requires it to exist **before**
implementation: `PRD.md` §13, restated as assumption Q4 of
[`design/03-price-structure.md`](../pricing/docs/design/03-price-structure.md).

Design: `docs/superpowers/specs/2026-08-01-bss-joint-fixture-corpus-design.md`.

## Why numbers and not prose

Prose admits two readings and the readings differ by money. Over the bands
`[0, 1000) @ 5` and `[1000, ∞) @ 3`:

| `Q` | `graduated` (marginal) | `volume` Variant A (one rate on the whole `Q`) |
|---:|---:|---:|
| 999 | 4995 | 4995 |
| 1000 | 5000 | 3000 |
| 1500 | 6500 | 4500 |

At `Q = 1000` the volume bill *drops* by 2000, because the half-open interval puts 1000 in
the second band. Read that interval as closed and every boundary customer is billed wrong.
The corpus fixes where the cliff sits.

## Layout

- `corpus/` — the truth. Hand-authored TOML, reviewed as normative text, readable from any
  language. One directory per family.
- `corpus/registry.toml` — **generated, committed.** What pricing's `FixtureGate` reads at
  publish time.
- `bss-fixtures/` — the registry, and behind the `corpus` feature the loader and case
  model. No arithmetic. The only crate a gear may take as a **production** dependency, and
  it takes it with `default-features = false`.
- `bss-fixtures-conformance/` — the `CorpusEvaluator` trait, the runner, and the reference
  oracle. **dev-dependency only**, so no evaluator reaches a gear even transitively.

## What a gear actually compiles

The publish gate asks one question — *is this `(kind, variant)` pair green* — and the answer
is a small generated file. So a gear takes this crate narrow:

```toml
bss-fixtures = { workspace = true, default-features = false }
```

That surface is `ModelKind` + `Variant` + `Registry` + `gate_open_for`, over
`serde`/`toml`/`thiserror`.
The loader, the case model and `chrono` sit behind the `corpus` feature — a gear never reads
the twenty-odd case files at runtime and has no business carrying the ability to.

Every other build in the workspace turns `corpus` on, so the narrow build is easy to break
without noticing. `tests/production_surface.rs` compiles it on purpose:

```sh
cargo test -p cf-gears-bss-fixtures --no-default-features --test production_surface
```

## A registry row is `(kind, variant)`

S3 design §6 keys the registry by `model_kind` **/ `variant`**, and the gate rules are
written in variants: `inst-la-fixture` makes `level-aggregation` a registered variant whose
absence blocks any non-`sum` row "exactly like a `modelKind` variant"; §6 registers
`variant = supersession_continuity` on the tiered kinds and D-22 has it gate their first
publish; `inst-fx-kinds` gives the reservation variant its own fixture.

**The families are the variants** — `Family::variant()`, not a second field in
`_family.toml`, so the two cannot drift. Four families are one variant (`model_kind`), three
are their own, and two map to none: `proration` gates nothing deliberately, and
`trailing-tier` is Slice 10's `inst-tt-fixture` on a family that carries no case.

So a kind has one row per fixture it needs, and each is earned independently:

| the publishing row is … | needs, on top of everything above |
|---|---|
| anything | `model_kind` |
| non-`sum` | `level_aggregation` |
| a tiered **usage** kind | `supersession_continuity` (D-22) |
| reserved | `reserved` (Slice 10) |

Which variants a row requires is the **gear's** question and lives in
`bss_pricing::infra::fixture_gate::required_variants`; the refusal names the variant, because
that is the row an operator then looks up here. An unregistered pair is never open, which is
what refuses a `peak` `volume` row: no case folds a level on one.

Keyed by kind alone — as it was — three of those four said nothing at all.

## What "green" means

Three flags per registry row, none set by hand:

| Flag | Earned by |
|---|---|
| `oracle` | the reference oracle reproduces every evaluation case |
| `publish` | pricing's validator reproduces every publish case |
| `rating` | rating's evaluator reproduces the same evaluation cases |

`oracle` is per family and therefore per variant; `publish` is per **kind** and is written
onto every variant of it, because a publish outcome is attributed to `successor.model_kind`
and to nothing else (see [below](#the-role-does-not-scope-what-a-publish-case-earns--on-purpose)).

`FixtureGate` reads `oracle AND publish`. The oracle is the executable §17.2 and stands in
for Tariffs until Tariffs exists; requiring the `rating` flag would block every publish at
launch, since the rating gear has no code. When rating's evaluator lands it must **agree**
with the oracle — disagreement reddens the corpus rather than overriding either side.

### Who writes the file, and why it is a `--example`

The two halves are earned in **different places**, and neither can see the other. `oracle`
needs the reference oracle, which lives in `bss-fixtures-conformance`; `publish` needs
pricing's `PublishValidator`, which lives in the gear — and the harness is a
**dev-dependency** of the gear, because no evaluator may reach a gear even transitively.

So `registry_gen::build` takes the earned publish set as an **argument** and stays a pure
function of `(corpus, earned set)`, knowing nothing about who ran what. The one build in
which both halves are visible is an *example* target of the gear, which compiles with
dev-dependencies:

```
gears/bss/pricing/pricing/examples/regen_registry/   # writes registry.toml
gears/bss/pricing/pricing/tests/corpus_publish.rs    # asserts it is fresh
```

The freshness assertion lives beside the writer, and **only** there. Two crates each
asserting a different expected content is how a generated file starts flapping.

## Adding a case

1. Put the file in its family directory. The `family` field must match the directory; the
   loader rejects drift, because the directory is the index.
2. `[snapshot]` may carry **only** fields the design set marks frozen in
   `pricingSnapshotRef`. Anything the consumer supplies at evaluation time belongs in
   `[runtime]`. Both deny unknown fields, so the gear ownership boundary is checked when the
   corpus loads — D-60's per-subscription trailing lock cannot be written into `[snapshot]`
   at all.
3. State `charge_kind`. It is **required** — `recurring | usage | one_time | one_time_setup`,
   the scope key's seventh axis — and it has no default, because a default is an inference
   and the gear used to carry one ("a row that names a `meter` is a usage row"). Derive it
   from what the case *means*, then check it against the `inst-mk-chargekind` matrix: `flat`
   and `per_unit` are the non-usage kinds; `per_unit`, `graduated`, `volume` and `package`
   are the usage kinds. If the honest value makes the row unpublishable, that is a finding
   about the case — raise it, do not swap the value or add the field that hides it.
4. Cite the clause the case encodes in `provenance`. It is mandatory.
5. Say **why** the expected number is what it is, in `why`. A number without a reason cannot
   be reviewed, and a green run over unreviewed numbers proves nothing.
6. Author the **whole row**, not the delta — every snapshot must describe a row that would
   actually publish. See [the rule below](#every-snapshot-is-a-publishable-row); it is
   enforced, over every snapshot of every case.
7. Run `cargo test -p cf-gears-bss-fixtures -p cf-gears-bss-fixtures-conformance`, and
   `cargo test -p cf-gears-bss-pricing --test corpus_snapshot_shape --test corpus_publish` for the
   two halves only the gear can answer.
8. Regenerate the registry and commit that regeneration **on its own**:
   `cargo run -p cf-gears-bss-pricing --example regen_registry`

## Every snapshot is a publishable row

**Every `[snapshot]`, `[predecessor]` and `[successor]` in the corpus MUST describe a row
the catalog would publish clean.** Not the row the case is *about* — every row it names.

The corpus is the conformance contract: a case pins what a row costs, or what publish does
with it, by describing the row. So a snapshot the catalog would refuse is a **specification
of an impossible row**. It teaches two gears the arithmetic of something that can never be
published, in the one artifact whose entire purpose is to be the agreed reading — and
whichever gear later implements the refusal is then in disagreement with a fixture that is
green.

It also breaks cases quietly rather than loudly, in two different ways:

- A **publish** case authored short stops at the first row-shape rejection, because the
  validator runs shape before the pair. The guard the case exists to test is never reached
  and the case passes or fails on something else entirely. Five `supersession-continuity`
  pairs sat exactly there against `EVAL_POLICY_MISSING`, and
  `reserved/consumption-on-level-rejected` was authored to assert D-53's
  `LEVEL_RESERVATION_CONSUMPTION_FORBIDDEN` while carrying a row that would have been
  refused for `TIER_BANDS_GAP` first — a case that could never, even once its slice landed,
  have tested its own rule.
- An **evaluation** case cannot fail that way at all: no evaluator runs the publish rules,
  so nothing anywhere looked at the rows those cases describe. Four `level-aggregation`
  cases, `package/repeating-block-roundup` and `reserved/capacity-on-level` all pinned
  arithmetic over rows missing the `tierAggregationWindow` their kind requires.

In practice: every usage row carries `billingGranularity` (`inst-tb-window`); every tiered
(`inst-tb-window`) and `package` (`inst-pk-window`, D-58) usage row carries
`tierAggregationWindow`; every tiered row carries a band set starting at the origin with an
open top (`inst-tb-first`, `inst-tb-top`); every non-`sum` row carries `max_hold_granules`
and the D-77 granularity pairing. The full set is the gear's `price_row_rules()`, and
reading it is cheaper than guessing.

### How it is enforced, and where

`gears/bss/pricing/pricing/tests/corpus_snapshot_shape.rs` runs `price_row_rules()` over
every snapshot of every case and names the case, the side and the violation. It lives
**gear-side** because only the gear has the rules: `bss-fixtures` is the crate a gear takes
as a production dependency, so a rule set inside it would be the corpus grading the code it
is supposed to constrain.

The projection it uses deliberately skips the publish validator's unrepresentable-field
gate. `proration_basis` and the Slice-10 reservation pair belong to other slices and the
Slice-3 rules have nothing to say about them — but the Slice-3 *part* of a `proration` or
`reserved` snapshot is still a row whose shape must hold, and excusing those two families
would exempt eight of the corpus's twenty-seven cases from the rule.

### Adding a field to make a row publish never moves a number

The repair is always to the **row**, never to the assertion. If a field cannot be added
without changing what the case asserts, that is a finding about the case: report it, and
leave it red. `tierAggregationWindow` and `meter` are safe on every case here because the
oracle reads neither — but that is a fact to re-check, not a licence. Re-run the oracle
suite after every such edit and confirm every expected value still holds.

## Coverage today

`tier-boundary` (gates `graduated`, `volume`), `package`, `per-unit`, and `flat` are the
`model_kind` variant — together they gate **every** catalog `modelKind`, which is what
`inst-fx-gate` requires: it blocks publish of *any* kind without a green fixture. Then the
three cross-cutting variants: `supersession-continuity` (gates `graduated`, `volume`, per
D-22), `level-aggregation` (`graduated`) and `reserved` (`graduated`). Plus `proration`
(AC #61), which gates nothing.

Each cross-cutting family gates exactly the kinds its own cases exercise, and that is a
floor rather than a shorthand: nothing folds a level on a `volume` row, so a non-`sum`
`volume` row is refused by name until something does.

Gated is not the same as **answerable**, and the two are checked separately.
`check_kind_coverage` asks whether a kind is gated by a family that is its *own* fixture — a
continuity fixture gating `volume` must not stand in for `tier-boundary` doing so.
`check_publish_case_coverage` asks whether the gate can ever *open*, because `publish` is
earned per kind by a passing run and a kind the corpus asks no publish question of earns
nothing — for ever, and indistinguishably from a run that failed. `flat` and `per_unit` sat
in exactly that state.

It asks one thing more: at least one of a kind's answerable publish cases must expect
**`accepted`**. `volume`'s flag rested entirely on `kind-flip-rejected` and `package`'s on
`package-size-change-rejected`, both expecting a refusal — so `publish = true` meant "the
gear reproduces one refusal" and nothing said such a row could be published at all. A gear
that rejected every one of them earned the identical flag. The negative and the positive pin
different things: the refusal says where the guard bites, the acceptance says the guard has a
far side, and neither substitutes for the other.

All three checks run inside the registry generator, where absent coverage becomes a named
build failure rather than a `false` in a file.

**`trailing-tier` is deliberately unbuilt.** SEAMS M12 is open: rating has no counterpart
for `tierQualificationWindow` at all, so a fixture would pin one side of a contract the
other has not accepted — the opposite of what "joint" means. It reads as *declined*, never
as green, and a test asserts exactly that.

### Declining one case rather than a whole family

`trailing-tier` can be declined by not existing. A case cannot: omitting it would delete an
authored rule the design set already states. So a publish case may carry
`declined_until = "<slice>"`, which says the same thing at case granularity — nothing has
built the slice this case is authored against, so a subject's refusal is the **anticipated**
answer rather than a disagreement.

**No committed case carries it today, and the mechanism is the better for it.** The one
that did was `reserved/consumption-on-level-rejected`, marked `slice-10-advanced-primitives`
while it expected `LEVEL_RESERVATION_CONSUMPTION_FORBIDDEN` from a `PriceRow` that had
nowhere to put `reservedRate` / `reservationFlavor`. Slice 10 landed the pair and the rules
on **2026-08-08**, the marker was retired in the same commit, and the case is answered
rather than declined now — `corpus_publish.rs`'s `the_reserved_case_is_answered_rather_than_declined_since_slice_10`
and `the_corpus_now_declines_nothing_and_nothing_is_stale` are the two tests that hold it
there. A marker is **self-retiring**: leaving it in place once its slice ships makes it
stale by the runner's own definition (`stale_decline()` fires on `declined_until.is_some()
&& actual.is_ok()`), so the corpus would be asserting that a built slice is unbuilt.

The mechanism's coverage therefore lives on a **synthesised** case rather than a committed
one: `runner_tests.rs:224`, `corpus_with_one_declined_case()`, loads the real corpus through
the real loader, clones the reserved publish case, gives the clone its own id and sets
`declined_until` on it. That is deliberate. Asserting emptiness instead would have been
green and would have deleted the only coverage `anticipated_decline()` and `stale_decline()`
have.

That retirement stranded three readers who each named the marker in the present tense — two
assertions in `runner_tests.rs`, which went red and stayed red for nine days, and this
section, which went on describing a corpus state that had ended. Both are repaired; the
lesson is the shape, not the incident. **A marker is a claim with a date on it, and every
document that repeats the claim inherits the date.**

A decline suspends the evidence; it does **not** suspend the row. The reserved case once
carried neither tier bands nor `tierAggregationWindow`, so the day Slice 10 landed it would
have gone red on `TIER_BANDS_GAP` — a verdict about a malformed row, read as a D-53
disagreement, in the very run that was supposed to test D-53. Both its rows are now whole
and clean under every Slice-3 rule, so the reservation rule is the first thing its successor
can fail on. A declined case is a case waiting to be answered, and it has to be answerable
the moment its slice arrives.

It suspends **evidence**, never the assertion:

- the verdict stays authored and stays checked — a subject that answers with the wrong
  verdict is red exactly as before, so the marker cannot make a case unfalsifiable;
- the case earns nothing and blocks nothing: it is not counted as coverage, and it does not
  hold back the kinds its successor shares with cases that *are* answerable;
- answering it **correctly** makes the declaration stale, which the runner reports and the
  regeneration prints. "Nothing can answer this yet" has to stop being written down the
  moment it stops being true.

## Two case kinds

`kind = "evaluation"` asks what something costs, or what share of a period is chargeable.
`kind = "publish"` asks what publish does with an authored change: it carries a
`predecessor` and a `successor` row and expects a verdict.

The two are separate Rust types, and the loader reads `kind` before parsing the rest. A
serde-tagged enum would have had to give up `deny_unknown_fields`, and rejecting stray keys
is what keeps the snapshot/runtime boundary honest.

A rejection must name its error code. "Publish fails" without saying how is not reviewable,
and the codes are part of the published contract — the integrity check enforces it.

Publish cases are answered by a `PublishValidator`, which **only the pricing gear
implements**. The reference oracle deliberately does not: reproducing the gear's validation
surface would mean checking the gear against a copy of itself.

The validator runs the successor's **row-shape** rules first and the **supersession unit
guard** second, reporting the first violation in that order — a malformed row is malformed
regardless of what it supersedes, and the pair guard is a comparison that only means
something between two rows the catalog would have accepted on their own. It returns an
`EvalError` **only** when a snapshot carries something the gear's row shape cannot hold; a
rejection by the rules under test is a verdict, which is the case doing its job.

Where the two readings differ, `tests/corpus_publish.rs` pins the open disagreements by case
id with the reasoning for each. That list is meant to be loud: it fails the moment either
side moves, which is when a human is due to decide which one was wrong. It is currently
empty, and the reasoning for each entry that was closed is kept there — the corpus was under-
authored in five cases and right in the sixth, and both outcomes are recorded because "who
was wrong" is the part a future reader needs.

## Two family roles

Not every joint fixture is a publish gate, so `_family.toml` states which it is:

- **`role = "publish"`** — blocks publish of the `modelKind`s it lists, **in this family's
  variant**. The variant half is not authored here; it is read off the family. So
  `supersession-continuity` gates `graduated` and `volume` without claiming to be their
  `modelKind` fixture, which is what D-22 says and what keying by kind alone could not
  express.
- **`role = "conformance"`** — a joint regression that blocks nothing. `proration` is this:
  AC #61 is a field-consumption contract shared with Subscriptions and Tariffs.

Stating the role is what separates "gates nothing deliberately" from "someone forgot the
list" — a publish family with no kinds, a conformance family *with* kinds, and a publish
family that maps to no variant at all are all violations.

`supersession-continuity`, `level-aggregation` and `reserved` were recorded as `conformance`
while the registry was keyed by `modelKind` alone, and under that keying the record was
accurate: there was no row for them to occupy, so they gated nothing however loudly the
design set said they did.

### The role does not scope what a publish case earns — on purpose

`PublishReport::earned_kinds` attributes an outcome to `successor.model_kind` and to nothing
else: not to the case's family, not to that family's `GateRole`, not to its `gates` list, and
not to its variant. So a failing publish case blocks **every variant** of the kind its
successor carries, wherever the case sits. That is not a corner: today every publish case
lives in a cross-cutting family (`supersession-continuity`, `reserved`), and all four
`model_kind` families carry only evaluation cases — so every `publish` flag in the committed
registry is earned across the family boundary.

**Kept as is.** A failed case is a rule of the design set the gear does not reproduce, and
which directory it was filed in does not make it less so; the flag is a claim about a
`modelKind`, so it follows the row under test. The alternative — count an outcome only when a
`role = "publish"` family lists that kind and the case belongs to it — is more precise about
what a gate means and strictly less safe: on this corpus it would earn nothing for any kind,
and it would let a real `supersession-continuity` disagreement sit beside an open gate.
Fail-closed and slightly over-broad, deliberately.

## What proration asserts, and why it is not money

`proration` reports a **unit ratio**, never an amount. Rating emits prorated components at
full intermediate precision and never rounds — Billing rounds — so a prorated minor amount
does not exist at the pricing↔rating seam, and a fixture must not invent one.

The unit follows the basis: days for the calendar bases, seconds for `by_second`, the whole
period for `whole_unit` and `none`. Keeping it integral is what makes rating's T-D-26 rule —
"a period's slice fractions sum to exactly 1" — an exact equality: the split case asserts
15 + 16 = 31 = the basis, so no day is billed twice or lost at the cut.

The canonical `prorationBasis` enum is pinned in code here, spelling included. `serde`'s
`snake_case` rule would render `calendar_days_30` as `calendar_days30`; the enum is adopted
verbatim across three gears under the CI gate `pricing.contracts.enum_drift`, so the wire
name is part of the contract and is set explicitly.

## Enumerated fields are enums, not strings

`tierAggregationWindow` and `billingGranularity` are pinned in code for the same reason and
in the same way. While they were `Option<String>` the corpus carried
`tier_aggregation_window = "billing_period"` in four case files and
`billing_granularity = "per_unit"` in three — a plausible synonym of `invoice_period`, and
the name of a `modelKind` written into a granularity field. Neither value appears in any
design document, both read as obviously correct, and nothing failed: they were only ever
noticed by a consumer that could not map them, which reported *itself* as unable to answer.

An undefined value is now a **load** failure. The corpus carries its own vocabulary, so a
value added or renamed on one side cannot ride into a case file on the other.

`chargeKind` is the fourth, and it was worse than a wrong value: the corpus carried **no**
value at all. It is an axis of the canonical scope key and it is snapshot-frozen, so the
pricing gear had to *infer* it — "a row that names a `meter` is a usage row" — to answer the
publish cases. An inferred axis is an axis a second gear can infer differently, and it made
one of the four model-kind rules unstateable: while no case could say `chargeKind`, no case
could describe the `flat` usage row or the `graduated` recurring row that `inst-mk-chargekind`
exists to refuse. It is now a required field on every snapshot, and the inference is deleted
rather than left as a fallback.

Families and kinds are **different axes** — nine families against five kinds, with
`tier-boundary` gating two — so a kind can quietly belong to no family at all. That is how
`flat` was missed. `check_kind_coverage` now asserts every kind is gated by a family that is
its own `model_kind` fixture, and `check_publish_case_coverage` asserts every kind is asked
at least one answerable publish question and that at least one of them expects `accepted`;
both run inside the registry generator, so a sixth kind cannot be added without a family to
cover it, a publish case to open its gate, and a case that demonstrates it opening.

The unbuilt families are reported **declined**, never green. An empty family is not green
either: without that rule absent coverage would read as success, which is the exact failure
this corpus exists to prevent.

## The rule that matters

Expected values are never adjusted to make a run pass. If the oracle disagrees with the
corpus, both go red and a human decides which is wrong. A corpus that heals itself has
stopped being a specification and become a transcript of whatever the code happens to do.
