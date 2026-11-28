# Contributing to CF/Gears

Thank you for your interest in contributing to Constructor Fabric Gears! This document provides guidelines and information for contributors.

We welcome contributions in:

- **New gears**: Add functionality to the platform
- **Bug fixes**: Fix issues in existing code
- **Documentation**: Improve guides and examples
- **Testing**: Add test coverage and improve test quality
- **Performance**: Optimize critical paths
- **Developer experience**: Improve tooling and workflows


## 1. Quick Start

### 1.1 Prerequisites

Follow the cross-platform [development setup guide](SETUP.md) before contributing. It covers macOS, Linux, and Windows setup, including Rust and Cargo through rustup, native build tools, `make setup`, and optional Constructor Studio installation.

- **Your favorite editor** (VS Code with rust-analyzer recommended)

### 1.2 Development Setup

```bash
# Clone the repository after completing SETUP.md.
git clone --recurse-submodules <repository-url>
cd gears-rust

# If you did not clone with --recurse-submodules
# (includes Constructor Studio for PR reviews)
git submodule update --init --recursive

# Build the whole-project example server release binary
make build

# Run workspace tests
make test

# Run the local quality gate
make check

# Start the development server (SQLite quickstart)
make quickstart

# Run the default example server
make run

# Build, test, or run one gear scope
make build GEAR=file-parser
make test GEAR=file-parser
make run GEAR=file-parser

# Start the development server with the example users_info gear
cargo run --bin cf-gears-example-server --features users-info-example -- --config config/quickstart.yaml run
```

## 2. Development Workflow

### 2.1. Create a Feature Branch or Fork

```bash
git checkout -b feature/your-feature-name
```

Use descriptive branch names:
- `feature/user-authentication`
- `fix/memory-leak-in-router`
- `docs/api-gateway-examples`
- `refactor/entity-to-contract-conversions`

As an alternative, you can fork the repository to your own GitHub account.

### 2.2. New Gears Development

Constructor Fabric Gears follows a spec-driven development (SDD) approach for large features. Gear development starts with specifications that live alongside the code. When you add features, make design decisions, or introduce upstream requirements, you must use the following templates and keep them aligned with the implementation:

- **[Overview & Guide](./studio-kit-gears/artifacts/README.md)** — Template system overview, governance, FDD ID conventions, and document placement rules
- **[PRD.md](./studio-kit-gears/artifacts/PRD/template.md)** — Product Requirements Document: vision, actors, capabilities, use cases, FR/NFR
- **[DESIGN.md](./studio-kit-gears/artifacts/DESIGN/template.md)** — Technical Design: architecture, principles, constraints, domain model, API contracts
- **[ADR.md](./studio-kit-gears/artifacts/ADR/template.md)** — Architecture Decision Record: decisions, options, trade-offs, consequences
- **[FEATURE.md](./studio-kit-gears/artifacts/FEATURE/template.md)** — Feature Specification: flows, algorithms, states, requirements
- **[UPSTREAM_REQS.md](./studio-kit-gears/artifacts/UPSTREAM_REQS/template.md)** — Upstream Requirements: technical requirements from other gears to this gear

#### Gear maturity

Every gear declares a maturity level in `gear.gdl`: `unpublished`, `preview`, `stable`, or `deprecated`. The level decides what consumers can rely on, how much specification the gear needs, which approvals a pull request needs (§2.10), and how breaking changes are versioned (§3). See [guidelines/MATURITY.md](./guidelines/MATURITY.md).

- A new gear starts as `unpublished`, with `publish = false` on every crate.
- A gear moves to `preview` (`0.1.0`) and then to `stable` (`1.0.0`) only through the acceptance process in [MATURITY.md §3](./guidelines/MATURITY.md#3-transitions). It never moves back.
- A new feature of a `stable` gear that is not proven yet ships as an `alpha` or `beta` feature behind a flag, never as a change to the stable surface. See [MATURITY.md §4](./guidelines/MATURITY.md#4-unstable-features-in-a-stable-gear).

### 2.3. Make Your Changes

Follow the coding standards and guidelines:

1. See common [RUST.md](./guidelines/DNA/languages/RUST.md) guideline
2. When develop new REST API use [API.md](./guidelines/DNA/REST/API.md), [STATUS_CODES](./guidelines/DNA/REST/STATUS_CODES.md)
3. When develop new Gear use [ToolKit Unified System](./docs/toolkit_unified_system/README.md)
4. Security policy [SECURITY.md](./SECURITY.md) and secure coding [guidelines/SECURITY.md](./guidelines/SECURITY.md)
5. ToolKit architecture and invariants [docs/toolkit_unified_system/README.md](./docs/toolkit_unified_system/README.md)

Gear directories under `gears/` must use kebab-case (validated by `tools/scripts/validate_gear_names.py` and enforced in CI).

#### Building, running, and testing one gear

The Makefile supports an optional `GEAR=<name>` scope for common top-level commands. Without `GEAR`, commands run at the whole-project or workspace level.

```bash
make build                    # whole-project example server release binary
make test                     # workspace tests
make run                      # default example server
make all                      # build + check + SQLite integration + local E2E + OpenAPI

make build GEAR=file-parser   # cf-gears-file-parser plus its SDK crate
make test GEAR=file-parser    # tests for cf-gears-file-parser plus its SDK crate
make run GEAR=file-parser     # example server with only this gear feature set
make e2e-local SUITE=file-parser
make coverage GEAR=file-parser
```

By default, `GEAR=<name>` maps to package names `cf-gears-<name>` and `cf-gears-<name>-sdk`, and `make run GEAR=<name>` enables the gear feature together with the static local development system gears. Override `GEAR_PKG`, `GEAR_SDK_PKG`, `GEAR_FEATURES`, `GEAR_BUILD_ARGS`, `GEAR_TEST_ARGS`, or `GEAR_RUN_ARGS` for non-standard package names, extra features, or runtime arguments.

Runtime configuration is YAML-driven: each gear reads its settings under `gears:<gear_name>:` with a `config` section and, when needed, a gear-owned `database` section. Cargo feature selection decides which gear code is compiled into the example server; YAML decides the runtime settings for the compiled-in gears.

#### Gear dependencies and `cargo-shear`

When a gear declares dependencies via `#[gear(deps = [...])]`, the dep entries are
**crate identifiers** (snake_case). The runtime gear name is derived by replacing
underscores with hyphens (`authn_resolver` → `"authn-resolver"`), matching the
convention that `lib-name == gear-name` across the workspace.

The macro generates hidden `pub use` re-exports that force the linker to keep each
dependency's `inventory::submit!` registration alive. Because these re-exports only
exist in macro-expanded code, `cargo-shear` cannot see them and will flag the
dependency crates as unused.

**When adding a new gear dependency**, add the crate name to the
`[workspace.metadata.cargo-shear] ignored` list in the workspace `Cargo.toml`.

`cargo-shear` does not run on pull requests — the scan takes about an hour, so it
runs once nightly (`.github/workflows/shear-nightly.yml`). Forgetting the `ignored`
entry therefore breaks `main` after your PR merges rather than failing your PR. The
nightly opens a tracking issue quoting the offending package and dependency, and
closes it once a later run passes.

Before merging a change that adds or removes dependencies, check it against
cargo-shear. The cheapest route needs no local setup and uses the same pinned
toolchain and cargo-shear version as CI: Actions → Unused Deps (nightly) → Run
workflow → select your branch.

To check offline instead, run `make shear`. It needs `cargo-shear` on your `PATH`
(`make setup` installs it, pinned to the version CI uses) plus the nightly toolchain
named by `RUST_NIGHTLY` in `gears.mk`, which rustup downloads on first use. Budget
about an hour for the scan.

Always include unit tests when introducing new code.

### 2.4. Run Code Quality Checks

The `Makefile` is a driver: it includes `gears-rust.mk` (the targets of this
repository) and `.gears-dna/gears.mk` (shared rules that other gear projects reuse). See
[the gears.mk guide](.gears-dna/docs/GEARS_MK.md) when adding or changing make targets.

Build and run all the quality checks:

```bash
# Run the complete quality check suite: formatting, linting, tests, and security
make check # Linux/Mac
python tools/scripts/ci.py check # Windows

# Run the full pipeline (includes build + e2e-local)
make all # Linux/Mac
python tools/scripts/ci.py all # Windows

# Scope common Makefile targets to one gear when iterating locally
make check GEAR=file-parser
make test GEAR=file-parser
make e2e-local SUITE=file-parser
```

Note: CI workflows may not run for PRs that only touch `*.md` files or `docs/**` due to path filters.

PRs run the unit tests on Ubuntu only. macOS and Windows run nightly
(`.github/workflows/macos-windows-tests.yml`), and a failure opens a tracking issue.

If your change touches platform-specific code (`cfg(windows)`, `cfg(target_os = "macos")`,
file paths, line endings, file locking), run those tests on your branch before merging:
Actions > macOS and Windows tests > Run workflow > pick your branch. If you can't run
workflows in this repo, run it in your fork.

Aim for high test coverage:
- **Unit tests**: Test individual functions and methods
- **Integration tests**: Test gear interactions
- **End-to-end tests**: Test complete request flows

```bash
# Run tests with coverage (automatically detects your OS)
make coverage # Run both unit and e2e tests with code coverage
make coverage-unit # Run only unit tests with code coverage
make coverage-e2e-local # Run only e2e tests with code coverage
```

### 2.5. Architecture Lints

Architecture lints enforce design boundaries at compile time (DTO placement, domain-layer isolation, contract-layer purity, versioned REST paths, etc.). The lint rules themselves live in the [`cargo-gears` CLI](https://github.com/constructorfabric/cargo-gears); this repository only configures which rules to run and their parameters.

#### Running lints locally

```bash
# Run architecture lints (requires cargo-gears installed via `make setup`)
make dylint

# Equivalent direct invocation
cargo gears lint --dylint
```

Architecture lints also run as part of `make safety` and in the CI pipeline.

#### Configuration files

| File | Purpose |
|------|---------|
| [`Gears.toml`](./Gears.toml) | Top-level manifest: enables/disables the dylint pass, lists rules to **skip** (pre-existing violations not yet fixed) |
| [`dylint.toml`](./dylint.toml) | Rule-specific parameters: allow-lists, thresholds, path exclusions |

#### Adopting a new lint rule in this repository

When a new architecture lint is added to `cargo-gears`, the following steps bring it into `gears-rust`:

1. **Update `cargo-gears`** — install the version that includes the new rule (`cargo install cargo-gears` or pin a specific version).
2. **Run `make dylint`** — see if the new rule produces violations in the current codebase.
3. **Fix or exclude** — either fix all violations, or (for large-scale migrations) add the rule to the `skip` list in `Gears.toml` and/or add path exclusions in `dylint.toml` while tracking the clean-up.
4. **Commit configuration changes** — include `Gears.toml` / `dylint.toml` updates in your PR alongside any code fixes.

> For instructions on **authoring** a new lint rule, see the [Adding a New Lint Rule](https://github.com/constructorfabric/cargo-gears/blob/main/crates/cargo-gears-lints/README.md#adding-a-new-lint-rule) guide in the `cargo-gears` repository.

#### Example: skipping a rule with pre-existing violations

In `Gears.toml`, add the rule ID to the `skip` list:

```toml
[apps.gears-rust.dev.lint.dylint]
enabled = true
skip = [
    "de0504_client_versioning",   # not yet addressed
    "de1101_tests_in_separate_files",  # migration in progress
]
```

In `dylint.toml`, use `excluded_paths` for per-rule path-level granularity:

```toml
[cargo-gears-lints]
excluded_paths = [
    "libs/toolkit",
    "gears/approval-service",
]
```

### 2.6. Run Fuzzing Tests (Recommended)

Before submitting changes to parsers or validation logic, run fuzzing:

```bash
# Quick smoke test (30s per target)
make fuzz

# Longer test for critical changes (5 minutes per target)
python tools/scripts/ci.py fuzz --seconds 300

# Target specific component
make fuzz-run FUZZ_TARGET=fuzz_odata_filter FUZZ_SECONDS=600
```

Fuzzing helps catch:
- Parser crashes
- Performance problems
- Edge cases

See `tools/fuzz/README.md` for detailed fuzzing documentation.

Helpful environment variables:

```bash
# Turn on debug-level logging
export RUST_LOG=debug

# Show backtraces on panic
export RUST_BACKTRACE=full
```


### 2.6.1. Git Hooks

The first `make` run installs git hooks from `.githooks/` as symlinks into your git hooks directory (it respects `core.hooksPath` and never overwrites an existing hook). They run the checks that most often fail in CI, scoped to what changed:

- `pre-commit` (staged files): rustfmt, `cfs validate`, offline lychee link check, CRLF.
- `pre-push` (changes vs a base ref): clippy on the changed workspace packages and the workspace packages that depend on them, architecture lints (`cargo gears lint --dylint`), forbidden words (needs bash >= 4 and jq: `brew install bash jq` on macOS).

The `pre-push` base is the first ref found of `$GEARS_HOOKS_BASE`, `git config gears.hooksBase`, `upstream/main`, `origin/main`, `main` (an explicit override that does not exist skips the checks). It is never fetched, so in a fork keep `upstream/main` up to date (it is preferred over the fork's `origin/main`) or set the override. Changed packages that are not workspace members (such as `tools/fuzz`) are skipped; without `jq` or when `cargo metadata` fails, clippy runs on the whole workspace instead (slower, but correct). dylint runs workspace-wide on any push touching Rust/Cargo files, plus the OAGW `-F cluster` run of `make dylint` when OAGW is affected. Measured on an Apple M2 Pro with 32 GB: with a cold cache (after a toolchain update or `cargo clean`) clippy on the whole workspace takes about 6 minutes, dylint about 4.5 and the OAGW pass about 1. With a warm cache, a push touching one gear or an SDK takes 20 seconds to 1.5 minutes of clippy (the upper end right after a build with a different package set, because features unify differently), a change to a crate most of the workspace depends on (such as the toolkit macros) about 3 minutes, and dylint a few seconds.

A check whose tool is not installed is skipped with a notice. Bypass with `git commit --no-verify` / `git push --no-verify`. Opt out with `GEARS_NO_HOOKS=1` and remove with `make uninstall-hooks`. The hooks run only in clones where `make` (or `make install-hooks`) ran once, which records `gears.hooks=true` in that clone's local git config; `make uninstall-hooks` disables them for that clone persistently (the auto-install respects it) and `make install-hooks` turns them back on. In other repos sharing the same hooks directory (global `core.hooksPath`) the hooks do nothing.

When editing `.githooks`, run `make shellcheck` (installed by `make setup`, pinned to the CI version) and `sh .githooks/tests/run.sh`; CI runs both (the tests on Linux, macOS and Windows Git Bash).

### 2.7. Sign Your Commits (DCO)

This project uses the Developer Certificate of Origin (DCO) version 1.1.
- The DCO text is included in `guidelines/DNA/DCO.txt` (Version 1.1). This is the current and widely adopted version; please keep it as 1.1.
- Every commit must include a Signed-off-by line to certify you have the right to submit the contribution under the project license (Apache-2.0).

Sign off your commits:
```bash
git commit -s -m "your message"
```
This adds a footer like:
```
Signed-off-by: Your Name <your.email@example.com>
```
Enable auto sign-off for all commits:
```bash
git config --global format.signoff true
```


### 2.8. Commit Changes

Follow a structured commit message format:

```text
<type>(<gear>): <description>
```

- `<type>`: change category (see table below)
- `<gear>` (optional): the area touched (e.g., api_gateway, toolkit, ecommerce)
- `<description>`: concise, imperative summary

Accepted commit types:

| Type       | Meaning                                                     |
|------------|-------------------------------------------------------------|
| feat       | A new feature                                               |
| fix        | A bug fix                                                   |
| tech       | A technical improvement                                     |
| cleanup    | Code cleanup                                                |
| refactor   | Code restructuring without functional changes               |
| test       | Adding or modifying tests                                   |
| docs       | Documentation updates                                       |
| style      | Code style changes (whitespace, formatting, etc.)           |
| chore      | Misc tasks (deps, tooling, scripts)                         |
| perf       | Performance improvements                                    |
| ci         | CI/CD configuration changes                                 |
| build      | Build system or dependency changes                          |
| revert     | Reverting a previous commit                                 |
| security   | Security fixes                                              |
| breaking   | Backward incompatible changes                               |

Examples:

```text
feat(auth): add OAuth2 support for login
fix(ui): resolve button alignment issue on mobile
tech(database): add error abstraction for database and API errors
refactor(database): optimize query execution
test(api): add unit tests for user authentication
docs(readme): update installation instructions
style(css): apply consistent spacing in stylesheet
```

Best practices:

- Keep the title concise (ideally ≤ 50 chars)
- Use imperative mood (e.g., "Fix bug", not "Fixed bug")
- Make commits atomic (one logical change per commit)
- Add details in the body when necessary (what/why, not how)
- For breaking changes, either use `feat!:`/`fix!:` or include a `BREAKING CHANGE:` footer. release-plz reads these markers to choose the version bump, so the `breaking` type alone is not enough for a published crate
- Changes to an `alpha` or `beta` feature of a `stable` gear mention the feature, for example `feat(file-storage): bulk-import (beta): add resume token`

New functionality development:

- Follow the repository structure in `README.md`
- Prefer soft-deletion for entities; provide hard-deletion with retention routines
- Include unit tests (and integration tests when relevant)

### 2.9. Push and Create PR

```bash
git push origin feature/your-feature-name
```

Then create a Pull Request on GitHub with:
- Clear title and description
- Reference to related issues
- Test coverage information
- The maturity level of each affected gear and, for a `stable` gear, the stage of each changed feature
- Contract and data impact: none, additive, or breaking, for each public contract (SDK, REST, gRPC, event, GTS, MCP, configuration) and for persisted data
- For a breaking change: the version bump and a link to the [migration note](#migration-notes)
- For a migration: proof that the previous release works on the new schema

Keep one gear per pull request when a change touches a public contract or persisted data, so the version bump and the review level are unambiguous.

Use the [PR template](./.github/pull_request_template.md) for the description.

### 2.10. Review Process

1. **Automated checks** must pass (CI/CD pipeline)
2. **Approvals** as required by the table below
3. **All conversations resolved** before merge
4. **Up-to-date with main** branch
5. **No open CRITICAL or HIGH finding** from the design or code review ([DESIGN_REVIEW.md §3](./guidelines/DESIGN_REVIEW.md#3-severity)). A MEDIUM finding is fixed or tracked in an issue linked from the maturity card

Required approvals depend on the maturity of the affected gear and on what the change touches. When several rows apply, the strictest one wins. A *contributor group* is one organization or team with maintainers in [MAINTAINERS.md](./MAINTAINERS.md); the gear owner is listed in [CODEOWNERS](./.github/CODEOWNERS).

| Change | `unpublished` | `preview` | `stable` |
|---|---|---|---|
| Internals, tests, docs, `alpha` features | 1 maintainer | 1 maintainer | 1 maintainer + gear owner |
| Public contract or persisted data (additive) | 1 maintainer | gear owner + 1 maintainer | gear owner + 1 maintainer from another group, after a design review |
| Breaking contract change, `beta` feature added or graduated | 1 maintainer | gear owner + 1 maintainer from another group | gear owner + 2 maintainers from other groups, after a design review |
| Security, tenant isolation, secrets | gear owner + 1 maintainer from another group | gear owner + 1 maintainer from another group | gear owner + 1 maintainer from another group, after a design review |
| Maturity transition | – | [MATURITY.md §3](./guidelines/MATURITY.md#3-transitions) | [MATURITY.md §3](./guidelines/MATURITY.md#3-transitions) |

ToolKit libraries (`libs/**`) follow the column of their crate version (`0.x` as `preview`, `>= 1.0` as `stable`) and always need a design review for public API changes.

Merge Strategy:

- **Squash and merge** for feature branches
- **Rebase and merge** for simple fixes
- **Merge commit** for release branches

### 2.11. Local PR Review with Constructor Studio

After pushing your PR and waiting for the cloud AI bots (CodeRabbit, Qodo, etc.) to complete their reviews, run a local Constructor Studio review to catch additional issues
before requesting human review:

```text
cf-gears-pr-review PR <number>
```

Use any supported IDE agent (Windsurf, Cursor, Claude, Copilot) — each redirects to the canonical workflows via `/cf-gears-pr-review` and `/cf-gears-pr-status` commands.

You can also check the PR status (unreplied comments, severity, etc.):

```text
cf-gears-pr-status PR <number>
```

See the results in `.prs/{ID}/` folder.

See [docs/pr-review/README.md](./docs/pr-review/README.md) for full setup (GitHub CLI authentication, configuration, available review prompts) and usage details.


## 3. Versioning

This topic defines how Constructor Fabric Gears versions crates and handles breaking changes.

## Scope

Applies to:
- All Rust crates in this repository (libraries, gears, SDKs, macros).
- Public APIs and contracts exposed to downstream users (Rust API, REST, gRPC/proto, CLI).

Non-goals:
- Internal-only refactors that do not change any public contract (still must be tested, but do not force version bumps).

## Maturity and Versions

The crate version follows the gear's maturity level ([MATURITY.md](./guidelines/MATURITY.md)):

| Maturity | Crate version | Breaking change |
|---|---|---|
| `unpublished` | any, `publish = false` | allowed in any PR without a version bump or migration note; the same PR updates every caller in this repository |
| `preview` | `0.x` | `0.(x+1).0`, `feat!` or `BREAKING CHANGE`, [migration note](#migration-notes) |
| `stable` | `>= 1.0` | next major, after the deprecation policy below, [migration note](#migration-notes) |

All published crates of a gear share the major version of its SDK crate.

## SemVer Rules (Crates)

We use Semantic Versioning: `MAJOR.MINOR.PATCH`.

### PATCH (x.y.Z)
Allowed:
- Bugfixes
- Performance improvements
- Internal refactors
- Doc and test updates
  Not allowed:
- Any public API or behavior change that can break downstream compilation

### MINOR (x.Y.z)
Allowed:
- Backward compatible new features
- New APIs that do not break existing code
  Not allowed:
- Breaking changes

### MAJOR (X.y.z)
Required for:
- Any breaking change (see the definition below)

## Pre-1.0 Policy (0.x)

For crates with version `0.x.y`:
- `0.(x+1).0` is treated as a breaking release.
- `0.x.(y+1)` is treated as non-breaking.

Rule of thumb: before 1.0, MINOR behaves like MAJOR.

## What Counts as a Breaking Change (Rust)

Breaking means: existing downstream code may fail to compile or a stable contract is violated.

Examples:
- Removing or renaming any `pub` item
- Changing function signatures (params, generics, bounds, return type)
- Changing public struct/enum layout in a way that breaks construction or pattern matching
- Removing a `pub` field
- Removing trait impls that downstream relies on
- Adding a method to a public trait without a default implementation
- Tightening trait bounds or visibility in a way that reduces what compiles

If in doubt: treat it as breaking.

## What Is Not Breaking (But Must Be Noted)

Not breaking (SemVer-wise), but must be documented in changelog/release notes:
- Performance changes
- Changes in logging text, error strings, metrics naming
- More strict validation returning errors in previously accepted edge cases (allowed only if it does not violate an explicit documented contract)

## Public Contract Types and Their Versioning

### Rust crate API
- Governed by SemVer rules above.

### REST API
- Versioned in the URL (`/v1/...`, `/v2/...`).
- Breaking REST changes require a new API version (for example `/v2`) and a deprecation period for old versions.
- Unstable (`alpha`/`beta`) operations and fields of a `stable` gear are served only under `/v{N}beta/...` ([MATURITY.md §4.3](./guidelines/MATURITY.md#43-how-each-contract-type-carries-an-unstable-feature)); `/v{N}` does not change for them.
- Deprecated operations return the `Deprecation` header (RFC 9745) and, once a removal date is set, the `Sunset` header (RFC 8594).

### Events, GTS types, MCP tools, configuration
- Additive changes only within a major version of the type, tool, or key.
- A breaking change is a new major version of the type or tool, served next to the old one.
- GTS major `v0` is used only by `unpublished` gears and `alpha` features; `preview` and `stable` gears and `beta` features use `v1` or higher ([MATURITY.md §4.6](./guidelines/MATURITY.md#46-gts-major-v0)).
- Unstable configuration lives under `features.<feature>` in the gear's `config` section.

### Persisted data
- Migrations are forward-only once a gear is `preview`. The previous release must work on the migrated schema (rolling deploy and rollback).
- `stable` gears use expand/contract: add the new shape, migrate readers and writers, and drop the old shape only in a later release.
- Migrations are never gated by a feature flag.

### gRPC / Protobuf
- Treat `.proto` as a public contract.
- Follow protobuf compatibility rules:
    - Do not reuse field numbers.
    - Do not change field types in incompatible ways.
    - Prefer adding optional fields over changing existing ones.
- Breaking proto changes require a MAJOR bump for affected crates and, if exposed externally, a new API/proto version strategy.

### CLI
- User-facing CLI flags, commands, and output formats are public.
- Breaking CLI changes require MAJOR.

## Deprecation Policy

When feasible, prefer deprecation over immediate removal:
- Mark APIs as deprecated for at least one MINOR release before removal.
- Include migration notes (what to use instead).

Removal of deprecated APIs is breaking and requires MAJOR.

Minimum deprecation periods by maturity:

| Contract | `preview` | `stable` |
|---|---|---|
| Rust SDK item | none; removal in `0.(x+1)` with a migration note | `#[deprecated]` in a `1.x` minor, kept for at least one minor release and 90 days, removed in the next major |
| REST, gRPC, event, GTS, MCP version | compatible with the previous release | the old version keeps working for at least 12 months after its replacement is `ga`; removal is a major release of the gear |
| `beta` feature of a `stable` gear | – | announced in one minor release, removed in a later one ([MATURITY.md §4.4](./guidelines/MATURITY.md#44-lifecycle-of-an-unstable-feature)) |
| `alpha` feature | – | none |

## Migration Notes

A migration note tells a consumer exactly what to change after a breaking change or a deprecation. It is required for:

- every breaking change of a `preview` or `stable` gear, a ToolKit library, or a `beta` feature;
- every deprecation of a `stable` item, contract version, or `beta` feature;
- every renamed GTS identifier (for example `v0` to `v1` at promotion).

`unpublished` gears and `alpha` features need no migration note.

**Where.** In the gear's `docs/MIGRATION.md` (for example `gears/file-storage/docs/MIGRATION.md`; for ToolKit, `libs/<crate>/MIGRATION.md`), in the same pull request as the change. The changelog shows only the commit title, so the note must live in this file. The commit links to it:

```text
feat(file-storage)!: take a request struct in list_files

BREAKING CHANGE: FileStorageApi::list_files takes ListFilesRequest.
See gears/file-storage/docs/MIGRATION.md#file-storage-sdk-14--20
```

**Rules.**

1. One section per release that contains breaking changes or deprecations, newest first. The heading names the crate and the version step: `## file-storage-sdk 1.4 → 2.0`. Use the next version from the bump rules above; several pull requests in one release add items to the same section.
2. One item per change, with these fields: **Affects** (Rust SDK users, REST clients, event consumers, plugin authors, operators), **Change** (before and after), **Action** (the steps, with a code or request example), and, when relevant, **Data** (what the migration does, whether rollback to the previous release works) and **Removal** (earliest release that removes a deprecated item).
3. Each item is complete without the pull request: the reader may not have access to it.
4. Never edit an item after its release, except to fix an error; add a new item instead.

Example:

````markdown
## file-storage-sdk 1.4 → 2.0

### `list_files` takes a request struct

- **Affects**: Rust SDK users.
- **Change**: `list_files(ctx, folder_id, limit)` is now `list_files(ctx, ListFilesRequest)`.
  The page size is optional and defaults to 50.
- **Action**:

  ```rust
  // before
  let page = api.list_files(&ctx, folder_id, 100).await?;
  // after
  let page = api
      .list_files(&ctx, ListFilesRequest::new(folder_id).with_limit(100))
      .await?;
  ```

### `GET /file-storage/v1/files?offset=` is deprecated

- **Affects**: REST clients.
- **Change**: offset pagination is replaced by cursor pagination (`$skiptoken`).
  Responses with `offset` carry the `Deprecation` header.
- **Action**: read `@odata.nextLink` from each page and request it for the next page.
- **Removal**: not before `/file-storage/v2` and at least 12 months after 2.0.
````

## Unstable Features

Items behind an `unstable-<feature>` Cargo feature, `/v{N}beta` REST paths, GTS types and MCP tools of `alpha`/`beta` features, and `features.<feature>` configuration keys are exempt from SemVer. They follow the stage rules of [MATURITY.md §4](./guidelines/MATURITY.md#4-unstable-features-in-a-stable-gear). Semver checks of a `stable` crate run without `unstable-*` features.

## Workspace Policy (Monorepo)

We use per-crate versioning, controlled via Cargo manifests and release automation.

### Publishable vs internal crates
- `publish = false` means the crate is internal and must not be published.
- Internal crates can still follow SemVer for sanity, but do not promise external stability.

### Version sources
- Gears and SDKs keep explicit `version = "..."` in their `Cargo.toml`.
- ToolKit libs may use `version.workspace = true` (unified framework versioning).

## ToolKit Unified Release Rule

ToolKit is released as a unified framework:
- Only `cf-gears-toolkit` produces changelog entries and GitHub releases.
- Other `cf-gears-toolkit-*` crates are published to crates.io but do not create separate changelog entries/releases.

## Release Process (Automation)

We use release-plz:
- Release PRs are labeled `release-plz`.
- Repository-level `CHANGELOG.md` is the single changelog source.
- GitHub releases are enabled where configured.

SemVer checks:
- We aim to run semver checks for published crates.
- If temporarily disabled (bootstrap or tooling noise), do not use that as an excuse to sneak breaking changes into MINOR/PATCH.

## How to Decide the Version Bump

Use this table:

| Change | Bump |
|------|------|
| Bugfix only | PATCH |
| Backward compatible new API/feature | MINOR |
| Any breaking change | MAJOR |
| Pre-1.0: breaking change | bump 0.(x+1).0 |
| Add, change, or remove an `alpha`/`beta` feature | MINOR |
| Graduate a feature to `ga` | MINOR |
| Remove a deprecated stable item or contract version | MAJOR |

## Required Release Notes

Every release must document:
- Added
- Changed
- Fixed
- Deprecated (with the replacement and the earliest removal release)
- Breaking (if any) with migration steps
- Unstable features: added, changed, graduated, or removed `alpha`/`beta` features

No "minor fixes" wording for releases that break users.

## Enforcement

Before merging changes that affect public crates/contracts:
- Tests must pass.
- If you touched a public surface, you must justify the version bump category in the PR description.
- The change follows the rules of the gear's maturity level; CI checks that `gear.gdl`, `publish`, and crate versions agree ([MATURITY.md §6](./guidelines/MATURITY.md#6-consistency-checks-ci)).

## Getting Help

- **GitHub Issues**: For bug reports and feature requests
- **GitHub Discussions**: For questions and general discussion
- **Documentation**: Check existing docs first
- **Code Examples**: Look at existing gears for patterns

---

Thank you for contributing to Constructor Fabric Gears!
