# Testing, Coverage and CI

Test strategy, coverage thresholds, test commands and the CI jobs that enforce them. How to write unit, E2E and DB-behaviour tests is in [12](12_unit_testing.md), [13](13_e2e_testing.md) and [14](14_db_behavior_testing.md).

Most checks run locally through `make` (or `tools/scripts/ci.py` on Windows) the same way CI runs them; CI adds cross-platform runs, scheduled suites and FIPS lanes. Arrive at review with a locally green build.

---

## 1. Test Pyramid

| Layer | Scope | Tooling | Feature gate | Runs in CI |
|-------|-------|---------|--------------|------------|
| **Unit** | Single function / struct / gear in isolation | `cargo test --workspace` | none (always compiled) | Every PR on Ubuntu (`ci.yml` — `test` job); macOS and Windows nightly (`macos-windows-tests.yml`) |
| **Integration** | Cross-crate or DB-backed logic (SQLite, Postgres, MySQL) | `cargo test -p <pkg> --features integration` | `#[cfg(feature = "integration")]` | Every PR (`ci.yml` — `integration` job, Ubuntu) |
| **E2E** | Full HTTP request → response through a running server | pytest + httpx against `cf-gears-example-server` | n/a (Python tests) | PRs to `main`, nightly schedule (`e2e.yml`) |
| **Fuzz** | Parser / validator robustness against arbitrary input | `cargo-fuzz` (libFuzzer) | nightly toolchain | PRs to `main`/`develop` that touch Rust code (`clusterfuzzlite.yml`) |
| **Static analysis** | Architectural rules, unsafe code, dependency licenses | clippy, `cargo gears lint`, cargo-deny; cargo-kani and cargo-geiger locally | varies | Every PR (`ci.yml` — `clippy`, `lint` jobs); `security` (cargo-deny) when dependencies or CI files change, or on manual dispatch; kani and geiger are not run in CI |

Additional testing categories such as performance (#4054), upgrade / migration (#4117), and long-haul (#4118) or soak testing are expected to be added over time, but they are not yet implemented or enforced as part of the current project test matrix.

```bash
make check                  # full local quality gate (see section 7.2)
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
make check                 # full local quality gate (see section 7.2)
make all                   # check + toolkit-db-sqlite lane + e2e-local + openapi

# CI gate: no database container built outside libs/test-containers (4.4)
cargo xtask check-test-container-pins

make build GEAR=file-parser      # one gear package plus SDK package
make test GEAR=file-parser       # gear + SDK packages and the crates that depend on them
make run GEAR=file-parser        # server with only this gear feature set
make e2e-local SUITE=file-parser # focused run: just this suite's tests
make e2e-local GEAR=credstore    # run every shared-server suite that exercises a gear
make coverage GEAR=file-parser   # unit + E2E coverage for one gear scope
```

---

## 2. Coverage

### 2.1 Thresholds

Two different numbers apply:

- **Local report: 80 % line coverage.** `tools/scripts/coverage.py` (`COVERAGE_THRESHOLD`, overridable with `--threshold`) flags every gear or library below it in the per-gear report. It is a report, not a gate.
- **CI floors: 85 % line coverage, for two gears only.** The `test-graph-storage` and `test-github-mirror-coverage` jobs in `ci.yml` run `cargo llvm-cov nextest ... --fail-under-lines 85` on their own packages and fail below it. Both jobs are path-filtered by the `changes` job: they run only when the PR touches their gear (`gears/graph-storage/**`; `gears/github-mirror/**`, `libs/**` or `gears-rust.mk` for github-mirror), dependency manifests (`**/Cargo.toml`, `Cargo.lock`, `deny.toml`) or CI files (`.github/workflows/**`, `.github/actions/**`), on manual dispatch, or when the `changes` job itself fails.
- **Codecov PR status checks** (`.codecov.yml`): `patch` requires 85 % coverage of the lines changed in the PR (threshold 0 %); `project` requires overall coverage not to drop below the base commit's (`target: auto`) by more than 1 %.

The workspace `coverage` job itself has no floor and does not fail on a percentage: it uploads the report to Codecov, which reports the two status checks above (see 2.3).

### 2.2 Coverage modes

| Mode | Command | What it measures |
|------|---------|------------------|
| **Unit** | `make coverage-unit` | All `#[test]` functions across the workspace (`cargo llvm-cov --workspace` with the example server's E2E features, not `--all-features`) |
| **E2E** | `make coverage-e2e-local` | Server code exercised by the pytest E2E suite via an instrumented binary |
| **Combined** | `make coverage` | Unit + E2E accumulated into a single report |

> **No "integration" coverage mode.** `make coverage-unit` does not enable the
> `integration` feature, so integration tests are not in the local coverage report.
> In CI, the `graph-storage` and `github-mirror` jobs upload coverage from their own
> PostgreSQL lanes (2.3).

### 2.3 CI coverage

The `coverage` job in `ci.yml` runs on every PR using `cargo-llvm-cov` (nightly) and
uploads an LCOV report to Codecov. It does not fail on a coverage percentage; Codecov
evaluates the uploaded reports and posts the `patch` and `project` status checks on the PR
(see 2.1). The
graph-storage and github-mirror jobs upload their own reports as well, since the workspace
job cannot run their PostgreSQL lanes.

### 2.4 Reports

Local coverage commands produce five report files under `coverage/<mode>/`:

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
- Tests live in a sibling file `<stem>_tests.rs`, included from the module under test with
  `#[cfg(test)] #[path = "<stem>_tests.rs"] mod <stem>_tests;`. Architecture lint DE1101
  (`cargo gears lint`, configured in [`dylint.toml`](../../dylint.toml)) rejects inline test
  blocks over 100 lines; see
  [ADR 0001](../adrs/tests/0001-tests-in-separate-files.md) and
  [`12_unit_testing.md` § Test File Organization](12_unit_testing.md#test-file-organization).
- Name tests after the behaviour they check, in snake_case and without a `test_` prefix:
  `{subject}_{scenario}_{outcome}`, e.g. `stream_events_returns_404_for_unknown_subscription`.
- Prefer deterministic assertions — avoid sleeping or time-dependent checks.

### 3.2 Running

```bash
make test                              # workspace unit tests via nextest
make test GEAR=file-parser             # gear + SDK packages and their reverse dependencies
make test GEAR=file-parser GEAR_FEATURES=integration
cargo test --workspace                 # all unit tests
cargo test -p cf-gears-oagw            # single package
cargo test -p cf-gears-toolkit-db -- cursor  # filtered by name
```

`GEAR=<name>` selects the packages under `GEAR_PKG_DIRS` (default `gears,libs`) whose names match `GEAR_NAME_REGEXP` (`<name>`, `<name>-sdk`, `cf-gears-<name>`, `cf-gears-<name>-sdk`, and a prefixed form such as `bss-ledger`). `make test` adds every workspace crate that depends on them (`cargo gears ls packages ... --include-rdeps` in the `test` target of `.gears-dna/gears.mk`), so a change to a shared crate also runs its consumers' tests. Override `GEAR_NAME_REGEXP` or `GEAR_PKG_DIRS` for non-standard package names, and use `GEAR_FEATURES` or `GEAR_TEST_ARGS` for extra Cargo flags.

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
| `cf-gears-event-broker` | `integration` | Gear database on SQLite, PostgreSQL and MySQL (testcontainers, or `EB_TEST_POSTGRES_URL` / `EB_TEST_MYSQL_URL`; `make test-integration LANE=eb-gear-db`) |

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

The `integration` job in `ci.yml` runs the toolkit-db SQLite, Postgres, PGQ (PostgreSQL 19)
and MySQL lanes, most gear lanes (`make test-integration LANE=<lane>`), and the macro UI tests
(`make test-macros`) on every PR (Ubuntu only).

### 4.4 Database container images

Every fixture that starts a database container goes through
[`libs/test-containers`](../../libs/test-containers/src/lib.rs) (crate
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
`kafka()`, `redis()`, `timescaledb()`, `mariadb()`, `clickhouse()`.

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
| `GEARS_TEST_KAFKA_TAG` | `KAFKA_TAG` |
| `GEARS_TEST_NETSHOOT_TAG` | `NETSHOOT_TAG` |

```bash
GEARS_TEST_PG_TAG=16-alpine cargo nextest run -p cf-gears-toolkit-db --features pg,integration
```

`GEARS_TEST_TIMESCALEDB_TAG` and `GEARS_TEST_CLICKHOUSE_TAG` are each read by *both* lanes: the
Rust plugin fixtures via `test_containers::timescaledb()` / `clickhouse()`, and the Python E2E
sidecars via `timescaledb_tag()` / `clickhouse_tag()` in
[`testing/e2e/lib/sidecars.py`](../../testing/e2e/lib/sidecars.py). A matrix run therefore keeps
migrations and E2E on the same image instead of silently testing two different ones.

Because those pins are spelled once per language, each is duplicated across a Rust constant
and a Python one; the same holds for the PostgreSQL pin (`POSTGRES_TAG`, `GEARS_TEST_PG_TAG`,
`postgres_tag()`), which self-managed PostgreSQL E2E lanes use. The unit tests
`e2e_sidecar_pins_the_same_timescaledb_image`, `e2e_sidecar_pins_the_same_postgres_image` and
`e2e_sidecar_pins_the_same_clickhouse_image` in `libs/test-containers` read `sidecars.py` and
fail the build if any copy drifts — so a version bump is still a single reviewed decision even
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
- Mark lightweight, fast tests with `@pytest.mark.smoke`; `make e2e-local-smoke` runs only them. CI
  does not select by the marker yet: PRs run the full suite (5.3).

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
- **Manual dispatch**: a `mode` input (`smoke` or `full`); both currently run the full suite (TODO in
  `e2e.yml` to switch smoke to `make e2e-local-smoke`).
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

Those containers are labelled per run so that concurrent sessions on one host do not reap
each other's database. The run-identity and reap-age settings (`CF_GEARS_E2E_RUN_ID`,
`CF_GEARS_E2E_REAP_MIN_AGE_SECS`) are documented in
[`testing/e2e/lib/sidecars.py`](../../testing/e2e/lib/sidecars.py).

Other quality-related GitHub Actions under `.github/workflows` complement the E2E
workflow:
- **`ci.yml`** runs the main quality gates: clippy, unit tests, integration tests, FIPS
  verification, coverage, security checks, architecture lints, and CFS validation.
- **`fmt.yml`** runs dedicated Rust formatting validation.
- **`docs.yml`** checks Markdown links for documentation changes.
- **`gts-validation.yml`** validates GTS identifiers in documentation and schema files.
- **`codeql.yml`** performs security and quality-oriented code scanning.

### 5.4 Writing E2E tests

The detailed E2E guide is [`testing/e2e/README.md`](../../testing/e2e/README.md). Use it as
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
- In CI, ClusterFuzzLite fuzzes pull requests to `main`/`develop` that touch Rust code.

### 6.2 Targets

Targets live in [`tools/fuzz/fuzz_targets/`](../../tools/fuzz/fuzz_targets/); `make fuzz-list` prints them.

| Target | Component |
|--------|-----------|
| `fuzz_odata_filter` | OData `$filter` parser |
| `fuzz_odata_cursor` | Cursor pagination decoder |
| `fuzz_odata_orderby` | OData `$orderby` parser |
| `fuzz_http_proxy_sanitize` | HTTP proxy request sanitization |

### 6.3 Running

```bash
make fuzz                                # smoke (30 s per target)
make fuzz-run FUZZ_TARGET=fuzz_odata_filter FUZZ_SECONDS=300  # longer run
make fuzz-list                           # list all available targets
make fuzz-build                          # build without running
make fuzz-clean                          # remove fuzzing artifacts
```

See [`fuzz/README.md`](../../tools/fuzz/README.md) for corpus management and crash reproduction.

---

## 7. CI / Development Commands

Gears uses a unified, cross-platform Python CI script (`tools/scripts/ci.py`).
This is the **primary entry point on Windows** where `make` is not available.
Requires Python 3.9+.

### 7.1 Cross-platform commands (`tools/scripts/ci.py`)

```bash
python tools/scripts/ci.py all            # check + toolkit-db SQLite integration + release build + e2e
python tools/scripts/ci.py check          # fmt, cfs-validate, docker-pins, clippy, test, gts-docs, audit, deny
python tools/scripts/ci.py fmt            # check formatting
python tools/scripts/ci.py fmt --fix      # auto-format code
python tools/scripts/ci.py clippy         # run linter
python tools/scripts/ci.py clippy --fix   # attempt to fix warnings
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
make all        # check + integration lane toolkit-db-sqlite + e2e-local + openapi
make check      # full local quality gate: formatting, clippy, architecture lints, security,
                # docs/link/GTS checks, Dockerfile pins, CFS validation, unit tests
                # (prerequisites of `check:` in gears-rust.mk)
make build      # whole-project example server release binary
make run        # default example server
make fmt        # formatting check (cargo fmt --all --check)
make dev-fmt    # auto-format (cargo fmt --all)
make clippy     # linting (clippy --workspace --all-targets --all-features)
make lint       # compile with -D warnings
make dylint     # custom architectural lints (cargo gears lint --dylint)
make deny       # cargo deny check
make kani       # Kani formal verification (optional, not run in CI)
make safety     # clippy + kani + lint + dylint
```

Common gear-scoped targets accept `GEAR=<name>` for focused local iteration. `make build GEAR=<name>` and `make test GEAR=<name>` select the gear crate plus its SDK crate. `make run GEAR=<name>` composes `cf-gears-example-server` with the gear feature and the static local development core gears. E2E instead uses `SUITE=<name>`: `make e2e-local SUITE=<name>` maps kebab-case suite names to `testing/e2e/suites/<name_with_underscores>` (a suite is often, but not always, a gear).

Gear runtime settings are YAML-driven under `gears:<gear_name>:` with a `config` section and optional gear-owned `database` section. Build-time Cargo features decide which gear code is present in the server binary; runtime YAML decides how those gears are configured.

## 7.3 CI Pipeline Summary

```
PR opened / updated
  ├── ci.yml
  │     ├── clippy        — make clippy, test-container pin check
  │     ├── repo-checks   — repo file checks (gear names, release config)
  │     ├── test          — unit tests (Ubuntu; test-suite.yml)
  │     ├── integration   — DB integration lanes and macro UI tests (Ubuntu)
  │     ├── test-fips     — FIPS verification / platform-specific FIPS test lanes
  │     ├── test-graph-storage, test-github-mirror-coverage — gear lanes with an 85 % coverage floor
  │     ├── test-onnx-embedding, test-magika — feature-specific lanes
  │     ├── security      — cargo-deny
  │     ├── coverage      — cargo-llvm-cov → Codecov upload
  │     ├── lint          — make dylint (cargo gears lint)
  │     └── cfs           — artifact / specification validation (cfs.yml)
  │
  ├── fmt.yml             — make fmt for Rust changes
  ├── docs.yml            — Markdown link checking for docs changes
  ├── gts-validation.yml  — GTS identifier validation for docs / schema changes
  ├── clusterfuzzlite.yml — fuzzing (PRs touching Rust code)
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
  ├── clippy-nightly.yml, shear-nightly.yml — make clippy-deep, make shear
  └── codeql.yml           — scheduled code scanning

Other schedules
  ├── pr-governance.yml    — governance / follow-up automation (08:00 and 13:00 UTC)
  └── pr-reviewer-check.yml — reviewer assignment checks (every 10 minutes)
```

### 7.4 CI build cache

Self-hosted runners, the shared sccache build cache (`.github/actions/ci-cache`) and its
read/write key policy are documented in [`.github/runners/README.md`](../../.github/runners/README.md).

## 8. Custom Architecture Lints

Custom architecture lints are the main project-specific lint layer. Unlike generic
linting tools such as `clippy`, they enforce Gears-specific architectural and repository
rules: layer separation, DTO placement, REST conventions, security-sensitive patterns,
documentation constraints, and GTS-related validation.

The lints are shipped as part of the `cargo-gears` CLI (crate `cargo-gears-lints` in the
separate `cf-cli` repository) and are run via `cargo gears lint`.

Useful local commands include:

```bash
cargo gears lint --dylint # run custom lints across the workspace
make dylint               # Makefile shortcut for the above
make gts-docs             # validate GTS identifiers in docs and schema files
```

The CI `lint` job runs `make dylint` on every PR. Lint settings (for example DE1101's
`max_inline_test_lines` and `excluded_paths`) are in [`dylint.toml`](../../dylint.toml).

---

## 9. Static Analysis & Safety

| Tool | Purpose | Command | CI job |
|------|---------|---------|--------|
| **clippy** | Lint for correctness and performance | `make clippy` | `clippy` |
| **rustfmt** | Formatting enforcement | `make fmt` | `fmt.yml` |
| **cargo gears lint** | Project-specific architectural lints (layer separation, DTO placement, test file placement) | `make dylint` | `lint` |
| **cargo-deny** | License compliance, advisories, banned crates | `make deny` | `security` |
| **cargo-kani** | Formal verification of unsafe code and invariants | `make kani` | manual |
| **cargo-geiger** | Audit of `unsafe` usage in dependencies | `make geiger` | manual |

---

## 10. Contributor Checklist

Before opening a PR, verify:

- [ ] `make check` passes (or, scoped to one gear, `make gear-ci GEAR=<gear>` and `make dylint`)
- [ ] New code has unit tests
- [ ] Integration tests added/updated if DB logic changed
- [ ] E2E tests added/updated if REST endpoints changed
- [ ] `make coverage-unit` shows no gear you touched newly below the 80 % report threshold
- [ ] Fuzz targets updated if parser/validator logic changed
- [ ] No `#[allow(unused)]` or `#[allow(dead_code)]` without justification

---

## 11. Related Documents

- [CONTRIBUTING.md](../../CONTRIBUTING.md) — development workflow, commit conventions, PR process
- [testing/e2e/README.md](../../testing/e2e/README.md) — E2E test guide, fixtures, advanced usage
- [fuzz/README.md](../../tools/fuzz/README.md) — fuzz target reference, corpus management
- `cargo-gears-lints` (in the `cf-cli` repository) — architecture lint catalog and development notes
- [guidelines/SECURITY.md](../../guidelines/SECURITY.md) — secure coding practices
- [docs/QUICKSTART_GUIDE.md](../QUICKSTART_GUIDE.md) — getting started with the project
