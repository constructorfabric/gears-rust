---
title: Getting started
description: Install the toolchain, run the example server, and verify it works.
sidebar:
  label: Install & run
  order: 1
---

This page gets the example server running so you have something to explore and call.
When you're ready to write code, continue to [Build your first gear](./your-first-gear/).

## Prerequisites

- A recent **stable Rust toolchain** (`rustup` + `cargo`).
- The Gears framework repository checked out locally (it contains the toolkit, the system
  gears, and the example server).
- Optional: build with `--features fips` to route TLS through validated crypto providers.

:::caution[No published crate yet]
The example server is built from the framework repository — there is no crate to
`cargo install` at this stage. Clone the repo and run the commands below from its root.
:::

## Run the example server

From the repository root, start the server with the example gears:

```sh
# Runs the server with example gears (tenant-resolver, users-info)
make example

# …or a minimal server with no example gears
make quickstart
```

The server listens on `http://127.0.0.1:8087`. The quickstart configuration mounts the
API gateway under the `/cf` prefix (set via `gears.api-gateway.config.prefix_path` in
`config/quickstart.yaml`).

:::tip[Which target should I run?]
Use `make example` to explore real endpoints — it loads the `tenant-resolver` and
`users-info` gears. Use `make quickstart` when you want the bare runtime with nothing
mounted but the system gears.
:::

:::note[About the `/cf` prefix]
Every path below is prefixed with `/cf` because of the gateway configuration above. If you
change `prefix_path`, adjust the URLs accordingly.
:::

## Verify it works

Check health:

```sh
curl -s http://127.0.0.1:8087/health
# {"status":"healthy","timestamp":"..."}
```

Open the interactive API docs in a browser:

```text
http://127.0.0.1:8087/cf/docs
```

Fetch the generated OpenAPI document:

```sh
curl -s http://127.0.0.1:8087/cf/openapi.json > openapi.json
```

Call an example endpoint (the `users-info` gear, mounted under `/cf`):

```sh
curl -s "http://127.0.0.1:8087/cf/users-info/v1/users" | python3 -m json.tool
```

## Stop the server

```sh
pkill -f cf-gears-server
```

## Alternative: use the CLI

If you prefer to scaffold and run a Gears project from scratch instead of using
the example server, the `cargo gears` CLI provides a manifest-driven workflow:

```sh
cargo install cargo-gears
cargo gears new /tmp/cf-demo
cd /tmp/cf-demo
cargo gears generate module --template background-worker
cargo gears generate config --template dev --app app1 --env dev
cargo gears config mod add background-worker -c config/app1-dev.yml
cargo gears run --app app1 --env dev
```

See the [CLI getting started guide](/cli/getting-started/) for a full end-to-end
walkthrough and the [command reference](/cli/commands/) for every command.

## Next

- [Build your first gear](./your-first-gear/) — write an SDK, a domain service,
  and a REST endpoint, and wire it into the runtime.
- [Core concepts](../concepts/) — the mental model behind what you just ran.
