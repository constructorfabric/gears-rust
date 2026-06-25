---
name: kitsoki-mcp-debug
description: Debug and test the Kitsoki studio MCP server and its tools from the repo checkout. Use when working on `kitsoki mcp`, `cmd/kitsoki/mcp_test_client.go`, `internal/mcp/studio`, MCP tool registration, stdio MCP handshake failures, `mcp-test` output, or when the user wants to verify studio MCP changes without reloading an LLM client.
---

# Kitsoki MCP Debug

Use `kitsoki mcp-test` as the first validation surface for studio MCP work. It
spawns the studio MCP server over stdio with the official Go MCP SDK client, so
it exercises the same transport boundary an attached coding agent uses without
requiring Claude/Codex to reload its MCP tool list.

## Default Smoke

Run from the repo root:

```sh
GOCACHE=$PWD/.cache/go-build go run ./cmd/kitsoki mcp-test --stories-dir ./stories --timeout 20s
```

Expected behavior:

- the child server prints `kitsoki: studio MCP server on stdio ...` on stderr
- the JSON report has `"ok": true`
- `tools` includes `studio.ping`, `studio.handles`, `story.validate`,
  `session.new`, and render tools
- `tool_runs` contains successful `studio.ping` and `studio.handles` calls

Use a repo-local `GOCACHE` in sandboxed environments. Remove `.cache/` before
committing unless it is already ignored.

## Test One Tool

Use `--tool` and a JSON object in `--tool-args`:

```sh
GOCACHE=$PWD/.cache/go-build go run ./cmd/kitsoki mcp-test \
  --stories-dir ./stories \
  --tool story.validate \
  --tool-args '{"dir":"stories/bugfix"}'
```

Other useful calls:

```sh
# read-only server shape, useful for meta-mode/Q&A surface checks
GOCACHE=$PWD/.cache/go-build go run ./cmd/kitsoki mcp-test --read-only

# workspace-bound authoring tools
GOCACHE=$PWD/.cache/go-build go run ./cmd/kitsoki mcp-test \
  --workspace stories/bugfix \
  --tool story.graph \
  --tool-args '{}'

# point at a built binary instead of go run's current executable
go build -o /tmp/kitsoki-mcp-test ./cmd/kitsoki
/tmp/kitsoki-mcp-test mcp-test --server-command /tmp/kitsoki-mcp-test --stories-dir ./stories
```

`mcp-test` defaults to spawning its current executable with generated `mcp`
args. Use repeated `--server-arg` flags only when the default generated server
args are not appropriate; they replace the generated `mcp` argument list.

## No-LLM Boundary

The default smoke is no-LLM:

- `mcp-test` only initializes, lists tools, and calls deterministic tools
- `kitsoki mcp` defaults driving sessions to `harness:replay`
- do not use `session.new` with `harness:"live"` unless the user explicitly asks
  for a live integration test
- tests must use replay/cassettes or in-process SDK transports, not a real LLM

## Local Test Targets

For changes in the CLI wrapper:

```sh
GOCACHE=$PWD/.cache/go-build go test ./cmd/kitsoki -run 'TestMCP|TestRunStudioMCPTest|TestCLI_TopLevelHelp'
```

For studio server/tool behavior:

```sh
GOCACHE=$PWD/.cache/go-build go test ./internal/mcp/studio
```

If `go test ./cmd/kitsoki` fails on sandboxed runs with `~/.kitsoki` writes or
Unix socket bind errors, treat that as unrelated unless the touched code is in
those areas. Keep verification focused and report the sandbox blocker.

## Debugging Failures

- `mcp-test: connect`: the child process did not initialize as an MCP server.
  Run with a longer `--timeout`, check the child stderr, and verify the server
  args start with `mcp`.
- missing tool in `tools`: inspect registration in `internal/mcp/studio/server.go`
  and the relevant `register*Tools` method.
- tool call returns `"is_error": true`: read the text/structured content in the
  JSON report; studio tools return typed error payloads such as `NO_WORKSPACE`,
  `BAD_REQUEST`, or `UNKNOWN_HANDLE`.
- `story.*` path surprises: pass `--stories-dir ./stories` for `@kitsoki/<name>`
  resolution and pass explicit `dir` or `--workspace` when testing workspace
  tools.
- image/render behavior: start with `render.tui` before `render.tui_png` or
  `render.web`; `render.web` may degrade when no browser-capable host is wired.

Before committing, run `git diff --check` and confirm only intended files are
staged. Leave unrelated untracked files alone.
