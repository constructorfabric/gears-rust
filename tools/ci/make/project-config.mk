# gears-rust-only development, integration, E2E and release targets.

# Python helper scripts must run inside a repository-managed virtual environment.
# PYTHON_BOOTSTRAP is used only to create that environment; PYTHON is the venv interpreter.
PYTHON_BOOTSTRAP ?= $(shell command -v python3 2>/dev/null || command -v python 2>/dev/null || echo python3)
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
EXAMPLE_SERVER_ALL_FEATURES := $(shell $(CF_RUN) features --manifest $(EXAMPLE_SERVER_MANIFEST))
EXAMPLE_SERVER_FEATURES ?= $(subst $(SPACE),$(COMMA),$(filter-out $(EXAMPLE_SERVER_FEATURE_EXCLUDES),$(EXAMPLE_SERVER_ALL_FEATURES)))
EXAMPLE_SERVER_FEATURE_ARGS ?= $(if $(EXAMPLE_SERVER_FEATURES),--features $(EXAMPLE_SERVER_FEATURES),)
E2E_FEATURES_FILE ?= config/e2e-features.txt
E2E_SERVER_FEATURES ?= $(strip $(shell cat $(E2E_FEATURES_FILE) 2>/dev/null))
E2E_SERVER_FEATURE_ARGS ?= $(if $(E2E_SERVER_FEATURES),--features $(E2E_SERVER_FEATURES),)
OPENAPI_SERVER_FEATURES ?= $(E2E_SERVER_FEATURES)
OPENAPI_SERVER_FEATURE_ARGS ?= $(E2E_SERVER_FEATURE_ARGS)

# Nightly toolchain for targets that need unstable rustc flags (currently only
# `shear`, which drives -Zunpretty=expanded). This default serves local runs;
# CI overrides it via `make shear RUST_NIGHTLY=...` so the toolchain it installs
# and caches cannot drift from the one that actually compiles.
RUST_NIGHTLY ?= nightly-2026-08-21


# -------- Utility macros --------

BANNER_WIDTH ?= 80
BANNER_RULE ?= ━
BANNER_PREFIX ?= ▶

define print_target_banner
	@timestamp=$$(date '+%Y-%m-%d %H:%M:%S %Z'); \
	line=$$(printf '%*s' "$(BANNER_WIDTH)" '' | tr ' ' '$(BANNER_RULE)'); \
	if [ -t 1 ] && [ -z "$${NO_COLOR:-}" ]; then \
		bold=$$(printf '\033[1m'); dim=$$(printf '\033[2m'); cyan=$$(printf '\033[36m'); reset=$$(printf '\033[0m'); \
	else \
		bold=; dim=; cyan=; reset=; \
	fi; \
	printf '\n%s%s%s\n%s%s make %s%s %s[%s]%s\n%s%s%s\n' "$$cyan" "$$line" "$$reset" "$$bold" "$(BANNER_PREFIX)" "$@" "$$reset" "$$dim" "$$timestamp" "$$reset" "$$cyan" "$$line" "$$reset"
endef


# Generic server start/stop with cleanup (cross-platform: Linux, Mac, Windows)
# Usage: $(call start_server_and_wait,<command>,<health_url>,<max_wait_seconds>)
# Args:
#   1. command - Full command to start the server (e.g., cargo run --bin server)
#   2. health_url - URL to poll for server readiness (e.g., http://localhost:8080/health)
#   3. max_wait_seconds - Maximum time to wait for server to be ready (e.g., 300)
# Returns: Sets $$SERVER_PID variable for use in the recipe
# Cleanup: Automatically kills server on EXIT/INT/TERM (normal or error)
# Features:
#   - Cross-platform: Works on Linux, Mac, and Windows (Git Bash/WSL/MSYS2)
#   - Logs server output to temp directory for debugging
#   - Exponential backoff polling (1s, 2s, 4s, 8s intervals)
#   - Detects if server dies unexpectedly during startup
#   - Graceful shutdown with SIGTERM, then SIGKILL if needed (or taskkill on Windows)
# Example:
#   @$(call start_server_and_wait,cargo run --bin my-server,http://localhost:8080/health,60); \
#   curl http://localhost:8080/api/data -o output.json
define start_server_and_wait
	TEMP_DIR=$$(if [ -n "$$TEMP" ]; then echo "$$TEMP"; elif [ -n "$$TMP" ]; then echo "$$TMP"; else echo "/tmp"; fi); \
	LOG_FILE="$$TEMP_DIR/server-$$$$.log"; \
	HEALTH_PORT=$$(echo "$(2)" | sed -E 's|.*://[^:]+:([0-9]+).*|\1|'); \
	if [ -n "$$HEALTH_PORT" ] && command -v lsof >/dev/null 2>&1; then \
		STALE_PIDS=$$(lsof -tiTCP:$$HEALTH_PORT -sTCP:LISTEN 2>/dev/null); \
		if [ -n "$$STALE_PIDS" ]; then \
			echo "Killing stale LISTEN processes on port $$HEALTH_PORT (PIDs: $$STALE_PIDS)"; \
			echo "$$STALE_PIDS" | xargs kill 2>/dev/null || true; \
			sleep 1; \
		fi; \
	fi; \
	$(1) > "$$LOG_FILE" 2>&1 & \
	SERVER_PID=$$!; \
	echo "Server started with PID: $$SERVER_PID (log: $$LOG_FILE)"; \
	is_process_running() { \
		if command -v kill >/dev/null 2>&1; then \
			kill -0 $$1 2>/dev/null; \
		elif command -v tasklist >/dev/null 2>&1; then \
			tasklist /FI "PID eq $$1" 2>NUL | grep -q "$$1"; \
		else \
			ps -p $$1 >/dev/null 2>&1; \
		fi; \
	}; \
	kill_process() { \
		PID_TO_KILL=$$1; \
		FORCE=$$2; \
		if command -v kill >/dev/null 2>&1; then \
			if [ "$$FORCE" = "force" ]; then \
				kill -9 $$PID_TO_KILL 2>/dev/null || true; \
			else \
				kill $$PID_TO_KILL 2>/dev/null || true; \
			fi; \
		elif command -v taskkill >/dev/null 2>&1; then \
			if [ "$$FORCE" = "force" ]; then \
				taskkill /PID $$PID_TO_KILL /F /T 2>NUL || true; \
			else \
				taskkill /PID $$PID_TO_KILL /T 2>NUL || true; \
			fi; \
		fi; \
	}; \
	cleanup_server() { \
		if is_process_running $$SERVER_PID; then \
			echo "Stopping server (PID $$SERVER_PID)..."; \
			kill_process $$SERVER_PID; \
			sleep 1; \
			if is_process_running $$SERVER_PID; then \
				echo "Server still running, forcing shutdown..."; \
				kill_process $$SERVER_PID force; \
				sleep 1; \
			fi; \
			wait $$SERVER_PID 2>/dev/null || true; \
			echo "Server stopped."; \
		fi; \
	}; \
	trap cleanup_server EXIT INT TERM; \
	echo "Waiting for $(2) to become ready..."; \
	ELAPSED=0; MAX_WAIT=$(3); SLEEP=1; \
	while ! curl -fsS "$(2)" -o /dev/null 2>/dev/null; do \
		if ! is_process_running $$SERVER_PID; then \
			echo "ERROR: Server process died unexpectedly. Check $$LOG_FILE"; \
			exit 1; \
		fi; \
		if [ $$ELAPSED -ge $$MAX_WAIT ]; then \
			echo "ERROR: Server did not become ready in time. Check $$LOG_FILE"; \
			exit 1; \
		fi; \
		echo "Waiting for server... ($$ELAPSED s)"; \
		sleep $$SLEEP; \
		ELAPSED=$$((ELAPSED + SLEEP)); \
		SLEEP=$$((SLEEP < 8 ? SLEEP*2 : 8)); \
	done; \
	echo "Server is ready!"
endef

# -------- Defaults --------

# -------- Set up --------

.PHONY: setup install-tools check-prereq-local cfs-ensure cfs-validate cfs-repair cfs-validate-kit-local py-env

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

# Re-run setup whenever the tool list changes (Makefile is the source of truth),
# so developers who already have .setup-stamp pick up newly-added tools.
.setup-stamp: $(MAKEFILE_LIST)
	$(call print_target_banner)
	@echo "Installing required development tools..."
	rustup component add clippy
	cargo install lychee
	cargo install cargo-geiger
	cargo install cargo-fuzz
	cargo install --locked cargo-hack@$(HACK_VERSION)
	cargo install --locked cargo-shear --version $(SHEAR_VERSION)
	cargo install gts-validator
	@if echo "$$OS" | grep -iq windows || [ -n "$$COMSPEC" ]; then \
		echo "NOTE: kani-verifier is not supported on Windows; skipping (use WSL2/Docker for Kani)."; \
		echo "Installing cargo-llvm-cov (supported on Windows; needs llvm-tools-preview)..."; \
		rustup component add llvm-tools-preview; \
		cargo install --locked cargo-llvm-cov@$(LLVM_COV_VERSION); \
		if ! command -v nasm >/dev/null 2>&1; then \
			echo "Installing NASM (required by aws-lc-sys on Windows)..."; \
			winget install NASM.NASM --accept-source-agreements --accept-package-agreements || \
				echo "WARNING: NASM auto-install failed. Install manually from https://www.nasm.us/"; \
		fi; \
	else \
		cargo install --locked kani-verifier && \
		cargo kani setup && \
		cargo install --locked cargo-llvm-cov@$(LLVM_COV_VERSION); \
	fi
	@echo "Setup complete. All tools installed."
	@touch .setup-stamp

# -------- Code safety checks --------
#
# Tool Comparison - What Each Tool Checks:
# +-------------+----------------------------------------------------------------------+
# | Tool        | Checks Performed                                                     |
# +-------------+----------------------------------------------------------------------+
# | clippy      | - Idiomatic Rust patterns (e.g., use of .iter() vs into_iter())      |
# |             | - Common mistakes (e.g., unnecessary clones, redundant closures)     |
# |             | - Performance issues (e.g., inefficient string operations)           |
# |             | - Style violations (e.g., naming conventions, formatting)            |
# |             | - Suspicious constructs (e.g., comparison to NaN, unused results)    |
# |             | - Complexity warnings (e.g., too many arguments, cognitive load)     |
# +-------------+----------------------------------------------------------------------+
# | kani        | - Memory safety proofs (buffer overflows, null pointer dereferences) |
# |             | - Arithmetic overflow/underflow in all possible execution paths      |
# |             | - Assertion violations (panics, unwrap failures)                     |
# |             | - Undefined behavior detection                                       |
# |             | - Concurrency issues (data races, deadlocks) with #[kani::proof]     |
# |             | - Custom invariants and postconditions verification                  |
# +-------------+----------------------------------------------------------------------+
# | geiger      | - Unsafe blocks in your code and dependencies                        |
# |             | - FFI (Foreign Function Interface) calls                             |
# |             | - Raw pointer dereferences                                           |
# |             | - Mutable static variables access                                    |
# |             | - Inline assembly usage                                              |
# |             | - Dependency tree visualization of unsafe code usage                 |
# +-------------+----------------------------------------------------------------------+
# | lint        | - Compiler warnings treated as errors (unused variables, imports)    |
# |             | - Dead code detection                                                |
# |             | - Type inference failures                                            |
# |             | - Deprecated API usage                                               |
# |             | - Missing documentation warnings                                     |
# |             | - Ensures clean compilation across all targets and features          |
# +-------------+----------------------------------------------------------------------+

.PHONY: fmt clippy clippy-deep lychee docs-preview kani geiger safety lint dylint dylint-list dylint-test shear gts-docs docker-pins cfs-ensure cfs-repair cfs-validate cfs-validate-kits cfs-validate-kit-local cfs-spec-coverage ensure-submodules

## Verify git submodules (e.g. guidelines/DNA) are initialized; fails otherwise.
ensure-submodules:
	$(call print_target_banner)
	@if git submodule status --recursive 2>/dev/null | grep -q '^-'; then \
		echo "ERROR: Uninitialized git submodules detected. Run 'git submodule update --init --recursive'." && exit 1; \
	fi

CFS ?= cfs
CFS_PIPX_SPEC ?= git+https://github.com/constructorfabric/studio.git
export PATH := $(HOME)/.local/bin:$(PATH)

# Fast two-pass clippy used in PR CI (target: <5 min with sccache).
#
# Pass 1 — one workspace-wide all-features run.
#   Covers every crate, every target, every additive feature combination.
#   80+ "leaf" crates (gears, plugins, SDKs) use `dep:` guards only, so
#   --all-features is sufficient — no --each-feature needed.
#
# Pass 2 — cargo-hack --each-feature on the three crates with real
#   #[cfg(feature = "...")] guards (mutually-exclusive DB backends, otel,
#   fips, db). These account for >95% of all cfg-gated lines in the repo.
#   See GH issue #1574 for original motivation.
#
# Use `make clippy-deep` for the full 182-run matrix (nightly / pre-release).
CLIPPY_FLAGS := -- -D warnings -D clippy::perf
CLIPPY_HACK_CRATES := -p cf-gears-toolkit -p cf-gears-toolkit-db -p cf-gears-toolkit-http
# `_any-backend` is toolkit-db's internal "some backend is on" marker (see its
# [features] block); enabling it *alone* asserts a driver exists while none does,
# which the outbox benchmarks reject with a compile_error!. Not a configuration
# anyone can ship, so it is not worth a lint pass.
CLIPPY_HACK_EXCLUDE := --exclude-features _any-backend

## Full feature-matrix clippy: one pass per (crate × feature).
## ~182 runs — intended for nightly CI and pre-release validation, not PRs.
clippy-deep:
	$(call print_target_banner)
	$(call check_rustup_component,clippy)
	$(call check_tool,cargo-hack)
	cargo hack clippy --workspace --all-targets --each-feature $(CLIPPY_HACK_EXCLUDE) $(CLIPPY_FLAGS)

# Run markdown checks with 'lychee'
lychee: ensure-submodules
	$(call print_target_banner)
	$(call check_tool,lychee)
	lychee --exclude-path 'docs/web-docs' docs examples guidelines gears/system/event-broker/docs studio-kit-gears

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

## The Kani Rust Verifier for checking safety of the code
kani:
	$(call print_target_banner)
	$(call check_tool,kani)
	cargo kani --workspace --all-features

## Run Geiger scanner for unsafe code in dependencies
geiger:
	$(call print_target_banner)
	$(call check_tool,cargo-geiger)
	cd apps/cf-gears-example-server && cargo geiger --all-features

## Check there are no compile time warnings
lint:
	$(call print_target_banner)
	RUSTFLAGS="-D warnings" $(CF_RUN) cargo check --all-targets --all-features

## Validate GTS identifiers in .md and .json files (DE0903)
# Uses gts-validator binary (install via: cargo install gts-validator)

## Check Dockerfile base images are digest-pinned and match rust-toolchain.toml
# Uses PYTHON_BOOTSTRAP, not the venv: this check is pure stdlib text parsing,
# so it must stay runnable without `make py-env` first.
docker-pins:
	$(call print_target_banner)
	$(PYTHON_BOOTSTRAP) tools/scripts/ci.py docker-pins

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

# Check for unused dependencies with cargo-shear.
shear:
	$(call print_target_banner)
	$(call check_tool,cargo-shear)
	cargo +$(RUST_NIGHTLY) shear --expand --deny-warnings

# Run all code safety checks
safety: clippy kani lint dylint # geiger
	$(call print_target_banner)
	@echo "OK. Rust Safety Pipeline complete"

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

# -------- Code security checks --------

.PHONY: deny deny-magika fips-policy security

# Same as `deny`, but with the file-parser `magika` feature enabled so the
# ort/ndarray dependency tree it pulls in is also audited.
deny-magika:
	$(call check_tool,cargo-deny)
	$(call check_tool_version,cargo-deny,>=$(DENY_MIN_VERSION))
	cargo deny --features magika check

## FIPS dependency-graph policy (see deny-fips.toml + ADR 0005).
## Refuses the build if any non-FIPS-validated crypto crate enters the
## --features fips dep graph. Build-time analogue of Go 1.25 fips140=only.
## Run on every PR that touches deps.
fips-policy:
	$(call print_target_banner)
	$(call check_tool,cargo-deny)
	$(call check_tool_version,cargo-deny,>=$(DENY_MIN_VERSION))
	cargo deny --config deny-fips.toml check bans

security: deny fips-policy
	$(call print_target_banner)

# -------- Studio --------

# Validate Constructor Studio artifacts (specs, code, templates).
cfs-validate: cfs-repair
	$(call print_target_banner)
	$(CFS) validate && echo "OK. Constructor Studio validation PASSED" || (echo "ERROR: Constructor Studio validation FAILED"; exit 1)

# Ensure the Constructor Studio CLI is available even when generated runtime
# files are ignored locally or absent in a clean checkout.
cfs-ensure:
	$(call print_target_banner)
	@if ! command -v $(CFS) >/dev/null 2>&1; then \
		echo "cfs not found; installing $(CFS_PIPX_SPEC) via pipx"; \
		if ! command -v pipx >/dev/null 2>&1; then \
			echo "ERROR: pipx is required before running this target"; \
			exit 1; \
		else \
			pipx install $(CFS_PIPX_SPEC); \
		fi; \
	fi
	@if ! command -v $(CFS) >/dev/null 2>&1; then \
		echo "ERROR: cfs was installed but is not on PATH"; \
		exit 1; \
	fi

## Repair ignored/generated Constructor Studio runtime files before validation.
cfs-repair: cfs-ensure
	$(call print_target_banner)
	$(CFS) init --yes

## Check Constructor Studio spec-to-code traceability coverage.
cfs-spec-coverage: cfs-repair
	$(call print_target_banner)
	$(CFS) spec-coverage --min-coverage 80

## Validate registered Constructor Studio kits.
cfs-validate-kits: cfs-repair
	$(call print_target_banner)
	$(CFS) validate-kits

## Validate the local studio-kit-gears checkout as a kit directory.
cfs-validate-kit-local: cfs-repair
	$(call print_target_banner)
	cd studio-kit-gears && $(CFS) validate-kits .

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

# -------- Development and auto fix --------

.PHONY: dev dev-fmt dev-clippy dev-test

## Run tests in development mode
dev-test: cf-require-nextest
	$(call print_target_banner)
	cargo nextest run --workspace

## Auto-fix code formatting
dev-fmt:
	$(call print_target_banner)
	cargo fmt --all

## Auto-fix clippy warnings
dev-clippy:
	$(call print_target_banner)
	cargo clippy --workspace --all-targets --all-features --fix --allow-dirty

# Auto-fix formatting and clippy warnings
dev: dev-fmt dev-clippy dev-test
	$(call print_target_banner)

# -------- Optional GEAR= scope for top-level targets --------
#
# Scope fmt, clippy, build, test, run, e2e-local, openapi, and coverage
# to a single gear instead of the whole workspace.
#
# Package resolution uses `cargo gears ls packages` with a regex filter
# on Cargo package names, so naming conventions (cf-gears-*, bss-*, bare)
# are handled automatically.
#
# Examples:
#   make fmt GEAR=file-parser         -> cargo fmt -p cf-gears-file-parser -p cf-gears-file-parser-sdk --check
#   make test GEAR=ledger             -> cargo nextest run -p bss-ledger -p bss-ledger-sdk ... (+ reverse deps)
#   make run GEAR=file-parser         -> cargo run --bin cf-gears-example-server --features file-parser,...

# --- User-facing knobs ---
GEAR ?=
GEAR_BUILD_ARGS ?=
GEAR_TEST_ARGS ?=
GEAR_RUN_ARGS ?=
# Extra Cargo features to pass with --features (e.g. GEAR_FEATURES=integration).
GEAR_FEATURES ?=
# Clippy flags for gear-scoped runs.
GEAR_CLIPPY_ARGS ?= --all-targets --all-features -- -D warnings
E2E_SIDECAR_PREREQ := $(if $(GEAR),$(if $(filter file-storage,$(GEAR)),.e2e-sidecar-build,),.e2e-sidecar-build)
E2E_SIDECAR_ENV := $(if $(GEAR),$(if $(filter file-storage,$(GEAR)),FS_SIDECAR_BINARY=target/debug/sidecar,E2E_SKIP_SIDECAR=1),FS_SIDECAR_BINARY=target/debug/sidecar)

# --- Package resolution ---
# Regex to match Cargo package names belonging to this gear.
# Handles: <gear>, <gear>-sdk, cf-gears-<gear>, cf-gears-<gear>-sdk,
#          <prefix>-<gear>, <prefix>-<gear>-sdk (e.g. bss-ledger).
# Override with GEAR_NAME_REGEXP= for non-standard naming.
GEAR_NAME_REGEXP ?= ^(?:(?:cf-gears-)?(?:[a-z]+-)?)?$(GEAR)(?:-sdk)?$$
# Package resolution is performed by tools/ci/run.py at recipe execution.

# --- Derived variables ---
GEAR_FEATURE_ARGS := $(if $(GEAR_FEATURES),--features $(GEAR_FEATURES),)
# Empty scoped test selections retain nextest's default failure policy.
# E2E suite key: kebab-case gear name → snake_case directory name.
GEAR_E2E_KEY := $(subst -,_,$(GEAR))
GEAR_E2E_TARGET ?= testing/e2e/suites/$(GEAR_E2E_KEY)
GEAR_E2E_SCOPE := $(if $(GEAR),$(GEAR_E2E_TARGET) $(E2E_TARGET),$(E2E_TARGET))
# Coverage: pass the first resolved package + e2e target path to coverage.py.
GEAR_COVERAGE_ARGS = $(if $(GEAR),--package "$$package" --e2e-target $(GEAR_E2E_TARGET),)

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
