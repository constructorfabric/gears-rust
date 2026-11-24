<!-- @cf:root-agents -->
```toml
cf-studio-path = ".cf-studio"
```

ALWAYS resolve and enforce prerequisites of skills/workflows/commands BEFORE applying user intent.
<!-- /@cf:root-agents -->

# gears-rust

## Layout

- `libs/` — ToolKit and shared crates (`cf-gears-toolkit-*` and a few others).
- `gears/system/<name>/` — platform gears (api-gateway, authn/authz resolvers, types-registry, ...); `gears/<name>/` — service gears (some grouped, e.g. `gears/bss/<name>/`). Crates are mostly `cf-gears-<name>` and `cf-gears-<name>-sdk`; directories are kebab-case.
- `apps/` — servers (`cf-gears-example-server`); `examples/` — reference gears, `examples/toolkit/users-info` is the canonical one.
- `testing/e2e/` — Python E2E suites; `tools/` — scripts, xtask, fuzz.
- Directories with their own `AGENTS.md`: `libs/`, `gears/`, `docs/`, `testing/e2e/`, `tools/`.

## Commands

All through make; `make help` lists targets and parameters.

- `make fmt | clippy | test GEAR=<name>` — the gear and its SDK (`test` adds every crate that depends on them). Without `GEAR`: the whole workspace.
- `make build` — the example server (release); `make build GEAR=<name>` — that gear's crates.
- `make gear-ci GEAR=<name>` — what gear-scoped CI runs (fmt, clippy, test).
- `make dylint` — architecture lints (`cargo gears lint`).
- `make check` — fmt, clippy, tests, link check, security and architecture lints for the workspace. When adding or renaming crates also run `make check-release-config check-packaging-metadata validate-gear-names`; CI runs them, `make check` does not.
- Windows: `python tools/scripts/ci.py <target>` covers a subset of targets with no `GEAR` scoping (`tools/AGENTS.md`).

## Rules CI enforces

- Workspace lints in the root `Cargo.toml`: clippy `pedantic` is deny; no `unwrap`/`expect` outside tests; `unsafe_code` forbidden; `non_ascii_literal` deny, so no em-dashes or curly quotes in string literals. `clippy.toml` disallows raw SeaORM execution methods; use the secure query builders.
- Published crates must build on the MSRV (`rust-version` in the root `Cargo.toml`), which is older than the toolchain in `rust-toolchain.toml`.
- Unused dependencies are caught in CI only by the nightly cargo-shear run (`shear-nightly.yml`), after merge. Remove a dependency when you remove its last use, and run `make shear` (nightly toolchain, cargo-shear) before opening the PR.

## Conventions

- Commits are signed off (`git commit -s`, DCO) and use `<type>(<scope>): <imperative summary>`; types are in `CONTRIBUTING.md` §2.8.
- Unit tests go in sibling `*_tests.rs` files (`docs/toolkit_unified_system/12_unit_testing.md`).

## Where to read more

Load only what the task needs.

- Gear or ToolKit work: the routing table in `docs/toolkit_unified_system/README.md`.
- Adding or changing dependencies: `guidelines/DEPENDENCIES.md`.
- GTS types and IDs: `guidelines/GTS.md`.
- Reviewing a PRD or DESIGN: `guidelines/DESIGN_REVIEW.md`.
- Security controls, FIPS, credentials: `docs/security/SECURITY.md`.
- Releases and versioning: `docs/RELEASING.md`.
- Other guideline entry points: `guidelines/README.md`.

`guidelines/DNA/` (submodule) is a stack-agnostic baseline. Where it conflicts with the ToolKit docs, the ToolKit docs and `docs/arch/errors/DESIGN.md` win: routes go through `OperationBuilder`, not `#[utoipa::path]`; validation errors are `InvalidArgument` → 400, not 422 (only the `toolkit::api::rest::extract` rejections still use 413/415/422).

## Generated or shared files

- Do not edit: `Makefile` (driver), `CHANGELOG.md` (release-plz), `docs/toolkit-pr-review/RULES.md` (`tools/scripts/toolkit-pr-review/lint.py --write-index`), `docs/api/api.json` (`make openapi`), `guidelines/DNA/**` (submodule; changes go to its own repository).
- Repository-specific make targets go in `gears-rust.mk`. `.gears-dna/gears.mk` is the source other gear projects copy: keep it project-agnostic and raise `Version:` and `GEARS_MK_VERSION` together when you change it (`.gears-dna/docs/GEARS_MK.md`).
