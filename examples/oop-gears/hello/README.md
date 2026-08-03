# Embedded-edge reverse-proxy demo (`hello`)

A minimal, self-contained demonstration of the `GatewayProvider`
(`cpt-cf-component-gateway-provider`): a
**platform-host** process (built-in `api-gateway` edge + `DirectoryService`)
reverse-proxying to a separate **out-of-process gear pod** (`hello`).

`hello` is the smallest possible REST gear: one **public** (`.exposed()`),
**anonymous** (`.anonymous()`) route — `GET /hello/v1/ping` — with no
dependencies and no database.

## What it proves

1. An OoP gear runs as its **own process**, serves its Axum REST router over
   HTTP, and self-registers its REST endpoint + OpenAPI spec with the
   `DirectoryService`.
2. The built-in `api-gateway` edge (a separate process) **discovers** it by
   polling `ListAllInstances` and **reverse-proxies** its public routes.
3. Route exposure is **dynamic**: stopping the gear prunes the route on the
   edge's next poll.

## Topology

```
                 curl :8087/hello/v1/ping
                          │
                          ▼
   ┌─────────────────────────────────────────┐        ┌───────────────────────┐
   │ platform-host (cf-gears-example-server)  │        │ hello-oop (OoP pod)   │
   │                                          │        │                       │
   │  api-gateway  :8087  ──reverse proxy──────────────▶  Axum REST  :9091     │
   │  grpc-hub     :50051 (DirectoryService)  │◀───────  self-register + HB    │
   │  gear-orchestrator (registry)            │  poll  │  GET /hello/v1/ping   │
   └─────────────────────────────────────────┘        └───────────────────────┘
```

## Automated: one-command test script

`run-demo.sh` builds both binaries, boots the host + OoP gear, and runs
assertion-based scenarios, cleaning up on exit:

```bash
examples/oop-gears/hello/run-demo.sh            # build + run all scenarios
examples/oop-gears/hello/run-demo.sh --no-build # reuse existing target/debug binaries
examples/oop-gears/hello/run-demo.sh --keep     # leave both processes running afterward
examples/oop-gears/hello/run-demo.sh -v         # tail host/gear logs on failure
```

Scenarios:

1. The edge discovers the OoP gear and proxies `GET /hello/v1/ping` (200).
2. The proxied response is byte-identical to a direct call to the pod (proves
   it was forwarded, not served locally).
3. An unknown path 404s at the edge (only exposed routes are proxied).
4. Stopping the gear prunes the route (edge → 404 with RFC-9457 body).
5. Restarting the gear re-discovers it (edge → 200 again).

Exit code is non-zero if any scenario fails; logs are at `/tmp/oop-demo-host.log`
and `/tmp/oop-demo-hello.log`.

The steps below describe the same flow manually.

## Run it (two terminals)

### 1. Platform-host

```bash
cargo run --bin cf-gears-example-server --features oop-example,single-tenant -- \
  --config config/oop-gateway-demo-host.yaml run
```

Wait for:

```
api_gateway::gear: HTTP server bound on 127.0.0.1:8087
api_gateway::gear: reverse-proxy directory-sync started endpoint=http://127.0.0.1:50051
grpc_hub::gear: gRPC hub listening bound_addr=127.0.0.1:50051 transport="tcp"
```

### 2. OoP `hello` gear

```bash
export TOOLKIT_DIRECTORY_ENDPOINT=http://127.0.0.1:50051
cargo run --bin hello-oop -p hello --features oop_module -- \
  --config config/oop-gateway-demo-hello.yaml
```

Wait for:

```
toolkit::runtime::oop_serve: OoP gear routes attached (now serving) gear=hello
toolkit::runtime::oop_registration: registered with DirectoryService gear=hello
```

### 3. Call it through the edge

Within one `sync_interval_secs` (5s) the host logs:

```
toolkit_gateway::toolkit_provider: registering gear proxy routes gear=hello endpoint=127.0.0.1:9091 routes=1
```

Then:

```bash
# Through the api-gateway EDGE (reverse-proxied to the OoP pod):
curl -s http://127.0.0.1:8087/hello/v1/ping
# {"message":"pong","served_by":"hello-oop (pid 136183)"}

# Directly against the OoP pod (for comparison — same served_by):
curl -s http://127.0.0.1:9091/hello/v1/ping
```

The identical `served_by` proves the edge forwarded the request to the OoP pod
rather than serving it itself.

### 4. Watch dynamic pruning

Stop the `hello` process (Ctrl-C — graceful `DeregisterInstance`). Within a poll
the host logs `deregistering gear proxy routes gear=hello removed=true`, and:

```bash
curl -s http://127.0.0.1:8087/hello/v1/ping
# {"type":"about:blank","title":"Not Found","status":404,
#  "detail":"no upstream route registered for '/hello/v1/ping'", ...}
```

## Key config knobs

- **Host** (`config/oop-gateway-demo-host.yaml`) — `api-gateway.config.gateway_proxy`:
  - `enabled: true` — turn on the directory-driven reverse proxy.
  - `directory_endpoint` — gRPC endpoint of the `DirectoryService` (grpc-hub).
  - `sync_interval_secs` — how often the edge polls the directory.
- **OoP gear** (`config/oop-gateway-demo-hello.yaml`) — top-level `oop_http`:
  - Its presence switches the OoP bootstrap into the HTTP-serving lifecycle
    (serve REST + self-register). `advertise_uri` is what the edge proxies to.

> `single-tenant` and the SQLite `database` block in the host config are only
> there because the example server unconditionally links a few DB-backed system
> gears (`credstore`, `resource-group`, `oagw`) that must initialize; they are
> not part of the reverse-proxy path.

## Platform plane (`cpt-cf-adr-platform-plane-auth` — internal auth over gRPC)

The demo also exercises the **platform (internal) authentication plane** — the
system-to-system credential carried on `DirectoryService` gRPC calls, distinct
from the tenant-plane user bearer. It closes the platform-plane gRPC loop
(`cpt-cf-adr-platform-plane-auth`):

- The host's `DirectoryService` (`gear-orchestrator.internal_auth`) **enforces**
  a valid `x-toolkit-internal-token` on every RPC.
- The OoP gear (`oop_http.internal_auth`) and the edge
  (`api-gateway.gateway_proxy.internal_auth`) **attach** that token to their
  outbound calls (`register`/`heartbeat`, `list_all_instances`).

For a Kubernetes-free demo, all three use the dependency-light `shared_secret`
provider with the same secret (`dev-internal-token`). In a real cluster these
become `provider: kube`: gears attach a **rotating projected ServiceAccount
token** (read by the SA-token reader) and the host validates it via the
Kubernetes `TokenReview` API (behind the `k8s-auth` feature). In-process callers
use the local directory client and bypass gRPC (and therefore this check)
entirely.

### See it enforce

The middleware validates the token on **inbound** calls. To observe a
system-to-system call being validated (or rejected) directly against a pod,
call the OoP gear with the internal-token header:

```bash
# Valid internal token -> handled normally.
curl -s -H 'X-ToolKit-Internal-Token: dev-internal-token' \
  http://127.0.0.1:9091/hello/v1/ping

# Invalid internal token -> 401 (RFC-9457), even before the tenant plane runs.
curl -s -H 'X-ToolKit-Internal-Token: wrong' \
  http://127.0.0.1:9091/hello/v1/ping
```

To watch **enforcement on the gRPC directory loop**, change the OoP gear's
`oop_http.internal_auth.secret` to a value that does *not* match the host and
restart it: its `register` RPC is rejected with `Unauthenticated`, so the edge
never discovers it and `curl :8087/hello/v1/ping` stays 404.

> The reverse-proxied edge path (`:8087`) does **not** attach an internal token
> to the upstream pod — the platform plane governs `DirectoryService` traffic,
> not proxied tenant requests, which carry the tenant-plane bearer instead.
