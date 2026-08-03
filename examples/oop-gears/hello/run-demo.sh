#!/usr/bin/env bash
#
# Embedded-edge reverse-proxy + two-plane-authentication demo runner.
#
# Boots the platform-host (api-gateway edge + DirectoryService, auth ENABLED)
# and TWO out-of-process gears:
#   - `hello`  : one `.anonymous()` route  (GET /hello/v1/ping)
#   - `secure` : one `.authenticated()` route (GET /secure/v1/whoami) whose OoP
#                pod links an in-process AuthN stack and RE-VALIDATES the bearer
#
# It then runs assertion-based scenarios proving the edge discovers/proxies/
# prunes OoP pods AND enforces two-plane auth (edge + in-process re-validation).
#
# Usage:
#   examples/oop-gears/hello/run-demo.sh [--no-build] [--keep] [-v]
#
#   --no-build   Skip `cargo build` (use existing target/debug binaries).
#   --keep       Leave all processes running after the scenarios pass.
#   -v           Verbose: stream a tail of the host/gear logs on failure.
#
# Exit code is non-zero if any scenario fails.
set -uo pipefail

# --- Locations -----------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
HOST_CONFIG="config/oop-gateway-demo-host.yaml"
HELLO_CONFIG="config/oop-gateway-demo-hello.yaml"
SECURE_CONFIG="config/oop-gateway-demo-secure.yaml"

# --- Endpoints (must match the demo configs) ----------------------------------
GATEWAY="http://127.0.0.1:8087"
HELLO="http://127.0.0.1:9091"
SECURE="http://127.0.0.1:9092"
DIRECTORY="http://127.0.0.1:50051"
PING_PATH="/hello/v1/ping"
WHOAMI_PATH="/secure/v1/whoami"
# `static-authn-plugin` runs in accept_all mode: any non-empty bearer is valid.
TOKEN="demo-token-abc123"

HOST_LOG="/tmp/oop-demo-host.log"
HELLO_LOG="/tmp/oop-demo-hello.log"
SECURE_LOG="/tmp/oop-demo-secure.log"

# --- Flags ---------------------------------------------------------------------
DO_BUILD=1
KEEP=0
VERBOSE=0
for arg in "$@"; do
  case "$arg" in
    --no-build) DO_BUILD=0 ;;
    --keep) KEEP=1 ;;
    -v|--verbose) VERBOSE=1 ;;
    -h|--help) sed -n '2,17p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

# --- Pretty output -------------------------------------------------------------
if [[ -t 1 ]]; then
  RED=$'\e[31m'; GREEN=$'\e[32m'; YELLOW=$'\e[33m'; BOLD=$'\e[1m'; RESET=$'\e[0m'
else
  RED=""; GREEN=""; YELLOW=""; BOLD=""; RESET=""
fi
info()  { echo "${BOLD}==>${RESET} $*"; }
pass()  { echo "${GREEN}  PASS${RESET} $*"; }
fail()  { echo "${RED}  FAIL${RESET} $*"; FAILURES=$((FAILURES + 1)); }
warn()  { echo "${YELLOW}  warn${RESET} $*"; }

FAILURES=0
HOST_PID=""
HELLO_PID=""
SECURE_PID=""

# --- Cleanup -------------------------------------------------------------------
cleanup() {
  if [[ "$KEEP" == "1" && "$FAILURES" == "0" ]]; then
    info "--keep set: leaving processes running (host=$HOST_PID, hello=$HELLO_PID, secure=$SECURE_PID)"
    info "  anonymous: $GATEWAY$PING_PATH"
    info "  authenticated: curl -H 'Authorization: Bearer $TOKEN' $GATEWAY$WHOAMI_PATH"
    info "  logs: $HOST_LOG , $HELLO_LOG , $SECURE_LOG"
    info "  stop with: pkill -f oop-gateway-demo"
    return
  fi
  info "cleaning up"
  [[ -n "$SECURE_PID" ]] && kill -TERM "$SECURE_PID" 2>/dev/null || true
  [[ -n "$HELLO_PID" ]]  && kill -TERM "$HELLO_PID"  2>/dev/null || true
  [[ -n "$HOST_PID" ]]   && kill -TERM "$HOST_PID"   2>/dev/null || true
  kill_stragglers  # belt-and-braces: anything still bound to the demo configs
}
trap cleanup EXIT

# --- Helpers -------------------------------------------------------------------
# `_req` takes an OPTIONAL full header ("Name: value"); the bearer wrappers below
# cover the common `Authorization: Bearer <token>` case used by most scenarios.

# _req code|body URL [HEADER]  -> HTTP status (000 if unreachable) or body
_req() {
  local mode="$1" url="$2" header="${3:-}" args=(-s --max-time 3)
  [[ "$mode" == code ]] && args+=(-o /dev/null -w '%{http_code}')
  [[ -n "$header" ]] && args+=(-H "$header")
  curl "${args[@]}" "$url" 2>/dev/null || { [[ "$mode" == code ]] && echo "000" || true; }
}
# http_code / body URL [TOKEN]  -> convenience bearer wrappers over `_req`.
http_code() { if [[ -n "${2:-}" ]]; then _req code "$1" "Authorization: Bearer $2"; else _req code "$1"; fi; }
body()      { if [[ -n "${2:-}" ]]; then _req body "$1" "Authorization: Bearer $2"; else _req body "$1"; fi; }

# wait_for_code URL EXPECTED TIMEOUT_SECS LABEL [TOKEN]
wait_for_code() {
  local url="$1" expected="$2" timeout="$3" label="$4" token="${5:-}" waited=0 code
  while (( waited < timeout )); do
    code="$(http_code "$url" "$token")"
    [[ "$code" == "$expected" ]] && return 0
    sleep 1; waited=$((waited + 1))
  done
  warn "$label: expected HTTP $expected within ${timeout}s, last=$code"
  return 1
}

dump_logs_on_verbose() {
  [[ "$VERBOSE" == "1" ]] || return 0
  local pair
  for pair in "host:$HOST_LOG" "hello:$HELLO_LOG" "secure:$SECURE_LOG"; do
    echo "----- ${pair%%:*} log (tail) -----"; tail -20 "${pair#*:}" 2>/dev/null || true
  done
}

# kill_stragglers  -- TERM any process still bound to one of the demo configs.
kill_stragglers() {
  local cfg
  for cfg in host hello secure; do
    pkill -TERM -f "oop-gateway-demo-${cfg}.yaml" 2>/dev/null || true
  done
}

# build_bin LABEL -- <cargo build args...>
build_bin() {
  local label="$1"; shift
  info "building $label"
  cargo build "$@" || { fail "$label build failed"; exit 1; }
}

# start_oop BIN CONFIG LOG  -- background an OoP gear (self-registers via the
# directory endpoint) and print its PID.
start_oop() {
  TOOLKIT_DIRECTORY_ENDPOINT="$DIRECTORY" "$1" --config "$2" > "$3" 2>&1 &
  echo $!
}

# --- Pre-flight ----------------------------------------------------------------
cd "$REPO_ROOT"
info "repo root: $REPO_ROOT"

info "killing any stragglers from previous runs"
kill_stragglers
sleep 1

if [[ "$DO_BUILD" == "1" ]]; then
  build_bin "platform-host (auth enabled via static-authn)" \
    --bin cf-gears-example-server --features oop-example,single-tenant,static-authn
  build_bin "hello-oop" --bin hello-oop -p hello --features oop_module
  build_bin "secure-oop (links in-process AuthN stack)" \
    --bin secure-oop -p secure --features oop_module
fi

HOST_BIN="$REPO_ROOT/target/debug/cf-gears-example-server"
HELLO_BIN="$REPO_ROOT/target/debug/hello-oop"
SECURE_BIN="$REPO_ROOT/target/debug/secure-oop"
for bin in "$HOST_BIN" "$HELLO_BIN" "$SECURE_BIN"; do
  [[ -x "$bin" ]] || { fail "missing $bin (run without --no-build)"; exit 1; }
done

# --- Boot the platform-host ----------------------------------------------------
info "starting platform-host -> $HOST_LOG"
"$HOST_BIN" --config "$HOST_CONFIG" run > "$HOST_LOG" 2>&1 &
HOST_PID=$!
if ! wait_for_code "$GATEWAY/healthz" 200 30 "gateway /healthz"; then
  fail "platform-host did not become healthy"; dump_logs_on_verbose; exit 1
fi
pass "platform-host up (api-gateway $GATEWAY, directory $DIRECTORY)"

# --- Boot the OoP gears --------------------------------------------------------
info "starting OoP hello gear -> $HELLO_LOG"
HELLO_PID="$(start_oop "$HELLO_BIN" "$HELLO_CONFIG" "$HELLO_LOG")"
if ! wait_for_code "$HELLO/healthz" 200 30 "hello /healthz"; then
  fail "hello-oop did not become healthy"; dump_logs_on_verbose; exit 1
fi
pass "OoP hello gear up ($HELLO)"

info "starting OoP secure gear -> $SECURE_LOG"
SECURE_PID="$(start_oop "$SECURE_BIN" "$SECURE_CONFIG" "$SECURE_LOG")"
if ! wait_for_code "$SECURE/healthz" 200 30 "secure /healthz"; then
  fail "secure-oop did not become healthy"; dump_logs_on_verbose; exit 1
fi
if grep -q "tenant-plane authenticator installed" "$SECURE_LOG"; then
  pass "OoP secure gear up ($SECURE) with in-process tenant-plane authenticator"
else
  warn "secure-oop up but tenant-plane authenticator not confirmed in log"
  pass "OoP secure gear up ($SECURE)"
fi

# ==============================================================================
# Scenarios
# ==============================================================================
echo
info "${BOLD}Scenario 1${RESET}: anonymous route is reachable at the edge WITHOUT a token"
# Auth is ENABLED at the edge, but `GET /hello/v1/ping` is `.anonymous()`, so
# the edge lets it through and reverse-proxies to the hello pod.
# Discovery is async (directory poll); allow up to a few sync intervals.
if wait_for_code "$GATEWAY$PING_PATH" 200 30 "edge $PING_PATH"; then
  edge_body="$(body "$GATEWAY$PING_PATH")"
  if [[ "$edge_body" == *'"message":"pong"'* ]]; then
    pass "edge returns 200 pong: $edge_body"
  else
    fail "unexpected edge body: $edge_body"
  fi
else
  fail "edge never began proxying $PING_PATH"; dump_logs_on_verbose
fi

echo
info "${BOLD}Scenario 2${RESET}: request is genuinely proxied (edge body == direct body)"
direct_body="$(body "$HELLO$PING_PATH")"
edge_body="$(body "$GATEWAY$PING_PATH")"
info "  direct ($HELLO): $direct_body"
info "  edge   ($GATEWAY): $edge_body"
if [[ -n "$direct_body" && "$direct_body" == "$edge_body" ]]; then
  pass "edge response identical to OoP pod response (proxied, not served locally)"
else
  fail "edge/direct bodies differ (not proxied?)"
fi

echo
info "${BOLD}Scenario 3${RESET}: edge ENFORCES auth on the authenticated route (no token -> 401)"
# `GET /secure/v1/whoami` is `.authenticated()`; the edge rejects it with 401
# before ever proxying (no bearer presented).
if wait_for_code "$GATEWAY$WHOAMI_PATH" 401 30 "edge $WHOAMI_PATH (no token)"; then
  pass "edge returns 401 for the authenticated route without a bearer"
else
  code="$(http_code "$GATEWAY$WHOAMI_PATH")"
  fail "expected 401 at the edge without a token, got $code"; dump_logs_on_verbose
fi

echo
info "${BOLD}Scenario 4${RESET}: edge accepts a valid bearer and proxies the authenticated route (200)"
if wait_for_code "$GATEWAY$WHOAMI_PATH" 200 30 "edge $WHOAMI_PATH (with token)" "$TOKEN"; then
  who_body="$(body "$GATEWAY$WHOAMI_PATH" "$TOKEN")"
  info "  identity: $who_body"
  if [[ "$who_body" == *'"subject_id"'* && "$who_body" == *'"tenant_id"'* ]]; then
    pass "edge returns 200 with the resolved identity"
  else
    fail "unexpected whoami body: $who_body"
  fi
else
  fail "edge did not return 200 for the authenticated route with a valid bearer"; dump_logs_on_verbose
fi

echo
info "${BOLD}Scenario 5${RESET}: two-plane auth — the OoP pod RE-VALIDATES the bearer itself"
# Hit the pod DIRECTLY (bypassing the edge). Without a token the pod's own
# security_context_middleware rejects it: proof it does not blindly trust the
# edge (zero-trust). With a token the pod re-validates in-process and serves.
pod_no_token="$(http_code "$SECURE$WHOAMI_PATH")"
if [[ "$pod_no_token" == "401" ]]; then
  pass "direct pod call without a token -> 401 (in-process re-validation)"
else
  fail "expected 401 directly from the pod without a token, got $pod_no_token"; dump_logs_on_verbose
fi
pod_body="$(body "$SECURE$WHOAMI_PATH" "$TOKEN")"
if [[ "$pod_body" == *'"served_by":"secure-oop'* ]]; then
  pass "direct pod call with a token -> 200 served by the pod: $pod_body"
else
  fail "expected a 200 whoami body from the pod with a token, got: $pod_body"; dump_logs_on_verbose
fi

echo
info "${BOLD}Scenario 6${RESET}: unknown path is 404 (authenticated) / 401 (anonymous) at the edge"
# The gears only expose their declared routes. With auth enabled, an unknown
# path with NO token is 401 (require-auth-by-default, don't leak existence);
# WITH a valid token the edge authenticates then finds no route -> 404.
anon_code="$(http_code "$GATEWAY/hello/v1/does-not-exist")"
auth_code="$(http_code "$GATEWAY/hello/v1/does-not-exist" "$TOKEN")"
if [[ "$anon_code" == "401" && "$auth_code" == "404" ]]; then
  pass "unknown path: 401 without a token, 404 with a valid token"
else
  fail "expected 401(no token)/404(token) for unknown path, got $anon_code/$auth_code"
fi

echo
info "${BOLD}Scenario 7${RESET}: dynamic prune when the hello OoP gear stops"
info "  stopping hello-oop (graceful SIGTERM -> DeregisterInstance)"
kill -TERM "$HELLO_PID" 2>/dev/null || true
wait "$HELLO_PID" 2>/dev/null || true
HELLO_PID=""
# Once pruned, `/hello/v1/ping` is no longer a known (public) route. Probe WITH
# a token so we observe the proxy-level 404 rather than the auth-level 401.
if wait_for_code "$GATEWAY$PING_PATH" 404 30 "edge $PING_PATH (after stop)" "$TOKEN"; then
  pruned_body="$(body "$GATEWAY$PING_PATH" "$TOKEN")"
  if [[ "$pruned_body" == *"no upstream route registered"* ]]; then
    pass "edge pruned the route: 404 $pruned_body"
  else
    pass "edge returns 404 after gear stop (body: $pruned_body)"
  fi
else
  fail "edge did not prune the route after the gear stopped"; dump_logs_on_verbose
fi

echo
info "${BOLD}Scenario 8${RESET}: re-discovery when the hello OoP gear comes back"
info "  restarting hello-oop"
HELLO_PID="$(start_oop "$HELLO_BIN" "$HELLO_CONFIG" "$HELLO_LOG")"
if wait_for_code "$HELLO/healthz" 200 30 "hello /healthz (restart)" \
   && wait_for_code "$GATEWAY$PING_PATH" 200 30 "edge $PING_PATH (re-discovery)"; then
  pass "edge re-discovered the restarted gear and resumed proxying"
else
  fail "edge did not re-discover the restarted gear"; dump_logs_on_verbose
fi

echo
info "${BOLD}Scenario 9${RESET}: platform plane — the hello pod validates the internal token"
# Platform-plane check (cpt-cf-adr-platform-plane-auth): the pod installs the
# internal-auth middleware. `/hello/v1/ping` is `.anonymous()`, so a MISSING
# internal token is permitted (permissive plane) and a MATCHING token accepted,
# but a PRESENT-but-INVALID token is rejected (401) before the handler runs.
# The valid secret matches the demo configs.
INTERNAL_TOKEN="dev-internal-token"
no_tok="$(http_code "$HELLO$PING_PATH")"
good_tok="$(_req code "$HELLO$PING_PATH" "X-ToolKit-Internal-Token: $INTERNAL_TOKEN")"
bad_tok="$(_req code "$HELLO$PING_PATH" "X-ToolKit-Internal-Token: wrong-secret")"
if [[ "$no_tok" == "200" && "$good_tok" == "200" && "$bad_tok" == "401" ]]; then
  pass "pod internal-auth: no-token=200, valid-token=200, invalid-token=401"
else
  fail "pod internal-auth mismatch: no-token=$no_tok valid=$good_tok invalid=$bad_tok"
  dump_logs_on_verbose
fi

# ==============================================================================
echo
if [[ "$FAILURES" == "0" ]]; then
  echo "${GREEN}${BOLD}All scenarios passed.${RESET}"
  exit 0
else
  echo "${RED}${BOLD}$FAILURES scenario(s) failed.${RESET} See $HOST_LOG , $HELLO_LOG , $SECURE_LOG."
  exit 1
fi
