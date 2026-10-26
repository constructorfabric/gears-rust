# Testing Policy

This document defines the test strategy, coverage requirements, and CI enforcement for
Gears.  It is the single source of truth for "what must be tested and how."

Gears promotes a **shift-left**, green-build-oriented development model: contributors
should be able to run the vast majority of checks on a local development machine before
opening a pull request.  This is practical because the codebase has a low memory and CPU footprint with help of Rust, fast and deterministic local test execution, and a packaging model where many gear combinations can be exercised through a small number of binaries rather than a large distributed deployment.

The unified `Makefile` and `tools/scripts/ci.py` entry points mirror CI behavior locally, so unit tests, most integration tests, linting, safety checks, coverage, and most E2E validation can usually be reproduced before the code ever reaches GitHub Actions. CI still remains the source of truth for cross-platform validation, scheduled suites, specialized lanes such as FIPS, and final merge protection, but contributors are expected to arrive with a locally green
build whenever feasible.

---

## 1. Test Pyramid

| Layer | Scope | Tooling | Feature gate | Runs in CI |
|-------|-------|---------|--------------|------------|
| **Unit** | Single function / struct / gear in isolation | `cargo test --workspace` | none (always compiled) | Every PR on Ubuntu (`ci.yml` — `test` job); macOS and Windows nightly (`macos-windows-tests.yml`) |
| **Integration** | Cross-crate or DB-backed logic (SQLite, Postgres, MySQL) | `cargo test -p <pkg> --features integration` | `#[cfg(feature = "integration")]` | Every PR (`ci.yml` — `integration` job, Ubuntu) |
| **E2E** | Full HTTP request → response through a running server | pytest + httpx against `cf-gears-example-server` | n/a (Python tests) | PRs to `main`, nightly schedule (`e2e.yml`) |
| **Fuzz** | Parser / validator robustness against arbitrary input | `cargo-fuzz` (libFuzzer) | nightly toolchain | PRs + nightly (`clusterfuzzlite.yml`) |
| **Static analysis** | Architectural rules, unsafe code, dependency licenses | clippy, `cargo gears lint`, cargo-deny, cargo-kani, cargo-geiger | varies | Every PR (`ci.yml` — `test`, `security`, `lint` jobs) |

Additional testing categories such as performance (#4054), upgrade / migration (#4117), and long-haul (#4118) or soak testing are expected to be added over time, but they are not yet implemented or enforced as part of the current project test matrix.

```bash
make check                  # full quality gate (fmt + clippy + test + security)
```

### Quick-reference commands

```bash
make build                 # whole-project example server release binary
make run                   # default example server
make test                  # unit tests (workspace, all OS)
make test-integration LANE=toolkit-db-sqlite  # integration — SQLite
make test-integration LANE=toolkit-db-pg  # integration — PostgreSQL
make test-integration LANE=toolkit-db-pgq  # integration — PostgreSQL 19 SQL/PGQ (Docker)
make test-integration LANE=toolkit-db-mysql  # integration — MySQL
make test-db               # all DB integration tests (sqlite, pg, pgq, mysql)
make test-integration LANE=users-info-pg  # users-info gear integration (Postgres)
make e2e-docker            # E2E — Docker environment
make e2e-docker-smoke      # E2E — Docker environment (smoke subset only)
make e2e-local             # E2E — local: run every shared-server suite against one shared server
make e2e-local-smoke       # E2E — smoke subset only
make e2e-local SUITE=mini-chat  # E2E — mini-chat lane (dedicated binary, offline mode)
make e2e-tr-authz          # E2E — AuthZ -> TR -> RG chain (resource_group/e2e.yaml, profile: tr-authz)
make e2e-usage-collector   # E2E — usage-collector lane, both backends (BACKEND= for one, below)
make e2e-usage-collector BACKEND=timescaledb  # E2E — usage-collector lane, TimescaleDB only (dedicated binary; needs Docker)
make e2e-usage-collector BACKEND=clickhouse  # E2E — usage-collector lane, ClickHouse only (dedicated binary; needs Docker)
make fuzz                  # fuzz — 30 s smoke per target
make check                 # full quality gate (fmt + clippy + test + security)
make all                   # full pipeline (build + check + toolkit-db-sqlite lane + e2e-local)

# CI gate: no database container built outside libs/test-containers (4.4)
cargo xtask check-test-container-pins

make build GEAR=file-parser      # one gear package plus SDK package
make test GEAR=file-parser       # one gear package plus SDK package
make run GEAR=file-parser        # server with only this gear feature set
make e2e-local SUITE=file-parser # focused run: just this suite's tests
make e2e-local GEAR=credstore    # run every shared-server suite that exercises a gear
make coverage GEAR=file-parser   # unit + E2E coverage for one gear scope
```

---

## 2. Coverage

### 2.1 Threshold

The project-wide **line-coverage threshold is 80 %**.  The threshold is enforced in
`tools/scripts/coverage.py` (`COVERAGE_THRESHOLD`) and printed as a warning when any gear
or library falls below it.

### 2.2 Coverage modes

| Mode | Command | What it measures |
|------|---------|------------------|
| **Unit** | `make coverage-unit` | All `#[test]` functions across the workspace (compiled with `--all-features`) |
| **E2E** | `make coverage-e2e-local` | Server code exercised by the pytest E2E suite via an instrumented binary |
| **Combined** | `make coverage` | Unit + E2E accumulated into a single report |

> **Why no separate "integration" coverage mode?**
> Unit coverage already compiles with `--all-features`, which enables the `integration`
> feature gate.  SQLite-backed integration tests therefore execute as part of
> `make coverage-unit`.  A separate mode would re-run the same tests and produce
> identical data — so it is intentionally omitted to avoid confusion.

### 2.3 CI coverage

The `coverage` job in `ci.yml` runs on every PR using `cargo-llvm-cov` (nightly) and
uploads an LCOV report to Codecov.

### 2.4 Reports

Local coverage commands produce four report formats under `coverage/<mode>/`:

| File | Format |
|------|--------|
| `html/index.html` | Interactive HTML (per-file, per-line) |
| `summary.txt` | Text summary |
| `lcov.info` | LCOV (for IDE plugins and CI upload) |
| `coverage.json` | JSON (machine-readable, used by the custom report) |
| `coverage_report.txt` | Custom per-gear/per-library table |

---

## 3. Unit Tests

### 3.1 Expectations

- Every new public function, method, or behaviour **must** have at least one unit test.
- Tests live next to the code they exercise, in a `#[cfg(test)] mod tests` block.
- Use descriptive names: `test_<function>_<scenario>_<expected>`.
- Prefer deterministic assertions — avoid sleeping or time-dependent checks.

### 3.2 Running

```bash
make test                              # workspace unit tests via nextest
make test GEAR=file-parser             # one gear plus SDK package scope
make test GEAR=file-parser GEAR_FEATURES=integration
cargo test --workspace                 # all unit tests
cargo test -p cf-gears-oagw            # single package
cargo test -p cf-gears-toolkit-db -- cursor  # filtered by name
```

`GEAR=<name>` defaults to packages `cf-gears-<name>` and `cf-gears-<name>-sdk`. Override `GEAR_PKG`, `GEAR_SDK_PKG`, `GEAR_FEATURES`, or `GEAR_TEST_ARGS` when a gear has non-standard package names or needs extra Cargo flags.

---

## 4. Integration Tests

Integration tests verify cross-crate or database-backed behaviour.  They are gated
behind the `integration` Cargo feature so that `cargo test --workspace` (without
`--all-features`) does **not** require a running database.

### 4.1 Feature gates

| Package | Features | Backend |
|---------|----------|---------|
| `cf-gears-toolkit-db` | `sqlite,integration` | SQLite (in-process) |
| `cf-gears-toolkit-db` | `pg,integration` | PostgreSQL (requires running instance) |
| `cf-gears-toolkit-db` | `pgq,integration` | PostgreSQL 19 SQL/PGQ (testcontainers; `make test-integration LANE=toolkit-db-pgq`) |
| `cf-gears-toolkit-db` | `mysql,integration` | MySQL (requires running instance) |
| `users-info` | `integration` | PostgreSQL |

### 4.2 Running

```bash
make test-integration LANE=toolkit-db-sqlite  # quick, no external services needed
make test-integration LANE=toolkit-db-pg      # requires Postgres
make test-integration LANE=toolkit-db-pgq     # requires Docker; PostgreSQL 19 SQL/PGQ lane (testcontainers)
make test-integration LANE=toolkit-db-mysql   # requires MySQL
make test-db                                  # all of the above, toolkit-db-pgq included
make test-integration LANE=users-info-pg      # users-info Postgres integration
make test-integration                         # every integration lane (`make help` lists them)
```

### 4.3 CI

The `integration` job in `ci.yml` runs the SQLite, Postgres, PGQ (PostgreSQL 19, with
`GEARS_TEST_PG_GRAPH_REQUIRED=1` so an unavailable image fails the step rather than skipping
it) and MySQL integration tests plus macro UI tests on every PR (Ubuntu only).

### 4.4 Database container images

Every fixture that starts a database container goes through
[`libs/test-containers`](../libs/test-containers/src/lib.rs) (crate
`cf-gears-test-containers`, imported as `test_containers`). It is the single place image
versions are pinned, so a version change is one reviewed diff instead of a grep across the
workspace.

**Do not call `Postgres::default()` or `Mysql::default()` in a test**, and do not call
`GenericImage::new()` outside that crate. Those constructors take their tag from
`testcontainers-modules`, which pins it transitively through `Cargo.lock` — a dependency bump
then changes the database under every test with nothing in the diff to show it.

`cargo xtask check-test-container-pins` enforces this in CI (the `clippy` job). It parses every
tracked `.rs` file with `syn`, so an import rename (`Postgres as Pg`), a type alias, a
non-literal image name or an oddly wrapped call is caught the same as the plain spelling.

```rust
// Add to [dev-dependencies]:  test-containers = { workspace = true }
use testcontainers::runners::AsyncRunner;

let container = test_containers::postgres().start().await?;

// Non-default database name (POSTGRES_DB is applied at image level):
let container = test_containers::postgres_named("cluster_test").start().await?;

// A suite pinned to a floor of its own, above or below the workspace pin —
// "16-alpine" is just an illustration value here. GEARS_TEST_PG_TAG still
// wins over it, so the suite stays in the CI version matrix; chaining
// `.with_tag(...)` onto `postgres()` would silently opt out of the override.
let container = test_containers::postgres_tagged("16-alpine").start().await?;
```

Helpers: `postgres()`, `postgres_named()`, `postgres_tagged()`, `postgres_graph()`, `mysql()`,
`redis()`, `timescaledb()`, `mariadb()`, `clickhouse()`.

`timescaledb()`, `mariadb()` and `clickhouse()` return a `GenericImage` rather than a
`ContainerRequest`: there is no `testcontainers-modules` image module for them, so the caller
still supplies the wait strategy and environment. ClickHouse in particular needs
`WaitFor::Nothing` plus an HTTP readiness poll — that image writes its server log to files under
`/var/log/clickhouse-server`, so a `message_on_stdout` wait can only ever time out.

#### Version-matrix overrides

Each tag can be overridden from the environment, so CI can run a matrix without touching code.
An unset *or empty* variable means "use the pinned constant".

| Variable | Overrides |
|---|---|
| `GEARS_TEST_PG_TAG` | `POSTGRES_TAG` |
| `GEARS_TEST_PG_GRAPH_TAG` | `POSTGRES_GRAPH_TAG` |
| `GEARS_TEST_MYSQL_TAG` | `MYSQL_TAG` |
| `GEARS_TEST_TIMESCALEDB_TAG` | `TIMESCALEDB_TAG` |
| `GEARS_TEST_MARIADB_TAG` | `MARIADB_TAG` |
| `GEARS_TEST_CLICKHOUSE_TAG` | `CLICKHOUSE_TAG` |
| `GEARS_TEST_REDIS_TAG` | `REDIS_TAG` |

```bash
GEARS_TEST_PG_TAG=16-alpine cargo nextest run -p cf-gears-toolkit-db --features pg,integration
```

`GEARS_TEST_TIMESCALEDB_TAG` and `GEARS_TEST_CLICKHOUSE_TAG` are each read by *both* lanes: the
Rust plugin fixtures via `test_containers::timescaledb()` / `clickhouse()`, and the Python E2E
sidecars via `timescaledb_tag()` / `clickhouse_tag()` in
[`testing/e2e/lib/sidecars.py`](../testing/e2e/lib/sidecars.py). A matrix run therefore keeps
migrations and E2E on the same image instead of silently testing two different ones.

Because those two pins are spelled once per language, each is duplicated across a Rust constant
and a Python one. The unit tests `e2e_sidecar_pins_the_same_timescaledb_image` and
`e2e_sidecar_pins_the_same_clickhouse_image` in `libs/test-containers` read `sidecars.py` and
fail the build if either copy drifts — so a version bump is still a single reviewed decision even
though it touches two files.

`GEARS_TEST_PG_GRAPH_REQUIRED=1` turns an unavailable PostgreSQL 19 image into a failure rather
than a graceful skip; it is off by default while that tag is pre-GA. Unset, empty, `0`, `false`,
`off` and `no` all mean off.

Only concrete tags belong in the constants — a floating alias such as `lts` or `latest`
re-points under CI with nothing in the diff, which is the drift this crate exists to prevent.

---

## 5. End-to-End (E2E) Tests

E2E tests exercise the full HTTP surface of the gears testing server using Python (pytest +
httpx).

### 5.1 Expectations

- Every user-facing REST endpoint **should** have at least one E2E smoke test.
- Critical flows (CRUD, auth, error responses) **must** have full E2E coverage.
- Mark lightweight, fast tests with `@pytest.mark.smoke` — these run on every PR.

### 5.2 Modes

A **suite** is one E2E scenario under `testing/e2e/suites/` (usually a gear).
A **shared-server suite** (marked `launcher: e2e-launcher` in its `e2e.yaml`) can
run against one common server; a **self-managed suite** (`launcher: pytest`)
starts its own server and must be run on its own.

`make e2e-local GEAR=<gear>` discovers and runs every shared-server suite whose
Cargo `features` (in its `e2e.yaml`, including features pulled in via
`features_file`) include that gear — no separate declaration is needed. `SUITE=`
and `GEAR=` are mutually exclusive.

| Mode | What it does | Use case |
|------|---------|----------|
| **Local, many suites** (`make e2e-local`) | Builds one server and runs **every shared-server suite** against it. Self-managed suites are skipped — run those with their own target (see 5.3). | Development, CI |
| **Local, one suite** (`make e2e-local SUITE=file-parser`) | Builds a server for just that suite and runs only its tests (`testing/e2e/suites/file_parser`). Works for shared-server and self-managed suites. | Focused suite iteration |
| **Local, one gear** (`make e2e-local GEAR=credstore`) | Discovers every shared-server suite whose `e2e.yaml` features include the gear (e.g. `credstore` → the `credstore` and `oagw` suites) and runs each as its own focused build+run. Self-managed suites are skipped. | Gear-scoped iteration |
| **Docker** (`make e2e-docker`) | Builds a Docker image and runs the tests in a container | Isolation, reproducibility |

### 5.3 CI

The `e2e.yml` workflow runs:
- **On PRs to `main`**: full E2E suite (local mode).
- **Nightly**: full E2E suite. Failures auto-create a GitHub issue assigned to the last commit author.
- **Manual dispatch**: smoke or full, selectable.
- **Specialized lanes**: the same workflow also runs the mini-chat E2E suite, the
  RG + AuthZ end-to-end chain tests, and the usage-collector suite.

The `tr-authz` lane is ordinary local mode with a full-config profile
(`testing/e2e/suites/resource_group/e2e.yaml`, profile `tr-authz`, applied over
`config/e2e-local.yaml`) and a `-k resource_group` selection. The `mini-chat` and
`usage-collector` lanes each build their own `cf-gears-example-server` with a feature
set the default local-mode binary does not carry, so they cannot share the common
server: they are `launcher: pytest` suites that own their server (and, for
usage-collector, a TimescaleDB container) in their own `conftest.py`. A plain
`make e2e-local` (no `SUITE`) runs **only** the shared-server suites (`launcher:
e2e-launcher`) and therefore **skips these** — run them via
`make e2e-local SUITE=mini-chat` and `make e2e-usage-collector`, which CI
invokes as separate steps.

The usage-collector lane additionally needs a reachable Docker daemon — its storage
plugin connects and migrates a real TimescaleDB (or ClickHouse) at gear init, which
`lib/sidecars.py` supplies as a throwaway container on a dynamically mapped port. When
Docker is unreachable the suite skips rather than fails, so verify the skip count before
reading a green run as coverage.

Those containers are labelled per run, so two sessions can share one host without
reaping each other's database: the label value ends in a per-process `RUN_ID`, and
containers left behind by a crashed run (where `atexit` never fired) are swept on the
next start by age rather than by label alone. Set `CF_GEARS_E2E_RUN_ID` to pin the run
identity, and `CF_GEARS_E2E_REAP_MIN_AGE_SECS` (default 3600) to change how old a
foreign container must be before it counts as leaked — lowering it below the length of
a full session reintroduces the cross-session reap it exists to prevent. Values below
the 600s floor (`REAP_MIN_AGE_FLOOR_SECS`), and anything that is not an integer, are
rejected at import with an error naming the variable; unset or empty means the default.
`reap_stale()` refuses a sub-floor threshold passed directly too, so neither a bad
override nor a careless caller can turn the age-gated sweep into a host-wide removal.

Other quality-related GitHub Actions under `.github/workflows` complement the E2E
workflow:
- **`ci.yml`** runs the main cross-platform quality gates: linting, unit tests,
  integration tests, FIPS verification, coverage, security checks, architecture lints, and Cypilot
  validation.
- **`fmt.yml`** runs dedicated Rust formatting validation.
- **`docs.yml`** checks Markdown links for documentation changes.
- **`gts-validation.yml`** validates GTS identifiers in documentation and schema files.
- **`codeql.yml`** performs security and quality-oriented code scanning.

### 5.4 Writing E2E tests

The detailed E2E guide is [`testing/e2e/README.md`](../testing/e2e/README.md). Use it as
the source of truth for:

- **Adding a new suite** — naming conventions, `testing/e2e/suites/<suite>/` layout,
  `e2e.yaml` fields, focused builds, config pruning, overlays, and profiles.
- **Choosing who starts the server (`launcher`)** — use `launcher: e2e-launcher`
  when tests are only HTTP clients and can run against the standard shared E2E
  server started by the framework; use `launcher: pytest` only when the suite's
  own `conftest.py` must start the server, choose ports/config, or manage sidecars.
- **Running tests** — `make e2e-local` runs all shared-server suites, while
  `make e2e-local SUITE=<suite>` focuses one suite and works for self-managed suites.
- **Writing test files** — pytest/httpx examples, common fixtures, smoke markers,
  deterministic data, and HTTP contract assertions.
- **Validating manifests** — use `python3 tools/scripts/run_e2e.py --suite <suite>
  --dry-run` before relying on a changed `e2e.yaml`.

---

## 6. Fuzz Testing

Fuzz testing targets parsers and validators to catch panics, logic bugs, and complexity
attacks.

### 6.1 When to fuzz

- Before submitting changes to **parsers**, **validators**, or **deserialization** logic.
- Nightly via ClusterFuzzLite in CI.

### 6.2 Targets

| Target | Priority | Component |
|--------|----------|-----------|
| `fuzz_odata_filter` | HIGH | OData `$filter` parser |
| `fuzz_odata_cursor` | HIGH | Cursor pagination decoder |
| `fuzz_yaml_config` | HIGH | YAML config parser |
| `fuzz_html_parser` | MEDIUM | HTML document parser |
| `fuzz_pdf_parser` | MEDIUM | PDF document parser |
| `fuzz_json_config` | MEDIUM | JSON config parser |
| `fuzz_odata_orderby` | MEDIUM | OData `$orderby` parser |
| `fuzz_markdown_parser` | LOW | Markdown parser |

### 6.3 Running

```bash
make fuzz                                # smoke (30 s per target)
make fuzz-run FUZZ_TARGET=fuzz_odata_filter FUZZ_SECONDS=300  # longer run
make fuzz-list                           # list all available targets
make fuzz-build                          # build without running
make fuzz-clean                          # remove fuzzing artifacts
```

Fuzzing runs automatically in CI via ClusterFuzzLite.
See [`fuzz/README.md`](../tools/fuzz/README.md) for corpus management and crash reproduction.

---

## 7. CI / Development Commands

Gears uses a unified, cross-platform Python CI script (`tools/scripts/ci.py`).
This is the **primary entry point on Windows** where `make` is not available.
Requires Python 3.9+.

### 7.1 Cross-platform commands (`tools/scripts/ci.py`)

```bash
python tools/scripts/ci.py all            # build + full check suite + e2e
python tools/scripts/ci.py check          # fmt, clippy, test, audit, deny
python tools/scripts/ci.py fmt            # check formatting
python tools/scripts/ci.py fmt --fix      # auto-format code
python tools/scripts/ci.py clippy         # run linter
python tools/scripts/ci.py clippy --fix   # attempt to fix warnings
python tools/scripts/ci.py lint           # custom project compliance lints (via cargo gears lint)
python tools/scripts/ci.py audit          # security audit
python tools/scripts/ci.py deny           # license & dependency checks
python tools/scripts/ci.py e2e-local      # build server + run E2E tests locally
python tools/scripts/ci.py e2e-local --smoke  # E2E smoke subset only
python tools/scripts/ci.py e2e-docker     # E2E in Docker
python tools/scripts/ci.py fuzz-build     # build fuzz targets
python tools/scripts/ci.py fuzz --seconds 60  # fuzz smoke run
python tools/scripts/ci.py fuzz-run fuzz_odata_filter --seconds 300  # single target
```

> On Windows, invoke these helpers with `python` (the `python3` alias is
> frequently absent).

Code coverage is produced by a separate script, `coverage.py`:

```bash
python tools/scripts/coverage.py unit       # unit-test coverage
python tools/scripts/coverage.py e2e-local  # e2e-local coverage
python tools/scripts/coverage.py combined   # unit + e2e-local (== make coverage)
```

Prerequisites (install once; `make setup` handles these on supported hosts):

```bash
pip install -r testing/e2e/requirements.txt   # pytest + httpx (e2e tests)
pip install -r testing/requirements.txt        # PyYAML + requests (coverage.py)
cargo install cargo-llvm-cov                    # coverage backend (works on Windows too)
```

### 7.2 Makefile shortcuts (Unix / Linux / macOS)

The `Makefile` wraps the same operations for convenience:

```bash
make all        # build + check + toolkit-db-sqlite lane + e2e-local
make check      # fmt + clippy + test + security
make build      # whole-project example server release binary
make run        # default example server
make fmt        # formatting check (cargo fmt --all -- --check)
make dev-fmt    # auto-format (cargo fmt --all)
make clippy     # linting (clippy --workspace --all-targets --all-features)
make lint       # compile with -D warnings
make gears-lint # custom architectural lints (via cargo gears lint)
make deny       # cargo deny check
make kani       # Kani formal verification (optional)
make safety     # clippy + kani + lint + gears-lint
```

Common gear-scoped targets accept `GEAR=<name>` for focused local iteration. `make build GEAR=<name>` and `make test GEAR=<name>` select the gear crate plus its SDK crate. `make run GEAR=<name>` composes `cf-gears-example-server` with the gear feature and the static local development system gears. E2E instead uses `SUITE=<name>`: `make e2e-local SUITE=<name>` maps kebab-case suite names to `testing/e2e/suites/<name_with_underscores>` (a suite is often, but not always, a gear).

Gear runtime settings are YAML-driven under `gears:<gear_name>:` with a `config` section and optional gear-owned `database` section. Build-time Cargo features decide which gear code is present in the server binary; runtime YAML decides how those gears are configured.

## 7.3 CI Pipeline Summary

```
PR opened / updated
  ├── ci.yml
  │     ├── clippy        — clippy
  │     ├── repo-checks   — repo file checks (gear names, release config)
  │     ├── test          — unit tests (Ubuntu)
  │     ├── integration   — DB integration tests (Ubuntu)
  │     ├── test-fips     — FIPS verification / platform-specific FIPS test lanes
  │     ├── security      — cargo-deny
  │     ├── coverage      — cargo-llvm-cov → Codecov upload
  │     ├── lint          — custom architectural lints (cargo gears lint)
  │     └── cypilot       — artifact / specification validation
  │
  ├── fmt.yml             — dedicated cargo fmt validation for Rust changes
  ├── docs.yml            — Markdown link checking for docs changes
  ├── gts-validation.yml  — GTS identifier validation for docs / schema changes
  └── e2e.yml (PRs to main only)
        └── e2e           — full E2E suite (local mode), plus mini-chat, TR/AuthZ
                            and usage-collector E2E lanes

Additional quality workflows
  ├── codeql.yml          — security and quality code scanning
  ├── pr-governance.yml   — review-governance automation
  └── pr-reviewer-check.yml — reviewer assignment hygiene

Nightly (schedule)
  ├── macos-windows-tests.yml — unit tests on macOS and Windows (auto-creates issue on failure)
  ├── e2e.yml              — full E2E + specialized E2E lanes (auto-creates issue on failure)
  ├── clusterfuzzlite      — fuzz testing
  ├── codeql.yml           — scheduled code scanning
  ├── pr-governance.yml    — governance / follow-up automation
  └── pr-reviewer-check.yml — reviewer assignment checks
```

### 7.4 CI build cache (sccache)

CI jobs compile with sccache, set up by `.github/actions/sccache`. It uses the first of
these that works:

1. S3-compatible storage (AWS S3, Cloudflare R2, MinIO and others), if set up as below.
2. The GitHub Actions cache. This is the default.
3. No cache. The job builds with plain rustc. Cache problems never fail a job.

Only runs on `main` write to the cache. Other runs, PRs included, only read it, so PRs
don't fill the 10 GB GitHub Actions cache. Each job log has a `sccache backend:` line with
the backend and its mode. A `main` run that can't write (for example, with a wrong key)
also shows a warning.

#### S3 setup

Add these repository variables (Settings > Secrets and variables > Actions > Variables):

| Variable | Value |
|----------|-------|
| `S3_BUCKET` | bucket name |
| `S3_ENDPOINT` | empty for AWS S3, else the provider URL, like `https://<account>.r2.cloudflarestorage.com` for R2 |
| `S3_REGION` | AWS S3: the bucket region. R2: `auto`. Default: `us-east-1` |

Put the keys in two environments (Settings > Environments), each with the secrets
`S3_ACCESS_KEY_ID` and `S3_SECRET_ACCESS_KEY`:

| Environment | Key | Deployment branches |
|-------------|-----|---------------------|
| `ci-cache-write` | read and write | `main` only |
| `ci-cache-read` | read only, same bucket | any |

Jobs on `main` use `ci-cache-write`. Other runs use `ci-cache-read`. PR code can read its
job's key, so only the branch rule keeps the write key out of PRs.

Jobs use these environments only when `S3_BUCKET` is set. Create both environments
first, then set `S3_BUCKET`. A job that names a missing environment fails before it starts.

- Don't add reviewers or a wait timer to `ci-cache-read`. Every PR job would wait.
- Don't add the keys as repository or organization secrets. Then every job could read them.

Treat the write key like a deploy key. CI builds binaries from cached crates, so anyone
with this key can change what later CI runs execute. To rotate a key, put a new one with
the same access in the environment. Revoke the old one after a green run.

PRs from forks and Dependabot get no secrets, so they use the GitHub Actions cache. With S3
set up, `main` doesn't write to that cache, so these runs compile everything.

sccache never deletes objects. Add a bucket rule that deletes objects under
`gears-rust/sccache/` 30 days after they were written. Each crate then misses the cache
once a month and is written again.

On Linux with S3, the action turns off TCP timestamps. Some S3 servers close each
connection after one request. Behind the runners' shared NAT, about 3% of new connections
then stalled for about 20 s. Turning timestamps off halved Ubuntu test job time.

#### Keep the cache keys stable

sccache puts every `CARGO_*` environment variable and `RUSTFLAGS` in its Rust cache keys.
Adding or changing one makes the job's next run compile everything.

---

## 8. Custom Architecture Lints

Custom architecture lints are the main project-specific lint layer. Unlike generic
linting tools such as `clippy`, they enforce Gears-specific architectural and repository
rules: layer separation, DTO placement, REST conventions, security-sensitive patterns,
documentation constraints, and GTS-related validation.

The lints are shipped as part of the `cargo-gears` CLI (crate `cargo-gears-lints` in the
separate `cf-cli` repository) and are run via `cargo gears lint`.

Useful local commands include:

```bash
cargo gears lint          # run custom lints across the workspace
make gears-lint           # Makefile shortcut for the above
make gts-docs             # validate GTS identifiers in docs and schema files
```

The CI `lint` job applies the architecture lints to the workspace on every PR.

---

## 9. Static Analysis & Safety

| Tool | Purpose | Command | CI job |
|------|---------|---------|--------|
| **clippy** | Lint for correctness and performance | `make clippy` | `test` |
| **rustfmt** | Formatting enforcement | `make fmt` | `test` |
| **cargo gears lint** | Project-specific architectural lints (layer separation, DTO placement) | `cargo gears lint` | `lint` |
| **cargo-deny** | License compliance, advisories, banned crates | `make deny` | `security` |
| **cargo-kani** | Formal verification of unsafe code and invariants | `make kani` | `test` (via `safety`) |
| **cargo-geiger** | Audit of `unsafe` usage in dependencies | `make geiger` | manual |

---

## 10. Contributor Checklist

Before opening a PR, verify:

- [ ] `make check` passes (fmt + clippy + unit tests + security)
- [ ] New code has unit tests
- [ ] Integration tests added/updated if DB logic changed
- [ ] E2E tests added/updated if REST endpoints changed
- [ ] `make coverage-unit` shows no regression below the 80 % threshold
- [ ] Fuzz targets updated if parser/validator logic changed
- [ ] No `#[allow(unused)]` or `#[allow(dead_code)]` without justification

---

## 11. Related Documents

- [CONTRIBUTING.md](../CONTRIBUTING.md) — development workflow, commit conventions, PR process
- [testing/e2e/README.md](../testing/e2e/README.md) — E2E test guide, fixtures, advanced usage
- [fuzz/README.md](../tools/fuzz/README.md) — fuzz target reference, corpus management
- `cargo-gears-lints` (in the `cf-cli` repository) — architecture lint catalog and development notes
- [guidelines/SECURITY.md](../guidelines/SECURITY.md) — secure coding practices
- [docs/QUICKSTART_GUIDE.md](./QUICKSTART_GUIDE.md) — getting started with the project
