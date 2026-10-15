# Exact CI versions. Read without a toolchain using `make ci-tool-versions`.
SCCACHE_VERSION := 0.18.0
CARGO_GEARS_VERSION := 0.0.8
NEXTEST_VERSION := 0.9.146
DENY_VERSION := 0.20.2
HACK_VERSION := 0.6.45
LLVM_COV_VERSION := 0.9.1
SHEAR_VERSION := 1.13.1
DENY_MIN_VERSION := 0.20.0
NEXTEST_MIN_VERSION := 0.9.143

.PHONY: ci-tool-versions
ci-tool-versions:
	@printf '%s\n' 'sccache=$(SCCACHE_VERSION)' 'cargo-gears=$(CARGO_GEARS_VERSION)' 'nextest=$(NEXTEST_VERSION)' 'cargo-deny=$(DENY_VERSION)' 'cargo-hack=$(HACK_VERSION)' 'cargo-llvm-cov=$(LLVM_COV_VERSION)' 'cargo-shear=$(SHEAR_VERSION)'
