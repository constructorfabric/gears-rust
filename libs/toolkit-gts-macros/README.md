# cf-gears-toolkit-gts-macros

Proc-macros backing the [`cf-gears-toolkit-gts`](../toolkit-gts/README.md) crate. Not intended for direct use — depend on `cf-gears-toolkit-gts` instead; it re-exports everything below and carries the `inventory` collectors the macros target.

## What's here

- **`#[gts_type_schema(type_id = "...", …)]`** — attribute macro. Applies
  `#[gts_macros::struct_to_gts_schema(...)]` to the struct, emits an
  `InventorySchema` entry (the GTS Type Schema record) into the
  process-wide inventory, and — for derived unit structs
  (`base = ParentStruct`) — auto-emits `impl Default` so generic helpers
  can construct the marker without the caller re-spelling the type.
- **`gts_instance! { … }`** — function-like macro for **typed** Instance
  declarations. Takes a single struct literal `Struct { id: "<full>", … }`,
  optionally preceded by `#[gts_static(NAME)]`. Upstream rewrites the
  `id`-field string literal into a `GtsInstanceId` and asserts at compile
  time that its prefix equals `<Struct as GtsSchema>::TYPE_ID`. With
  `#[gts_static(NAME)]`, additionally emits `pub static NAME: LazyLock<T>`
  for typed runtime access.
- **`gts_instance_raw!({ … });`** — function-like macro for **raw-JSON**
  declarations. Takes a single brace-delimited JSON object literal whose
  top-level `"id"` key holds the full Instance Identifier. Use when no
  Rust struct corresponds to the instance.

Both macros resolve the `cf-gears-toolkit-gts` crate path at expansion time via
`proc_macro_crate`, so callers only need `cf-gears-toolkit-gts` as a dependency
— no separate dep on this crate.

## Full docs & examples

See **[`cf-gears-toolkit-gts` README](../toolkit-gts/README.md)**:

- [Adding a platform base Type Schema](../toolkit-gts/README.md#adding-a-platform-base-type-schema-inside-this-crate)
- [Declaring a well-known GTS instance](../toolkit-gts/README.md#declaring-a-well-known-gts-instance) — preferred `segment` form, `instance_id` fallback, generic base types
- [Boundary with `types-registry`](../toolkit-gts/README.md#boundary-with-types-registry)

Integration tests covering both macros live in
[`libs/toolkit-gts/tests/macro_integration.rs`](../toolkit-gts/tests/macro_integration.rs).

Crate-local collector macro tests and compile-fail cases live in
[`tests/local_collectors.rs`](tests/local_collectors.rs). Their
[`declaring-crate` fixture](tests/fixtures/declaring-crate) provides a second
compiled crate and uses a renamed `toolkit-gts` dependency. Merge behaviour is
tested in [`toolkit-gts`](../toolkit-gts/tests/local_collectors.rs).

Run the macro and merge tests with:

```sh
cargo test -p cf-gears-toolkit-gts-macros -p cf-gears-toolkit-gts
```

The release/fat-LTO check is opt-in because it builds the fixture example in a
separate target directory:

```sh
cargo test -p cf-gears-toolkit-gts-macros --test release_lto -- --ignored
```
