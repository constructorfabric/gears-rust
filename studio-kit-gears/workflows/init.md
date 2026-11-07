---
cf-studio: true
type: workflow
name: cf-gears-init
description: Invoke when setting up this kit for a project for the first time, or when {ci_commands}/{project_paths} look unconfigured for this project — missing, fully blanked, or still pointing at constructorfabric/gears-rust's own Makefile targets and paths in a project that doesn't have them. Discovers this project's real build/test commands and key paths and writes them into the kit's two facades. e.g. "set up the gears kit for this project", "configure ci-commands", "/cf-gears-init"
version: 1.0
purpose: Discover this project's actual build/lint/test commands and key reference paths, and populate {ci_commands}/{project_paths} so GearsCodeClose/GearsCodeChainSweep's gate commands and path-scoped checks actually run correctly here, instead of gears-rust's own shipped defaults.
---

# cf-gears-init - facade setup

Read-write setup workflow, run once per project (or re-run whenever this project's own
build system or layout changes). Discovers what this project's own build/test tooling and
key reference paths actually are, proposes a mapping for every entry in `{ci_commands}` and
`{project_paths}`, confirms it with the user, and writes only what was confirmed. Writes
nothing else — this workflow never touches artifact content, code, or any other kit file.

```pdsl
UNIT GearsInit
PURPOSE: Discover this project's real build/lint/test commands and key paths, and populate {ci_commands}/{project_paths} so the kit's facades actually match this project.
STATE:
  SET GEARS_INIT_PROJECT_KIND: gears-rust-shaped | other-rust | non-rust | unset (default unset, scope workflow_run)
  SET GEARS_INIT_PROPOSED_COMMANDS: object | unset (default unset, scope workflow_run)
  SET GEARS_INIT_PROPOSED_PATHS: object | unset (default unset, scope workflow_run)
DO:
  LOAD {ci_commands} and {project_paths} and read their current values
  RUN GearsInitDetectProjectKind
  CONTINUE GearsInitAlreadyMatches WHEN GEARS_INIT_PROJECT_KIND == gears-rust-shaped
  CONTINUE GearsInitDiscover WHEN GEARS_INIT_PROJECT_KIND != gears-rust-shaped
RULES:
  NEVER write to {ci_commands} or {project_paths} without the user confirming the full proposed mapping first
  ALWAYS treat gears-rust-specific operations (openapi, gts_docs, test_fips, test_graph_storage, test_onnx_embedding_plugin, test_onnx_embedding_compile_check, test_magika, e2e_local) and gears-rust-specific paths (reference_gear_path, example_server_path, fips_lib_paths) as "leave empty" by default outside a genuinely gears-rust-shaped project; never invent a fictitious equivalent just to fill a field
  ALWAYS keep this workflow scoped to {ci_commands} and {project_paths} only; never propose editing modules/code-stage-router.md, codebase/rules.md, design-led-rules.md, or docs/TESTING.md to work around a facade gap — a real gap in what the facades can express is a kit bug, not something this workflow should route around
```

```pdsl
UNIT GearsInitDetectProjectKind
PURPOSE: Decide whether this project's own build system already matches what the shipped facade defaults assume, or needs real discovery.
DO:
  RUN check whether this project has a `Makefile` at its root defining every target {ci_commands}'s current values already assume (`gear-ci`, `dylint`, `check`, `deny`, `shear`, `openapi`, `gts-docs`, `test-fips`, `test-graph-storage`, `e2e-local`)
  RUN check whether `{project_paths}`'s current `reference_gear_path`/`example_server_path` actually exist at those paths in this project, and whether `fips_lib_paths`'s globs match any real crate here
  SET GEARS_INIT_PROJECT_KIND = gears-rust-shaped WHEN every Makefile target above is defined here AND every configured {project_paths} entry that's non-empty actually resolves to something real in this project
  RUN, otherwise, probe for what this project actually has: a `Makefile`/`Justfile`/`justfile` and its own real target/recipe names, and whether a root `Cargo.toml` exists (single crate or workspace)
  SET GEARS_INIT_PROJECT_KIND = other-rust WHEN a `Cargo.toml` exists but the project isn't gears-rust-shaped per the checks above
  SET GEARS_INIT_PROJECT_KIND = non-rust WHEN no `Cargo.toml` exists at the project root
RULES:
  ALWAYS prefer an existing Makefile/Justfile's own real target names over inventing new commands — assume this project usually already has SOME build tooling, not none
  NEVER declare GEARS_INIT_PROJECT_KIND = gears-rust-shaped from the Makefile check alone; the path facade must independently check out too, since a project can share Makefile target names without sharing this kit's path conventions
```

```pdsl
UNIT GearsInitAlreadyMatches
PURPOSE: Confirm the shipped defaults already work here and stop without writing anything.
DO:
  EMIT "This project's own Makefile and layout already match every entry {ci_commands}/{project_paths} currently assume — both facades already work as shipped. Nothing to change. Re-run this workflow only if this project's own Makefile targets or key paths are renamed or moved later."
  LOAD {cf-studio-path}/.core/skills/studio/modules/ui/next-actions.md
  RUN NextActionsOffer
```

```pdsl
UNIT GearsInitDiscover
PURPOSE: Propose a grounded mapping for every {ci_commands}/{project_paths} entry, confirm it, then write it.
DO:
  SET GEARS_INIT_PROPOSED_COMMANDS = for each of {ci_commands}'s 13 operations (gear_ci, dylint, full_check, deny, shear, openapi, gts_docs, test_fips, test_graph_storage, test_onnx_embedding_plugin, test_onnx_embedding_compile_check, test_magika, e2e_local):
    a generically-meaningful operation (gear_ci, dylint, full_check, deny, shear) -> this project's own closest real equivalent, found from its actual Makefile/Justfile/Cargo tooling observed in GearsInitDetectProjectKind (e.g. a plain `cargo build && cargo clippy --all-targets -- -D warnings && cargo test` for gear_ci when no Makefile target exists; `cargo deny check` for deny only when a `deny.toml` is actually present here; "" when nothing comparable exists)
    a gears-rust-specific operation (openapi, gts_docs, test_fips, test_graph_storage, test_onnx_embedding_plugin, test_onnx_embedding_compile_check, test_magika, e2e_local) -> "" (no equivalent) unless this project genuinely has the matching concept (an OpenAPI-generating surface, a GTS-like schema system, a FIPS build lane, a graph-storage-shaped component, ONNX/Magika feature flags, an e2e suite directory) — never invent a fictitious equivalent just to fill the field
  SET GEARS_INIT_PROPOSED_PATHS = for each of {project_paths}'s 3 entries (reference_gear_path, example_server_path, fips_lib_paths):
    this project's own real equivalent when one genuinely exists (a canonical example module/service, a dev/example server binary, library crates with a real FIPS or equivalent security-build concept) -> that real path/list
    "" (or `[]` for fips_lib_paths) when nothing comparable exists here, rather than guessing
  EMIT the full proposed mapping for both facades as two tables (operation/entry | current value | proposed value | reasoning) for the user to review
  EMIT_MENU GearsInitConfirmMenu
RULES:
  ALWAYS ground every non-empty proposal in something actually observed in this project (a real file, a real Makefile/Justfile target, a real Cargo feature, a real directory) — never propose a command or path that was merely guessed
  ALWAYS propose empty for an entry with no observed real equivalent, and say why in the reasoning column, rather than silently omitting it from the table

MENU GearsInitConfirmMenu
TITLE: Review the proposed {ci_commands}/{project_paths} mapping above. Write it, edit specific entries first, or cancel?
OPTIONS:
  1 write as proposed -> CONTINUE GearsInitWrite
  2 edit some entries -> RUN interactively revise the flagged entries per the user's corrections, then EMIT_MENU GearsInitConfirmMenu again with the updated tables
  3 cancel -> EMIT "No changes written." and STOP_TURN
  INVALID -> EMIT_MENU GearsInitConfirmMenu
```

```pdsl
UNIT GearsInitWrite
PURPOSE: Write the confirmed mapping and spot-check it before declaring success.
DO:
  WRITE the confirmed GEARS_INIT_PROPOSED_COMMANDS values into {ci_commands} and GEARS_INIT_PROPOSED_PATHS into {project_paths}, preserving each file's existing comments/structure, only replacing the value side of each key
  RUN for every entry now mapped to a non-empty command, a cheap existence check (`command -v <first token>`, or `make -n <target>` when the command is a `make` invocation) and report which ones didn't resolve, without reverting the write — flag them instead as a named follow-up
  RUN for every non-empty path entry, confirm it still resolves on disk (a file or directory actually exists there) and flag any that don't
  EMIT a summary: what changed in each facade, what was left empty and why, and which spot-checks passed/failed
  LOAD {cf-studio-path}/.core/skills/studio/modules/ui/next-actions.md
  RUN NextActionsOffer
RULES:
  NEVER silently drop an existence-check failure; report it as a named follow-up item, since a wrong command or path is worse than an honestly-empty one
```

---

## Validation Criteria

- [ ] `{ci_commands}`/`{project_paths}` read before any proposal is made
- [ ] Every proposed non-empty entry is grounded in something actually observed in this project, never guessed
- [ ] Every gears-rust-specific operation/path defaults to empty unless this project genuinely has the matching concept
- [ ] Nothing was written without the user confirming the full proposed mapping first
- [ ] Every non-empty command/path was spot-checked to actually resolve, and any that didn't are flagged, not silently accepted
- [ ] No kit prompt file (`modules/code-stage-router.md`, `codebase/rules.md`, `design-led-rules.md`, `docs/TESTING.md`) was edited by this workflow

---

## Next Steps

### Options
- [ ] Run `cf-gears-coding` or `cf-gears-implement` on a small slice to exercise the newly-configured gates for real
- [ ] Re-run this workflow later if this project's own build system or layout changes
