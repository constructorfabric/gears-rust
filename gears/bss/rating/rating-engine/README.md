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
| `vendor.toml` | what is vendored: first-party sources copied byte for byte, and the registry crates of the numeric closure |
| `golden/` | the golden corpus: `<case>.input.json` and the recorded `<case>.expected.json` |
| `build.rs`, `build/` | `engine_digest`: SHA-256 over all of the above and the manifest, compiled in |

`tests/` is not part of the frozen tree.

## Working on a generation

- **Re-vendor** after changing `vendor.toml` or before a release:
  `python3 tools/scripts/vendor_engine_generation.py vendor gears/bss/rating/rating-engine/gen-<n>`.
  The vendored crates' `Cargo.toml` (and the `derived` wrapper `src/lib.rs`) are written by hand.
- **Check** what CI checks: `make check-engine-generations` (vendored files match `LOCK.toml`, the
  workspace still resolves the pinned numeric closure to the same versions, checksums and features)
  and `make check-pure-crates` (the generation depends only on its allow list in
  `tools/scripts/pure_crates.toml`).
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

Generation 1 is still under construction: it is frozen once `bss_money` has R-35's exact-fraction
API (`gen-1/src/fraction.rs` bridges it until then) and the pricing semantics of feature 02 §5.2
are in. Freezing also vendors the registry crates `vendor/LOCK.toml` pins today (`rust_decimal`,
`bigdecimal`, `num-*`, `arrayvec`, `libm`) as renamed path crates, so that a workspace upgrade of any
of them cannot reach a released generation.
