CF_FMT_ARGS ?= --check
CF_CLIPPY_ARGS ?= --all-targets -- -D warnings
CF_CLIPPY_HACK_PACKAGES ?=
CF_CLIPPY_HACK_ARGS ?= --all-targets --each-feature -- -D warnings
CF_TEST_ARGS ?=
CF_CI_TEST_ARGS ?= $(CF_TEST_ARGS)
CF_DENY_CONFIG ?= deny.toml
CF_DENY_ARGS ?= check
CF_DYLINT_ARGS ?= --dylint

.PHONY: fmt clippy test deny dylint cf-fmt cf-clippy cf-test cf-ci-test cf-deny cf-dylint
# Check Rust formatting.
fmt: cf-fmt
# Run Clippy and configured additional feature checks.
clippy: cf-clippy
# Run project tests with nextest.
test: cf-test
# Check licenses and dependency security policy.
deny: cf-deny
# Run Gears architecture lints.
dylint: cf-dylint

# Check formatting in this project's workspace or explicitly selected packages.
cf-fmt:
	$(call check_rustup_component,rustfmt)
	$(CF_RUN) cargo fmt $(CF_FMT_ARGS)

# Lint the project; an optional additional cargo-hack pass extends this check.
cf-clippy:
	$(call check_rustup_component,clippy)
	$(CF_RUN) cargo clippy $(CF_CLIPPY_ARGS)
ifneq ($(strip $(CF_CLIPPY_HACK_PACKAGES)),)
	$(call check_tool,cargo-hack)
	cd "$(CF_REPO_ROOT)" && cargo hack clippy $(foreach pkg,$(CF_CLIPPY_HACK_PACKAGES),-p $(pkg)) $(CF_CLIPPY_HACK_ARGS)
endif

# Run tests. Empty selections are errors (nextest's default).
cf-test: cf-require-nextest
	$(call nextest_run,test,$(CF_TEST_ARGS))
cf-ci-test: cf-require-nextest
	$(call nextest_run,ci,$(CF_CI_TEST_ARGS))

# Check dependency licenses, advisories, bans and sources.
cf-deny: cf-require-deny
	cd "$(CF_REPO_ROOT)" && cargo deny --config "$(CF_DENY_CONFIG)" $(CF_DENY_ARGS)

# Check the architecture rules configured by this Gears project.
cf-dylint: cf-require-gears
	cd "$(CF_REPO_ROOT)" && cargo gears lint $(CF_DYLINT_ARGS)
