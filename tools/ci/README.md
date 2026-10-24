# CI compilation cache and timing

Linux jobs restore one bounded (3.5 GiB) local sccache archive from the base
branch. Only the Linux test-suite job saves it after successful pushes to main.
PRs never upload compilation or Cargo registry caches. macOS and Windows nightly
jobs retain upstream behavior. No remote per-object GHA sccache backend is used.
The pinned cargo-gears executable is cached by OS, architecture and compiler;
only main writes that cache. CI preinstalls nextest; make's opt-in
CI_PREINSTALLED_TOOLS checks it rather than installing unrelated tools. Local
make behavior is unchanged.

Cache write errors and hits/misses are retained as sccache-* artifacts.
SCCACHE_IDLE_TIMEOUT=0 preserves counters across long test phases.
The bounded archive can evict objects as the workspace grows: inspect metrics,
not merely a successful restore. Multiple main snapshots and other workflows
share the repository cache quota, so retained size must also be monitored.

## Reproducible comparison

Pin BASE_SHA, BASELINE_REF and OPTIMIZED_REF in OWNER/REPO. Create both refs
from the same source snapshot. Apply only these CI changes to OPTIMIZED_REF.
For an instrumented baseline retain the original cache/install behavior; copy
only dispatch inputs and metric collection. Record the Rust source and lockfile
diff (must be empty), runner images, tool versions and test inventories.

A manual optimized run can seed a separate cache with benchmark_namespace
starting with bench- and benchmark_seed=true. Subsequent manual runs use the
same namespace with benchmark_seed=false; they must not save archives. These
are diagnostic same-source samples, not proof of PR acceleration.

For the PR scenario first seed the base ref's archive, then open a PR based on
that ref with a small source change. A cache seeded on a sibling feature branch
is not visible to that PR. Compare baseline and optimized PR events separately
from manual events. Retain at least three completed warm samples per variant;
measure job runtime including restore/save, runner-minutes and PR critical path.
Failed/cancelled runs are preserved but cannot establish acceleration.

Collect each attempt with tools/ci/benchmark.py collect --repo OWNER/REPO
--run RUN_ID --attempt ATTEMPT --cache-state warm --output OUTPUT.json.
Compare two files using tools/ci/benchmark.py compare BEFORE.json AFTER.json.
Do not infer improvement from queue time or old-source benchmark percentages.
