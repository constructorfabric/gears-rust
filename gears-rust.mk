# gears-rust.mk: settings for gears.mk and the make targets of gears-rust.
# The driver Makefile includes this file first, then .gears-dna/gears.mk (shared rules).
# See .gears-dna/docs/GEARS_MK.md.

# -------- Settings for gears.mk --------

# Fast two-pass clippy used in PR CI (target: <5 min with sccache).
# Pass 1 covers the 80+ "leaf" crates (gears, plugins, SDKs): they use `dep:`
# guards only, so --all-features is sufficient — no --each-feature needed.
# Pass 2 runs cargo-hack --each-feature on the three crates with real
# #[cfg(feature = "...")] guards (mutually-exclusive DB backends, otel, fips, db).
# These account for >95% of all cfg-gated lines in the repo. See GH issue #1574.
CLIPPY_HACK_CRATES := -p cf-gears-toolkit -p cf-gears-toolkit-db -p cf-gears-toolkit-http
# `_any-backend` is toolkit-db's internal "some backend is on" marker (see its
# [features] block); enabling it *alone* asserts a driver exists while none does,
# which the outbox benchmarks reject with a compile_error!. Not a configuration
# anyone can ship, so it is not worth a lint pass.
CLIPPY_HACK_EXCLUDE := --exclude-features _any-backend

FUZZ_DIR := tools/fuzz

# Targets `make ci` runs locally (requires Docker for the integration lanes).
# Its test-integration lanes are set by `ci: LANE = ...` under Main targets below.
GEARS_CI_TARGETS := fmt clippy test-no-macros test-macros test-db deny test-integration \
	lychee gts-docs dylint

# nextest_run adds source locations to the JUnit reports with this script.
GEARS_JUNIT_ENRICH := tools/scripts/junit_enrich.py

# Keep this order: Make expands := assignments and ifeq conditions when it reads them.

# -------- Settings --------

# cargo-shear version installed by `make setup`. Pinned because an unused-dep
# verdict that disagrees with CI is worse than no local check at all.
# Keep in sync with the `Install cargo-shear` step in shear-nightly.yml.
SHEAR_VERSION ?= 1.13.1

CI := 1

# Python helper scripts must run inside a repository-managed virtual environment.
# PYTHON_BOOTSTRAP (gears.mk) is used only to create that environment;
# PYTHON is the venv interpreter.
PY_ENV_DIR ?= .venv
ifeq ($(OS),Windows_NT)
PYTHON ?= $(PY_ENV_DIR)/Scripts/python
else
PYTHON ?= $(PY_ENV_DIR)/bin/python
endif
PY_ENV_STAMP := $(PY_ENV_DIR)/.requirements-stamp
PY_REQUIREMENTS := testing/requirements.txt testing/e2e/requirements.txt

OPENAPI_PORT ?= 8087
OPENAPI_URL ?= http://127.0.0.1:$(OPENAPI_PORT)/openapi.json
OPENAPI_OUT ?= docs/api/api.json
OPENAPI_CONFIG ?= config/e2e-local.yaml

EMPTY :=
SPACE := $(EMPTY) $(EMPTY)
COMMA := ,

EXAMPLE_SERVER_BIN ?= cf-gears-example-server
EXAMPLE_SERVER_DEBUG_BINARY ?= target/debug/$(EXAMPLE_SERVER_BIN)
EXAMPLE_SERVER_MANIFEST ?= apps/cf-gears-example-server/Cargo.toml
EXAMPLE_SERVER_FEATURE_EXCLUDES ?= default fips k8s otel oop-example timescaledb-usage-collector clickhouse-usage-collector magika graph-storage quota-enforcement
EXAMPLE_SERVER_ALL_FEATURES := $(strip $(shell cargo gears ls features --manifest $(EXAMPLE_SERVER_MANIFEST) 2>/dev/null))
EXAMPLE_SERVER_FEATURES ?= $(subst $(SPACE),$(COMMA),$(filter-out $(EXAMPLE_SERVER_FEATURE_EXCLUDES),$(EXAMPLE_SERVER_ALL_FEATURES)))
EXAMPLE_SERVER_FEATURE_ARGS ?= $(if $(EXAMPLE_SERVER_FEATURES),--features $(EXAMPLE_SERVER_FEATURES),)
E2E_FEATURES_FILE ?= config/e2e-features.txt
E2E_SERVER_FEATURES ?= $(strip $(shell cat $(E2E_FEATURES_FILE) 2>/dev/null))
E2E_SERVER_FEATURE_ARGS ?= $(if $(E2E_SERVER_FEATURES),--features $(E2E_SERVER_FEATURES),)
OPENAPI_SERVER_FEATURES ?= $(E2E_SERVER_FEATURES)
OPENAPI_SERVER_FEATURE_ARGS ?= $(E2E_SERVER_FEATURE_ARGS)

# -------- Set up --------

.PHONY: setup py-env

py-env: $(PY_ENV_STAMP)
	$(call print_target_banner)

$(PY_ENV_STAMP): $(PY_REQUIREMENTS)
	@echo "Creating/updating Python virtual environment in $(PY_ENV_DIR)..."
	$(PYTHON_BOOTSTRAP) -m venv $(PY_ENV_DIR)
	$(PYTHON) -m pip install --upgrade pip
	$(PYTHON) -m pip install -r testing/requirements.txt -r testing/e2e/requirements.txt
	@mkdir -p $(dir $(PY_ENV_STAMP))
	@cat $(PY_REQUIREMENTS) > $(PY_ENV_STAMP)

## Install all required development tools
setup: .setup-stamp py-env
	$(call print_target_banner)

# Re-run setup when a makefile changes (prerequisites are set at the end of this file),
# so developers who already have .setup-stamp pick up newly-added tools.
.setup-stamp:
	$(call print_target_banner)
	@echo "Installing required development tools..."
	rustup component add clippy
	cargo install lychee
	cargo install cargo-geiger
	cargo install cargo-deny
	cargo install --locked cargo-gears@$(CARGO_GEARS_VERSION)
	cargo install cargo-fuzz
	cargo install cargo-hack
	cargo install --locked cargo-shear --version $(SHEAR_VERSION)
	cargo install gts-validator
	@if echo "$$OS" | grep -iq windows || [ -n "$$COMSPEC" ]; then \
		echo "NOTE: kani-verifier is not supported on Windows; skipping (use WSL2/Docker for Kani)."; \
		echo "Installing cargo-llvm-cov (supported on Windows; needs llvm-tools-preview)..."; \
		rustup component add llvm-tools-preview; \
		cargo install cargo-llvm-cov; \
		if ! command -v nasm >/dev/null 2>&1; then \
			echo "Installing NASM (required by aws-lc-sys on Windows)..."; \
			winget install NASM.NASM --accept-source-agreements --accept-package-agreements || \
				echo "WARNING: NASM auto-install failed. Install manually from https://www.nasm.us/"; \
		fi; \
	else \
		cargo install --locked kani-verifier && \
		cargo kani setup && \
		cargo install cargo-llvm-cov; \
	fi
	@echo "Setup complete. All tools installed."
	@touch .setup-stamp

# -------- Repository checks --------

.PHONY: ensure-submodules lychee web-docs-check geiger gts-docs docker-pins validate-gear-names pr-review-lint pr-review-test check-packaging-metadata check-release-config check-release-resolver

## Verify git submodules (e.g. guidelines/DNA) are initialized; fails otherwise.
ensure-submodules:
	$(call print_target_banner)
	@if git submodule status --recursive 2>/dev/null | grep -q '^-'; then \
		echo "ERROR: Uninitialized git submodules detected. Run 'git submodule update --init --recursive'." && exit 1; \
	fi

# Run markdown checks with 'lychee'.
# Mirrors the pre-commit hook's scope: every tracked .md outside docs/web-docs (that tree has
# Starlight route-relative links, see web-docs-check). git ls-files does not descend into the
# guidelines/DNA submodule, so it is passed explicitly.
lychee: ensure-submodules
	$(call print_target_banner)
	$(call check_tool,lychee)
	git ls-files -z '*.md' ':!:docs/web-docs/**' | xargs -0 lychee --offline --no-progress --config lychee.toml guidelines/DNA

## Validate internal links in web-docs.
# The web-docs pages use Starlight route-relative links (e.g. ../foo/) that only
# resolve against the *built* site, not the markdown source — so we build the
# docs site with the local content and run lychee over the generated HTML.
WEB_DOCS_CACHE ?= .web-docs-preview
web-docs-check:
	$(call print_target_banner)
	$(call check_tool,lychee)
	@bash tools/scripts/docs-preview.sh build
	lychee --offline --root-dir '$(abspath $(WEB_DOCS_CACHE)/dist)' --exclude 'i18n' '$(WEB_DOCS_CACHE)/dist/**/*.html'

## Run Geiger scanner for unsafe code in dependencies
geiger:
	$(call print_target_banner)
	$(call check_tool,cargo-geiger)
	cd apps/cf-gears-example-server && cargo geiger --all-features

## Check Dockerfile base images are digest-pinned and match rust-toolchain.toml
# Uses PYTHON_BOOTSTRAP, not the venv: this check is pure stdlib text parsing,
# so it must stay runnable without `make py-env` first.
docker-pins:
	$(call print_target_banner)
	$(PYTHON_BOOTSTRAP) tools/scripts/ci.py docker-pins

## Validate GTS identifiers in .md and .json files (DE0903)
# Uses gts-validator binary (install via: cargo install gts-validator)
gts-docs:
	$(call print_target_banner)
	$(call check_tool,gts-validator)
	gts-validator \
		--vendor cf,vendor,example,fabrikam \
		--exclude "target/*" \
		--exclude "docs/api/*" \
		--exclude "docs/web-docs/*" \
		--exclude "gears/chat-engine/*" \
		--exclude "**/helm/*/templates/*" \
		docs gears libs examples

## Validate gear folder names follow kebab-case convention
validate-gear-names: py-env
	$(call print_target_banner)
	@$(PYTHON) tools/scripts/validate_gear_names.py

## Check the toolkit-pr-review rule set agrees with the orchestrators, Cargo.toml and the toolchain
pr-review-lint: py-env
	$(call print_target_banner)
	@$(PYTHON) tools/scripts/toolkit-pr-review/lint.py

## Run the toolkit-pr-review script tests (saved fixtures, no network)
pr-review-test: py-env
	$(call print_target_banner)
	@$(PYTHON) -m unittest discover -s tools/scripts/toolkit-pr-review/tests

## Validate readme/license-file paths declared by publishable crates exist
check-packaging-metadata: py-env
	$(call print_target_banner)
	@$(PYTHON) tools/scripts/check_packaging_metadata.py

## Validate that examples/apps/tools are unpublishable and release-plz.toml matches the workspace
check-release-config: py-env
	$(call print_target_banner)
	@$(PYTHON) tools/scripts/check_release_config.py

## Run the release resolver tests (resolve_release.py + deploy/components.yaml manifest integrity)
check-release-resolver: py-env
	$(call print_target_banner)
	@$(PYTHON) -m unittest discover -s tools/scripts/tests

# -------- Studio --------

.PHONY: cfs-validate-kit-local

## Validate the local studio-kit-gears checkout as a kit directory.
cfs-validate-kit-local: cfs-repair
	$(call print_target_banner)
	cd studio-kit-gears && $(CFS) validate-kits .

# -------- GEAR= scope --------
#
# Additions to the GEAR scope of gears.mk:
# build, run, e2e-local, openapi and coverage use the example server and E2E suites.
#
# Examples:
#   make run GEAR=file-parser         -> cargo run --bin cf-gears-example-server --features file-parser,...

GEAR_BUILD_ARGS ?=
GEAR_RUN_ARGS ?=

E2E_SIDECAR_PREREQ := $(if $(GEAR),$(if $(filter file-storage,$(GEAR)),.e2e-sidecar-build,),.e2e-sidecar-build)
E2E_SIDECAR_ENV := $(if $(GEAR),$(if $(filter file-storage,$(GEAR)),FS_SIDECAR_BINARY=target/debug/sidecar,E2E_SKIP_SIDECAR=1),FS_SIDECAR_BINARY=target/debug/sidecar)

# E2E suite key: kebab-case gear name → snake_case directory name.
GEAR_E2E_KEY := $(subst -,_,$(GEAR))
GEAR_E2E_TARGET ?= testing/e2e/suites/$(GEAR_E2E_KEY)
GEAR_E2E_SCOPE := $(if $(GEAR),$(GEAR_E2E_TARGET) $(E2E_TARGET),$(E2E_TARGET))
# Coverage: pass the first resolved package + e2e target path to coverage.py.
GEAR_COVERAGE_ARGS = $(if $(GEAR),--package $(firstword $(subst -p ,,$(GEAR_PKGS))) --e2e-target $(GEAR_E2E_TARGET),)

# --- Server feature selection for run / openapi ---
# Base features always enabled when running a focused server.
GEAR_SERVER_BASE_FEATURES ?= static-tenants,static-authn,static-authz
# System gears that are non-optional deps of the example server (always linked).
GEAR_SERVER_ALWAYS_LINKED ?= api-gateway service-discovery types-registry tenant-resolver authn-resolver authz-resolver
# Check whether GEAR is a valid example-server feature or an always-linked gear.
# When GEAR has no server feature (e.g. toolkit-db, toolkit-http), server-dependent
# targets (run, openapi, e2e-local) are skipped; library-safe targets still work.
GEAR_HAS_SERVER_FEATURE := $(or $(filter $(GEAR),$(GEAR_SERVER_ALWAYS_LINKED)),$(filter $(GEAR),$(EXAMPLE_SERVER_ALL_FEATURES)))
# The gear itself as an optional feature (empty if it's an always-linked gear).
GEAR_SERVER_OPTIONAL_FEATURES := $(if $(GEAR_HAS_SERVER_FEATURE),$(filter-out $(GEAR_SERVER_ALWAYS_LINKED),$(GEAR)),)
# Extra local-dev plugins a gear needs to start (GEAR_SERVER_EXTRA_FEATURES_<gear>).
# mini-chat registers OAGW upstreams whose secret_ref OAGW checks in credstore.
GEAR_SERVER_EXTRA_FEATURES_mini-chat ?= static-credstore
GEAR_SERVER_EXTRA_FEATURES := $(GEAR_SERVER_EXTRA_FEATURES_$(GEAR))
GEAR_SERVER_FEATURES ?= $(GEAR_SERVER_OPTIONAL_FEATURES)$(if $(GEAR_SERVER_OPTIONAL_FEATURES),$(COMMA),)$(GEAR_SERVER_BASE_FEATURES)$(if $(GEAR_SERVER_EXTRA_FEATURES),$(COMMA)$(GEAR_SERVER_EXTRA_FEATURES),)
GEAR_SERVER_FEATURE_ARGS := $(if $(GEAR),$(if $(GEAR_HAS_SERVER_FEATURE),--no-default-features --features $(GEAR_SERVER_FEATURES),),$(EXAMPLE_SERVER_FEATURE_ARGS))

# --- OpenAPI ---
GEAR_OPENAPI_TMP ?= target/openapi/$(GEAR).json
# GTS envelope providers that config/e2e-local.yaml's seeds depend on at link
# time (account-management registers tenant_type base schemas via inventory).
OPENAPI_ENVELOPE_FEATURES ?= account-management,static-idp
GEAR_OPENAPI_FEATURE_ARGS := --no-default-features --features $(GEAR_SERVER_FEATURES),$(OPENAPI_ENVELOPE_FEATURES)
# No GEAR: full curated server.  GEAR=xxx: focused server, merged into api.json.
OPENAPI_BUILD_FEATURE_ARGS := $(if $(GEAR),$(GEAR_OPENAPI_FEATURE_ARGS),$(OPENAPI_SERVER_FEATURE_ARGS))

help.GEAR.targets += build dist run openapi coverage coverage-unit coverage-e2e-local e2e-local e2e-local-smoke all
help.GEAR_FEATURES.targets += build
HELP_PARAMS += GEAR_BUILD_ARGS GEAR_RUN_ARGS
help.GEAR_BUILD_ARGS.usage := GEAR_BUILD_ARGS="<args>"
help.GEAR_BUILD_ARGS.targets := build
help.GEAR_BUILD_ARGS.text = Extra cargo build arguments for GEAR= builds.
help.GEAR_RUN_ARGS.usage := GEAR_RUN_ARGS="<args>"
help.GEAR_RUN_ARGS.targets := run
help.GEAR_RUN_ARGS.text = Extra arguments for the example server, after its run command.

# -------- API and docs --------

.PHONY: openapi md-fabric slides web-docs-preview .example-server-build arch_status_svg_update

.example-server-build:
	$(call print_target_banner)
ifneq ($(GEAR),)
ifeq ($(GEAR_HAS_SERVER_FEATURE),)
	@echo "SKIP: GEAR=$(GEAR) has no example-server feature — skipping server build."
else
	cargo build --bin $(EXAMPLE_SERVER_BIN) $(OPENAPI_BUILD_FEATURE_ARGS)
endif
else
	cargo build --bin $(EXAMPLE_SERVER_BIN) $(OPENAPI_BUILD_FEATURE_ARGS)
endif

# Generate OpenAPI spec from running cf-gears-example-server.
# Skipped when GEAR has no corresponding example-server feature.
openapi: .example-server-build py-env
	$(call print_target_banner)
ifneq ($(GEAR),)
ifeq ($(GEAR_HAS_SERVER_FEATURE),)
	@echo "SKIP: GEAR=$(GEAR) has no example-server feature — skipping openapi."
else
	@command -v curl >/dev/null || (echo "curl is required to generate OpenAPI spec" && exit 1)
	@echo "Starting cf-gears-example-server to generate OpenAPI spec..."
	@mkdir -p $$(dirname "$(if $(GEAR),$(GEAR_OPENAPI_TMP),$(OPENAPI_OUT))") && \
	$(call start_server_and_wait,$(EXAMPLE_SERVER_DEBUG_BINARY) --config $(OPENAPI_CONFIG) --port $(OPENAPI_PORT),$(OPENAPI_URL),300) && \
	echo "Fetching OpenAPI spec..." && \
	curl -fsS "$(OPENAPI_URL)" -o "$(if $(GEAR),$(GEAR_OPENAPI_TMP),$(OPENAPI_OUT))" && \
	if [ -n "$(GEAR)" ]; then \
		echo "Merging $(GEAR) OpenAPI into $(OPENAPI_OUT)..." && \
		$(PYTHON) tools/scripts/merge_openapi_json.py "$(OPENAPI_OUT)" "$(GEAR_OPENAPI_TMP)"; \
	fi && \
	echo "Sorting OpenAPI JSON for deterministic ordering..." && \
	$(PYTHON) tools/scripts/sort_openapi_json.py "$(OPENAPI_OUT)" && \
	echo "OpenAPI spec saved to $(OPENAPI_OUT)"
endif
else
	@command -v curl >/dev/null || (echo "curl is required to generate OpenAPI spec" && exit 1)
	@echo "Starting cf-gears-example-server to generate OpenAPI spec..."
	@mkdir -p $$(dirname "$(OPENAPI_OUT)") && \
	$(call start_server_and_wait,$(EXAMPLE_SERVER_DEBUG_BINARY) --config $(OPENAPI_CONFIG) --port $(OPENAPI_PORT),$(OPENAPI_URL),300) && \
	echo "Fetching OpenAPI spec..." && \
	curl -fsS "$(OPENAPI_URL)" -o "$(OPENAPI_OUT)" && \
	echo "Sorting OpenAPI JSON for deterministic ordering..." && \
	$(PYTHON) tools/scripts/sort_openapi_json.py "$(OPENAPI_OUT)" && \
	echo "OpenAPI spec saved to $(OPENAPI_OUT)"
endif

## Generate Markdown files map
md-fabric: py-env
	$(call print_target_banner)
	$(PYTHON) ./tools/scripts/md-fabric.py --inline-data --out docs/md-fabric/md-fabric.html

## Build the slides with Marp
slides:
	$(call print_target_banner)
	@command -v npx >/dev/null || (echo "npx is required to build slides. Install Node.js or run 'npm install' from the repo root." && exit 1)
	npx marp docs/slides/[0-9]*.md --theme-set docs/slides/css/slides.css --allow-local-files

# Preview the documentation website with local docs/web-docs content.
# Clones the web docs site into .web-docs-preview/ and serves it at localhost:4321.
web-docs-preview:
	$(call print_target_banner)
	@bash tools/scripts/docs-preview.sh

## Regenerate docs/img/architecture.drawio.svg with live gear status from the GitHub board
arch_status_svg_update: py-env
	$(call print_target_banner)
	@$(PYTHON) tools/scripts/architecture_status_svg.py -v

# -------- Tests --------

.PHONY: test-no-macros test-macros test-db test-integration test-graph-storage test-graph-storage-pg test-cluster-k8s coverage-cluster-k8s test-durable-execution-pg coverage-durable-execution-pg test-fips

# Run the workspace tests except the macro test crates (CI test job)
test-no-macros: install-tools
	$(call print_target_banner)
	$(call nextest_run,test-no-macros,--workspace --exclude cf-gears-toolkit-macros-tests --exclude cf-gears-toolkit-db-macros)

# Run the toolkit macro tests (cf-gears-toolkit-db-macros, cf-gears-toolkit-macros-tests)
test-macros: install-tools
	$(call print_target_banner)
	$(call nextest_run,macros-db,-p cf-gears-toolkit-db-macros)
	$(call nextest_run,macros-toolkit,-p cf-gears-toolkit-macros-tests)

## Run the graph-storage gear's database-free suites
## Unit tests, the in-memory conformance lane, the domain-service and REST
## lanes. The PostgreSQL lanes skip here; `test-graph-storage-pg` runs them.
test-graph-storage: install-tools
	$(call print_target_banner)
	cargo nextest run -p cf-gears-graph-storage -p cf-gears-graph-storage-sdk

## Run the graph-storage gear's PostgreSQL 19 lane (Docker required)
## The same conformance suite against the built-in store, plus the SQL/PGQ cases that only a real
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
test-graph-storage-pg: install-tools
	$(call print_target_banner)
	GEARS_TEST_PG_GRAPH_REQUIRED=1 cargo nextest run -p cf-gears-graph-storage \
		--test pg_conformance --test-threads=$(GRAPH_PG_TEST_THREADS)

# Integration lanes
#
# A lane is one `cargo nextest run` of an integration suite (most need Docker).
# Run lanes with `make test-integration LANE="<lane> ..."`; without LANE it runs
# every lane in INTEGRATION_LANES order. Each lane is described by:
#   lane_args_<lane>  nextest arguments (package, features, filters)
#   lane_env_<lane>   optional environment prefix for the run
#   lane_post_<lane>  optional command run after the tests pass
# The lane name is also the JUnit report name: $(JUNIT_DIR)/<lane>.xml.
# The tables use `=` so a `$$` in a filter reaches the shell as `$`.

TOOLKIT_DB_LANES := toolkit-db-sqlite toolkit-db-pg toolkit-db-pgq toolkit-db-mysql
INTEGRATION_LANES := $(TOOLKIT_DB_LANES) users-info-pg usage-collector-pg usage-collector-ch \
	types-registry-db oagw-db cluster-pg cluster-redis rg-pg settings-service-pg \
	pricing-pg coord-pg products-pg ledger-pg quota-enforcement-pg

## toolkit-db on SQLite; the examples must still build after the tests.
lane_args_toolkit-db-sqlite = -p cf-gears-toolkit-db --features sqlite,integration
lane_post_toolkit-db-sqlite = cargo build -p cf-gears-toolkit-db --examples --features sqlite

## toolkit-db on PostgreSQL.
lane_args_toolkit-db-pg = -p cf-gears-toolkit-db --features pg,integration

## The SQL/PGQ lane: toolkit-db's unit suites under the `pgq` feature (the
## secure graph builder and its tests compile only there), the PostgreSQL 19
## integration suite (tests/pg, Docker required; a testcontainers PG19) and the
## trybuild guards. `pgq` implies `pg`, so without the filter this would re-run
## every PG18 Docker suite the toolkit-db-pg lane just ran; the filter keeps only
## what this lane adds. The PG19 suite skips itself while the pre-GA image is
## unavailable; CI sets GEARS_TEST_PG_GRAPH_REQUIRED=1 so there the skip is a
## failure — do the same locally once the image is expected to be present.
lane_args_toolkit-db-pgq = -p cf-gears-toolkit-db --features pgq,integration -E 'kind(lib) | binary(mod) | binary(ui)'

## toolkit-db on MySQL.
lane_args_toolkit-db-mysql = -p cf-gears-toolkit-db --features mysql,integration

## users-info gear integration tests.
lane_args_users-info-pg = -p users-info --features integration

## TimescaleDB usage-collector plugin integration tests (Docker required;
## the suite spins up its own timescale/timescaledb container via testcontainers)
lane_args_usage-collector-pg = -p cf-gears-timescaledb-usage-collector-plugin --features postgres

## ClickHouse usage-collector plugin integration tests (Docker required;
## every test gets its own database on one shared, named clickhouse container,
## `uc-clickhouse-test-harness-<tag>`, which is reused across runs — remove it
## with `docker rm -fv` to start clean).
## `--run-ignored all` because the suite is double-gated: the `clickhouse`
## feature compiles the test files, and every Docker-backed test inside them is
## `#[ignore]`d. CH_REQUIRE_DOCKER=1 turns an unreachable Docker into a panic —
## without it bring_up_or_skip() reports `ok` on a suite that ran nothing.
lane_args_usage-collector-ch = -p cf-gears-clickhouse-usage-collector-plugin --features clickhouse --run-ignored all --no-fail-fast
lane_env_usage-collector-ch = CH_REQUIRE_DOCKER=1

## types-registry PostgreSQL + MySQL integration tests (Docker required;
## each test spins up its own postgres or mysql container via testcontainers).
lane_args_types-registry-db = -p cf-gears-types-registry --features integration -E 'binary(/_backends_test$$/)'

## OAGW's database tier: the repository conformance suite and the
## migration tests on PostgreSQL and MySQL (Docker required; no skip without
## it; images pinned in test-containers).
## The in-memory and SQLite runs of the same suites are part of `make test`,
## except the SQLite lock races, which need the `integration` test hooks and
## run here (no Docker needed for them).
lane_args_oagw-db = -p cf-gears-oagw --features integration -E 'test(/postgres|mysql|races_on_sqlite/)'

## The Postgres cluster plugin's conformance (Layer 2) and Layer 3
## integration suites (Docker required;
## each spins up its own postgres container per test via testcontainers —
## see gears/system/cluster/plugins/postgres-cluster-plugin/docs/TESTING.md §7).
##
## `--retries 1` because the container/pool *setup* in tests/common/mod.rs is
## load-sensitive on a busy host: it already retries `Postgres::start()` itself,
## and exhausting that budget surfaces as a failure in whichever test drew the
## short straw. A genuine logic regression fails both attempts, so this absorbs
## Docker churn without masking one.
lane_args_cluster-pg = -p cf-postgres-cluster-plugin --features integration --retries 1

## The Redis cluster plugin's conformance (Layer 2) and Layer 3 integration
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
## `--retries 1` for the same reason as the cluster-pg lane: container setup is
## load-sensitive on a busy host, and a genuine logic regression fails both
## attempts, so this absorbs Docker churn without masking one.
lane_args_cluster-redis = -p cf-redis-cluster-plugin --features integration --retries 1

## resource-group gear PostgreSQL smoke tests (Docker required; spins up
## its own postgres container via testcontainers -- see
## gears/system/resource-group/resource-group/tests/pg_smoke_test.rs)
lane_args_rg-pg = -p cf-gears-resource-group --features integration

## The settings-service gear's PostgreSQL migration suite (Docker required;
## spins up its own postgres container via testcontainers -- see
## gears/settings-service/settings-service/tests/pg_migrations_test.rs). The
## suites beside the gear's migrations run on SQLite, where a statement
## PostgreSQL refuses can still pass; this lane runs the chain a stand's startup
## runs, on the backend it runs it on.
lane_args_settings-service-pg = -p cf-gears-settings-service --features integration --test pg_migrations_test

## bss-pricing's Postgres tier (Docker required; each suite spins up its own
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
lane_args_pricing-pg = -p cf-gears-bss-pricing --run-ignored ignored-only -E 'binary(/^postgres_/)' --no-fail-fast

## coord's Postgres tier (Docker required; testcontainers).
##
## `coord`'s `m0001_…` builds two independent SQL literals — a schema-qualified
## PG `CREATE TABLE` and a bare SQLite one. The in-crate tests connect
## `sqlite::memory:` and reach the SQLite literal only, so the `IF NOT EXISTS`
## that stops the two-gear boot crash (`bss-pricing` starting beside a
## long-running `bss-ledger`) was unexercised on the dialect the crash happened
## on. `Migration::in_schema("bss")` — the constructor both consumers actually
## pass — is likewise unreachable from SQLite, which has one namespace.
##
## Same `--run-ignored ignored-only` shape as the pricing-pg lane above, and the
## same reason: the gate is `#[ignore]` rather than a feature.
lane_args_coord-pg = -p cf-gears-bss-coord --run-ignored ignored-only -E 'binary(/^postgres_/)'

## bss-products' Postgres tier (Docker required; testcontainers).
##
## Same `--run-ignored ignored-only` shape and the same reason as the two above:
## the gate is an ignored-test attribute rather than a feature. This gear's tier was added on
## 2026-08-30 and, until this lane existed, repeated the donor's own 2026-08-11
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
lane_args_products-pg = -p cf-gears-bss-products --run-ignored ignored-only -E 'binary(/^postgres_/)' --no-fail-fast

## bss-ledger's Postgres tier (Docker required; testcontainers).
##
## Same `--run-ignored ignored-only` shape as pricing, coord and products: the ledger tests that
## need a database are `#[ignore]` with a Docker reason — the `postgres_*` and
## `rest_*` binaries and the lib's `pg` modules — so there is no binary filter.
## One ignored test is not a Docker test: `grain_fold_timing` is a manual timing helper that
## prints a cost. It is excluded by name, so do not add it back.
## `--no-fail-fast` for the reason the pricing lane gives.
lane_args_ledger-pg = -p cf-gears-bss-ledger --run-ignored ignored-only -E 'not test(grain_fold_timing)' --no-fail-fast

## The Quota Enforcement storage plugin's PostgreSQL suites (Docker required;
## each test spins up its own postgres container via testcontainers): the row
## locks, NOWAIT stripes and idempotency races SQLite cannot show.
lane_args_quota-enforcement-pg = -p cf-gears-quota-enforcement-storage-plugin --features postgres

# run_lane(lane): the lane's nextest run, then its post command if it has one.
run_lane = $(call nextest_run,$(1),$(lane_args_$(1)),$(lane_env_$(1)))$(if $(lane_post_$(1)),$(newline)$(lane_post_$(1)))

# Run integration test lanes (LANE="<lane> ...", default all lanes)
## Each lane is one nextest run of one suite, and most need Docker. The lanes
## and their nextest arguments are the lane_args_<lane> table in this file; the
## lane name is also the JUnit report name.
test-integration: install-tools
	$(call print_target_banner)
	$(if $(filter-out $(INTEGRATION_LANES),$(LANE)),$(error Unknown LANE '$(filter-out $(INTEGRATION_LANES),$(LANE))'. Use: $(INTEGRATION_LANES)))
	$(foreach lane,$(or $(LANE),$(INTEGRATION_LANES)),$(call run_lane,$(lane))$(newline))

# Run cf-gears-toolkit-db integration tests on every engine (sqlite, pg, pgq, mysql)
test-db: install-tools
	$(call print_target_banner)
	$(foreach lane,$(TOOLKIT_DB_LANES),$(call run_lane,$(lane))$(newline))

HELP_PARAMS += LANE GRAPH_PG_TEST_THREADS
help.LANE.usage := LANE="<lane> ..."
help.LANE.targets := test-integration
help.LANE.text = Integration lanes to run, in order; default: all. Most need Docker.
help.LANE.values = $(INTEGRATION_LANES)
help.NEXTEST_PROFILE.targets += test-no-macros test-macros test-db test-integration
help.JUNIT_DIR.targets += test-no-macros test-macros test-db test-integration
help.GRAPH_PG_TEST_THREADS.usage := GRAPH_PG_TEST_THREADS=<n>
help.GRAPH_PG_TEST_THREADS.targets := test-graph-storage-pg
help.GRAPH_PG_TEST_THREADS.text = Parallel test cases; each runs its own PostgreSQL. Default: $(GRAPH_PG_TEST_THREADS).

# Cluster and FIPS suites

# k8s_cluster_suite(runner, label[, first-commands]): run the k8s cluster
# plugin's single integration binary against one shared k3s container, with
# cleanup and one retry. The runner is `cargo test` or `cargo llvm-cov test --no-report`.
define k8s_cluster_suite
	@set -e; \
	k3s_cleanup() { docker ps -aq --filter label=org.cf-gears.test-fixture=cf-k8s-cluster-plugin | xargs -r docker rm -f >/dev/null 2>&1 || true; }; \
	trap k3s_cleanup EXIT; \
	k3s_cleanup; \
	$(3) \
	echo "=== $(2)k8s-cluster-plugin: integration (single shared k3s) ==="; \
	$(1) -p cf-k8s-cluster-plugin --features integration --test integration -- --test-threads=4 \
		|| { echo "=== integration failed; retrying once (flake guard) ==="; k3s_cleanup; \
			$(1) -p cf-k8s-cluster-plugin --features integration --test integration -- --test-threads=4; } \
		|| exit 1
endef
K8S_PROVIDER_REGISTRY_CHECK = echo "=== cf-gears-cluster: k8s provider registration (lib, no k3s) ==="; \
	cargo test -p cf-gears-cluster --features k8s --lib provider_registry_resolves_k8s_for_every_primitive;

## Run the k8s cluster plugin's L2/L3 suites against a real k3s (Docker, privileged)
## Kubernetes cluster plugin: L2 (conformance) + L3 (integration) against a real
## k3s API server (docs/TESTING.md 4, 7). Docker required, and k3s needs
## **privileged** mode -- the one CI capability beyond a Docker daemon.
##
## Uses `cargo test`, not `nextest` (unlike the cluster-pg lane): k3s takes ~20s to
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
test-cluster-k8s: install-tools
	$(call print_target_banner)
	$(call k8s_cluster_suite,cargo test,,$(K8S_PROVIDER_REGISTRY_CHECK))

## Add the k8s cluster plugin's L2/L3 coverage to the current cargo-llvm-cov session
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
	$(call k8s_cluster_suite,cargo llvm-cov test --no-report,coverage: )

# Use one test binary so PostgreSQL scenarios share the testcontainers fixture.
DURABLE_EXECUTION_PG_TEST_ARGS := -p cf-gears-durable-execution --lib --features integration \
	-- --skip infra::storage::process_tests::worker_child --test-threads=4

## Run the durable-execution gear's PostgreSQL and subprocess recovery suites (Docker required)
## Uses `cargo test`, not `nextest`: the scenarios live in one test binary so
## they share a single testcontainers PostgreSQL fixture.
test-durable-execution-pg: install-tools
	$(call print_target_banner)
	cargo test $(DURABLE_EXECUTION_PG_TEST_ARGS)

## Add the durable-execution PostgreSQL coverage to the current cargo-llvm-cov session
## The coverage sibling of `test-durable-execution-pg`. Like
## `coverage-cluster-k8s`, it does not clean or report: the coverage job in
## .github/workflows/ci.yml brackets it.
coverage-durable-execution-pg:
	$(call print_target_banner)
	cargo llvm-cov test --no-rustc-wrapper --no-report $(DURABLE_EXECUTION_PG_TEST_ARGS)

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
test-fips: install-tools
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

.PHONY: bench-toolkit-db-outbox

# Outbox throughput benchmarks of cf-gears-toolkit-db, per database engine:
# the Cargo feature that enables the engine and the benchmark filter.
BENCH_DBS := pg mysql mariadb sqlite
bench_feature_pg := pg
bench_feature_mysql := mysql
bench_feature_mariadb := mysql
bench_feature_sqlite := sqlite
bench_filter_pg := postgres
bench_filter_mysql := mysql
bench_filter_mariadb := mariadb
bench_filter_sqlite := sqlite

# Run outbox throughput benchmarks (DB=pg|mysql|mariadb|sqlite, default all; LONGHAUL=1 for long-haul runs)
## Long-haul sizes: 1M+10M rows on pg, mysql and mariadb; 100K on sqlite.
bench-toolkit-db-outbox:
	$(call print_target_banner)
	$(if $(filter-out $(BENCH_DBS),$(DB)),$(error Unknown DB '$(DB)'. Use one of: $(BENCH_DBS)))
	$(foreach db,$(or $(DB),$(BENCH_DBS)),cargo bench -p cf-gears-toolkit-db --features $(bench_feature_$(db)) --bench outbox_throughput -- $(bench_filter_$(db))$(if $(LONGHAUL),_longhaul)$(newline))

HELP_PARAMS += DB LONGHAUL
help.DB.usage := DB=<db>
help.DB.targets := bench-toolkit-db-outbox
help.DB.text = Database engine to benchmark; default: all.
help.DB.values = $(BENCH_DBS)
help.LONGHAUL.usage := LONGHAUL=1
help.LONGHAUL.targets := bench-toolkit-db-outbox
help.LONGHAUL.text = Run the long-haul benchmarks (1M+10M rows; 100K on sqlite).

# -------- E2E tests --------

.PHONY: e2e e2e-local e2e-local-smoke e2e-docker e2e-docker-smoke e2e-tr-authz e2e-oagw-sqlite e2e-usage-collector e2e-event-broker e2e-oop

E2E_TARGET ?=
# E2E selectors for `make e2e-local`:
#   SUITE=<suite>  run ONE named scenario under testing/e2e/suites/ (often, but
#                  not always, a gear crate) — e.g. SUITE=file-parser,
#                  SUITE=scope-enforcement.
#   GEAR=<gear>    run EVERY e2e-launcher suite whose e2e.yaml features (incl.
#                  features_file) include <gear> — e.g. GEAR=credstore runs both
#                  the credstore and oagw suites. SUITE and GEAR are mutually
#                  exclusive (enforced by tools/scripts/run_e2e.py).
SUITE ?=

# SUITE= and GEAR= are mutually exclusive; that check lives in
# tools/scripts/run_e2e.py (the runner rejects the combination).

# Run E2E tests in Docker (default)
e2e: e2e-docker
	$(call print_target_banner)

## Run E2E tests in Docker environment
e2e-docker: py-env
	$(call print_target_banner)
	$(PYTHON) tools/scripts/ci.py e2e-docker -- $(E2E_TARGET)

## Run E2E smoke tests in Docker (only tests marked @pytest.mark.smoke)
e2e-docker-smoke: py-env
	$(call print_target_banner)
	$(PYTHON) tools/scripts/ci.py e2e-docker -- -m smoke $(E2E_TARGET)

# Run E2E tests locally: one suite (SUITE=), one gear's suites (GEAR=) or all shared-server suites
# Three ways to use it:
#   make e2e-local SUITE=<suite>   run ONE suite: build a server for just that
#                                  suite and run its tests.
#   make e2e-local GEAR=<gear>     run EVERY e2e-launcher suite whose e2e.yaml
#                                  features (incl. features_file) include <gear>,
#                                  each as its own focused build+run. If no suite
#                                  matches, it is a no-op (so `make all GEAR=` is
#                                  safe for gears without E2E coverage).
#   make e2e-local                 run MANY suites: build one server with every
#                                  E2E feature (config/e2e-features.txt) and run
#                                  every shared-server suite against it. A
#                                  "shared-server suite" is one whose e2e.yaml
#                                  has `launcher: e2e-launcher`.
# Self-managed suites (`launcher: pytest` — mini-chat, usage-collector) and the
# profile lanes (tr-authz, oagw-sqlite) start their own server, so plain
# `make e2e-local` and GEAR= runs skip them; run them via
# `make e2e-local SUITE=mini-chat` and their own targets (e2e-usage-collector,
# e2e-tr-authz, e2e-oagw-sqlite). All feature/config/sidecar/gear knowledge
# lives in config/e2e-launcher.yaml and testing/e2e/suites/<suite>/e2e.yaml, so
# this recipe stays suite-agnostic.
e2e-local: py-env
	$(call print_target_banner)
	$(PYTHON) tools/scripts/run_e2e.py --suite "$(SUITE)" --gear "$(GEAR)" -- $(E2E_TARGET)

## Run RG + AuthZ barrier E2E tests with tr-authz-plugin going through TR -> RG
e2e-tr-authz: py-env
	$(call print_target_banner)
	$(PYTHON) tools/scripts/run_e2e.py --suite resource-group --profile tr-authz --

## Run OAGW E2E tests with OAGW on SQLite and upstreams/routes from the types registry
e2e-oagw-sqlite: py-env
	$(call print_target_banner)
	$(PYTHON) tools/scripts/run_e2e.py --suite oagw --profile sqlite -- $(E2E_TARGET)

## Run E2E smoke tests locally (only tests marked @pytest.mark.smoke)
e2e-local-smoke: py-env
	$(call print_target_banner)
	$(PYTHON) tools/scripts/run_e2e.py --suite "$(SUITE)" --gear "$(GEAR)" --smoke -- $(E2E_TARGET)

MINI_CHAT_FEATURES = mini-chat,static-authn,static-authz,single-tenant,static-credstore
MINI_CHAT_K8S_FEATURES = $(MINI_CHAT_FEATURES),k8s

MINI_CHAT_IMAGE ?= cf-gears-mini-chat
MINI_CHAT_TAG   ?= $(shell git rev-parse --short HEAD 2>/dev/null || echo latest)

UC_E2E_FEATURES = usage-collector,static-tenants,static-authn,static-authz

UC_E2E_BACKENDS := timescaledb clickhouse

HELP_PARAMS += SUITE E2E_TARGET BACKEND
help.SUITE.usage := SUITE=<suite>
help.SUITE.targets := e2e-local e2e-local-smoke
help.SUITE.text = Run one suite under testing/e2e/suites/, e.g. SUITE=mini-chat. Do not combine with GEAR.
help.E2E_TARGET.usage := E2E_TARGET="<args>"
help.E2E_TARGET.targets := e2e-local e2e-local-smoke e2e-docker e2e-docker-smoke e2e-oagw-sqlite e2e-usage-collector
help.E2E_TARGET.text = Extra pytest arguments or test paths, e.g. E2E_TARGET="-k smoke".
help.BACKEND.usage := BACKEND=<backend>
help.BACKEND.targets := e2e-usage-collector
help.BACKEND.text = Storage backend to test; default: both, one after the other.
help.BACKEND.values = $(UC_E2E_BACKENDS)

# Run usage-collector E2E tests (BACKEND=timescaledb|clickhouse, default both; Docker required)
## Each backend gets a dedicated binary: UC_E2E_FEATURES plus that backend's
## storage plugin. The two plugins cannot share a binary: every linked gear is
## initialized, and both fail init without their own live database. Hence one
## build and one run per backend, in sequence: both build the same
## target/debug/cf-gears-example-server path, bind 127.0.0.1:8088, and share
## ~/.cf-gears, so they must never run concurrently.
e2e-usage-collector: py-env
	$(call print_target_banner)
	$(if $(filter-out $(UC_E2E_BACKENDS),$(BACKEND)),$(error Unknown BACKEND '$(BACKEND)'. Use one of: $(UC_E2E_BACKENDS)))
	$(foreach b,$(or $(BACKEND),$(UC_E2E_BACKENDS)),cargo build --bin cf-gears-example-server --features=$(UC_E2E_FEATURES),$(b)-usage-collector$(newline)E2E_BINARY=target/debug/cf-gears-example-server UC_E2E_BACKEND=$(b) $(PYTHON) -m pytest testing/e2e/suites/usage_collector/ -vv $(E2E_TARGET)$(newline))

## Run event-broker E2E tests (its own standalone binary, not a cf-gears-example-server feature)
e2e-event-broker: py-env
	$(call print_target_banner)
	cargo build -p cf-gears-event-broker --bin cf-gears-event-broker-server
	E2E_BINARY=target/debug/cf-gears-event-broker-server \
		$(PYTHON) -m pytest testing/e2e/suites/event_broker/ -vv

## Run the out-of-process (loopback, no-k8s) E2E suite
# Out-of-process (loopback) E2E: boots flight-control + OoP gears as local
# processes (no Kubernetes) and asserts the cross-process seams. Self-managed:
# its conftest builds the binaries and owns the process lifecycle, so it runs
# pytest directly (not via run_e2e.py). `timeout_func_only` keeps the per-test
# timeout from counting the heavy fixture setup (build + boot + route sync), and
# the 30s per-test cap (vs pytest.ini's default) leaves headroom for the
# lifecycle tests' process stop/start + polling while still failing fast.
e2e-oop: py-env
	$(call print_target_banner)
	cargo build -p cf-gears-flight-control --bin flight-control
	cargo build -p hello --features oop_module --bin hello-oop
	cargo build -p cf-api-contracts --features oop_module --bin api-contracts-oop
	cargo build -p cf-api-contracts-consumer --features oop_module --bin api-contracts-consumer-oop
	OOP_E2E=1 $(PYTHON) -m pytest testing/e2e/suites/oop \
		-o timeout_func_only=true --timeout=30 -vv

# -------- Code coverage --------

.PHONY: coverage coverage-unit coverage-e2e-local check-prereq-e2e-local

# Generate code coverage report (unit + e2e-local tests)
coverage: py-env
	$(call print_target_banner)
	$(call check_tool,cargo-llvm-cov)
	$(PYTHON) tools/scripts/coverage.py combined $(GEAR_COVERAGE_ARGS)

# Generate code coverage report (unit tests only)
coverage-unit: py-env
	$(call print_target_banner)
	$(call check_tool,cargo-llvm-cov)
	$(PYTHON) tools/scripts/coverage.py unit $(if $(GEAR),--package $(firstword $(subst -p ,,$(GEAR_PKGS))),)

## Ensure needed packages and programs installed for local e2e testing
check-prereq-e2e-local: py-env
	$(call print_target_banner)
	$(PYTHON) tools/scripts/check_local_env.py --mode e2e-local

# Generate code coverage report (e2e-local tests only)
coverage-e2e-local: check-prereq-e2e-local
	$(call print_target_banner)
	$(call check_tool,cargo-llvm-cov)
	$(PYTHON) tools/scripts/coverage.py e2e-local $(if $(GEAR),--e2e-target $(GEAR_E2E_TARGET),)

# -------- Mini chat --------

# mini-chat targets are for running the mini-chat gear in Kubernetes, with options for building Docker images and deploying with Helm.
# Run it locally with `make run PRESET=mini-chat`.

.PHONY: mini-chat-docker mini-chat-helm mini-chat-helm-template mini-chat-up mini-chat-down mini-chat-port-forward

## Build mini-chat Docker image for K8s (dev build by default, RELEASE=1 for optimized)
## On linux: builds on host (reuses local target/), then packages the binary.
## On other OS: full multi-stage Docker build with BuildKit caching.
MINI_CHAT_PROFILE = $(if $(RELEASE),release,dev)
MINI_CHAT_CARGO_RELEASE_FLAG = $(if $(RELEASE),--release,)
MINI_CHAT_TARGET_DIR = $(or $(CARGO_TARGET_DIR),target)/$(if $(RELEASE),release,debug)
mini-chat-docker:
	$(call print_target_banner)
ifeq ($(shell uname -s),Linux)
	@echo "==> Linux host: building on host, packaging into image"
	cargo build $(MINI_CHAT_CARGO_RELEASE_FLAG) --bin cf-gears-example-server --package=cf-gears-example-server \
		--features "$(MINI_CHAT_K8S_FEATURES)"
	@mkdir -p .docker-stage
	@cp $(MINI_CHAT_TARGET_DIR)/cf-gears-example-server .docker-stage/cf-gears-example-server
	DOCKER_BUILDKIT=1 docker build \
		-f gears/mini-chat/deploy/docker/mini-chat-prebuilt.Dockerfile \
		--build-arg BINARY_PATH=".docker-stage/cf-gears-example-server" \
		-t $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) .
	@rm -rf .docker-stage
else
	@echo "==> Non-linux host: full Docker build"
	DOCKER_BUILDKIT=1 docker build \
		-f gears/mini-chat/deploy/docker/mini-chat.Dockerfile \
		--build-arg CARGO_FEATURES="$(MINI_CHAT_K8S_FEATURES)" \
		--build-arg BUILD_PROFILE="$(MINI_CHAT_PROFILE)" \
		-t $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) .
endif

## Deploy mini-chat Helm chart to local K8s cluster (build + load + install)
mini-chat-helm: mini-chat-docker
	$(call print_target_banner)
	@if command -v k3s >/dev/null 2>&1; then \
		docker save $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) | sudo k3s ctr images import -; \
	elif command -v minikube >/dev/null 2>&1; then \
		minikube ssh "docker rmi -f $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) 2>/dev/null" || true; \
		minikube image load $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG); \
	else \
		echo "ERROR: k3s or minikube required"; exit 1; \
	fi
	helm upgrade --install mini-chat gears/mini-chat/deploy/helm/mini-chat/ \
		--set image.tag="$(MINI_CHAT_TAG)" \
		--set secrets.azureOpenaiApiKey="$${AZURE_OPENAI_API_KEY}" \
		--set secrets.azureOpenaiApiHost="$${AZURE_OPENAI_API_HOST}" \
		--set postgres.host="$${PG_HOST:-postgres.default.svc.cluster.local}" \
		--set postgres.password="$${PG_PASSWORD}"
	kubectl rollout restart deployment/mini-chat
	kubectl rollout status deployment/mini-chat --timeout=120s

## Render mini-chat Helm templates (dry-run)
mini-chat-helm-template:
	$(call print_target_banner)
	helm template mini-chat gears/mini-chat/deploy/helm/mini-chat/

## One-command: ensure minikube is up, deploy latest chart, port-forward
## Usage: make mini-chat-up
## If image was rebuilt (make mini-chat-docker), re-run this to pick it up.
mini-chat-up:
	$(call print_target_banner)
	@# --- 1. Ensure cluster is running ---
	@if command -v minikube >/dev/null 2>&1; then \
		STATUS=$$(minikube status -f '{{.Host}}' 2>/dev/null || true); \
		if [ "$$STATUS" != "Running" ]; then \
			echo "Starting minikube..."; \
			minikube start; \
		fi; \
	elif command -v k3s >/dev/null 2>&1; then \
		: ; \
	else \
		echo "ERROR: minikube or k3s required"; exit 1; \
	fi
	@# --- 2. Load latest image if it exists locally ---
	@if docker image inspect $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) >/dev/null 2>&1; then \
		echo "Loading image $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) into cluster..."; \
		if command -v minikube >/dev/null 2>&1; then \
			minikube ssh "docker rmi -f $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) 2>/dev/null" || true; \
			minikube image load $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG); \
		else \
			docker save $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) | sudo k3s ctr images import -; \
		fi; \
	else \
		echo "No local image found. Run 'make mini-chat-docker' first to build."; \
		exit 1; \
	fi
	@# --- 3. Helm install/upgrade ---
	@if [ -z "$${AZURE_OPENAI_API_KEY}" ] || [ -z "$${AZURE_OPENAI_API_HOST}" ]; then \
		echo "WARNING: AZURE_OPENAI_API_KEY or AZURE_OPENAI_API_HOST not set."; \
		echo "  export AZURE_OPENAI_API_KEY=... AZURE_OPENAI_API_HOST=..."; \
	fi
	helm upgrade --install mini-chat gears/mini-chat/deploy/helm/mini-chat/ \
		--set image.tag="$(MINI_CHAT_TAG)" \
		--set secrets.azureOpenaiApiKey="$${AZURE_OPENAI_API_KEY}" \
		--set secrets.azureOpenaiApiHost="$${AZURE_OPENAI_API_HOST}" \
		--set postgres.host="$${PG_HOST:-postgres.default.svc.cluster.local}" \
		--set postgres.password="$${PG_PASSWORD}"
	kubectl rollout restart deployment/mini-chat
	kubectl rollout status deployment/mini-chat --timeout=120s
	@echo ""
	@echo "mini-chat is running. In a separate terminal run:"
	@echo "  make mini-chat-port-forward"
	@echo "Then access: http://localhost:8087/cf/mini-chat"

## Persistent port-forward with auto-reconnect (run in a separate terminal)
mini-chat-port-forward:
	$(call print_target_banner)
	@echo "Port-forward: localhost:8087 -> svc/mini-chat:8087 (auto-reconnect, Ctrl+C to stop)"
	@while true; do \
		kubectl port-forward svc/mini-chat 8087:8087 2>&1 || true; \
		echo "connection lost, reconnecting in 2s..."; \
		sleep 2; \
	done

## Tear down mini-chat from the cluster
mini-chat-down:
	$(call print_target_banner)
	helm uninstall mini-chat 2>/dev/null || true
	@echo "mini-chat uninstalled"

HELP_PARAMS += RELEASE
help.RELEASE.usage := RELEASE=1
help.RELEASE.targets := mini-chat-docker mini-chat-helm
help.RELEASE.text = Build an optimized (release) image instead of a dev build.

# -------- Main targets --------

.PHONY: all dist check gear-ci ci_test ci_docs build build-debug .cargo-build .split-debug quickstart example run full-make-matrix

# Start server with quickstart config
quickstart:
	$(call print_target_banner)
	$(MAKE) run GEAR=types-registry

# Run server with example gear
example:
	$(call print_target_banner)
	$(MAKE) run GEAR=

# Server presets for `make run PRESET=<preset>`: Cargo features, config file
# (default config/quickstart.yaml) and an optional command run before the server.
RUN_PRESETS := fips mini-chat oop-example
run_features_fips := fips,static-authn,static-authz,single-tenant,static-credstore,otel
run_features_mini-chat := mini-chat,static-authn,static-authz,single-tenant,static-credstore,otel
run_config_mini-chat := config/mini-chat.yaml
run_features_oop-example := oop-example,users-info-example,static-authn,static-authz,static-tenants,static-credstore
run_prebuild_oop-example := cargo build -p calculator --features oop_gear

HELP_PARAMS += PRESET MATRIX_PRESET BENCH_GROUP BENCH_GEAR BENCH_SCENARIO BENCH_VERBOSE
help.PRESET.usage := PRESET=<preset>
help.PRESET.targets := run
help.PRESET.text = Run the example server with a preset feature set and config instead of GEAR.
help.PRESET.values = $(RUN_PRESETS)
help.MATRIX_PRESET.usage := MATRIX_PRESET=<preset>
help.MATRIX_PRESET.targets := full-make-matrix
help.MATRIX_PRESET.text = Which target matrix to run. Default: $(MATRIX_PRESET).
help.MATRIX_PRESET.values = smoke default extended custom
help.BENCH_GROUP.usage := BENCH_GROUP=<group>
help.BENCH_GROUP.targets := make-benchmark
help.BENCH_GROUP.text = Scenario group to benchmark.
help.BENCH_GROUP.values = all-gears specific-gear
help.BENCH_GEAR.usage := BENCH_GEAR=<gear>
help.BENCH_GEAR.targets := make-benchmark
help.BENCH_GEAR.text = Gear for the specific-gear group (implies BENCH_GROUP=specific-gear).
help.BENCH_SCENARIO.usage := BENCH_SCENARIO=<n>
help.BENCH_SCENARIO.targets := make-benchmark
help.BENCH_SCENARIO.text = Run only scenario number <n>.
help.BENCH_VERBOSE.usage := BENCH_VERBOSE=1
help.BENCH_VERBOSE.targets := make-benchmark
help.BENCH_VERBOSE.text = Print each scenario's output.

# Run the example server: default features, one gear (GEAR=<gear>) or a preset (PRESET=fips|mini-chat|oop-example)
# Skipped when GEAR has no corresponding example-server feature (e.g. libs).
run:
	$(call print_target_banner)
ifneq ($(PRESET),)
	$(if $(filter $(PRESET),$(RUN_PRESETS)),,$(error Unknown PRESET '$(PRESET)'. Use one of: $(RUN_PRESETS)))
	$(run_prebuild_$(PRESET))
	cargo run --bin $(EXAMPLE_SERVER_BIN) --features $(run_features_$(PRESET)) -- --config $(or $(run_config_$(PRESET)),config/quickstart.yaml) run $(GEAR_RUN_ARGS)
else ifneq ($(GEAR),)
ifeq ($(GEAR_HAS_SERVER_FEATURE),)
	@echo "SKIP: GEAR=$(GEAR) has no example-server feature — nothing to run."
else
	cargo run --bin $(EXAMPLE_SERVER_BIN) $(GEAR_SERVER_FEATURE_ARGS) -- --config config/quickstart.yaml run $(GEAR_RUN_ARGS)
endif
else
	cargo run --bin $(EXAMPLE_SERVER_BIN) $(GEAR_SERVER_FEATURE_ARGS) -- --config config/quickstart.yaml run $(GEAR_RUN_ARGS)
endif

# Run all quality checks
check: fmt cfs-validate docker-pins clippy lychee security dylint gts-docs test
	$(call print_target_banner)

# Lightweight quality check for gear-scoped CI (gear-scoped-ci.yml).
# Runs only targets that need no extra tools beyond cargo, rustfmt, clippy,
# nextest, and cargo-gears. Skips cfs-validate, lychee, security, dylint,
# gts-docs, test-integration, e2e-local, and openapi.
gear-ci: fmt clippy test
	$(call print_target_banner)

ci_test: fmt clippy
	$(call print_target_banner)

ci_docs: lychee gts-docs
	$(call print_target_banner)

## Build the cf-gears-example-server release binary, or a single gear when GEAR=<gear> is set
.cargo-build:
	$(call print_target_banner)
	$(if $(GEAR),cargo build --release $(GEAR_PKGS) $(GEAR_FEATURE_ARGS) $(GEAR_BUILD_ARGS),cargo build --release --bin $(EXAMPLE_SERVER_BIN) $(EXAMPLE_SERVER_FEATURE_ARGS))

## Split debug symbols into separate artifact(s) and strip the binary.
## Requires platform tools: objcopy (Linux), dsymutil+strip (macOS).
## On Windows MSVC the PDB is already separate; no extra tools needed.
.split-debug:
	$(call print_target_banner)
	cargo xtask split-debug cf-gears-example-server

# Build the cf-gears-example-server with full debuginfo (the 'debugging' profile)
# Artifacts land in target/debugging/ and are not stripped, so no split-debug step
# here. Costs a separate rebuild of the dependency graph; target/debug is untouched.
build-debug:
	cargo build --profile debugging --bin cf-gears-example-server $(E2E_ARGS)
	@echo "binary: target/debugging/cf-gears-example-server"

# Build the release binary, or a single gear when GEAR=<gear> is set.
build:
	$(call print_target_banner)
	$(MAKE) .cargo-build GEAR="$(GEAR)" GEAR_FEATURES="$(GEAR_FEATURES)" GEAR_BUILD_ARGS="$(GEAR_BUILD_ARGS)"

# Build distributable release artifacts.
dist: build
	$(call print_target_banner)
	@if [ -z "$(GEAR)" ]; then $(MAKE) .split-debug; fi

## Run the full Makefile target matrix across all tracked gears (preset: default).
## Override with MATRIX_PRESET=smoke|extended|custom.
MATRIX_PRESET ?= default
full-make-matrix: py-env
	$(call print_target_banner)
	$(PYTHON) tools/scripts/run_make_matrix.py $(MATRIX_PRESET)

## Benchmark make targets: measure time, status, and target/ size.
## Options: BENCH_GROUP=all-gears|specific-gear  BENCH_GEAR=<name>  BENCH_SCENARIO=<num>  BENCH_VERBOSE=1
BENCH_GROUP ?=
BENCH_GEAR ?=
BENCH_SCENARIO ?=
BENCH_VERBOSE ?=
make-benchmark: py-env
	@$(call ensure-log-root,.logs/make-benchmark)
	$(PYTHON) tools/scripts/run_make_benchmark.py \
		$(if $(BENCH_GROUP),--group $(BENCH_GROUP)) \
		$(if $(BENCH_SCENARIO),--scenario $(BENCH_SCENARIO)) \
		$(if $(BENCH_GEAR),--gear $(BENCH_GEAR)) \
		$(if $(filter-out 0,$(BENCH_VERBOSE)),--verbose)

# Run all necessary quality checks and tests using reusable debug artifacts.
all: check test-integration e2e-local openapi
	$(call print_target_banner)
	@echo ""
	@echo "  CONGRATULATIONS! All 'make all' tasks have been completed!"
	@echo ""
	@echo "  Next suggestions:"
	@echo "    - make test-db        # run full DB integration tests"
	@echo "    - make mini-chat-up   # deploy and try the mini-chat demo"
	@echo ""
	@echo "  Tip: run 'git status' to inspect changes."
	@echo ""

# Integration lanes that `make all` and `make ci` run (target-specific LANE, see tests.mk).
all: LANE = toolkit-db-sqlite
ci: LANE = users-info-pg usage-collector-pg usage-collector-ch types-registry-db oagw-db

# Re-run setup when a make file changes.
.setup-stamp: Makefile gears-rust.mk .gears-dna/gears.mk
