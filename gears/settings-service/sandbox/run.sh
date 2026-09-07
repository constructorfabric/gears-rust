#!/usr/bin/env bash
# Created: 2026-09-07 by Constructor Tech
#
# The settings-service sandbox: a focused example server with the demo
# declarations, and a static page that drives every endpoint through a
# same-origin proxy.
#
#   ./run.sh server   build and start the example server on $SETTINGS_PORT (8087)
#   ./run.sh ui       serve the sandbox page on $SANDBOX_PORT (8090), proxying to the server
#
# Run each in its own terminal, then open http://127.0.0.1:8090/.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
SETTINGS_PORT="${SETTINGS_PORT:-8087}"
SANDBOX_PORT="${SANDBOX_PORT:-8090}"
FEATURES="settings-service,settings-demo,static-credstore,static-tenants,static-authn,static-authz,account-management,static-idp"

case "${1:-}" in
  server)
    cd "$ROOT"
    echo "building the focused example server (features: $FEATURES) ..."
    cargo build --bin cf-gears-example-server --no-default-features --features "$FEATURES"
    echo "starting on http://127.0.0.1:$SETTINGS_PORT with config/e2e-local.yaml"
    echo "  data lives in ~/.cf-gears/settings-service/ (sqlite); delete it for a fresh start"
    exec target/debug/cf-gears-example-server --config config/e2e-local.yaml --port "$SETTINGS_PORT"
    ;;
  ui)
    echo "sandbox page on http://127.0.0.1:$SANDBOX_PORT/ -> proxying /settings-service/* to http://127.0.0.1:$SETTINGS_PORT"
    exec env SETTINGS_UPSTREAM="http://127.0.0.1:$SETTINGS_PORT" SANDBOX_PORT="$SANDBOX_PORT" python3 "$HERE/proxy.py"
    ;;
  *)
    echo "usage: $0 server | ui" >&2
    exit 2
    ;;
esac
