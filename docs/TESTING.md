# Testing Policy

This document defines the test strategy, coverage requirements, and CI enforcement for
Cyber Fabric.  It is the single source of truth for "what must be tested and how."

---

## 1. Test Pyramid

| Layer | Scope | Tooling | Feature gate | Runs in CI |
|-------|-------|---------|--------------|------------|
| **Unit** | Single function / struct / module in isolation | `cargo test --workspace` | none (always compiled) | Every PR (`ci.yml` — `test` job, all OS) |
| **Integration** | Cross-crate or DB-backed logic (SQLite, Postgres, MySQL) | `cargo test -p <pkg> --features integration` | `#[cfg(feature = "integration")]` | Every PR (`ci.yml` — `integration` job, Ubuntu) |
| **E2E** | Full HTTP request → response through a running server | pytest + httpx against `hyperspot-server` | n/a (Python tests) | PRs to `main`, nightly schedule (`e2e.yml`) |
| **Fuzz** | Parser / validator robustness against arbitrary input | `cargo-fuzz` (libFuzzer) | nightly toolchain | PRs + nightly (`clusterfuzzlite.yml`) |
| **Static analysis** | Architectural rules, unsafe code, dependency licenses | clippy, dylint, cargo-deny, cargo-kani, cargo-geiger | varies | Every PR (`ci.yml` — `test`, `security`, `dylint` jobs) |

```bash
make check                  # full quality gate (fmt + clippy + test + security)
```

### Quick-reference commands

```bash
make test                  # unit tests (workspace, all OS)
make test-sqlite           # integration — SQLite
make test-pg               # integration — PostgreSQL
make test-mysql            # integration — MySQL
make test-db               # all DB integration tests
make test-users-info-pg    # users-info module integration (Postgres)
make e2e-docker            # E2E — Docker environment
make e2e-docker-smoke      # E2E — Docker environment (smoke subset only)
make e2e-local             # E2E — local server (builds + starts automatically)
make e2e-local-smoke       # E2E — smoke subset only
make fuzz                  # fuzz — 30 s smoke per target
make check                 # full quality gate (fmt + clippy + test + security)
make all                   # full pipeline (build + check + test-sqlite + e2e-local)
```

---

## 2. Coverage

### 2.1 Threshold

The project-wide **line-coverage threshold is 80 %**.  The threshold is enforced in
`scripts/coverage.py` (`COVERAGE_THRESHOLD`) and printed as a warning when any module
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
| `coverage_report.txt` | Custom per-module/per-library table |

---

## 3. Unit Tests

### 3.1 Expectations

- Every new public function, method, or behaviour **must** have at least one unit test.
- Tests live next to the code they exercise, in a `#[cfg(test)] mod tests` block.
- Use descriptive names: `test_<function>_<scenario>_<expected>`.
- Prefer deterministic assertions — avoid sleeping or time-dependent checks.

### 3.2 Running

```bash
cargo test --workspace          # all unit tests
cargo test -p cf-oagw           # single package
cargo test -p cf-modkit-db -- cursor  # filtered by name
```

---

## 4. Integration Tests

Integration tests verify cross-crate or database-backed behaviour.  They are gated
behind the `integration` Cargo feature so that `cargo test --workspace` (without
`--all-features`) does **not** require a running database.

### 4.1 Feature gates

| Package | Features | Backend |
|---------|----------|---------|
| `cf-modkit-db` | `sqlite,integration` | SQLite (in-process) |
| `cf-modkit-db` | `pg,integration` | PostgreSQL (requires running instance) |
| `cf-modkit-db` | `mysql,integration` | MySQL (requires running instance) |
| `users-info` | `integration` | PostgreSQL |

### 4.2 Running

```bash
make test-sqlite           # quick, no external services needed
make test-pg               # requires Postgres
make test-mysql            # requires MySQL
make test-db               # all three
make test-users-info-pg    # users-info Postgres integration
```

### 4.3 CI

The `integration` job in `ci.yml` runs SQLite, Postgres, and MySQL integration tests
plus macro UI tests on every PR (Ubuntu only).

---

## 5. End-to-End (E2E) Tests

E2E tests exercise the full HTTP surface of `hyperspot-server` using Python (pytest +
httpx).

### 5.1 Expectations

- Every user-facing REST endpoint **should** have at least one E2E smoke test.
- Critical flows (CRUD, auth, error responses) **must** have full E2E coverage.
- Mark lightweight, fast tests with `@pytest.mark.smoke` — these run on every PR.

### 5.2 Modes

| Mode | Backend | Use case |
|------|---------|----------|
| **Local** (`make e2e-local`) | Builds a release binary, starts it locally | Development, CI |
| **Docker** (`make e2e-docker`) | Builds a Docker image, runs in container | Isolation, reproducibility |

### 5.3 CI

The `e2e.yml` workflow runs:
- **On PRs to `main`**: full E2E suite (local mode).
- **Nightly (02:00 UTC)**: full E2E suite.  Failures auto-create a GitHub issue
  assigned to the last commit author.
- **Manual dispatch**: smoke or full, selectable.

### 5.4 Writing E2E tests

See [`testing/e2e/README.md`](../testing/e2e/README.md) for fixtures, examples, and
environment variables.

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
See [`fuzz/README.md`](../fuzz/README.md) for corpus management and crash reproduction.

---

## 7. Static Analysis & Safety

| Tool | Purpose | Command | CI job |
|------|---------|---------|--------|
| **clippy** | Lint for correctness and performance | `make clippy` | `test` |
| **rustfmt** | Formatting enforcement | `make fmt` | `test` |
| **dylint** | Project-specific architectural lints (layer separation, DTO placement) | `make dylint` | `dylint` |
| **cargo-deny** | License compliance, advisories, banned crates | `make deny` | `security` |
| **cargo-kani** | Formal verification of unsafe code and invariants | `make kani` | `test` (via `safety`) |
| **cargo-geiger** | Audit of `unsafe` usage in dependencies | `make geiger` | manual |

---

## 8. CI / Development Commands

Cyber Fabric uses a unified, cross-platform Python CI script (`scripts/ci.py`).
This is the **primary entry point on Windows** where `make` is not available.
Requires Python 3.9+.

### 8.1 Cross-platform commands (`scripts/ci.py`)

```bash
python scripts/ci.py all            # build + full check suite + e2e
python scripts/ci.py check          # fmt, clippy, test, audit, deny
python scripts/ci.py fmt            # check formatting
python scripts/ci.py fmt --fix      # auto-format code
python scripts/ci.py clippy         # run linter
python scripts/ci.py clippy --fix   # attempt to fix warnings
python scripts/ci.py dylint         # custom project compliance lints
python scripts/ci.py audit          # security audit
python scripts/ci.py deny           # license & dependency checks
python scripts/ci.py e2e-local      # build server + run E2E tests locally
python scripts/ci.py e2e-local --smoke  # E2E smoke subset only
python scripts/ci.py e2e-docker     # E2E in Docker
python scripts/ci.py fuzz-build     # build fuzz targets
python scripts/ci.py fuzz --seconds 60  # fuzz smoke run
python scripts/ci.py fuzz-run fuzz_odata_filter --seconds 300  # single target
```

### 8.2 Makefile shortcuts (Unix / Linux / macOS)

The `Makefile` wraps the same operations for convenience:

```bash
make all        # build + check + test-sqlite + e2e-local
make check      # fmt + clippy + test + security
make fmt        # formatting check (cargo fmt --all -- --check)
make dev-fmt    # auto-format (cargo fmt --all)
make clippy     # linting (clippy --workspace --all-targets --all-features)
make lint       # compile with -D warnings
make dylint     # custom architectural lints
make deny       # cargo deny check
make kani       # Kani formal verification (optional)
make safety     # clippy + kani + lint + dylint
```

---

## 9. CI Pipeline Summary

```
PR opened / updated
  ├── ci.yml
  │     ├── test          — fmt + clippy + unit tests (Ubuntu, macOS, Windows)
  │     ├── integration   — DB integration tests (Ubuntu)
  │     ├── security      — cargo-deny
  │     ├── coverage      — cargo-llvm-cov → Codecov upload
  │     └── dylint        — custom architectural lints
  │
  └── e2e.yml (PRs to main only)
        └── e2e           — full E2E suite (local mode)

Nightly (schedule)
  ├── e2e.yml             — full E2E (auto-creates issue on failure)
  └── clusterfuzzlite     — fuzz testing
```

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
- [fuzz/README.md](../fuzz/README.md) — fuzz target reference, corpus management
- [guidelines/SECURITY.md](../guidelines/SECURITY.md) — secure coding practices
- [docs/QUICKSTART_GUIDE.md](./QUICKSTART_GUIDE.md) — getting started with the project
