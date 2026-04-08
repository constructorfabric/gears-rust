#!/usr/bin/env bash
#
# run.sh — Run E2E tests with auto-managed Python venv.
#
# Usage:
#   ./testing/e2e/run.sh modules/mini_chat/ -v        # offline (default)
#   ./testing/e2e/run.sh modules/mini_chat/ --mode online -v
#   ./testing/e2e/run.sh modules/mini_chat/ -m openai -v

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
VENV_DIR="$SCRIPT_DIR/.venv"

# ── Python venv ───────────────────────────────────────────────────────────
REQUIREMENTS="$SCRIPT_DIR/requirements.txt"
STAMP="$VENV_DIR/.requirements-stamp"

if [[ ! -d "$VENV_DIR" ]]; then
    echo "==> Creating Python venv"
    python3 -m venv "$VENV_DIR"
    "$VENV_DIR/bin/pip" install -q -r "$REQUIREMENTS"
    cp "$REQUIREMENTS" "$STAMP"
    echo "[OK] Venv ready"
elif ! diff -q "$REQUIREMENTS" "$STAMP" &>/dev/null; then
    echo "==> Updating venv (requirements changed)"
    "$VENV_DIR/bin/pip" install -q -r "$REQUIREMENTS"
    cp "$REQUIREMENTS" "$STAMP"
    echo "[OK] Venv updated"
fi

# ── Run tests ─────────────────────────────────────────────────────────────
echo ""
echo "==> Running pytest"
cd "$SCRIPT_DIR"
exec "$VENV_DIR/bin/python" -m pytest -v "$@"
