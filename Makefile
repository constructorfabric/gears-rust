# Repository configuration; target implementations live in the make includes.
CF_SCOPE_DIRS := gears,libs
CF_FEATURES = $(GEAR_FEATURES)
CF_TEST_ARGS = $(GEAR_TEST_ARGS)
CF_CLIPPY_ARGS = $(if $(GEAR),$(GEAR_CLIPPY_ARGS),--all-targets --all-features -- -D warnings -D clippy::perf)
CF_CLIPPY_HACK_PACKAGES = $(if $(GEAR),,cf-gears-toolkit cf-gears-toolkit-db cf-gears-toolkit-http)
CF_CLIPPY_HACK_ARGS := --all-targets --each-feature --exclude-features _any-backend -- -D warnings -D clippy::perf
CF_CI_TEST_ARGS := --workspace --exclude cf-gears-toolkit-macros-tests --exclude cf-gears-toolkit-db-macros
CI_EXTRA_TARGETS := test-macros test-db test-users-info-pg test-usage-collector-pg test-usage-collector-ch test-types-registry-db lychee gts-docs

include tools/ci/make/common.mk
include tools/ci/make/project.mk
