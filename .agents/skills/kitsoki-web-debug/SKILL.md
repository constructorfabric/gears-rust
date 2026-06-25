---
name: kitsoki-web-debug
description: Debug problems with the kitsoki web UI (`make web-dev` / `kitsoki web`). Use when the user reports 504s, blank pages, SSE stream stalls, session errors, proxy failures, or wants to inspect web server logs. Covers log locations, common failure patterns, proxy/backend interaction, and the RPC surface.
---

# Kitsoki Web Debug

`make web-dev` runs two cooperating processes:

- **Go backend** — `kitsoki web` on `http://127.0.0.1:7777`, serves the JSON-RPC surface (`/rpc`, `/rpc/events`, `/rpc/meta-stream`)
- **Vite dev server** — `http://localhost:5173`, serves the Vue SPA with HMR; proxies `/rpc/**` to the Go backend

Both processes write to **stdout/stderr AND a rotating log file** under `.artifacts/logs/`. The 10 most recent runs are kept.

## Finding and tailing the logs

```sh
# List recent log files (newest last):
ls -lt .artifacts/logs/web-dev-*.log | head

# Tail the latest log (convenience target):
make web-dev-logs

# Or manually:
tail -f .artifacts/logs/$(ls .artifacts/logs/ | sort | tail -1)

# Grep for errors across all recent logs:
grep -i "error\|panic\|warn\|504\|500" .artifacts/logs/web-dev-*.log | tail -50
```

The log file path is also printed to stderr at startup: `kitsoki: debug log → .artifacts/logs/web-dev-<timestamp>.log`.

## Common failure patterns

### 504 Gateway Timeout

The Vite proxy returned 504 to the browser — the Go backend didn't respond in time (or at all).

**Causes and fixes:**

| Cause | Signal in logs | Fix |
|-------|---------------|-----|
| Go backend not started yet | No `kitsoki: web UI` line in log | Wait or restart `make web-dev` |
| LLM oracle call slow (30–120s) | Long gap between `session.turn` request and response | Normal — proxy timeout is now disabled (`timeout: 0`); wait it out |
| Go backend panicked and died | `panic:` in log, no further output | Read the panic trace in `.artifacts/logs/`, file a bug |
| Port conflict — something else on 7777 | `bind: address already in use` in log | `lsof -i :7777` to find the process |

### Blank page / SPA not loading

The SPA is bundled into the binary only after `make build`. In `make web-dev` mode the SPA is served by Vite, not the Go binary, so the binary can be stale. Check:

```sh
# Is Vite running? (look for the dev server URL)
grep "Local:" .artifacts/logs/web-dev-*.log | tail -3

# Did pnpm install fail?
grep -i "ERR\|error" .artifacts/logs/web-dev-*.log | head -20
```

### SSE stream stalls / no live updates

The SSE subscription (`/rpc/events?subscription_id=…`) holds an open connection. If the browser disconnects and reconnects, the server resumes from the watermark. If it never delivers events:

1. Open browser devtools → Network → filter `events` — check the SSE connection status
2. Look for `subscription_id` in the Go logs: `grep "sub-" .artifacts/logs/web-dev-*.log`
3. Check if the Go backend is polling: the default poll interval is 500ms, so events arrive within ~500ms of being appended to the session store

### Session not found (codeNotFound -32002)

The browser's session_id is stale — sessions live only in the Go process's memory and die on restart. Reload the home page to get a fresh session list.

### Read-only surface error (codeReadOnly -32001)

`kitsoki status serve` (not `kitsoki web`) was used. The status-serve path is trace-file read-only: turn/submit/continue RPCs are not available. Use `kitsoki web` or `make web-dev` for the interactive surface.

## Key source locations

| What | Where |
|------|-------|
| HTTP server setup, RPC dispatch | `internal/runstatus/server/server.go` |
| Live session (in-process event sink) | `internal/runstatus/server/live.go` |
| Session registry, story catalogue | `cmd/kitsoki/registry.go` |
| `kitsoki web` command setup | `cmd/kitsoki/web.go` |
| Vite proxy config | `tools/runstatus/vite.config.ts` |
| Makefile `web-dev` target | `Makefile` (search `web-dev:`) |

## Enabling verbose Go HTTP logging

There is no `--debug` flag today, but you can get structured output by running the Go backend directly with `GODEBUG=http2debug=1` or adding a `slog`-based middleware. For now, the log file captures all stderr from the process including any `fmt.Fprintf(os.Stderr, …)` calls in the server.

To capture a one-off verbose run:

```sh
# Start the Go backend manually with verbose output (go run — no stray binary):
go run ./cmd/kitsoki web --addr 127.0.0.1:7777 2>&1 | tee .artifacts/logs/manual-debug.log
```

Then start Vite separately:
```sh
cd tools/runstatus && pnpm dev
```

## RPC surface quick reference

All calls are `POST /rpc` with JSON-RPC 2.0 body. Quick test from the CLI:

```sh
# List stories:
curl -s http://127.0.0.1:7777/rpc -d '{"jsonrpc":"2.0","id":1,"method":"runstatus.stories.list","params":{}}' | jq .

# List active sessions:
curl -s http://127.0.0.1:7777/rpc -d '{"jsonrpc":"2.0","id":1,"method":"runstatus.sessions.list","params":{}}' | jq .

# Get session view (replace <sid>):
curl -s http://127.0.0.1:7777/rpc \
  -d '{"jsonrpc":"2.0","id":1,"method":"runstatus.session.view","params":{"session_id":"<sid>"}}' | jq .
```

Error codes: `-32000` server error, `-32001` read-only surface, `-32002` unknown session_id, `-32601` unknown method.
