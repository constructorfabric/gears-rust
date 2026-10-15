NEXTEST_PROFILE ?= default
JUNIT_DIR ?=
comma := ,

# nextest_run(report-name, cargo-args[, env-prefix]). Always preserve the test
# exit status. A successful run with a requested but missing report is an error.
define nextest_run
	@cd "$(CF_REPO_ROOT)" || exit $$?; \
	junit="$${CARGO_TARGET_DIR:-target}/nextest/$(NEXTEST_PROFILE)/junit.xml"; \
	rm -f "$$junit"; \
	if [ -n "$(JUNIT_DIR)" ]; then rm -f "$(JUNIT_DIR)/$(1).xml"; fi; \
	(set -x; $(3) $(CF_RUN) --include-rdeps cargo nextest run --profile $(NEXTEST_PROFILE) $(2)); rc=$$?; \
	if [ -n "$(JUNIT_DIR)" ]; then \
		mkdir -p "$(JUNIT_DIR)"; \
		if [ -f "$$junit" ] && cp "$$junit" "$(JUNIT_DIR)/$(1).xml"; then \
			$(CF_PYTHON) "$(CF_REPO_ROOT)/tools/scripts/junit_enrich.py" $${GITHUB_ACTIONS:+--github} "$(JUNIT_DIR)/$(1).xml" \
				|| echo "::warning::could not enrich JUnit report for '$(1)'"; \
		elif [ $$rc -eq 0 ]; then \
			echo "::error::nextest passed but no JUnit report was saved for '$(1)'"; rc=1; \
		else echo "::warning::nextest produced no JUnit report for '$(1)'"; fi; \
	fi; \
	exit $$rc
endef
