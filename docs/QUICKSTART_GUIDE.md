# Gears Server - Quickstart Guide

The maintained getting-started guide is [Build with Gears](web-docs/build-with-gears/index.mdx): prerequisites, first run, calling endpoints, and troubleshooting. This page keeps only the commands.

```bash
make example     # example server with the example gears (tenant-resolver, users-info, ...)
make quickstart  # minimal server: types-registry plus the always-linked system gears
```

Both run `cf-gears-example-server` with [`config/quickstart.yaml`](../config/quickstart.yaml) (see the `run` target in [`gears-rust.mk`](../gears-rust.mk)). The API is served at `http://127.0.0.1:8087/cf/`:

```bash
curl -s http://127.0.0.1:8087/cf/health          # health check
curl -s http://127.0.0.1:8087/cf/openapi.json     # OpenAPI spec
pkill -f cf-gears-example-server                  # stop the server
```

Interactive API docs: <http://127.0.0.1:8087/cf/docs>.
