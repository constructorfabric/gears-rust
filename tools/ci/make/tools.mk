# Tool checks never install. Local minimums and CI pins are checked directly,
# so running nextest does not itself require cargo-gears.
.PHONY: setup install-tools cf-install-tools cf-require-nextest cf-require-deny cf-require-gears
# Explicit local bootstrap. Checks never depend on these targets.
setup: cf-install-tools
install-tools: cf-install-tools
cf-install-tools:
	rustup component add rustfmt clippy
	cargo install --locked cargo-nextest@$(NEXTEST_VERSION)
	cargo install --locked cargo-deny@$(DENY_VERSION)
	cargo install --locked cargo-gears@$(CARGO_GEARS_VERSION)

cf-require-nextest:
	@$(CF_RUN) require cargo-nextest $(NEXTEST_MIN_VERSION) $(NEXTEST_VERSION)
cf-require-deny:
	@$(CF_RUN) require cargo-deny $(DENY_MIN_VERSION) $(DENY_VERSION)
cf-require-gears:
	@$(CF_RUN) require cargo-gears $(CARGO_GEARS_VERSION) $(CARGO_GEARS_VERSION)

define check_tool
	@command -v $(1) >/dev/null || (echo "ERROR: $(1) is not installed. Run 'make setup'." && exit 1)
endef
define check_tool_version
	@$(CF_RUN) require $(1) $(patsubst >=%,%,$(2))
endef
define check_rustup_component
	@command -v rustup >/dev/null || (echo "ERROR: rustup is missing. Run 'make setup'." && exit 1)
	@rustup component list --installed | grep -q '^$(1)' || (echo "ERROR: Run 'rustup component add $(1)' or 'make setup'." && exit 1)
endef
