# gears.mk: shared make rules for gear projects

`gears.mk` holds the make targets that any project built on gears can reuse:
formatting, clippy, tests, dependency and architecture checks, fuzzing,
Constructor Studio validation and `make help`. It is one file with a version
in its header, so a project can copy it as is.

## Three make files

| File | Who edits it | Contents |
|---|---|---|
| `Makefile` | nobody | Driver: sets the default goal and includes the two files below |
| `<project>.mk` | the project | Settings for `gears.mk` and the project's own targets |
| `.gears-dna/gears.mk` | nobody (copy of the source) | Shared rules, versioned |

In this repository the project file is `gears-rust.mk`. It holds the targets of
gears-rust itself: setup, integration lanes, E2E suites, benchmarks, docs,
mini-chat and the example server.

The driver includes `<project>.mk` first. `gears.mk` reads some settings when
make parses it, and all its settings use `?=`, so the project's values win.

## Use gears.mk in a separate gear repository

Copy `.gears-dna/gears.mk` from this repository to the same path in your
project. Do not edit the copy; to update it, copy a newer version. The
`.gears-dna/` directory holds files that come from gears, not from the project;
a later `cargo gears` release may update them for you. The `Version:` line in
the header of `gears.mk` and `GEARS_MK_VERSION` show which version you have.

`Makefile`:

```make
.DEFAULT_GOAL := help

include my-gear.mk
include .gears-dna/gears.mk
```

`my-gear.mk`:

```make
# Settings for gears.mk.
CLIPPY_HACK_CRATES := -p my-gear

.PHONY: project-integration

# Add a project check to `make ci`.
ci: project-integration

# Run tests with the integration feature.
project-integration: install-tools
	$(call print_target_banner)
	$(call nextest_run,my-gear,-p my-gear --features integration)
```

Then run:

```sh
make help
make ci
make test GEAR_FEATURES=integration GEAR_TEST_ARGS="--retries 2"
```

A repository with several gears sets `GEAR_PKG_DIRS` (for example `gears`) and
gets `make test GEAR=<gear>`: the gear's packages and the crates that depend on
them.

## Help

- `make help` lists the targets by section and every documented parameter,
  with its targets and allowed values. `gears.mk` comes first, then the
  project's targets.
- `make help-<target>` prints one target's full comment, the file that defines
  it and its parameters, e.g. `make help-test`.

The help script is part of `gears.mk` and needs Python 3 (standard library only).

Describe a target with the comment block right above it. The first line is the
summary in `make help`; `#` marks a primary target and `##` a secondary one.
A blank line between the comment and the target detaches the comment.

```make
# Run the project integration tests (needs Docker)
## Longer explanation, shown only by make help-project-integration.
project-integration:
	cargo nextest run -p my-gear --features integration
```

Describe a parameter next to the code that reads it. `text` and `values` are
expanded when help runs, so a list of allowed values stays in one place:

```make
SUITES := smoke full
HELP_PARAMS += SUITE
help.SUITE.usage   := SUITE=<suite>
help.SUITE.targets := project-integration
help.SUITE.text     = Suite to run; default: all.
help.SUITE.values   = $(SUITES)
```

Add a library parameter to your own targets with `help.GEAR.targets += my-target`.

## Settings

Set these in `<project>.mk`, in the environment, or on the command line.

| Variable | Default | Used by |
|---|---|---|
| `CLIPPY_FLAGS` | `-- -D warnings -D clippy::perf` | `clippy`, `clippy-deep` |
| `CLIPPY_HACK_CRATES` | empty | `clippy`: second pass, cargo-hack `--each-feature`. Empty skips it. |
| `CLIPPY_HACK_EXCLUDE` | empty | Extra cargo-hack options, e.g. `--exclude-features x` |
| `GEARS_CI_TARGETS` | `fmt clippy test deny dylint` | `ci` |
| `GEAR`, `GEAR_FEATURES`, `GEAR_TEST_ARGS` | empty | Scope `fmt`, `clippy`, `lint`, `test` to one gear |
| `GEAR_CLIPPY_ARGS` | `--all-targets --all-features -- -D warnings` | `clippy` arguments when `GEAR` is set |
| `GEAR_PKG_DIRS` | `gears,libs` | Directories searched for the gear's packages |
| `GEAR_NAME_REGEXP` | `cf-gears-<gear>`, `<prefix>-<gear>`, `-sdk` | Package name filter for `GEAR` |
| `DENY_FIPS_CONFIG` | `deny-fips.toml` | `fips-policy`; `security` runs it only when the file exists |
| `FUZZ_DIR` | `fuzz` | `fuzz*` targets |
| `NEXTEST_PROFILE`, `JUNIT_DIR` | `default`, empty | `nextest_run` |
| `NEXTEST_STORE_DIR` | `target/nextest` | where `nextest_run` finds nextest's JUnit report (nextest's `[store] dir`, from the workspace root) |
| `GEARS_JUNIT_ENRICH` | empty | script that `nextest_run` runs on each saved JUnit report |
| `DENY_MIN_VERSION`, `NEXTEST_MIN_VERSION`, `CARGO_GEARS_VERSION` | see `gears.mk` | `install-tools`, `deny` |
| `CI_PREINSTALLED_TOOLS` | empty | `install-tools` only checks `cargo-<tool>` for each listed tool and installs nothing |
| `RUST_NIGHTLY` | see `gears.mk` | `shear` |
| `CFS`, `CFS_PIPX_SPEC` | `cfs`, Constructor Studio git URL | `cfs-*` |
| `PYTHON_BOOTSTRAP` | `python3` from `PATH` | `help`, `GEARS_JUNIT_ENRICH` |

## Extend the rules

- Add prerequisites to an aggregate target: `ci: project-check` adds a check
  and keeps the others.
- Replace an aggregate list: set `GEARS_CI_TARGETS` in `<project>.mk`.
- Use `$(call print_target_banner)`, `$(call nextest_run,...)` and the other
  macros of `gears.mk` in your own targets.

## Tools and project files

Use GNU Make, Bash, Python 3, Rust and rustup. On Windows, use GNU Make with
Git Bash. The checks expect `Cargo.toml` and `Cargo.lock`, `deny.toml` and
`Gears.toml` in your project. Install the Rust components and cargo-hack, then
let `install-tools` install cargo-gears, cargo-nextest and cargo-deny:

```sh
rustup component add rustfmt clippy
cargo install --locked cargo-hack
make install-tools
```

Run `make -n ci` to print the commands without running them.

## Change the make files of this repository

- A target that works only in gears-rust (it names a crate, app, suite, script
  or path of this repository) goes to `gears-rust.mk`.
- A target in `gears.mk` must take every project value from a setting.
  `tools/scripts/tests/test_makefiles.py` checks this with a separate consumer
  project. Raise `Version:` and `GEARS_MK_VERSION` together when you change it.
- Keep the order of `gears-rust.mk`. Make expands `:=` assignments and `ifeq`
  conditions when it reads them.
