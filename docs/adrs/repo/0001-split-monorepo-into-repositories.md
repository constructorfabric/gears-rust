# ADR-0001: Split the gears-rust monorepo into several repositories

- **Status:** Proposed
- **Date:** 2026-10-22
- **Author:** @modcrafter77
- **Language:** [ASD-STE100 Issue 9](https://www.asd-ste100.org/about_STE.html)

## Summary

The `gears-rust` repository contains all libraries, all system gears and all application gears in one Cargo workspace. We propose to keep the platform core in `gears-rust`. We propose to move groups of independent gears to new repositories: `gears-bss`, `gears-genai` and `gears-serverless`.

The new repositories use the core crates from crates.io. The CI workflows stay in `gears-rust` as reusable workflows. The new repositories call these workflows. They do not copy them.

## Problem

The workspace has approximately 150 crates. All gears share one CI pipeline, one E2E server, one OpenAPI specification and one release process. This causes these problems:

1. A pull request that changes more than one gear starts the full CI for all gears.
2. All gears share one release process and one `CHANGELOG.md`.
3. The owners of application gears must follow the review and merge process of the core.
4. The example server links all gears. A build failure in one gear stops the E2E tests for all gears.
5. The core team must review CI, configuration and test changes for gears that it does not own.

## Current state

We analyzed the dependency graph at commit `ec7619e18`. These are the important facts:

- No crate in `libs/` depends on a gear crate. This is true for normal, dev and build dependencies.
- Almost all core crates are on crates.io at the current version.
- The gear manifest (`gear.toml`, later `*.gdl`) has a `category` field. The categories agree almost fully with the proposed repositories.
- `system/oagw` and `system/account-management/plugins/keycloak-idp-plugin` depend on `credstore`. `credstore` depends on system gears. Thus `credstore` must stay in the core.
- No gear outside `gears/bss/` depends on a BSS gear.
- Application gears depend on the implementation crates of system gears, not only on the SDK crates. The macro `#[toolkit::gear(deps = [...])]` causes this. For example, a gear that declares `types-registry` needs `types-registry` and `types-registry-sdk`.
- These items use fixed paths in one repository:
  - the example server;
  - the E2E harness;
  - the OpenAPI specification;
  - the Helm charts;
  - the release tooling.

  We must change them before the split.

## Proposal

### Target repositories

| Repository | Contents | Notes |
|---|---|---|
| `gears-rust` (core) | `libs/**`, `gears/system/**`, `gears/credstore`, `examples/`, `apps/cf-gears-flight-control`, a core-only `apps/cf-gears-example-server`, `docs/`, `guidelines/`, `deploy/helm/toolkit-common`, `deploy/helm/toolkit-platform`, `tools/`, `.gears-dna/`, `.cf-studio/`, `studio-kit-gears/` | Gears with category `core-functionality`, `core-platform-integration` and `api-ingress`. `credstore` stays here because of the dependency cycle above. |
| `gears-bss` | All of `gears/bss/**`, which includes `gears/bss/libs` (money, coord, approval, rest) | This is the most independent group. It depends only on the core. The `pricing` ↔ `products` cycle stays inside this repository. |
| `gears-genai` | `mini-chat`, `chat-engine`, `llm-gateway`, `model-registry`, `construct`, `graph-storage` (with the ONNX and remote embedding plugins), `graph-analytics` (docs only) | Gears with category `gen-ai`. The dependency `construct` → `settings-service-sdk` goes through crates.io. |
| `gears-serverless` | `serverless-runtime` (docs only), `simple-resource-registry` (docs only). To be decided: `settings-service`, `simple-user-settings` | Gears with category `serverless`. See [Open questions](#open-questions). |

We do not propose a `gears-oss` repository now. Only two gears have category `oss`:
- `infrastructure-resource-manager` has only documents;
- `account-management` must stay in the core, because system gears and BSS depend on it.

We can add `gears-oss` later, when it has code.

`file-parser`, `file-storage` and `github-mirror` depend only on the core. We can put them in any repository. See [Open questions](#open-questions).

The manifest of `gears/system/cluster` has category `serverless`. This value is not correct:
- `cluster` supplies the cluster primitives for all gears;
- `event-broker` and `quota-enforcement` depend on it.

We will change its category to `core-functionality`. `cluster` stays in the core.

### Repository layout

Each new repository keeps the layout `gears/<name>/...`. This gives these advantages:

- `.gears-dna/gears.mk`, `gear-scoped-ci`, the cfs autodetect and `cargo gears` operate without changes.
- You can move open pull requests with `git format-patch` and `git am`. The file paths do not change.

### How the new repositories use the core

- The new repositories use the core crates from crates.io:
  - `cf-gears-toolkit`;
  - the SDK crates;
  - the implementation crates of the system gears.
- For local work against a core version that is not released, use `[patch.crates-io]` with a git branch or a local path. Each repository keeps this block as a comment in its root `Cargo.toml`.
- Each new repository has a nightly compatibility job. This job builds the repository against the `main` branch of the core. It finds breaking changes before a core release.
- Dependabot or Renovate updates the `cf-gears-*` crates as one group.
- Test-only crates such as `test-containers` stay unpublished. The new repositories use them as git dev-dependencies with a tag. Cargo removes dev-dependencies that have no `version` when it publishes a crate. Thus a published crate does not need them.

### CI workflows

Do not copy the workflows into each repository. Copies become different in a short time.

Also, the current workflows do not use composite actions. The setup steps are the same in approximately 10 workflows: protoc, Go, the Rust toolchain, sccache, rust-cache and free-disk-space.

We propose these items in `gears-rust`:

- **Composite actions:** `setup-rust-env` (all setup steps) and `free-disk-space`.
- **Reusable workflows** (`on: workflow_call`):
  - `rust-ci.yml`: fmt, clippy, tests on 3 operating systems, cargo-deny, coverage, `cargo gears lint --dylint`;
  - `gear-scoped-ci.yml`;
  - `e2e.yml`, with the suites and features as inputs;
  - `release-plz.yml` and `publish-artifacts.yml`;
  - `helm.yml`, `docs-lychee.yml`, `gts-validation.yml`, `cfs.yml`, `api-contracts.yml`;
  - hygiene: `clippy-nightly`, `shear-nightly`, `codeql`, `scorecard`, `crlf-check`, `cache_cleanup`, `test-report`.

We give the workflows version tags, for example `ci-v1` and `ci-v2`. This is the same method as for `.gears-dna/gears.mk` v1.0.0.

Each new repository keeps only small caller workflows:

```yaml
jobs:
  ci:
    uses: constructorfabric/gears-rust/.github/workflows/rust-ci.yml@ci-v1
    with: { integration-lanes: "ledger-pg pricing-pg products-pg" }
    secrets: inherit
```

**Copy these files into each new repository without changes:**
- `.editorconfig`, `.gitattributes`, `.gitignore`, `.dockerignore`;
- `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`, `.config/nextest.toml`;
- `LICENSE`, `NOTICE`, `SECURITY.md`, `CONTRIBUTING.md`;
- `.github/pull_request_template.md`, `.github/copilot-instructions.md`, `.github/forbidden-words.json`;
- `.claude/` and the other AI tool directories;
- `.cf-studio-kit.toml`;
- the `guidelines/DNA` submodule;
- `Makefile` and `.gears-dna/gears.mk`.

**Make a version for each repository of these files:**
- the members, `[workspace.dependencies]` and lints in `Cargo.toml`;
- `deny.toml`, `dylint.toml`, `Gears.toml`, `release-plz.toml`;
- `.github/CODEOWNERS`, `.github/dependabot.yml`, `.codecov.yml`;
- the ignore list in `artifacts.toml`;
- a `<repo>.mk` file with the integration lanes and the E2E targets.

**Keep these workflows only in the core:**
- `notify-docs` and `web-docs-check` (they send a dispatch to `gears-rust-web-docs`);
- `clusterfuzzlite`, `pr-dashboard`.

`dco-signoff` and `forbidden-words` can also be reusable workflows.

## Prerequisites

We do these tasks in the current monorepo, before the split. Each task is a usual pull request. After these tasks, the split is a mechanical procedure.

### Blocking tasks

1. **Dependency paths.**
   - Replace the relative `path = "../../system/..."` dependencies in gears with `{ workspace = true }`. This applies to:
     - `construct`, `file-storage`, `mini-chat`, `settings-service`, `simple-user-settings`, `llm-gateway-sdk`;
     - the dev-dependencies of BSS `pricing` and `products` on `event-broker`;
     - the dev-dependency of `cluster-sdk` on `toolkit-contract-protogen`.
   - Correct the old version `authn-resolver-sdk 0.3.32` in `settings-service`.
2. **Publish the core crates that are not on crates.io:**
   - `durable-execution` and `durable-execution-sdk`;
   - `quota-enforcement` and `quota-enforcement-storage-plugin`;
   - `sqlite-event-broker-plugin`;
   - `settings-service-sdk` 0.2.0, if `construct` and `settings-service` go to different repositories.

   Make sure that the published `event-broker` has the `test-utils` feature.
3. **Test-only crates.** Use `test-containers` as a git dev-dependency with a tag. Do not publish it.
4. **Example server.** Today `apps/cf-gears-example-server` is one binary with approximately 40 features for all gears. E2E, `make openapi`, `make run` and the Docker images use it.
   - In the core, keep only the system gears, `credstore` and the core leaf gears.
   - Each new repository gets its own small server, `apps/<repo>-server`. This server uses the core from crates.io and adds the gears of the repository.
5. **E2E harness.**
   - Move these files into a reusable Python package:
     - `tools/scripts/run_e2e.py`;
     - `tools/scripts/ci.py`;
     - `testing/e2e/{conftest.py,lib,helpers,pytest.ini}`.

     Publish the package from the core, or install it with `pip install git+...@<tag>`.
   - Make the server binary and the configuration file into parameters. Today the paths `apps/cf-gears-example-server` and `config/e2e-local.yaml` are fixed.
6. **E2E suites that use gears from more than one group.**
   - The `resource_group/tr-authz` profile uses `mini-chat`, `file-storage` and `bss-ledger`. Remove them from the core suite. Move the related checks to the new repositories.
   - `scope_enforcement` uses all features. In the core, use only the core features. Each new repository gets a copy for its own routes.
   - The BSS suites and the `settings_service` suite depend only on the core. They move without changes.
7. **Configuration.**
   - Divide `config/e2e-local.yaml`, `config/quickstart.yaml` and `config/e2e-features.txt` into core sections and sections for each repository.
   - Gear presets go with their gears: `mini-chat.yaml`, `chat-engine-dev.yaml`, `github-mirror-dev.yaml`.
8. **OpenAPI.** Today `docs/api/api.json` is one specification for all gears.
   - Make one specification for each repository.
   - `api_contracts.yml` becomes a reusable workflow.
9. **Helm.**
   - Publish `toolkit-common` to `oci://ghcr.io/constructorfabric/charts`.
   - Gear charts (for example `mini-chat`) must use an `oci://` dependency, not `file://../..`.
   - Each repository has its own `deploy/components.yaml`.
10. **Release.**
    - Change `release-plz.toml` and `tools/scripts/check_release_config.py`. The rule "libs go into the toolkit release, gears get their own release" must operate in all repositories.
    - Make the check `github.repository == 'constructorfabric/gears-rust'` into a parameter.
    - Each repository has its own `CHANGELOG.md`.
11. **Gear categories.** Change the category of `gears/system/cluster` to `core-functionality`.

### Optional tasks

1. **event-broker server dependencies.** These dependencies are necessary only for `src/bin/server.rs`:
   - `api-gateway`, `authn-resolver`;
   - `static-authn-plugin`, `static-authz-plugin`, `standalone-cluster-plugin`.

   Today the `event-broker` crate declares them as normal dependencies (see `gears/system/event-broker/event-broker/Cargo.toml`). This does not block the split, because all these crates are in the core. But all crates that use `event-broker` must also compile these dependencies. Move them behind a `server` feature, or move the binary into a separate crate. This decreases the build time in the new repositories.
2. **CI corrections.**
   - The `tools/dylint_lints/**` filter in `ci.yml` points to a directory that does not exist.
   - No CI job runs `make e2e-event-broker`.
   - `test-report.yml` uses the workflow names "CI" and "e2e". The new repositories must use the same names.

## Migration procedure

Do this procedure for each new repository. The example uses `gears-bss`.

1. Tell the teams about a freeze of `gears/bss/**` for 1–2 days. Merge the open pull requests before the split, or move them after the split.
2. Make a copy of the repository and keep only the related paths. This keeps the git history.

   ```bash
   git clone --no-local gears-rust gears-bss && cd gears-bss
   git filter-repo \
     --path gears/bss/ \
     --path testing/e2e/suites/bss/ \
     --path config/<bss-config-files> \
     --path <other-gear-files>
   ```

3. Add a second commit with the repository frame:
   - the `Cargo.toml` workspace, `Makefile` and `.gears-dna/`;
   - the caller workflows, `CODEOWNERS`, `release-plz.toml` and `CHANGELOG.md`;
   - `apps/bss-server`.
4. Push the repository:

   ```bash
   git remote add origin git@github.com:constructorfabric/gears-bss.git
   git push -u origin main
   ```

5. Make sure that the CI in the new repository passes: build, tests, `gear-ci`, the BSS E2E suites and `release-plz release --dry-run`.
6. In the core, remove all BSS items in one pull request:
   - `gears/bss/**`;
   - the members and `[workspace.dependencies]` entries in `Cargo.toml`;
   - the example server features and the E2E suites;
   - the integration lanes in `gears-rust.mk`;
   - the lines in `CODEOWNERS`, `dylint.toml`, `deny.toml`, `.codecov.yml` and `artifacts.toml`.

   Add a link to the new repository in the root `README.md`.
7. Move the open pull requests with `git format-patch` and `git am`.
8. Move the issues with `gh issue transfer`.
9. Update the links in `docs/` and in the web docs.
10. Configure the secrets: `CRATES_IO_TOKEN`, `DOCS_DISPATCH_TOKEN` and the ghcr access.
11. Configure branch protection and `CODEOWNERS`.
12. On crates.io, add the team as owner of the published BSS crates. Then the new repository can publish them.

## Phases

| Phase | Tasks | Result |
|---|---|---|
| 0 | The prerequisites. Do blocking tasks 1–3 and 11 and optional task 2 first. Do blocking tasks 4–10 in parallel. | The monorepo operates as before. It is ready for the split. |
| 1 | Reusable workflows, composite actions and the E2E package, tag `ci-v1` | The core uses these items through `uses: ./.github/workflows/...`. |
| 2 | Pilot: `gears-bss` | We test the procedure and find the dependencies that we did not see before. |
| 3 | `gears-serverless`, then `gears-genai` | — |
| 4 | Clean the core: core-only example server, core-only `api.json`, review of `docs/` | The core contains only the platform. |

## Acceptance criteria

**In each new repository:**
- `cargo metadata` shows no `path` dependency that goes outside the repository.
- `cargo build --workspace --all-features` and `make test` pass.
- `make GEAR=<gear> gear-ci` passes for each gear.
- The E2E suites pass on `apps/<repo>-server`.
- `release-plz release --dry-run`, `helm lint` and `make lychee` pass.
- The nightly compatibility job against the core `main` branch passes.

**In the core, after the removal:**
- The full `ci.yml` and `e2e.yml` (local and Docker) pass.
- `make openapi` gives a specification only for the core.
- `cargo shear` and `cargo deny` show no errors.

## Alternatives that we examined

1. **Keep the monorepo. Improve the gear-scoped CI and `CODEOWNERS`.** This needs no migration. But the shared release, the shared example server and the shared E2E tests stay.
2. **Copy the workflows into each repository.** This is fast at the start. But the copies become different, and each CI correction must be done more than once.
3. **Use git submodules or git dependencies for the core.** The core crates are already on crates.io. Git dependencies make version control more difficult. Also, a crate that is published to crates.io cannot use them.

## Open questions

1. Where do `file-parser`, `file-storage` and `github-mirror` go? Options:
   - the core (this is the default; their category is core);
   - `gears-genai` (applies to `file-parser`);
   - a new `gears-foundation` repository for general application gears.
2. Where do `settings-service` and `simple-user-settings` go: `gears-serverless` or the core?
   - `settings-service` depends on `credstore`, which is in the core.
   - `construct` in `gears-genai` depends on `settings-service-sdk`.
3. Where do the reusable workflows go: `gears-rust` or a separate `gears-ci` repository? We recommend `gears-rust`. A separate repository is necessary only if repositories outside the gears project use the workflows.
4. Do we need one combined `api.json` for all repositories, for example for the web docs?
5. When do we need `gears-oss`?
