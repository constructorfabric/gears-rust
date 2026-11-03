# CI compilation cache and timing

sccache uses S3 storage when it is configured, else the GitHub Actions cache,
else plain rustc (see `.github/actions/sccache` and docs/TESTING.md, section 7.4).
Only main writes the compilation cache; PRs read it. PRs also never upload Cargo
registry caches: every rust-cache step saves only on pushes to main.
The pinned cargo-gears executable is cached by version, OS and architecture;
only main writes that cache, and a cached executable that does not run is
reinstalled. CI preinstalls nextest; make's opt-in CI_PREINSTALLED_TOOLS checks
it rather than installing unrelated tools. Local make behavior is unchanged.

Cache hits, misses and write errors are retained as sccache-* artifacts.
SCCACHE_IDLE_TIMEOUT=0 keeps the server, its backend settings and its counters
across long test phases without compilation.

## Reproducible comparison

Pin BASE_SHA, BASELINE_REF and OPTIMIZED_REF in OWNER/REPO. Create both refs
from the same source snapshot. Apply only these CI changes to OPTIMIZED_REF.
For an instrumented baseline retain the original cache/install behavior; copy
only metric collection. Record the Rust source and lockfile diff (must be
empty), runner images, tool versions and test inventories.

Compare like with like: the compilation cache is warm only after main has
written it, so measure PR events against a base ref whose cache main runs
filled, and keep PR and manual events apart. Retain at least three completed
warm samples per variant; measure job runtime including cache restore,
runner-minutes and the PR critical path. Failed/cancelled runs are preserved
but cannot establish acceleration.

Collect each attempt with tools/ci/benchmark.py collect --repo OWNER/REPO
--run RUN_ID --attempt ATTEMPT --cache-state warm --output OUTPUT.json.
Compare two files using tools/ci/benchmark.py compare BEFORE.json AFTER.json.
Do not infer improvement from queue time or old-source benchmark percentages.
