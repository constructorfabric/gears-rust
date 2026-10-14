# CI tooling and Makefile includes

The root `Makefile` holds this repository's configuration and includes the
implementation modules under `tools/ci/make/`. These modules require Python
3.11+, GNU Make and a POSIX shell (Git Bash on Windows).

## Module layout

- `common.mk` wires the internal helpers, help output and CI targets.
- `versions.mk` supplies the tool versions to both Make and GitHub Actions.
- `tools.mk`, `reports.mk` and `rust.mk` contain tool checks, JUnit handling and
  Rust checks.
- `project-*.mk` group repository setup, integration tests, E2E, coverage, runtime
  and maintenance targets; `project.mk` includes these groups.

Run `make setup` explicitly when tools are missing. Test and lint targets only
check their required tools. `make help`, `make ci-tool-versions` and `make -n`
need no Cargo invocation or virtual environment.

## Local validation

```sh
python3 -m unittest discover -s tools/ci/tests -v
make help
make -n gear-ci GEAR=file-parser
```

The make regression tests exercise the actual root Makefile and its includes in
an isolated repository-shaped fixture. Instrumented processes check command
arguments, package selection, missing tools, failures and JUnit reports without
compiling the workspace. `make-contracts.yml` runs them on Linux, macOS and
Windows. Full workspace, database, E2E and coverage checks remain in their
existing workflows.

## Measure speed, without reducing checks

Use one repository and one pinned source snapshot. The local branches
`ci-bench-baseline` and `ci-bench-optimized` are based on
`ebdb691f9a08fd1ff442d53abf77e49b82c53143`: same Rust code, Cargo.lock, features, runner labels and
measurement instrumentation; only the optimization differs. Record the source
diff and versions. Historical runs diagnose bottlenecks but are not a paired
baseline. For the target-cache experiment both variants can use the same SHA.

The main workflow accepts two **manual-only** experiment inputs:

- `benchmark_cache_namespace`: unique namespace for rust-cache, sccache and the
  cargo-gears installation cache. Empty uses normal caches. Use separate values
  per variant/experiment; reuse a value for its warm samples. Confirm cache misses
  in the first run rather than assuming that a new branch is cold.
- `benchmark_target_cache`: default false. Enables `target/` caching only for
  the Linux unit-test job with a nonempty namespace. It has no effect on normal PR/push runs. Use different
  namespaces for the with/without-target variants to prevent contamination.

Example (after the benchmark branches have been pushed):

```sh
gh workflow run ci.yml -R modcrafter77/gears-rust --ref ci-bench-baseline -f benchmark_cache_namespace=baseline-exp1
# Wait for completion, save its run ID.
gh workflow run ci.yml -R modcrafter77/gears-rust --ref ci-bench-optimized -f benchmark_cache_namespace=optimized-exp1
```

For target-cache A/B runs use the fixed-sccache SHA on both sides, separate
namespaces, and `-f benchmark_target_cache=true` only for B. For E2E use
`gh workflow run e2e.yml ... -f mode=full -f benchmark_cache_namespace=...`.
The baseline branch contains the same measurement inputs and tool pins, but
retains the original Makefile, hidden installations and local sccache startup.
Its architecture job uses the project toolchain on both sides as a measurement
control. Do not compare different event types.

The branches are local until explicitly pushed. Before publishing or running,
verify `git diff ci-bench-baseline..ci-bench-optimized -- gears libs apps Cargo.lock`
is empty and review the remaining diff. Preserve these branch tips during a
sample series; create new experiment branches for later code changes.

Begin with one cold sample and three warm samples per variant, alternating A/B
without overlapping runs. Re-run the targeted job to avoid repeating the whole
matrix; use the job's `databaseId`, not its browser URL ID:

```sh
gh run view RUN_ID -R modcrafter77/gears-rust --json jobs --jq '.jobs[] | {name,databaseId}'
gh run rerun RUN_ID -R modcrafter77/gears-rust --job JOB_DATABASE_ID
```

After each completed attempt collect the timing data (this tool only reads):

```sh
python3 tools/ci/benchmark.py collect --repo modcrafter77/gears-rust --run RUN_ID --attempt 1 --cache-state cold --output .logs/ci-bench/baseline.json
python3 tools/ci/benchmark.py collect --repo modcrafter77/gears-rust --run RUN_ID --attempt 2 --cache-state warm --output .logs/ci-bench/baseline.json
# Collect optimized attempts into a separate file, then:
python3 tools/ci/benchmark.py compare .logs/ci-bench/baseline.json .logs/ci-bench/optimized.json
```

Compare per-job medians/ranges, setup/build/test/restore/post-save steps, total
runner-minutes and full PR completion time. CFS is a separate workflow after the
change: include that run when measuring the full PR gate. A manual `ci.yml` run
alone does not represent completion of all PR checks. The collector includes post-steps,
separates cache states and refuses failed/cancelled samples. A job's start offset
includes its dependency wait; it is not a measurement of the runner queue alone.
Keep the source run URLs and sccache artifacts; compare runner image versions in
the setup logs, cache sizes, hits/misses/errors and non-cacheable calls. Check the
complete test inventories/features/skips/retries and all four coverage passes.

Only claim acceleration when the gain exceeds the observed variation and cache
overhead, with unchanged checks. Expand the sample series if inconclusive.
Finish with full CI/E2E and ordinary PR-triggered runs. Test cancellation with
two commits in one PR; test CFS once for code and docs-only changes. Those are
behavior tests, separate from build-speed measurements. No target-cache result,
sharding or suite selection is enabled by default without measured evidence.
