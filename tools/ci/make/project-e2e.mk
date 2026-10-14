# -------- E2E tests --------

.PHONY: e2e e2e-local e2e-local-smoke e2e-mini-chat e2e-docker e2e-docker-smoke e2e-tr-authz e2e-usage-collector e2e-usage-collector-timescaledb e2e-usage-collector-clickhouse e2e-event-broker e2e-oop

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

# Run E2E tests locally. Three ways to use it:
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
# tr-authz profile lane start their own server, so plain `make e2e-local` and
# GEAR= runs skip them; run them via their own targets (e2e-mini-chat,
# e2e-usage-collector, e2e-tr-authz). All feature/config/sidecar/gear knowledge
# lives in config/e2e-launcher.yaml and testing/e2e/suites/<suite>/e2e.yaml, so
# this recipe stays suite-agnostic.
e2e-local: py-env
	$(call print_target_banner)
	$(PYTHON) tools/scripts/run_e2e.py --suite "$(SUITE)" --gear "$(GEAR)" -- $(E2E_TARGET)

## Run RG + AuthZ barrier E2E tests with tr-authz-plugin going through TR -> RG
e2e-tr-authz: py-env
	$(call print_target_banner)
	$(PYTHON) tools/scripts/run_e2e.py --suite resource-group --profile tr-authz --

## Run E2E smoke tests locally (only tests marked @pytest.mark.smoke)
e2e-local-smoke: py-env
	$(call print_target_banner)
	$(PYTHON) tools/scripts/run_e2e.py --suite "$(SUITE)" --gear "$(GEAR)" --smoke -- $(E2E_TARGET)

MINI_CHAT_FEATURES = mini-chat,static-authn,static-authz,single-tenant,static-credstore
MINI_CHAT_K8S_FEATURES = $(MINI_CHAT_FEATURES),k8s

MINI_CHAT_IMAGE ?= cf-gears-mini-chat
MINI_CHAT_TAG   ?= $(shell git rev-parse --short HEAD 2>/dev/null || echo latest)

## Run mini-chat E2E tests (alias for focused local E2E)
e2e-mini-chat:
	$(call print_target_banner)
	$(MAKE) e2e-local SUITE=mini-chat

UC_E2E_FEATURES = usage-collector,static-tenants,static-authn,static-authz

## Run usage-collector E2E tests against TimescaleDB (dedicated binary; Docker required)
## Shared UC_E2E_FEATURES plus this backend's storage plugin. The two plugins
## cannot share a binary: every linked gear is initialized, and both fail init
## without their own live database. Hence two builds, two runs.
e2e-usage-collector-timescaledb: py-env
	$(call print_target_banner)
	cargo build --bin cf-gears-example-server --features=$(UC_E2E_FEATURES),timescaledb-usage-collector
	E2E_BINARY=target/debug/cf-gears-example-server UC_E2E_BACKEND=timescaledb \
		$(PYTHON) -m pytest testing/e2e/suites/usage_collector/ -vv $(E2E_TARGET)

## Run usage-collector E2E tests against ClickHouse
## (dedicated binary + ClickHouse container; Docker required)
e2e-usage-collector-clickhouse: py-env
	$(call print_target_banner)
	cargo build --bin cf-gears-example-server --features=$(UC_E2E_FEATURES),clickhouse-usage-collector
	E2E_BINARY=target/debug/cf-gears-example-server UC_E2E_BACKEND=clickhouse \
		$(PYTHON) -m pytest testing/e2e/suites/usage_collector/ -vv $(E2E_TARGET)

## Run usage-collector E2E tests against both storage backends.
## Sequential even under `make -j`: both lanes build the same
## target/debug/cf-gears-example-server path, bind 127.0.0.1:8088, and share
## ~/.cf-gears, so they must never run concurrently.
e2e-usage-collector:
	$(call print_target_banner)
	$(MAKE) e2e-usage-collector-timescaledb
	$(MAKE) e2e-usage-collector-clickhouse

## Run event-broker E2E tests (its own standalone binary, not a cf-gears-example-server feature)
e2e-event-broker: py-env
	$(call print_target_banner)
	cargo build -p cf-gears-event-broker --bin cf-gears-event-broker-server
	E2E_BINARY=target/debug/cf-gears-event-broker-server \
		$(PYTHON) -m pytest testing/e2e/suites/event_broker/ -vv

# Out-of-process (loopback) E2E: boots flight-control + OoP gears as local
# processes (no Kubernetes) and asserts the cross-process seams. Self-managed:
# its conftest builds the binaries and owns the process lifecycle, so it runs
# pytest directly (not via run_e2e.py). `timeout_func_only` keeps the per-test
# timeout from counting the heavy fixture setup (build + boot + route sync), and
# the 30s per-test cap (vs pytest.ini's default) leaves headroom for the
# lifecycle tests' process stop/start + polling while still failing fast.
## Run the out-of-process (loopback, no-k8s) E2E suite
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
	$(if $(GEAR),@package=$$($(CF_RUN) scope --first) || exit $$?;,) $(PYTHON) tools/scripts/coverage.py combined $(GEAR_COVERAGE_ARGS)

# Generate code coverage report (unit tests only)
coverage-unit: py-env
	$(call print_target_banner)
	$(call check_tool,cargo-llvm-cov)
	$(if $(GEAR),@package=$$($(CF_RUN) scope --first) || exit $$?;,) $(PYTHON) tools/scripts/coverage.py unit $(if $(GEAR),--package "$$package",)

## Ensure needed packages and programs installed for local e2e testing
check-prereq-e2e-local: py-env
	$(call print_target_banner)
	$(PYTHON) tools/scripts/check_local_env.py --mode e2e-local

# Generate code coverage report (e2e-local tests only)
coverage-e2e-local: check-prereq-e2e-local
	$(call print_target_banner)
	$(call check_tool,cargo-llvm-cov)
	$(PYTHON) tools/scripts/coverage.py e2e-local $(if $(GEAR),--e2e-target $(GEAR_E2E_TARGET),)
