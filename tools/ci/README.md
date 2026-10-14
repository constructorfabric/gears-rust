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
