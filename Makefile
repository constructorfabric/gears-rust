CI := 1

OPENAPI_URL ?= http://127.0.0.1:8087/cf/openapi.json
OPENAPI_OUT ?= docs/api/api.json

# E2E feature set (single source of truth: config/e2e-features.txt)
E2E_FEATURES ?= $(strip $(shell cat config/e2e-features.txt 2>/dev/null))
E2E_ARGS ?= $(if $(E2E_FEATURES),--features $(E2E_FEATURES),)

# -------- Utility macros --------

define check_tool
    @command -v $(1) >/dev/null || (echo "ERROR: $(1) is not installed. Run 'make setup' to install required tools." && exit 1)
endef

define check_rustup_component
    @command -v rustup >/dev/null || (echo "ERROR: rustup not installed. Install rustup or run 'make setup'." && exit 1)
	@rustup component list --installed | grep -q '^$(1)' || (echo "ERROR: $(1) component not installed. Run 'rustup component add $(1)' or 'make setup'." && exit 1)
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

# Show the help message with list of commands (default target)
help:
	@python3 tools/scripts/make_help.py Makefile


# -------- Set up --------
# Note: .setup-stamp should be added to .gitignore

.PHONY: setup

## Install all required development tools
setup: .setup-stamp

.setup-stamp:
	@echo "Installing required development tools..."
	rustup component add clippy
	cargo install lychee
	cargo install cargo-geiger
	cargo install cargo-deny
	cargo install cargo-dylint
	cargo install dylint-link
	cargo install cargo-fuzz
	@if echo "$$OS" | grep -iq windows || [ -n "$$COMSPEC" ]; then \
		echo "WARNING: kani-verifier and cargo-llvm-cov installation skipped on Windows."; \
		echo "These tools are not supported on Windows. Use WSL2 or Docker to install instead."; \
	else \
		cargo install --locked kani-verifier && \
		cargo kani setup && \
		cargo install cargo-llvm-cov; \
	fi
	@echo "Setup complete. All tools installed."
	@touch .setup-stamp

# -------- Code formatting --------

.PHONY: fmt

# Check code formatting
fmt:
	$(call check_rustup_component,rustfmt)
	cargo fmt --all -- --check

# -------- Module naming validation --------

.PHONY: validate-module-names

## Validate module folder names follow kebab-case convention
validate-module-names:
	@python3 tools/scripts/validate_module_names.py

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
# | dylint      | - Project-specific architectural conventions (custom lints)          |
# |             | - DTO declaration and placement (only in api/rest folders)           |
# |             | - DTO isolation (no references from domain/contract layers)          |
# |             | - API endpoint versioning requirements (e.g., /users/v1/users)       |
# |             | - Contract layer purity (no serde, HTTP types, or ToSchema)          |
# |             | - Layer separation and dependency rules enforcement                  |
# |             | - Use 'make dylint-list' to see all available custom lints           |
# +-------------+----------------------------------------------------------------------+

.PHONY: clippy lychee kani geiger safety lint dylint dylint-list dylint-test gts-docs gts-docs-vendor gts-docs-release gts-docs-vendor-release gts-docs-test cypilot-validate cypilot-spec-coverage

# Run clippy linter (excludes gts-rust submodule which has its own lint settings)
clippy:
	$(call check_rustup_component,clippy)
	cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::perf

# Check cypilot spec-to-code traceability coverage
cypilot-spec-coverage:
	@python3 .cypilot/.core/skills/cypilot/scripts/cypilot.py spec-coverage --min-coverage 80

# Validate cypilot artifacts (specs, code, templates)
cypilot-validate:
	@python3 .cypilot/.core/skills/cypilot/scripts/cypilot.py validate && echo "OK. cypilot validation PASSED" || (echo "ERROR: cypilot validation FAILED"; exit 1)

# Run markdown checks with 'lychee'
lychee:
	$(call check_tool,lychee)
	lychee docs examples tools/dylint_lints guidelines

## The Kani Rust Verifier for checking safety of the code
kani:
	$(call check_tool,kani)
	cargo kani --workspace --all-features

## Run Geiger scanner for unsafe code in dependencies
geiger:
	$(call check_tool,cargo-geiger)
	cd apps/hyperspot-server && cargo geiger --all-features

## Check there are no compile time warnings
lint:
	RUSTFLAGS="-D warnings" cargo check --workspace --all-targets --all-features

## Validate GTS identifiers in .md and .json files (DE0903)
# Uses gts-docs-validator from apps/gts-docs-validator
# Vendor enforcement is available via the gts-docs-vendor target (--vendor x)

# REDUCING THE SCOPE OF THE VALIDATION UNTIL IT IS STABLE
gts-docs:
	cargo run -p gts-docs-validator -- \
		--exclude "target/*" \
		--exclude "docs/api/*" \
		--exclude "modules/chat-engine/*" \
		--exclude "**/helm/*/templates/*" \
		docs modules libs examples

## Validate GTS docs with vendor check (ensures all IDs use vendor "x")
gts-docs-vendor:
	cargo run -p gts-docs-validator -- \
		--vendor x \
		--exclude "target/*" \
		--exclude "docs/api/*" \
		--exclude "modules/chat-engine/*" \
		--exclude "**/helm/*/templates/*" \
		docs modules libs examples

## Validate GTS identifiers (release build)
gts-docs-release:
	cargo run --release -p gts-docs-validator -- \
		--exclude "target/*" \
		--exclude "docs/api/*" \
		--exclude "modules/chat-engine/*" \
		--exclude "**/helm/*/templates/*" \
		docs modules libs examples

## Validate GTS docs with vendor check (release build)
gts-docs-vendor-release:
	cargo run --release -p gts-docs-validator -- \
		--vendor x \
		--exclude "target/*" \
		--exclude "docs/api/*" \
		--exclude "modules/chat-engine/*" \
		--exclude "*/helm/*/templates/*" \
		docs modules libs examples

install-tools:
	@command -v cargo-nextest >/dev/null 2>&1 || cargo install cargo-nextest

## Run tests for GTS documentation validator
gts-docs-test: install-tools
	cargo nextest run -p gts-docs-validator

## List all custom project compliance lints (see tools/dylint_lints/README.md)
dylint-list:
	@cd tools/dylint_lints && \
	DYLINT_LIBS=$$(find target/release -maxdepth 1 \( -name "libde*@*.so" -o -name "libde*@*.dylib" -o -name "de*@*.dll" \) -type f | sort -u); \
	if [ -z "$$DYLINT_LIBS" ]; then \
		echo "ERROR: No dylint libraries found. Run 'make dylint' first to build them."; \
		exit 1; \
	fi; \
	for lib in $$DYLINT_LIBS; do \
		echo "=== $$lib ==="; \
		cargo dylint list --lib-path "$$lib"; \
	done

## Test dylint lints on UI test cases (compile and verify violations)
dylint-test: install-tools
	@cd tools/dylint_lints && cargo nextest run

# Run project compliance dylint lints on the workspace (see `make dylint-list`)
dylint:
	$(call check_tool,cargo-dylint)
	$(call check_tool,dylint-link)
	cargo +nightly-2026-01-22 dylint --all --workspace

# Run all code safety checks
safety: clippy kani lint dylint # geiger
	@echo "OK. Rust Safety Pipeline complete"

# -------- Code security checks --------

.PHONY: deny security

## Check licenses and dependencies
deny:
	$(call check_tool,cargo-deny)
	cargo deny check

security: deny

# -------- API and docs --------

.PHONY: openapi

# Generate OpenAPI spec from running hyperspot-server
openapi:
	@command -v curl >/dev/null || (echo "curl is required to generate OpenAPI spec" && exit 1)
	@echo "Starting hyperspot-server to generate OpenAPI spec..."
	@$(call start_server_and_wait,cargo run --bin hyperspot-server $(E2E_ARGS) -- --config config/quickstart.yaml,$(OPENAPI_URL),300) && \
	echo "Fetching OpenAPI spec..." && \
	mkdir -p $$(dirname "$(OPENAPI_OUT)") && \
	curl -fsS "$(OPENAPI_URL)" -o "$(OPENAPI_OUT)" && \
	echo "Sorting OpenAPI JSON for deterministic ordering..." && \
	python3 tools/scripts/sort_openapi_json.py "$(OPENAPI_OUT)" && \
	echo "OpenAPI spec saved to $(OPENAPI_OUT)"

# -------- Development and auto fix --------

.PHONY: dev dev-fmt dev-clippy dev-test

## Run tests in development mode
dev-test: install-tools
	cargo nextest run --workspace

## Auto-fix code formatting
dev-fmt:
	cargo fmt --all

## Auto-fix clippy warnings
dev-clippy:
	cargo clippy --workspace --all-targets --all-features --fix --allow-dirty

# Auto-fix formatting and clippy warnings
dev: dev-fmt dev-clippy dev-test

# -------- Tests --------

.PHONY: test test-no-macros test-macros test-sqlite test-pg test-mysql test-db test-users-info-pg

# Run all tests
test: install-tools
	cargo nextest run --workspace

test-no-macros: install-tools
	cargo nextest run --workspace --exclude cf-modkit-macros-tests --exclude cf-modkit-db-macros

test-macros: install-tools
	cargo nextest run -p cf-modkit-db-macros
	cargo nextest run -p cf-modkit-macros-tests

## Run SQLite integration tests
test-sqlite: install-tools
	cargo nextest run -p cf-modkit-db --features sqlite,integration,preview-outbox
	cargo build -p cf-modkit-db --examples --features sqlite,preview-outbox

## Run PostgreSQL integration tests
test-pg: install-tools
	cargo nextest run -p cf-modkit-db --features pg,integration,preview-outbox

## Run MySQL integration tests
test-mysql: install-tools
	cargo nextest run -p cf-modkit-db --features mysql,integration,preview-outbox

# Run all database integration tests
test-db: test-sqlite test-pg test-mysql

## Run users-info module integration tests
test-users-info-pg: install-tools
	cargo nextest run -p users-info --features "integration"

# -------- Benchmarks --------

.PHONY: bench-pg bench-pg-profiler bench-mysql bench-mariadb bench-sqlite bench-db \
       bench-pg-longhaul bench-mysql-longhaul bench-mariadb-longhaul bench-sqlite-longhaul bench-db-longhaul

## Run outbox throughput benchmarks against PostgreSQL
bench-pg:
	cargo bench -p cf-modkit-db --features pg,preview-outbox --bench outbox_throughput -- postgres

## Run outbox throughput benchmarks against MySQL
bench-mysql:
	cargo bench -p cf-modkit-db --features mysql,preview-outbox --bench outbox_throughput -- mysql

## Run outbox throughput benchmarks against MariaDB
bench-mariadb:
	cargo bench -p cf-modkit-db --features mysql,preview-outbox --bench outbox_throughput -- mariadb

## Run outbox throughput benchmarks against SQLite
bench-sqlite:
	cargo bench -p cf-modkit-db --features sqlite,preview-outbox --bench outbox_throughput -- sqlite

## Run outbox throughput benchmarks against all database engines
bench-db: bench-pg bench-mysql bench-mariadb bench-sqlite

## Run long-haul (1M+10M) outbox benchmarks against PostgreSQL
bench-pg-longhaul:
	cargo bench -p cf-modkit-db --features pg,preview-outbox --bench outbox_throughput -- postgres_longhaul

## Run long-haul (1M+10M) outbox benchmarks against MySQL
bench-mysql-longhaul:
	cargo bench -p cf-modkit-db --features mysql,preview-outbox --bench outbox_throughput -- mysql_longhaul

## Run long-haul (1M+10M) outbox benchmarks against MariaDB
bench-mariadb-longhaul:
	cargo bench -p cf-modkit-db --features mysql,preview-outbox --bench outbox_throughput -- mariadb_longhaul

## Run long-haul (100K 1P) outbox benchmarks against SQLite
bench-sqlite-longhaul:
	cargo bench -p cf-modkit-db --features sqlite,preview-outbox --bench outbox_throughput -- sqlite_longhaul

## Run long-haul outbox benchmarks against all database engines
bench-db-longhaul: bench-pg-longhaul bench-mysql-longhaul bench-mariadb-longhaul bench-sqlite-longhaul

# -------- E2E tests --------

.PHONY: e2e e2e-local e2e-local-smoke e2e-mini-chat e2e-docker e2e-docker-smoke

# Run E2E tests in Docker (default)
e2e: e2e-docker

## Run E2E tests in Docker environment
e2e-docker:
	python3 tools/scripts/ci.py e2e-docker $(E2E_ARGS)

## Run E2E smoke tests in Docker (only tests marked @pytest.mark.smoke)
e2e-docker-smoke:
	python3 tools/scripts/ci.py e2e-docker $(E2E_ARGS) -- -m smoke

# Run E2E tests locally
e2e-local:
	python3 tools/scripts/ci.py e2e-local

## Run E2E smoke tests locally (only tests marked @pytest.mark.smoke)
e2e-local-smoke:
	python3 tools/scripts/ci.py e2e-local --smoke

MINI_CHAT_FEATURES = mini-chat,static-authn,static-authz,single-tenant,static-credstore
MINI_CHAT_K8S_FEATURES = $(MINI_CHAT_FEATURES),k8s

MINI_CHAT_IMAGE ?= hyperspot-mini-chat
MINI_CHAT_TAG   ?= $(shell git rev-parse --short HEAD 2>/dev/null || echo latest)

## Run mini-chat E2E tests (separate binary with mini-chat features)
e2e-mini-chat:
	cargo build --bin hyperspot-server --features=$(MINI_CHAT_FEATURES)
	E2E_BINARY=target/debug/hyperspot-server \
		python3 -m pytest testing/e2e/modules/mini_chat/ --mode offline -vv

# -------- Code coverage --------

.PHONY: coverage coverage-unit coverage-e2e-local check-prereq-e2e-local

# Generate code coverage report (unit + e2e-local tests)
coverage:
	$(call check_tool,cargo-llvm-cov)
	python3 tools/scripts/coverage.py combined

# Generate code coverage report (unit tests only)
coverage-unit:
	$(call check_tool,cargo-llvm-cov)
	python3 tools/scripts/coverage.py unit

## Ensure needed packages and programs installed for local e2e testing
check-prereq-e2e-local:
	python3 tools/scripts/check_local_env.py --mode e2e-local

# Generate code coverage report (e2e-local tests only)
coverage-e2e-local: check-prereq-e2e-local
	$(call check_tool,cargo-llvm-cov)
	python3 tools/scripts/coverage.py e2e-local

# -------- Fuzzing --------

.PHONY: fuzz fuzz-build fuzz-list fuzz-run fuzz-clean fuzz-corpus

## Check cargo-fuzz is installed (required for fuzzing)
fuzz-install:
	$(call check_tool,cargo-fuzz)

## Build all fuzz targets
fuzz-build: fuzz-install
	cargo +nightly fuzz build --fuzz-dir tools/fuzz

## List all available fuzz targets
fuzz-list: fuzz-install
	cargo +nightly fuzz list --fuzz-dir tools/fuzz

## Run a specific fuzz target (use FUZZ_TARGET=name)
## Example: make fuzz-run FUZZ_TARGET=fuzz_odata_filter FUZZ_SECONDS=60
fuzz-run: fuzz-install
	@if [ -z "$(FUZZ_TARGET)" ]; then \
		echo "ERROR: FUZZ_TARGET is required. Example: make fuzz-run FUZZ_TARGET=fuzz_odata_filter"; \
		exit 1; \
	fi
	cargo +nightly fuzz run --fuzz-dir tools/fuzz $(FUZZ_TARGET) -- -max_total_time=$(or $(FUZZ_SECONDS),60)

## Run all fuzz targets for a short time (smoke test)
fuzz: fuzz-build
	@echo "Running all fuzz targets for 30 seconds each..."
	@FAILED=0; \
	for target in $$(cargo +nightly fuzz list --fuzz-dir tools/fuzz); do \
		echo "=== Fuzzing $$target ==="; \
		cargo +nightly fuzz run --fuzz-dir tools/fuzz $$target -- -max_total_time=30 || FAILED=1; \
	done; \
	if [ $$FAILED -ne 0 ]; then \
		echo "Fuzzing found crashes. Check tools/fuzz/artifacts/ for details."; \
		exit 1; \
	fi
	@echo "Fuzzing complete. No crashes found."

## Clean fuzzing artifacts and corpus
fuzz-clean:
	rm -rf tools/fuzz/artifacts/
	rm -rf tools/fuzz/corpus/*/
	rm -rf tools/fuzz/target/

## Minimize corpus for a specific target
fuzz-corpus: fuzz-install
	@if [ -z "$(FUZZ_TARGET)" ]; then \
		echo "ERROR: FUZZ_TARGET is required. Example: make fuzz-corpus FUZZ_TARGET=fuzz_odata_filter"; \
		exit 1; \
	fi
	cargo +nightly fuzz cmin --fuzz-dir tools/fuzz $(FUZZ_TARGET)

# -------- Main targets --------

.PHONY: all check ci build cargo-build split-debug quickstart example mini-chat mini-chat-docker mini-chat-helm mini-chat-helm-template mini-chat-up mini-chat-down mini-chat-port-forward

# Start server with quickstart config
quickstart:
	mkdir -p data
	cargo run --bin hyperspot-server -- --config config/quickstart.yaml run

## Run server with example module
example:
	cargo run --bin hyperspot-server $(E2E_ARGS) -- --config config/quickstart.yaml run

# mini-chat targets are for running the mini-chat module locally and in Kubernetes, with options for building Docker images and deploying with Helm.
## Run server with fips module
fips:
	cargo run --bin hyperspot-server --features fips,static-authn,static-authz,single-tenant,static-credstore,otel -- --config config/quickstart.yaml run

## Run server with mini-chat module
mini-chat:
	cargo run --bin hyperspot-server --features mini-chat,static-authn,static-authz,single-tenant,static-credstore,otel -- --config config/mini-chat.yaml run

## Build mini-chat Docker image for K8s (dev build by default, RELEASE=1 for optimized)
## On linux: builds on host (reuses local target/), then packages the binary.
## On other OS: full multi-stage Docker build with BuildKit caching.
MINI_CHAT_PROFILE = $(if $(RELEASE),release,dev)
MINI_CHAT_CARGO_RELEASE_FLAG = $(if $(RELEASE),--release,)
MINI_CHAT_TARGET_DIR = $(or $(CARGO_TARGET_DIR),target)/$(if $(RELEASE),release,debug)

mini-chat-docker:
ifeq ($(shell uname -s),Linux)
	@echo "==> Linux host: building on host, packaging into image"
	cargo build $(MINI_CHAT_CARGO_RELEASE_FLAG) --bin hyperspot-server --package=hyperspot-server \
		--features "$(MINI_CHAT_K8S_FEATURES)"
	@mkdir -p .docker-stage
	@cp $(MINI_CHAT_TARGET_DIR)/hyperspot-server .docker-stage/hyperspot-server
	DOCKER_BUILDKIT=1 docker build \
		-f modules/mini-chat/deploy/docker/mini-chat-prebuilt.Dockerfile \
		--build-arg BINARY_PATH=".docker-stage/hyperspot-server" \
		-t $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) .
	@rm -rf .docker-stage
else
	@echo "==> Non-linux host: full Docker build"
	DOCKER_BUILDKIT=1 docker build \
		-f modules/mini-chat/deploy/docker/mini-chat.Dockerfile \
		--build-arg CARGO_FEATURES="$(MINI_CHAT_K8S_FEATURES)" \
		--build-arg BUILD_PROFILE="$(MINI_CHAT_PROFILE)" \
		-t $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) .
endif

## Deploy mini-chat Helm chart to local K8s cluster (build + load + install)
mini-chat-helm: mini-chat-docker
	@if command -v k3s >/dev/null 2>&1; then \
		docker save $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) | sudo k3s ctr images import -; \
	elif command -v minikube >/dev/null 2>&1; then \
		minikube ssh "docker rmi -f $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG) 2>/dev/null" || true; \
		minikube image load $(MINI_CHAT_IMAGE):$(MINI_CHAT_TAG); \
	else \
		echo "ERROR: k3s or minikube required"; exit 1; \
	fi
	helm upgrade --install mini-chat modules/mini-chat/deploy/helm/mini-chat/ \
		--set image.tag="$(MINI_CHAT_TAG)" \
		--set secrets.azureOpenaiApiKey="$${AZURE_OPENAI_API_KEY}" \
		--set secrets.azureOpenaiApiHost="$${AZURE_OPENAI_API_HOST}" \
		--set postgres.host="$${PG_HOST:-postgres.default.svc.cluster.local}" \
		--set postgres.password="$${PG_PASSWORD}"
	kubectl rollout restart deployment/mini-chat
	kubectl rollout status deployment/mini-chat --timeout=120s

## Render mini-chat Helm templates (dry-run)
mini-chat-helm-template:
	helm template mini-chat modules/mini-chat/deploy/helm/mini-chat/

## One-command: ensure minikube is up, deploy latest chart, port-forward
## Usage: make mini-chat-up
## If image was rebuilt (make mini-chat-docker), re-run this to pick it up.
mini-chat-up:
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
	helm upgrade --install mini-chat modules/mini-chat/deploy/helm/mini-chat/ \
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
	@echo "Port-forward: localhost:8087 -> svc/mini-chat:8087 (auto-reconnect, Ctrl+C to stop)"
	@while true; do \
		kubectl port-forward svc/mini-chat 8087:8087 2>&1 || true; \
		echo "connection lost, reconnecting in 2s..."; \
		sleep 2; \
	done

## Tear down mini-chat from the cluster
mini-chat-down:
	helm uninstall mini-chat 2>/dev/null || true
	@echo "mini-chat uninstalled"

oop-example:
	cargo build -p calculator --features oop_module
	cargo run --bin hyperspot-server --features oop-example,users-info-example,static-authn,static-authz,static-tenants,static-credstore -- --config config/quickstart.yaml run

# Run all quality checks
check: .setup-stamp fmt cypilot-validate clippy lychee security dylint-test dylint gts-docs test

ci_test: fmt clippy

ci_docs: lychee

# Run CI pipeline locally, requires docker
ci: fmt clippy test-no-macros test-macros test-db deny test-users-info-pg lychee dylint dylint-test

# Build the hyperspot-server release binary using the stable toolchain.
cargo-build:
	cargo +stable build --release --bin hyperspot-server $(E2E_ARGS)

# Split debug symbols into separate artifact(s) and strip the binary.
# Requires platform tools: objcopy (Linux), dsymutil+strip (macOS).
# On Windows MSVC the PDB is already separate; no extra tools needed.
split-debug:
	cargo xtask split-debug hyperspot-server

# Build the release binary, then split debug symbols.
# Use 'make cargo-build' if you don't need stripped artifacts or lack
# platform debug-splitting tools (objcopy, dsymutil).
build: cargo-build split-debug

# Run all necessary quality checks and tests and then build the release binary
all: build check test-sqlite e2e-local openapi
	@echo ""
	@echo "============================================================="
	@echo "  CONGRATULATIONS! All 'make all' tasks have been completed!"
	@echo ""
	@echo "  Next suggestions:"
	@echo "    - make test-db        # run full DB integration tests"
	@echo "    - make mini-chat-up   # deploy and try the mini-chat demo"
	@echo ""
	@echo "  Tip: run 'git status' to inspect changes."
	@echo "============================================================="
