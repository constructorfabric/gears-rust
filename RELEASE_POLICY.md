# CyberFabric Core Release Policy

This document defines the branching, versioning, and release rules for crates in the `cyberfabric-core` repository.

## Scope

This repository is a Rust workspace containing multiple crates that may be released independently.

The release unit is the **crate**.
Versioning, compatibility, and publishing decisions must be made per crate or per tightly coupled crate family.

This policy applies to:

- published Cargo crates (`publish` not set to `false` in `Cargo.toml`)
- internal crates when they affect published crates
- service crates that expose REST APIs
- crates that expose FFI interfaces, if any

### Publishable vs Internal Crates

- `publish = false` in `Cargo.toml` means the crate is internal and must not be published to crates.io.
- Internal crates can still follow SemVer for sanity, but do not promise external stability.
- Modules and SDKs keep explicit `version = "..."` in their `Cargo.toml`.
- ModKit libs may use `version.workspace = true` (unified framework versioning; workspace version is defined in the root `Cargo.toml`).

---

## Compatibility Rules

A version must be bumped when a crate changes its public compatibility surface.

The compatibility surface includes:

- public Rust API
- FFI ABI, if exposed
- REST or gRPC API contracts owned by the crate
- behavior relied on by downstream consumers when such behavior is part of the documented contract

### Breaking Change

A change is breaking if it can require downstream users to modify code, configuration, requests, responses, schemas, or integration behavior.

Examples:

- removing or renaming a public type, function, trait, field, endpoint, or schema field
- changing a public function signature (params, generics, bounds, return type)
- changing a response format in an incompatible way
- changing validation or behavior in a way that breaks documented expectations
- changing FFI layout or calling conventions

#### Rust-Specific Edge Cases

The following changes are also breaking and must be treated accordingly:

- adding a required trait bound to a public generic type or function
- adding a method to a public trait without a default implementation
- tightening generic constraints or visibility in a way that reduces what compiles
- changing Cargo feature defaults (consumers relying on default features may break)
- raising MSRV — even if the Rust API is unchanged, consumers pinned to an older toolchain may fail to build
- adding a variant to a public enum that is **not** marked `#[non_exhaustive]`
- changing struct/enum layout in a way that breaks construction or pattern matching

#### Contract Edge Cases

These changes are also breaking and require a version bump:

- changing error codes or error response body shapes that consumers match on
- changing configuration file format in a way that rejects previously valid configs
- removing or renaming OpenAPI-defined fields — OpenAPI schemas are normative, not illustrative
- changing JSON default values that consumers depend on
- removing or reordering enum variants in serde-exposed types (affects deserialization)

When in doubt, treat the change as breaking.

### Non-Breaking Change

A change is non-breaking if existing consumers continue to work without modification.

Examples:

- backward-compatible bug fixes
- backward-compatible security fixes
- additive REST fields where consumers can safely ignore them
- additive Rust API changes (new types, new trait impls, new optional fields)
- performance improvements
- internal refactors

Not breaking (SemVer-wise), but **must be documented** in changelog/release notes:

- performance regressions or significant behavior shifts
- changes in logging text, error message strings, or metrics naming
- stricter validation that rejects previously accepted edge cases (only allowed if it does not violate an explicit documented contract)

---

## Versioning Rules

Versioning follows Cargo SemVer conventions.

### Crates at 1.0.0 and Above

- breaking change -> bump **major**
- backward-compatible feature -> bump **minor**
- backward-compatible bug fix or security fix -> bump **patch**

Examples:

- `1.4.2` -> `2.0.0` for a breaking change
- `1.4.2` -> `1.5.0` for a new backward-compatible feature
- `1.4.2` -> `1.4.3` for a fix

### Crates Below 1.0.0

For pre-1.0 crates, the supported release version is `0.<minor>` (e.g. all `0.5.x` releases are one version series).

- breaking change -> bump **minor**
- backward-compatible feature or fix -> bump **patch**

Examples:

- `0.1.12` -> `0.2.0` for a breaking change
- `0.1.12` -> `0.1.13` for a bug fix
- `0.5.2` -> `0.6.0` for a breaking change
- `0.5.2` -> `0.5.3` for a compatible fix or feature

### Quick Reference

| Change                                 | Pre-1.0 bump       | Post-1.0 bump |
|----------------------------------------|---------------------|---------------|
| Bugfix only                            | PATCH               | PATCH         |
| Backward-compatible new API / feature  | PATCH               | MINOR         |
| Any breaking change                    | MINOR (`0.(x+1).0`) | MAJOR         |

### Pre-1.0 Stability Expectations

For pre-1.0 crates, patch bumps are intended to be backward compatible. However:

- Pre-1.0 crates carry **elevated churn risk**. Consumers should expect more frequent breaking changes than post-1.0 crates.
- High-usage crates with stable APIs should **graduate to 1.0** instead of hiding major compatibility signals behind repeated `0.x` bumps.
- Long-lived `0.x` versions rationalize instability. If a crate has been stable for multiple releases, promote it.

### Notes

- For Rust crates, compatibility is defined primarily by the **public Rust API**.
- If a crate exposes FFI, then FFI ABI compatibility must be evaluated separately.
- If in doubt, treat the change as breaking.

---

## Branching Policy

### Main Branch

`main` is the primary development branch.

Rules:

- `main` contains the latest development state
- `main` tracks the newest supported release versions
- new features land on `main`
- breaking changes land on `main`
- regular releases are cut from `main`

### Maintenance Branches

Maintenance branches are used for older supported release versions.

They are created only when an older version series still requires support for:

- security fixes
- critical bug fixes
- urgent production fixes

> **Current state:** All crates are pre-1.0. No maintenance branches exist yet. This section defines the convention for when they become necessary.

#### Branch Naming

For crates below `1.0.0`:

- `release/<crate-or-family>-v0.<minor>`

Examples:

- `release/cf-file-parser-v0.1`
- `release/cf-modkit-v0.5`

For crates at `1.0.0` and above:

- `release/<crate-or-family>-v<major>`

Examples:

- `release/cf-file-parser-v1`
- `release/cf-modkit-v2`

#### Why `v0.<minor>` for pre-1.0 crates

For pre-1.0 crates, `0.1.x` and `0.2.x` are different supported release versions.
A branch named only `v0` is too broad and does not clearly identify which version series is being maintained.

#### When to Create a Maintenance Branch

Create a maintenance branch when all of the following are true:

- `main` has already moved to a newer version series
- an older version series is still supported
- a fix must be released for that older series

#### What Can Go Into Maintenance Branches

Allowed:

- security fixes
- critical bug fixes
- minimal dependency updates required for the fix
- release metadata and changelog updates required for publication

Not allowed by default:

- new features
- refactors unrelated to the fix
- breaking changes
- broad dependency upgrades not required for the fix

Exceptions require maintainer approval.

---

## Crate Family Policy

Some crates may need to move together because they form one public product or version unit.

In such cases, maintainers may define a **crate family** and use one maintenance branch for that family.

Examples of when a family branch is appropriate:

- a service crate and its SDK crate must stay aligned
- a plugin crate and its host integration crate must stay aligned
- several crates expose one combined public contract

In those cases, branch names should use the family name, for example:

- `release/cf-modkit-v0.5`

### Operational Rules for Crate Families

- Family members must be **tested together** before any member is released.
- A family release can be **blocked** if one member is unreleasable (e.g. has a failing semver check).
- Internal dependency ranges within a family should use **exact versions** (e.g. `version = "=0.5.3"`) or tight ranges to prevent skew.
- Individual family members may publish alone only if the change does not affect other members' compatibility.
- If a family member publishes alone, CI must still pass for the entire family.

### ModKit Unified Release Rule

ModKit is released as a unified framework:

- Only **`cf-modkit`** produces changelog entries and GitHub releases.
- Other `cf-modkit-*` crates are published to crates.io but do **not** create separate changelog entries or GitHub releases.
- This is enforced in `release-plz.toml` via per-package `changelog_update` and `git_release_enable` settings.

---

## Release Automation (release-plz)

Releases are automated using [release-plz](https://release-plz.dev/).

Configuration lives in `release-plz.toml`. The workflow is defined in `.github/workflows/release-plz.yml`.

### How It Works

1. **Release PR creation** — On every push to `main`, release-plz analyzes changed crates, computes version bumps, updates `CHANGELOG.md`, and opens (or updates) a release PR labeled `release-plz`.
2. **Review** — Maintainers review the release PR. Version bumps and changelog entries can be adjusted manually before merge.
3. **Publish** — When the `release-plz`-labeled PR is merged into `main`, the release job runs:
   - workspace tests are re-run as a gate
   - changed crates are published to crates.io
   - Git tags are created per crate (format: `<crate-name>-v<version>`)
   - GitHub Releases are created for eligible crates (see below)
4. **Manual trigger** — The release workflow can also be triggered manually via `workflow_dispatch` (mode: `release` or `release-pr`).

### Configuration Model

`release-plz.toml` uses **permissive workspace defaults** (`changelog_update = true`, `git_release_enable = true`). Individual `[[package]]` entries override these defaults to suppress changelog entries or GitHub Releases for specific crates.

This means any publishable crate **not** listed in a `[[package]]` override inherits the workspace defaults and will automatically get changelog entries and a GitHub Release. When adding a new publishable crate, you **must** add a `[[package]]` entry in `release-plz.toml` and explicitly set `changelog_update` and `git_release_enable` to match the intended behavior.

> **Known gap:** Some publishable crates (plugins, `cf-modkit-http`, `types-sdk`) are not yet listed in `release-plz.toml` overrides and therefore inherit workspace defaults. This is tracked for cleanup.

### SemVer Checks

`semver_check = true` is enabled globally in `release-plz.toml`. release-plz runs [cargo-semver-checks](https://github.com/obi1kenobi/cargo-semver-checks) to detect accidental breaking changes in published crates.

Limitations: `cargo-semver-checks` only analyzes Rust library API surfaces. It does not cover REST/gRPC contracts, CLI interfaces, configuration formats, or behavioral changes. Reviewer judgment is still required for those.

If temporarily disabled for specific crates (bootstrap or tooling noise), do not use that as an excuse to sneak breaking changes into MINOR/PATCH.

### Conventional Commits

release-plz uses [conventional commits](https://www.conventionalcommits.org/) to generate changelog entries. The commit message format is:

```
<type>(<scope>): <description>
```

For breaking changes, use `feat!:`/`fix!:` or include a `BREAKING CHANGE:` footer. See `CONTRIBUTING.md` for the full list of accepted commit types.

Merge commits are filtered out of the changelog. Only meaningful commit messages appear in release notes.

> **Important:** Conventional commit messages are useful metadata for automation, but they are **not a substitute for reviewer responsibility**. People will forget `!`, mis-scope commits, or squash multiple changes together. Reviewers must independently verify whether a change is breaking, regardless of what the commit message says.

---

## Tagging Policy

Tags are created automatically by release-plz during the publish step.

Format:

- `<crate-name>-v<version>`

Examples:

- `cf-file-parser-v0.1.12`
- `cf-modkit-v0.5.3`
- `cf-nodes-registry-sdk-v0.1.11`

Tags point to the commit from which the crate was released.

---

## GitHub Release Policy

The workspace-level default in `release-plz.toml` is `git_release_enable = true`. This means any publishable crate creates a GitHub Release **unless** it has an explicit `[[package]]` override with `git_release_enable = false`.

Currently:

- **`cf-modkit`** is the primary product release — its GitHub Releases carry the unified ModKit changelog.
- Most other crates have explicit `[[package]]` overrides that suppress GitHub Releases.
- Some crates (certain plugins, `cf-modkit-http`, `types-sdk`) do **not** have overrides yet and therefore also produce GitHub Releases via the workspace default.

When adding a new publishable crate, you **must** add a `[[package]]` entry in `release-plz.toml` and explicitly set `git_release_enable` to prevent unintended releases.

---

## Changelog Policy

This repository maintains a **single `CHANGELOG.md`** at the repository root, configured via `changelog_path` in `release-plz.toml`.

release-plz updates it automatically in the release PR. The changelog uses [Keep a Changelog](https://keepachangelog.com/) format with entries generated from conventional commits.

Each release entry should document:

- what changed
- whether the change is breaking (marked with `[**breaking**]`)
- migration notes, if applicable

Contributors do not need to edit `CHANGELOG.md` manually — write clear conventional commit messages and the changelog will reflect them.

> **Tradeoff:** A single root changelog keeps things simple but mixes entries from unrelated crates. Consumers of a specific crate must filter mentally. If this becomes a pain point as the workspace grows, consider switching to per-crate changelogs or adding package-scoped sections to the root file.

---

## CI Pipeline

The following CI workflows run on PRs and releases. Which workflows are **required** for merge is determined by GitHub branch protection settings (not documented here).

| Workflow               | Trigger                        | Purpose                                               |
|------------------------|--------------------------------|-------------------------------------------------------|
| **CI**                 | PRs to `main`                  | Lint (clippy), tests (multi-OS), integration tests, coverage, security (cargo-deny), dylint |
| **E2E**                | PRs to `main`, nightly, manual | End-to-end tests against a running server             |
| **Release (release-plz)** | Push to `main`, release PR merge | Create release PRs, publish crates, create tags/releases |
| **API Contracts**      | PRs                            | Validate OpenAPI / contract changes                   |
| **CodeQL**             | Scheduled                      | Security analysis                                     |
| **ClusterFuzzLite**    | Scheduled                      | Continuous fuzzing                                    |

CI workflows skip runs for PRs that only touch `*.md` files or `docs/**` (path filters in `ci.yml` and `e2e.yml`).

---

## Dependency Update Policy

Internal dependency updates should not trigger version bumps unless they change the public compatibility surface or require republishing.

A crate must be republished when:

- its own code changed in a way that requires release
- dependency changes affect its public behavior or compatibility
- lockstep release is required for a crate family

Avoid unnecessary republishing of unrelated crates.

---

## Backport Policy

A backport is a change applied from `main` to an older supported maintenance branch.

Rules:

- backport the smallest safe change
- avoid mixing unrelated fixes
- revalidate tests on the maintenance branch
- preserve compatibility within that version series

Preferred process:

1. merge the fix into `main`
2. cherry-pick or re-implement the minimal fix onto the maintenance branch
3. bump the crate version in that branch
4. publish from that branch

In urgent cases, maintainers may fix the maintenance branch first, then forward-port to `main`.

> **Note:** Maintenance branches do not currently have release-plz automation. Releases from maintenance branches require manual `cargo publish` and tagging. This is a known gap — maintenance releases are exactly where auditability and repeatability matter most. If maintenance branches become common, automating their release process should be prioritized.

---

## Support Window

Only explicitly supported release versions receive maintenance releases.

Maintainers must define which versions are supported.

Recommended default:

- support the latest version series on `main`
- support older series only when there is a clear operational need
- retire maintenance branches when support ends

When support ends for a version series:

- no further fixes are guaranteed
- the branch may remain for history
- the branch should be marked unsupported in documentation

---

## Deprecation Policy

When feasible, prefer deprecation over immediate removal:

- Mark APIs as deprecated for at least one MINOR release before removal.
- Include migration notes (what to use instead).

Removal of deprecated APIs is breaking and requires a MAJOR (or MINOR for pre-1.0) bump.

---

## Examples

### Example 1: Fix on Latest Version

Current state:

- `main` contains `cf-file-parser 0.2.x`

A compatible bug fix is added.

Result:

- fix lands on `main` via PR
- release-plz opens a release PR bumping `0.2.3` → `0.2.4`
- maintainer merges the release PR; crate is published

### Example 2: Fix on Older Supported Pre-1.0 Version

Current state:

- `main` contains `cf-file-parser 0.2.x`
- users still depend on `cf-file-parser 0.1.x`

A security fix is needed for `0.1.x`.

Result:

- create or use `release/cf-file-parser-v0.1`
- apply minimal fix there
- manually bump version and publish `0.1.12`

### Example 3: Breaking Change on Pre-1.0 Crate

Current state:

- `cf-modkit 0.5.2`

A public API change breaks downstream code.

Result:

- change lands on `main` via PR with a `feat!:` or `BREAKING CHANGE:` commit
- release-plz opens a release PR bumping `0.5.2` → `0.6.0`

### Example 4: Breaking Change on Stable Crate

Current state:

- `cf-types-registry 1.4.2`

A REST response format changes incompatibly.

Result:

- change lands on `main`
- release-plz opens a release PR bumping `1.4.2` → `2.0.0`

---

## Maintainer Checklist

Before merging a release PR:

- confirm version bumps are correct (breaking vs non-breaking)
- review changelog entries for accuracy and clarity
- verify no unintended crates are included in the release
- verify `release-plz.toml` is up to date for any new crates

Before backport:

- confirm the old version series is still supported
- confirm the fix is necessary
- keep the patch minimal
- avoid unrelated changes

---

## Summary

- the release unit is the crate
- releases are automated via release-plz (release PR → merge → publish)
- `main` is for the latest development line
- older supported versions use maintenance branches (manual publish)
- pre-1.0 maintenance branches must include `0.<minor>`
- breaking changes start a new version series
- compatible fixes stay within the existing series
- tags are crate-specific (`<crate-name>-v<version>`)
- GitHub Releases are created for `cf-modkit` and any crate not overridden in `release-plz.toml`
- a single `CHANGELOG.md` at the repo root is updated by release-plz
- conventional commit messages drive changelog generation