# -------- Tests --------

.PHONY: test test-no-macros test-macros test-sqlite test-pg test-pgq test-mysql test-db test-users-info-pg test-usage-collector-pg test-usage-collector-ch test-types-registry-db test-cluster-pg test-cluster-redis test-cluster-k8s coverage-cluster-k8s test-rg-pg test-settings-service-pg test-pricing-pg test-coord-pg test-products-pg test-ledger-pg test-fixtures-narrow test-fips

# Run workspace tests excluding trybuild suites (run separately).
test-no-macros: cf-require-nextest
	$(call print_target_banner)
	$(call nextest_run,test-no-macros,--workspace --exclude cf-gears-toolkit-macros-tests --exclude cf-gears-toolkit-db-macros)

test-macros: cf-require-nextest
	$(call print_target_banner)
	$(call nextest_run,macros-db,-p cf-gears-toolkit-db-macros)
	$(call nextest_run,macros-toolkit,-p cf-gears-toolkit-macros-tests)

## Run SQLite integration tests
test-sqlite: cf-require-nextest
	$(call print_target_banner)
	$(call nextest_run,toolkit-db-sqlite,-p cf-gears-toolkit-db --features sqlite$(comma)integration)
	cargo build -p cf-gears-toolkit-db --examples --features sqlite

## Run PostgreSQL integration tests
test-pg: cf-require-nextest
	$(call print_target_banner)
	$(call nextest_run,toolkit-db-pg,-p cf-gears-toolkit-db --features pg$(comma)integration)

## Run the SQL/PGQ lane: toolkit-db's unit suites under the `pgq` feature (the
## secure graph builder and its tests compile only there), the PostgreSQL 19
## integration suite (tests/pg, Docker required; a testcontainers PG19) and the
## trybuild guards. `pgq` implies `pg`, so without the filter this would re-run
## every PG18 Docker suite `make test-pg` just ran; the filter keeps only what
## this lane adds. The PG19 suite skips itself while the pre-GA image is
## unavailable; CI sets GEARS_TEST_PG_GRAPH_REQUIRED=1 so there the skip is a
## failure — do the same locally once the image is expected to be present.
test-pgq: cf-require-nextest
	$(call print_target_banner)
	$(call nextest_run,toolkit-db-pgq,-p cf-gears-toolkit-db --features pgq$(comma)integration \
		-E 'kind(lib) | binary(mod) | binary(ui)')

## Run the graph-storage gear's database-free suites: unit tests, the
## in-memory conformance lane, the domain-service and REST lanes. The
## PostgreSQL lanes skip here; `test-graph-storage-pg` runs them.
test-graph-storage: cf-require-nextest
	$(call print_target_banner)
	cargo nextest run -p cf-gears-graph-storage -p cf-gears-graph-storage-sdk

## Run the graph-storage gear's PostgreSQL 19 lane: the same conformance
## suite against the built-in store, plus the SQL/PGQ cases that only a real
## server can answer. It needs an image carrying PostgreSQL 19 **and**
## pgvector — `test_containers::postgres_graph()` pins a stock 19beta
## alpine, which has only the former — so point GEARS_TEST_PG_GRAPH_IMAGE at
## one until the platform pin carries both. GEARS_TEST_PG_GRAPH_REQUIRED=1
## turns "no such image, skipping" into a failure, so CI cannot go green by
## running nothing; that is the default here, because a target whose whole
## purpose is the database has no business passing without one.
## Every case in this lane gets its own PostgreSQL instance (two of them are
## operator surgery on server-wide state), and more than a handful at once is
## more than Docker and PostgreSQL will take: the pools time out and a
## different case fails on each run. nextest runs each case in its own
## process, so the bound has to be its own — GRAPH_PG_TEST_THREADS raises it
## on a host with the memory for it. Two is what held on an 8-core, 23 GiB
## developer machine; four failed about half its runs there.
GRAPH_PG_TEST_THREADS ?= 2
test-graph-storage-pg: cf-require-nextest
	$(call print_target_banner)
	GEARS_TEST_PG_GRAPH_REQUIRED=1 cargo nextest run -p cf-gears-graph-storage \
		--test pg_conformance --test-threads=$(GRAPH_PG_TEST_THREADS)

## Run MySQL integration tests
test-mysql: cf-require-nextest
	$(call print_target_banner)
	$(call nextest_run,toolkit-db-mysql,-p cf-gears-toolkit-db --features mysql$(comma)integration)

# Run all database integration tests
test-db: test-sqlite test-pg test-pgq test-mysql
	$(call print_target_banner)

## Run users-info gear integration tests
test-users-info-pg: cf-require-nextest
	$(call print_target_banner)
	$(call nextest_run,users-info-pg,-p users-info --features integration)

## Run TimescaleDB usage-collector plugin integration tests (Docker required;
## the suite spins up its own timescale/timescaledb container via testcontainers)
test-usage-collector-pg: cf-require-nextest
	$(call print_target_banner)
	$(call nextest_run,usage-collector-pg,-p cf-gears-timescaledb-usage-collector-plugin --features postgres)

## Run ClickHouse usage-collector plugin integration tests (Docker required;
## every test gets its own database on one shared, named clickhouse container,
## `uc-clickhouse-test-harness-<tag>`, which is reused across runs — remove it
## with `docker rm -fv` to start clean).
## `--run-ignored all` because the suite is double-gated: the `clickhouse`
## feature compiles the test files, and every Docker-backed test inside them is
## `#[ignore]`d. CH_REQUIRE_DOCKER=1 turns an unreachable Docker into a panic —
## without it bring_up_or_skip() reports `ok` on a suite that ran nothing.
test-usage-collector-ch: cf-require-nextest
	$(call print_target_banner)
	$(call nextest_run,usage-collector-ch,-p cf-gears-clickhouse-usage-collector-plugin \
		--features clickhouse --run-ignored all --no-fail-fast,CH_REQUIRE_DOCKER=1)

## Run types-registry PostgreSQL + MySQL integration tests (Docker required;
## each test spins up its own postgres or mysql container via testcontainers).
test-types-registry-db: cf-require-nextest
	$(call print_target_banner)
	$(call nextest_run,types-registry-db,-p cf-gears-types-registry --features integration \
	  -E 'binary(/_backends_test$$/)')

## Run the Postgres cluster plugin's conformance (Layer 2) and Layer 3
## integration suites (Docker required;
## each spins up its own postgres container per test via testcontainers —
## see gears/system/cluster/plugins/postgres-cluster-plugin/docs/TESTING.md §7).
##
## `--retries 1` because the container/pool *setup* in tests/common/mod.rs is
## load-sensitive on a busy host: it already retries `Postgres::start()` itself,
## and exhausting that budget surfaces as a failure in whichever test drew the
## short straw. A genuine logic regression fails both attempts, so this absorbs
## Docker churn without masking one.
test-cluster-pg: cf-require-nextest
	$(call print_target_banner)
	$(call nextest_run,cluster-pg,-p cf-postgres-cluster-plugin --features integration --retries 1)

## Kubernetes cluster plugin: L2 (conformance) + L3 (integration) against a real
## k3s API server (docs/TESTING.md 4, 7). Docker required, and k3s needs
## **privileged** mode -- the one CI capability beyond a Docker daemon.
##
## Uses `cargo test`, not `nextest` (unlike test-cluster-pg): k3s takes ~20s to
## become ready, so the fixture starts ONE k3s container (a `static` OnceCell,
## see tests/integration/common) and isolates every scenario by namespace. All
## scenarios live in a SINGLE test binary (`tests/integration/main.rs`) so that
## one static -- hence one container -- serves the whole suite; nextest's
## process-per-test, or the former file-per-binary layout, would each start a
## fresh k3s per process. `--test-threads` caps how many scenarios hit the shared
## cluster at once so they do not overload it.
##
## The container lives in a `static` (never `Drop`ped) and **testcontainers 0.27
## ships no ryuk reaper**, so it keeps running after the test process exits: the
## `trap` cleans up that single leftover, and a leading `k3s_cleanup` (plus the
## fixture's own `reap_stale_fixture_containers` on startup) clears any from a
## prior run. Cleanup filters on the fixture's own label so it never touches an
## unrelated k3s container. One retry guards a flaky boot.
test-cluster-k8s: cf-require-nextest
	$(call print_target_banner)
	@set -e; \
	k3s_cleanup() { docker ps -aq --filter label=org.cf-gears.test-fixture=cf-k8s-cluster-plugin | xargs -r docker rm -f >/dev/null 2>&1 || true; }; \
	trap k3s_cleanup EXIT; \
	k3s_cleanup; \
	echo "=== cf-gears-cluster: k8s provider registration (lib, no k3s) ==="; \
	cargo test -p cf-gears-cluster --features k8s --lib provider_registry_resolves_k8s_for_every_primitive; \
	echo "=== k8s-cluster-plugin: integration (single shared k3s) ==="; \
	cargo test -p cf-k8s-cluster-plugin --features integration --test integration -- --test-threads=4 \
		|| { echo "=== integration failed; retrying once (flake guard, cf. test-cluster-pg's --retries 1) ==="; k3s_cleanup; \
			cargo test -p cf-k8s-cluster-plugin --features integration --test integration -- --test-threads=4; } \
		|| exit 1

## Accumulate the k8s plugin's L2/L3 coverage into the CURRENT cargo-llvm-cov
## session -- the coverage sibling of `test-cluster-k8s`: the same single-binary
## `cargo test` becomes `cargo llvm-cov test --no-report`. Deliberately does NOT
## `clean` or `report`, and deliberately has no `install-tools` prereq: the
## caller brackets it (a workspace pass before, `cargo llvm-cov report` after) so
## it composes into one lcov, and supplies cargo-llvm-cov + CARGO_LLVM_COV_TARGET_DIR
## itself. The single-container k3s handling is identical to test-cluster-k8s --
## see that target's comment for the why. Called by the coverage job in
## .github/workflows/ci.yml.
##
## Standalone (local): a report needs a bracketing clean + report --
##   cargo llvm-cov clean --workspace && make coverage-cluster-k8s && cargo llvm-cov report --summary-only
coverage-cluster-k8s:
	$(call print_target_banner)
	@set -e; \
	k3s_cleanup() { docker ps -aq --filter label=org.cf-gears.test-fixture=cf-k8s-cluster-plugin | xargs -r docker rm -f >/dev/null 2>&1 || true; }; \
	trap k3s_cleanup EXIT; \
	k3s_cleanup; \
	echo "=== coverage: k8s-cluster-plugin: integration (single shared k3s) ==="; \
	cargo llvm-cov test --no-report -p cf-k8s-cluster-plugin --features integration --test integration -- --test-threads=4 \
		|| { echo "=== integration failed; retrying once (flake guard) ==="; k3s_cleanup; \
			cargo llvm-cov test --no-report -p cf-k8s-cluster-plugin --features integration --test integration -- --test-threads=4; } \
		|| exit 1

## Run resource-group gear PostgreSQL smoke tests (Docker required; spins up
## its own postgres container via testcontainers -- see
## gears/system/resource-group/resource-group/tests/pg_smoke_test.rs)
test-rg-pg: cf-require-nextest
	$(call nextest_run,rg-pg,-p cf-gears-resource-group --features integration)

## Run the settings-service gear's PostgreSQL migration suite (Docker required;
## spins up its own postgres container via testcontainers -- see
## gears/settings-service/settings-service/tests/pg_migrations_test.rs). The
## suites beside the gear's migrations run on SQLite, where a statement
## PostgreSQL refuses can still pass; this lane runs the chain a stand's startup
## runs, on the backend it runs it on.
test-settings-service-pg: cf-require-nextest
	$(call nextest_run,settings-service-pg,-p cf-gears-settings-service --features integration --test pg_migrations_test)

## Run bss-pricing's Postgres tier (Docker required; each suite spins up its own
## postgres container via testcontainers).
##
## Gated behind `#[ignore]` rather than a feature, which is why this needs
## `--run-ignored ignored-only` where its siblings above pass `--features`. Every
## test in `tests/postgres_*.rs` carries the attribute — 376 of them on
## 2026-08-18, stated as the invariant rather than as a number because the count
## has already drifted once here — and until 2026-08-11
## nothing in this Makefile or in `.github/` passed `--run-ignored` at all — so
## the tier compiled on every run and executed on none.
##
## What that cost is specific, not theoretical. Every proof this crate owns about
## two racing writers, a lock held by a crashed pass, `FOR UPDATE` semantics,
## READ COMMITTED re-evaluation and the PL/pgSQL half of every dual-spelled
## trigger lives here: the five test files using `tokio::spawn`/`join!` are all
## `postgres_*`. Each suite's module doc states that its property is unprovable
## on SQLite, and two note that the SQLite twin passes either way —
## `sqlite_bulk_commit.rs` is a test that cannot fail on the property it is named
## for. `sqlite_append_only.rs:110` names the cost from experience: "D-236 is the
## record of what that costs — a premise living on one tier only means a run
## without Docker reports a clean change through a guard that stopped guarding."
##
## `--no-fail-fast`, and it is not a preference. On 2026-08-18 this tier was red
## with nine failures: one stale probe at test 68 of 376 and, behind it, six
## proving that price supersession was broken on the engine that ships. nextest
## stops at the first failure by default, so the PR page reported the stale probe
## and nothing else — the cheap failure hid the expensive one, and a reader fixing
## the one line would have met the other six only on the next run. A tier whose
## whole purpose is to be the one place a Postgres-only defect surfaces must
## report every failure it found, not the first.
test-pricing-pg: cf-require-nextest
	$(call nextest_run,pricing-pg,-p cf-gears-bss-pricing --run-ignored ignored-only -E 'binary(/^postgres_/)' --no-fail-fast)

## Run coord's Postgres tier (Docker required; testcontainers).
##
## `coord`'s `m0001_…` builds two independent SQL literals — a schema-qualified
## PG `CREATE TABLE` and a bare SQLite one. The in-crate tests connect
## `sqlite::memory:` and reach the SQLite literal only, so the `IF NOT EXISTS`
## that stops the two-gear boot crash (`bss-pricing` starting beside a
## long-running `bss-ledger`) was unexercised on the dialect the crash happened
## on. `Migration::in_schema("bss")` — the constructor both consumers actually
## pass — is likewise unreachable from SQLite, which has one namespace.
##
## Same `--run-ignored ignored-only` shape as `test-pricing-pg` above, and the
## same reason: the gate is `#[ignore]` rather than a feature.
test-coord-pg: cf-require-nextest
	$(call nextest_run,coord-pg,-p cf-gears-bss-coord --run-ignored ignored-only -E 'binary(/^postgres_/)')

## Run bss-products' Postgres tier (Docker required; testcontainers).
##
## Same `--run-ignored ignored-only` shape and the same reason as the two above:
## the gate is an ignored-test attribute rather than a feature. This gear's tier was added on
## 2026-08-30 and, until this target existed, repeated the donor's own 2026-08-11
## failure exactly — 23 tests across six `postgres_*` binaries that compiled on
## every run and executed on none, while the DoD they discharge
## (`cpt-cf-bss-products-dod-concurrency`) had already been ticked on their
## strength. Two review lenses found it independently.
##
## What lives here and nowhere else: every proof about two writers racing for one
## reservation index, one head row or one idempotency key, and the **Postgres
## half of every trigger in the gear**. Both engine arms are written from the
## same design clauses but in different shapes — SQLite gets one trigger per
## clause, Postgres one `plpgsql` function with sequential `IF` blocks — so the
## in-crate SQLite suite cannot fail for anything the Postgres arm gets wrong.
## That asymmetry is what let a `json`-column publish defect ship in Phase 6.
##
## `--no-fail-fast` for the donor's reason: a tier whose whole purpose is to be
## the one place a Postgres-only defect surfaces must report every failure it
## found, not the first.
test-products-pg: cf-require-nextest
	$(call nextest_run,products-pg,-p cf-gears-bss-products --run-ignored ignored-only -E 'binary(/^postgres_/)' --no-fail-fast)

## Run bss-ledger's Postgres tier (Docker required; testcontainers).
##
## Same `--run-ignored ignored-only` shape as pricing, coord and products: the ledger tests that
## need a database are `#[ignore]` with a Docker reason — the `postgres_*` and
## `rest_*` binaries and the lib's `pg` modules — so there is no binary filter.
## One ignored test is not a Docker test: `grain_fold_timing` is a manual timing helper that
## prints a cost. It is excluded by name, so do not add it back.
## `--no-fail-fast` for the reason the pricing target gives.
test-ledger-pg: cf-require-nextest
	$(call nextest_run,ledger-pg,-p cf-gears-bss-ledger --run-ignored ignored-only -E 'not test(grain_fold_timing)' --no-fail-fast)

## Run the Redis cluster plugin's conformance (Layer 2) and Layer 3 integration
## suites (Docker required; each spins up its own redis container via
## testcontainers — see
## gears/system/cluster/plugins/redis-cluster-plugin/docs/TESTING.md §7).
##
## Several container shapes run per PR, all single-node: a stock server (declares
## EventuallyConsistent), an `--appendonly yes --appendfsync always` node (declares
## Linearizable, which the leader-election conformance suite requires), and the
## variants the declaration-and-environment scenarios need — no keyspace
## notifications, an unsafe `maxmemory-policy`, a tiny `maxmemory` that forces real
## eviction, `appendfsync everysec`, a `CONFIG`-denied ACL user, and one Redis 6.
## The Sentinel and 3-node Cluster fixtures run here too — `RD-SPEC-002` on
## Sentinel, `RD-SPEC-008/009/010` and `RD-LOCK-014` on the Cluster. A quorum or a
## slot assignment costs tens of seconds, but nextest overlaps that startup with
## the rest of the suite, so it is not wall clock the run as a whole pays.
##
## `--retries 1` for the same reason as test-cluster-pg: container setup is
## load-sensitive on a busy host, and a genuine logic regression fails both
## attempts, so this absorbs Docker churn without masking one.
test-cluster-redis: cf-require-nextest
	$(call nextest_run,cluster-redis,-p cf-redis-cluster-plugin --features integration --retries 1)

## Run FIPS-mode integration tests (requires Go for aws-lc-fips-sys).
## Covers:
##   - cf-gears-toolkit         : bootstrap + init_crypto_provider dispatch
##   - cf-gears-toolkit-http    : TLS client fail-closed path (NoCryptoProvider,
##                                apply_fips_hardening, builder/client FIPS-feature
##                                test surface). See issue #1935.
##   - cf-gears-oagw           : startup validation rejects allow_http_upstream
##                                under --features fips (PR #1985).
##   - cf-gears-clickhouse-usage-collector-plugin
##                              : the ClickHouse HTTP transport is built from the
##                                installed CryptoProvider rather than the
##                                `clickhouse` crate's hardcoded non-FIPS
##                                aws-lc-rs (see infra::storage::pool).
##
## Per-package `pkg/feat` syntax is required because `bootstrap` exists only
## on `cf-gears-toolkit` and the crates have independent FIPS feature
## spaces (toolkit doesn't depend on toolkit-http; oagw forwards toolkit-http/fips
## via its own `fips` feature). Single invocation so the shared FIPS dep graph
## compiles once.
test-fips: cf-require-nextest
	$(call print_target_banner)
	cargo nextest run -p cf-gears-toolkit -p cf-gears-toolkit-http -p cf-gears-oagw -p cf-gears-clickhouse-usage-collector-plugin \
		--features cf-gears-toolkit/bootstrap,cf-gears-toolkit/fips,cf-gears-toolkit-http/fips,cf-gears-oagw/fips

## Cross-compile gate for the Windows+FIPS path (Windows handshake
## verification is the manual runbook in cf-gears-fips-probe/README.md). Catches
## type / cfg / feature-graph regressions for `rustls-cng-crypto` and the
## dep-graph (`rustls-cng-crypto` present, `aws-lc-fips-sys` absent).
##
## Uses `cargo-xwin` because the workspace pulls transitive C deps
## (`aws-lc-sys`, `libz-ng-sys`) whose build.rs scripts need a Windows
## sysroot (`windows.h`, MSVC headers). `cargo-xwin` downloads the MSVC
## redistributable and CRT/Windows headers automatically — works on Linux,
## macOS, and Windows hosts without a pre-installed Visual Studio.
##
## Prerequisites:
##   cargo install cargo-xwin    # MSVC sysroot bridge
##   # And a cmake build driver for the cmake-based transitive C deps:
##   # macOS:  brew install ninja
##   # Linux:  apt-get install ninja-build  (or: dnf install ninja-build)
##
## Pair this with the dep-graph regression check, which needs no toolchain
## at all and runs on any host:
##   cargo tree --target x86_64-pc-windows-msvc -p cf-gears-example-server \
##       --features fips -e features | grep aws-lc-fips    # must be empty
.PHONY: check-windows-fips
check-windows-fips:
	$(call print_target_banner)
	$(call check_tool,cargo-xwin)
	$(call check_tool,ninja)
	rustup target add x86_64-pc-windows-msvc
	cargo xwin check --target x86_64-pc-windows-msvc -p cf-gears-example-server --features fips

# -------- Benchmarks --------

.PHONY: bench-pg bench-pg-profiler bench-mysql bench-mariadb bench-sqlite bench-db \
       bench-pg-longhaul bench-mysql-longhaul bench-mariadb-longhaul bench-sqlite-longhaul bench-db-longhaul

## Run outbox throughput benchmarks against PostgreSQL
bench-pg:
	$(call print_target_banner)
	cargo bench -p cf-gears-toolkit-db --features pg --bench outbox_throughput -- postgres

## Run outbox throughput benchmarks against MySQL
bench-mysql:
	$(call print_target_banner)
	cargo bench -p cf-gears-toolkit-db --features mysql --bench outbox_throughput -- mysql

## Run outbox throughput benchmarks against MariaDB
bench-mariadb:
	$(call print_target_banner)
	cargo bench -p cf-gears-toolkit-db --features mysql --bench outbox_throughput -- mariadb

# Run outbox throughput benchmarks against SQLite
bench-sqlite:
	$(call print_target_banner)
	cargo bench -p cf-gears-toolkit-db --features sqlite --bench outbox_throughput -- sqlite

## Run outbox throughput benchmarks against all database engines
bench-db: bench-pg bench-mysql bench-mariadb bench-sqlite
	$(call print_target_banner)

## Run long-haul (1M+10M) outbox benchmarks against PostgreSQL
bench-pg-longhaul:
	$(call print_target_banner)
	cargo bench -p cf-gears-toolkit-db --features pg --bench outbox_throughput -- postgres_longhaul

## Run long-haul (1M+10M) outbox benchmarks against MySQL
bench-mysql-longhaul:
	$(call print_target_banner)
	cargo bench -p cf-gears-toolkit-db --features mysql --bench outbox_throughput -- mysql_longhaul

## Run long-haul (1M+10M) outbox benchmarks against MariaDB
bench-mariadb-longhaul:
	$(call print_target_banner)
	cargo bench -p cf-gears-toolkit-db --features mysql --bench outbox_throughput -- mariadb_longhaul

## Run long-haul (100K 1P) outbox benchmarks against SQLite
bench-sqlite-longhaul:
	$(call print_target_banner)
	cargo bench -p cf-gears-toolkit-db --features sqlite --bench outbox_throughput -- sqlite_longhaul

## Run long-haul outbox benchmarks against all database engines
bench-db-longhaul: bench-pg-longhaul bench-mysql-longhaul bench-mariadb-longhaul bench-sqlite-longhaul
	$(call print_target_banner)
