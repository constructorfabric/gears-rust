# Rating engine generations

Each directory here is one frozen generation of Rating's evaluation semantics (rating ADR-0006
"Retain Frozen Engine Generations", DESIGN §4.1), a crate of its
own (`cf-gears-bss-rating-engine-gen-<n>`) that `rating-core` re-exports as `engine::gen_<n>` and
dispatches to by `engine_generation`. A result records `(engine_generation, engine_digest)`; a
replay or correction runs the generation it recorded, never another one.

## What a generation holds

| Path | Content |
|---|---|
| `src/` | its own input and outcome types, the step order, the error taxonomy, the arithmetic and its budget |
| `vendor/` | renamed copies of the external code that runs inside evaluation, and `LOCK.toml` |
| `vendor.toml` | what is vendored: first-party sources copied byte for byte, the shared crates used in place (`bss_money`, recorded by the SHA-256 of its library sources), and the registry crates of the numeric closure |
| `golden/` | the golden corpus: `<case>.input.json` (run through `evaluate`, or the `entry` it names: `evaluate_order`, `minimum_fee_floor`) and the recorded `<case>.expected.json` |
| `build.rs`, `build/` | `engine_digest`: SHA-256 over all of the above and the manifest, compiled in |

`tests/` is not part of the frozen tree.

## Working on a generation

- **Re-vendor** after changing `vendor.toml` or before a release:
  `python3 tools/scripts/vendor_engine_generation.py vendor gears/bss/rating/rating-engine/gen-<n>`.
  The vendored crate's `Cargo.toml` (and the `derived` wrapper `src/lib.rs`) are written by hand.
- **Re-lock** after a change to `bss_money` that keeps every golden vector:
  `python3 tools/scripts/vendor_engine_generation.py lock gears/bss/rating/rating-engine/gen-<n>`
  records the new sources (and the registry pins) without copying anything, so no Products change
  comes with it. Generations use `bss_money` in place (money ADR rule 2: one crate, no Rating copy;
  DECISIONS T-D-84), so the re-lock changes their `engine_digest`.
- **Check** what CI checks: `make check-engine-generations` (vendored files and the library sources
  of `bss_money` match `LOCK.toml`, the workspace still resolves the pinned numeric closure to the
  same versions, checksums and features) and `make check-pure-crates` (the generation depends only
  on its allow list in `tools/scripts/pure_crates.toml`).
- **Digest freshness.** The build script reruns when Cargo sees a newer modification time under the
  frozen tree, so a copy that keeps old mtimes (`rsync -a`, `cp -p`, `tar`) onto an existing build
  can leave a stale `ENGINE_DIGEST`; `tests/digest.rs` then fails. Build a release from a clean
  checkout.
- **Bless** the golden corpus of a generation that is not yet released:
  `RATING_GOLDEN_BLESS=1 cargo test -p cf-gears-bss-rating-engine-gen-<n> --test golden`.
  `rating-core`'s `tests/replay.rs` replays the same corpus with a later generation current.

## Releasing and freezing

A generation is released by the migration that records its `engine_digest` in
`bss_rating__engine_generation` (feature 07). From then on its tree never changes: a change of
formulas, step order, arithmetic, budget, bounds or error classification is a new generation
directory, and a refactor must reproduce the golden corpus byte for byte.

Generation 1 is still under construction. Freezing it vendors the registry crates
`vendor/LOCK.toml` pins today (`rust_decimal`, `bigdecimal`, `num-*`, `arrayvec`, `libm`) as renamed
path crates, so that a workspace upgrade of any of them cannot reach a released generation. How a
released generation holds `bss_money`, which it uses in place, is settled before that freeze
(T-D-84).
