# T1 — `gts-rust` 0.12.0 upgrade report

Task: [`todo.md`](./todo.md) T1 · Plan: [`plan.md`](./plan.md) · Spec: [`SPEC.md`](./SPEC.md) §7

This report discharges T1's third acceptance criterion — *"every difference in
`#[gts_type_schema]`-generated schema documents versus 0.11.0 is enumerated and accounted
for — none silent"* — and records what the re-validation sweep actually covered.

Source: `/Users/vasily/dev/gts-rust` at `f65bf62`, the commit SPEC §7 verified against.

## 1. What changed in the workspace

**`Cargo.toml`** — one `[patch.crates-io]` block redirecting `gts`, `gts-id` and
`gts-macros` to the local checkout. No version requirement is edited: the local crates
still declare `0.11.0`, so the three existing `gts* = "0.11.0"` requirements resolve
against them unchanged. The block carries a DO-NOT-MERGE marker and names O1 as its exit
condition.

`Cargo.lock` **is not committed.** The patch removes the registry `source` and `checksum`
of all three crates and adds `num-cmp` (a new transitive dependency of the local `gts`), so
a committed lockfile would encode this machine's absolute path.

**One source break.** `GtsIdSegment::ver_major() -> u32` is gone in 0.12.0, replaced by
`ver_major_opt() -> Option<u32>` — this is the *"distinguish v0 from wildcard versions"*
fix: `Some(0)` (an explicit `v0`) is no longer indistinguishable from `None` (a version the
segment never specified). One call site in the repository, `TR/src/api/rest/dto.rs`, fixed
as `ver_major_opt().unwrap_or(0)`.

That reproduces 0.11.0 *exactly* rather than approximating it: 0.11.0's accessor was
`self.parts().map_or(0, |p| p.ver_major)`, i.e. it already yielded `0` for the only segment
kind without parts (a UUID tail). Concrete — non-pattern — identifier segments always carry
a major version, so `None` is unreachable here for any other reason. The
`GtsIdSegmentDto.ver_major: u32` REST response field therefore keeps its shape and values.

Surfacing the v0/unspecified distinction is deliberately *not* done here. It belongs to the
major-0 quarantine slice (ADR-0015, T18), which is where the exactness this fix enables is
actually wanted.

**One test added.** `TR/tests/gts_012_semantics_tests.rs`, 5 tests pinning the capabilities
DESIGN §4 requires and 0.11.0 lacks — the tri-state verdict, per-level content-model
classification, the document-level comparison entry point, and the checker versions used as
admission provenance. Under 0.11.0 the file does not compile: none of
`CompatibilityVerdict`, `ContentModel`, `compare_documents`, `classify_object_levels`,
`GTS_SPECIFICATION_VERSION` or the `schema_evolution` module exists anywhere in published
`gts 0.11.0`.

## 2. The generated-schema diff, enumerated

Method: boot `cf-gears-example-server` (features from `config/e2e-features.txt`, config
`config/quickstart.yaml`) before and after the patch and dump
`GET /cf/types-registry/v1/entities`. That is the whole process-wide `toolkit-gts` inventory
as the registry actually admitted it — every `#[gts_type_schema]`-generated document and
every `gts_instance!` payload from the linked gears, after macro expansion. The
`#[gts_type_schema]` macro writes no files (`dir_path` only feeds a `GTS_SCHEMA_FILE_PATH`
constant), so there is no on-disk artifact to diff instead.

Both runs admitted **118 entities — 34 Type Schemas and 84 Instances**. Nothing was added,
nothing dropped, nothing refused.

**9 of 118 documents differ, all by one and the same structural change**, and every one of
the nine is a Type Schema with a field that is *both* typed as a referenced GTS identifier
(`GtsInstanceId` / `GtsTypeId`) *and* carries a Rust doc comment:

| Type Schema | Property |
|---|---|
| `gts.cf.toolkit.plugins.plugin.v1~` | `id` |
| `gts.cf.toolkit.authz.permission.v1~` | `id` |
| `gts.cf.core.am.tenant.v1~` | `id` |
| `gts.cf.core.am.tenant_type.v1~` | `id` |
| `gts.cf.core.am.tenant_metadata.v1~` | `id` |
| `gts.cf.core.am.user.v1~` | `id` |
| `gts.cf.core.am.conversion_request.v1~` | `id` |
| `gts.cf.core.rg.type.v1~` | `id` |
| `gts.cf.core.credstore.secret.v1~` | `gts_type` |

0.11.0 emitted the field's own keywords and the doc comment in one object, with the doc
comment **overwriting** the referenced type's `description`:

```json
"id": {
  "type": "string",
  "format": "gts-instance-id",
  "title": "GTS Instance ID",
  "description": "GTS instance identifier",      // the referenced type's description,
  "x-gts-ref": "gts.*"                           // clobbered by the field's doc comment
}
```

0.12.0 keeps the referenced type's subschema intact inside `allOf` and attaches the field's
doc comment as a sibling annotation:

```json
"id": {
  "allOf": [
    {
      "type": "string",
      "format": "gts-instance-id",
      "title": "GTS Instance ID",
      "description": "GTS instance identifier",
      "x-gts-ref": "gts.*"
    }
  ],
  "description": "Full GTS Instance Identifier for this plugin instance."
}
```

**Accounted for — the change is semantically inert, and here is why for each keyword that
moved:**

- `type` / `format` — `allOf: [X]` with no sibling assertions accepts exactly what `X`
  accepts, so the set of valid instances is unchanged. (`format` is annotation-only in the
  Draft-07 dialect these documents declare.)
- `title` / `description` — annotations, never assertions. What changed is that the
  referenced type's `description` survives instead of being overwritten, which is strictly
  more information, and the field's own doc comment is now readable separately.
- `x-gts-ref` — the one keyword with teeth, and it is still enforced: `XGtsRefValidator`
  recurses through `allOf` branches (`gts/src/x_gts_ref.rs`), so a reference nested one
  level deeper is validated identically.
- Content-model classification is unaffected: all nine properties are `string`-typed, and
  `classify_object_levels` classifies *object* levels only.

**Positional readers in this repository were checked, not assumed.** The only code that
reads `x-gts-ref` by position is `TR/src/infra/storage/debug_diagnostics.rs`
(`collect_schema_refs`), which is single-level and used solely to enrich a debug log on
validation failure — it never descended into `properties`, so the extra nesting changes
nothing for it. `account-management-sdk`'s pointer assertion on
`/x-gts-traits-schema/properties/allowed_parent_types/items/x-gts-ref` targets an `items`
subschema with no field doc comment and is unaffected; the full test run confirms it.

No other class of difference appeared. In particular the two upstream fixes SPEC §7 warned
would reach beyond Types Registry produced **no** observable change in this repository's
documents: *"fix(traits): Stop materializing const values"* — no `x-gts-traits` output
moved; *"fix(macros): Preserve explicit additional properties models"* — no
`additionalProperties` keyword changed in any of the 118 documents.

## 3. Re-validation sweep — what was covered

| Check | Result |
|---|---|
| Identifiers in `.md` / `.json` — `gts-validator` built from the local checkout, same paths and vendors as `make gts-docs` | **797 files scanned, 0 errors** |
| Same, with the installed `gts-validator` (`make gts-docs`) | **797 files, 0 errors** |
| Runtime admission of the whole linked inventory (server boot + entity dump) | **118/118 admitted**, before and after |
| Every `gts_id!` / `gts_uri!` / `#[gts_type_schema]` literal in the workspace | validated at macro-expansion time by `cargo nextest run --workspace`, which compiles every crate |
| `cf-gears-types-registry` suite | **208 passed, 0 failed** |
| New capability tests | **5 passed** |
| Workspace regression | see §4 |

**A note on the "202 declared identifiers" figure.** SPEC §7 and §16 both quantify the
sweep as *"all 202 declared GTS identifiers"*, but the derivation of 202 is not recorded
anywhere, and I could not reproduce it. The measurements this task did make, all
reproducible:

- **123 declaration sites** in the repository — 40 `type_id = gts_id!(…)` Type Schema
  declarations plus 83 `gts_instance!` / `gts_instance_raw!` Instance declarations.
- **118 entities** actually admitted at runtime under the e2e feature set (34 + 84).
- **137 distinct `$id`s** in `docs/**/*.schema.json`, covered by the validator sweep.
- 490 distinct fully-qualified `gts.*` strings across `.rs`/`.md`/`.json`/`.yaml`, and 507
  distinct macro-argument identifiers — but those count *references*, not declarations, so
  neither is the sweep population.

Whichever population 202 denotes, no identifier anywhere in the repository failed under
0.12.0: the validator passes every doc and JSON file, every macro literal compiles, and
every linked declaration admits.

## 4. Verification actually run, and the three gaps

Run and green:

- `cargo nextest run -p cf-gears-types-registry` — 208 passed.
- `cargo nextest run --workspace` — *see the summary appended below.*
- `make gts-docs` — 797 files, 0 errors. Plus the same sweep with the 0.12.0 validator.
- Server boots and admits the full inventory under the patch (behaviour verified at
  runtime, not only at compile time).

Not run, and why — these are environment limits on this machine, not passing results:

- **`make test-db`, `make test-users-info-pg`, `make test-usage-collector-pg`** — the Docker
  daemon is not running, and all three need testcontainers. None of them links `gts` in a
  way this upgrade touches (they exercise `toolkit-db` against real engines), so T1's risk
  surface is still covered; but they are part of `make ci` and were not executed. They
  become load-bearing at T2, which is the first task to write a migration.
- **`make dylint`** — `cargo-gears` is not installed, so the architecture lints could not
  run. Nothing in this task adds a layer boundary, a DTO or a REST route, so the DE-rule
  surface is a doc-comment edit and one test file.
- **Gears outside `config/e2e-features.txt`** are not in the runtime dump: `bss-rate-provider`
  and its plugins, `usage-collector` and its TimescaleDB plugin, `tr-authz`, the OoP
  calculator example, and `chat-engine` (which `make gts-docs` excludes by policy anyway).
  Their declarations are still re-validated at compile time by the workspace test run, and
  ~6 Type Schema declaration sites of the 40 are theirs — the difference between 40 declared
  and 34 dumped. What is *not* proven for them is runtime admission into a live registry.
  A cheap follow-up, if wanted: repeat the dump with those features enabled once the
  configs they need exist.

**A stale comment, left alone deliberately.** The three `gts*` version lines carry
`# Keep in sync with tools/dylint_lints/Cargo.toml`, but that path does not exist in this
repository — the lints come from the external `cargo-gears` tool. There is consequently no
second workspace whose `gts` dependency needs the same patch. Fixing the comment is
out of T1's scope; it is noted here so the next reader does not go looking for that file.

## 5. Blocking exit condition (O1)

The `[patch.crates-io]` block must not merge. Until `gts` / `gts-id` / `gts-macros` 0.12.0
are published:

- CI and every other machine cannot resolve the absolute path.
- `Cargo.lock` must stay uncommitted, so lockfile-dependent results on this branch are
  local-only and `make ci` is advisory rather than reproducible.

Publish, then delete the block — the version requirements above it already say `0.11.0`
and will need bumping to `0.12.0` at that point, which is the one place a version edit
belongs.
