# Runners

Every Linux x64 job in `.github/workflows` runs on one of three self-hosted
labels of the organisation's runners, or on a GitHub-hosted runner. Each
self-hosted runner is ephemeral: it takes one job and is destroyed afterwards,
so nothing carries over between jobs.

| Label | Machine | A job belongs here when |
|---|---|---|
| `gears-small` | a container: 1 CPU, 2-4 GiB, no Docker, no Rust toolchain; Python, Node, gh, yq, helm, make, git, pipx | it starts no container, needs no Rust toolchain, uses no secret beyond `GITHUB_TOKEN`, and its token has no `contents`, `packages`, `id-token` or `attestations` write (`issues`, `checks`, `security-events`, `pull-requests` write are fine). Short. |
| `gears-small-vm` | a small VM with Docker and the Rust toolchains | short, but needs cargo or rustfmt, holds a secret, or needs a write permission |
| `gears-large` | a large VM with Docker, privileged containers allowed | it compiles, tests, builds images, or starts containers (testcontainers, compose, k3s) |

## The rule

```yaml
runs-on: ${{ (vars.GEARS_FORCE_GITHUB_HOSTED == 'true' || github.repository != 'constructorfabric/gears-rust' || (github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name != github.repository)) && 'ubuntu-latest' || 'gears-large' }}
```

| Event | Runner |
|---|---|
| pull request from a branch of this repository, Dependabot included | the job's `gears-*` label |
| pull request from a fork | `ubuntu-latest` |
| `push`, `merge_group`, `schedule`, `workflow_dispatch`, `workflow_run` | the job's `gears-*` label |
| any run inside a fork of this repository | `ubuntu-latest` |
| any of the above with `GEARS_FORCE_GITHUB_HOSTED=true` (the kill switch) | `ubuntu-latest` |

**Whoever wants the self-hosted runners pushes a branch to this repository;
forks never reach them.** Pushing a branch takes write access, so a pull
request from a branch here is trusted by where it comes from, not by who opened
it. Contributors with write access push branches here instead of opening pull
requests from their forks. Approval for an outside contributor's workflow is an
admission gate, not a sandbox, so fork pull requests stay on GitHub-hosted
runners.

A fork with Actions enabled runs its own pushes and pull requests; no runner
there serves the `gears-*` labels, so those runs go to GitHub-hosted runners
instead of queueing until they time out.

**Routing is not a security boundary.** A pull request runs the workflow from
its own merge commit, so its author can rewrite `runs-on`; the runner is picked
before any step executes.

Matrix jobs with OS legs route the Linux leg only:
`${{ matrix.os == 'ubuntu-latest' && (<the rule>) || matrix.os }}`.
`test-suite.yml` takes the runner as `inputs.os`: `ci.yml` passes the rule with
`gears-large`, `macos-windows-tests.yml` passes `macos-latest` and
`windows-latest`.

## Exceptions

- **Publishing stays on GitHub-hosted runners**: `release-plz.yml` jobs
  `release`, `plan-publish` and `publish`, and `publish-artifacts.yml`.
  `release-pr` compiles and runs on `gears-large`.
- **macOS and Windows legs** (`macos-windows-tests.yml`, the matrix legs of
  `test-fips` in `ci.yml` and of `makefiles.yml`) stay on `macos-latest` and
  `windows-latest` under every event and with the kill switch on.
- **`code-ranker.yml`** calls an external reusable workflow that picks its own
  runner.

## Caches

`.github/actions/ci-cache` wires a job to the shared build cache: sccache for
Rust and a download cache for the Cargo registry and git checkouts. Only a
`push` to `main` writes, through the `ci-trusted` environment, whose secrets
`CI_CACHE_RW_KEY_ID` and `CI_CACHE_RW_SECRET` hold the read-write key and whose
deployment-branch policy admits `main` alone; every other run reads with the
key the runner carries. On a runner without the cache (GitHub-hosted) the
action prints a notice and does nothing, and the job's `Swatinem/rust-cache`
step, guarded by `steps.cache.outputs.mode == 'none'`, keeps the registry
cached there. The action's post step writes `sccache --show-stats` into the job
summary.

Workflows that never run on a `push` to `main` (`api_contracts`, `e2e`, `fmt`,
`gear-scoped-ci`, `gts-validation`, `shear-nightly`) call the action without
the key inputs and without an environment: they only read.

## What the runner image provides

On the self-hosted runners the toolchain and the tools come from the runner
image: Rust (the `rust-toolchain.toml` pin and the nightly pinned in `ci.yml`
and `shear-nightly.yml`), protoc, nextest, cargo-llvm-cov, cargo-hack,
cargo-deny, cargo-shear, cargo-gears, gts-validator, Go, cmake. The install
steps for these carry `if: runner.environment == 'github-hosted'`, as does
`jlumbroso/free-disk-space`. Bumping one of these versions in a workflow also
needs the matching change in the runner image, which the platform owners
rebuild weekly; until then the self-hosted run uses the image's version
(`cargo install` steps, as in `gts-validation.yml`, compile the requested one
instead).
