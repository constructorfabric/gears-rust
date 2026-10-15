# Internal helpers and Rust checks for this repository.
ifndef CF_COMMON_MAKE_INCLUDED
CF_COMMON_MAKE_INCLUDED := 1
CF_MAKE_DIR := $(dir $(lastword $(MAKEFILE_LIST)))
CF_REPO_ROOT := $(CURDIR)
CF_PYTHON ?= python3
# Keep include paths relative, and quote repository paths in subprocesses.
CF_PACKAGES ?=
CF_FEATURES ?=
CF_SCOPE_DIRS ?=
GEAR ?=
GEAR_NAME_REGEXP ?= ^(?:(?:cf-gears-)?(?:[a-z]+-)?)?$(GEAR)(?:-sdk)?$$
CF_RUN = $(CF_PYTHON) "$(CF_REPO_ROOT)/tools/ci/run.py" --root "$(CF_REPO_ROOT)" --gear "$(GEAR)" --scope-dirs "$(CF_SCOPE_DIRS)" --package-filter '$(GEAR_NAME_REGEXP)' --packages "$(CF_PACKAGES)" --features "$(CF_FEATURES)"
CI_EXTRA_TARGETS ?=
.DEFAULT_GOAL := help

include $(CF_MAKE_DIR)versions.mk
include $(CF_MAKE_DIR)tools.mk
include $(CF_MAKE_DIR)reports.mk
include $(CF_MAKE_DIR)rust.mk

# -------- CI orchestration --------
.PHONY: help ci cf-ci
# List targets from all repository make includes without bootstrap.
help:
	@$(CF_PYTHON) "$(CF_REPO_ROOT)/tools/scripts/make_help.py" $(MAKEFILE_LIST)

# Run the repository's Rust checks and integration/documentation targets.
ci: cf-ci
cf-ci: cf-fmt cf-clippy cf-ci-test cf-deny cf-dylint $(CI_EXTRA_TARGETS)
endif
