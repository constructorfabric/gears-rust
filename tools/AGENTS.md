# tools/

- `tools/scripts/` — Python helpers behind the make targets. Run them through make (`make py-env` creates the environment). Tests use `unittest`, not pytest: `make check-release-resolver` runs `tools/scripts/tests/`, `make pr-review-test` runs `tools/scripts/toolkit-pr-review/tests/`, `make pr-review-lint` checks the review rules.
- `tools/scripts/ci.py` — the E2E orchestration engine, and the Windows entry point for a subset of targets (fmt, clippy, test, audit, deny, security, check, quickstart, e2e-local, e2e-docker, fuzz). When you change one of those make targets, check whether `ci.py` needs the same change.
- `tools/scripts/tests/test_makefiles.py` checks that `.gears-dna/gears.mk` takes every project value from a setting; see `.gears-dna/docs/GEARS_MK.md` before editing either make file.
- `tools/fuzz/` — outside the workspace, nightly toolchain, own `Cargo.lock`. Use `make fuzz-list`, `make fuzz-run`; see `tools/fuzz/README.md`.
- `tools/xtask/` and `tools/gts-analyze/` are workspace members with `publish = false`; keep it that way (`make check-release-config`).
- `tools/ci/` — CI-only scripts (benchmarks); see its `README.md`.
