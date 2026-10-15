# -------- Main targets --------

.PHONY: all dist check gear-ci ci ci_test ci_docs build build-debug .cargo-build .split-debug quickstart example mini-chat mini-chat-docker mini-chat-helm mini-chat-helm-template mini-chat-up mini-chat-down mini-chat-port-forward full-make-matrix

# Start server with quickstart config
quickstart:
	$(call print_target_banner)
	$(MAKE) run GEAR=types-registry

# Run server with example gear
example:
	$(call print_target_banner)
	cargo run --bin $(EXAMPLE_SERVER_BIN) $(EXAMPLE_SERVER_FEATURE_ARGS) -- --config config/quickstart.yaml run

# Run the default server, or the example server with only one gear feature when GEAR=<gear> is set.
# Skipped when GEAR has no corresponding example-server feature (e.g. libs).
run:
	$(call print_target_banner)
ifneq ($(GEAR),)
ifeq ($(GEAR_HAS_SERVER_FEATURE),)
	@echo "SKIP: GEAR=$(GEAR) has no example-server feature — nothing to run."
else
	cargo run --bin $(EXAMPLE_SERVER_BIN) $(GEAR_SERVER_FEATURE_ARGS) -- --config config/quickstart.yaml run $(GEAR_RUN_ARGS)
endif
else
	cargo run --bin $(EXAMPLE_SERVER_BIN) $(GEAR_SERVER_FEATURE_ARGS) -- --config config/quickstart.yaml run $(GEAR_RUN_ARGS)
endif

## Run server with fips gear
fips:
	$(call print_target_banner)
	cargo run --bin cf-gears-example-server --features fips,static-authn,static-authz,single-tenant,static-credstore,otel -- --config config/quickstart.yaml run

## Run server with out-of-process example gear
oop-example:
	$(call print_target_banner)
	cargo build -p calculator --features oop_gear
	cargo run --bin cf-gears-example-server --features oop-example,users-info-example,static-authn,static-authz,static-tenants,static-credstore -- --config config/quickstart.yaml run

# Run all quality checks
check: fmt cfs-validate docker-pins clippy lychee security dylint gts-docs test
	$(call print_target_banner)

# Lightweight quality check for gear-scoped CI (gear-scoped-ci.yml).
# Runs only targets that need no extra tools beyond cargo, rustfmt, clippy,
# nextest, and cargo-gears. Skips cfs-validate, lychee, security, dylint,
# gts-docs, test-sqlite, e2e-local, and openapi.
gear-ci: fmt clippy test
	$(call print_target_banner)

ci_test: fmt clippy
	$(call print_target_banner)

ci_docs: lychee gts-docs
	$(call print_target_banner)


## Build the cf-gears-example-server release binary, or a single gear when GEAR=<gear> is set
.cargo-build:
	$(call print_target_banner)
	$(if $(GEAR),$(CF_RUN) cargo build --release $(GEAR_FEATURE_ARGS) $(GEAR_BUILD_ARGS),cargo build --release --bin $(EXAMPLE_SERVER_BIN) $(EXAMPLE_SERVER_FEATURE_ARGS))

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
	$(MAKE) .cargo-build GEAR=$(GEAR) GEAR_FEATURES=$(GEAR_FEATURES) GEAR_BUILD_ARGS=$(GEAR_BUILD_ARGS)

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
all: check test-sqlite e2e-local openapi
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
