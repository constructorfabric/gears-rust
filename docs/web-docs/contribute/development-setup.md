---
title: Development setup
description: Prerequisites, cloning with submodules, and building the Gears framework locally.
sidebar:
  label: Development setup
  order: 3
---

Set up a local environment to build, run, and test the framework.

## Prerequisites

Complete the [cross-platform setup](../setup/) before building or contributing. It covers macOS, Linux, and Windows, including Rust and Cargo through rustup, native build tools, and Constructor Studio.

- **An editor** — VS Code with rust-analyzer is recommended.

## Clone and build

```bash
git clone --recurse-submodules https://github.com/constructorfabric/gears-rust
cd gears-rust
make build
make test
```

## Run the server locally

```bash
# SQLite quickstart (minimal runtime)
make quickstart

# With the example users_info gear
cargo run --bin cf-gears-example-server --features users-info-example -- --config config/quickstart.yaml run
```

For running and exploring the server in depth, see [Install and run](../../build-with-gears/).

## Helpful environment variables

```bash
export RUST_LOG=debug        # debug-level logging
export RUST_BACKTRACE=full   # backtraces on panic
```

## Next

- [Code contribution guide](../code-contribution-guide/) — the branch/commit/PR workflow.
- [Architecture and quality gates](../architecture-and-quality-gates/) — the checks to run before pushing.
