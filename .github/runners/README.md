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

The expression below selects `gears-large`. In every other job, `'gears-large'`
is replaced with that job's `gears-*` label from the table above.

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
fork pull requests are routed to GitHub-hosted runners.** Pushing a branch
takes write access, so a pull request from a branch here is trusted by where it
comes from, not by who opened it. Contributors with write access push branches
here instead of opening pull requests from their forks.

A fork with Actions enabled runs its own pushes and pull requests; no runner
there serves the `gears-*` labels, so those runs go to GitHub-hosted runners
instead of queueing until they time out.

**Routing is not a security boundary.** A pull request runs the workflow from
its own merge commit, so its author can rewrite `runs-on`; the runner is picked
before any step executes, and no runner-group setting rejects fork-originated
workflows. The boundary is GitHub's approval gate for workflow runs from
outside contributors' fork pull requests: approving such a run admits its
workflow file, so read the workflow changes, `runs-on` included, before
approving one. The repository policy must require approval for all outside
contributors, not only first-time ones. That gate does not cover organisation
members: a member's fork run needs no approval, whatever the member's access to
this repository, so a member can route a fork pull request onto these runners
by editing the workflow. Organisation members are the trust scope the runners
are provided for. Each runner is a fresh machine destroyed after its one job;
while the job runs the machine holds whatever that job holds: its
`GITHUB_TOKEN`, any secret the job is given, the read-only cache key, and on a
push to `main` the read-write key. A fork pull request gets a read-only
`GITHUB_TOKEN`, no job secrets and never the read-write key. Outside
contributors are the ones the approval gate keeps out.

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
- **`scorecard.yml`** stays on `ubuntu-latest`: `ossf/scorecard-action` with
  `publish_results: true` accepts results only from a GitHub-hosted Ubuntu
  runner, and a rejected publish only warns.

## Caches

`.github/actions/ci-cache` wires a job to the shared build cache: sccache for
Rust and a download cache for the Cargo registry and git checkouts. Only a
`push` to `main` writes, through the `ci-trusted` environment, whose secrets
`CI_CACHE_RW_KEY_ID` and `CI_CACHE_RW_SECRET` hold the read-write key and whose
deployment-branch policy admits `main` alone; every other run reads with the
key the runner carries. A read needs both that key and a reachable cache
endpoint: on a runner without the key (GitHub-hosted), or one whose endpoint is
missing or does not answer, the action prints a notice, reports mode `none` and
the job builds without the shared cache. The job's `Swatinem/rust-cache` step,
guarded by `steps.cache.outputs.mode == 'none'`, then keeps the registry cached
through GitHub's cache. The action's post step writes `sccache --show-stats`
into the job summary.

Workflows that never run on a `push` to `main` (`api_contracts`, `e2e`, `fmt`,
`gear-scoped-ci`, `gts-validation`, `shear-nightly`) call the action without
the key inputs and without an environment: they only read.

## What the runner image provides

On the self-hosted runners the toolchain and the tools come from the runner
image: Rust (the `rust-toolchain.toml` pin and the nightly pinned in `ci.yml`
and `shear-nightly.yml`), protoc, nextest, cargo-llvm-cov, cargo-hack,
cargo-deny, cargo-shear, cargo-gears, gts-validator, Go, cmake. The install
steps for protoc, nextest, cargo-llvm-cov, cargo-hack, cargo-deny, cargo-shear
and the pinned nightly carry `if: runner.environment == 'github-hosted'`, as
does `jlumbroso/free-disk-space`. The `rustup toolchain install` of the
`rust-toolchain.toml` pin, `actions/setup-go` and the `cargo install` steps run
everywhere and are no-ops when the image already has the version. Bumping one
of these versions in a workflow also
needs the matching change in the runner image, which the platform owners
rebuild weekly; until then the self-hosted run uses the image's version
(`cargo install` steps, as in `gts-validation.yml`, compile the requested one
instead).
