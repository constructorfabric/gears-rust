#!/usr/bin/env bash
#
# Preview the documentation website (gears-rust-web-docs) with the LOCAL
# docs/web-docs content from this gears-rust checkout — including uncommitted edits.
#
# The web docs site lives in a separate repo (TypeScript/Astro). This script
# clones it into a gitignored cache dir, then runs its dev server pointed at the
# local content via GEARS_RUST_PATH. The site's `predev` hook syncs the content.
#
# Usage: make docs-preview   (or: bash tools/scripts/docs-preview.sh)

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
CACHE_DIR="${GEARS_DOCS_CACHE:-$REPO_ROOT/.web-docs-preview}"
DOCS_REPO="${GEARS_DOCS_REPO:-https://github.com/constructorfabric/gears-rust-web-docs.git}"
# Site branch to preview against. Override to test an unmerged site PR, e.g.
# GEARS_DOCS_REF=fix/asto-config-new-section make docs-preview
DOCS_REF="${GEARS_DOCS_REF:-main}"

# --- Prerequisite: Node >= 22.13 (required by Astro / the docs site) ---
if ! command -v node >/dev/null 2>&1; then
  echo "ERROR: node not found. Install Node.js >= 22.13 to preview the docs site." >&2
  exit 1
fi

# Try to switch to Node 22 via nvm if current version is too old
node_major="$(node -p 'process.versions.node.split(".")[0]')"
node_minor="$(node -p 'process.versions.node.split(".")[1]')"
if [ "$node_major" -lt 22 ] || { [ "$node_major" -eq 22 ] && [ "$node_minor" -lt 13 ]; }; then
  if command -v nvm >/dev/null 2>&1 || [ -s "$HOME/.nvm/nvm.sh" ]; then
    echo "==> Current Node $(node -v) is too old. Switching to Node 22 via nvm..."
    # shellcheck source=/dev/null
    [ -s "$HOME/.nvm/nvm.sh" ] && . "$HOME/.nvm/nvm.sh"
    nvm use 22 >/dev/null 2>&1 || nvm use 22
    echo "==> Now using Node $(node -v)"
  else
    echo "ERROR: Node $(node -v) is too old (need >= 22.13). Install nvm or upgrade Node.js." >&2
    exit 1
  fi
fi

# --- Clone or update the docs site into the cache dir (on $DOCS_REF) ---
# The cache is disposable: hard-reset to the fetched ref so a stale checkout or
# locally-modified tracked files (e.g. .sync-lock.json) can't leave it behind.
if [ -d "$CACHE_DIR/.git" ]; then
  echo "==> Updating cached docs site in $CACHE_DIR ($DOCS_REF)"
  git -C "$CACHE_DIR" fetch --depth 1 origin "$DOCS_REF"
  git -C "$CACHE_DIR" reset --hard FETCH_HEAD
else
  echo "==> Cloning docs site into $CACHE_DIR ($DOCS_REF)"
  git clone --depth 1 --branch "$DOCS_REF" "$DOCS_REPO" "$CACHE_DIR"
fi

# --- Resolve the cargo-gears docs source so the CLI section appears too ---
# The site syncs from two repos. Prefer a sibling checkout (picks up your local,
# uncommitted edits); otherwise shallow-clone into a gitignored cache. Override
# with CARGO_GEARS_PATH to point at an arbitrary checkout.
CARGO_GEARS_REPO="${CARGO_GEARS_REPO:-https://github.com/constructorfabric/cargo-gears.git}"
if [ -z "${CARGO_GEARS_PATH:-}" ]; then
  if [ -d "$REPO_ROOT/../cargo-gears/docs/web-docs" ]; then
    CARGO_GEARS_PATH="$(cd "$REPO_ROOT/../cargo-gears" && pwd)"
  else
    CG="${GEARS_DOCS_SOURCES:-$REPO_ROOT/.web-docs-sources}/cargo-gears"
    if [ -d "$CG/.git" ]; then
      echo "==> Updating cached cargo-gears source in $CG"
      git -C "$CG" pull --ff-only || echo "WARNING: could not fast-forward cached cargo-gears; using existing checkout." >&2
    else
      echo "==> Cloning cargo-gears source into $CG"
      git clone --depth 1 "$CARGO_GEARS_REPO" "$CG"
    fi
    CARGO_GEARS_PATH="$CG"
  fi
fi
export CARGO_GEARS_PATH
echo "==> cargo-gears docs source: $CARGO_GEARS_PATH"

cd "$CACHE_DIR"

# Mode: "dev" (default) serves the site; "build" produces a static build in
# dist/ (used by `make web-docs-check` for offline link checking).
MODE="${1:-dev}"

PKG="npm"
command -v pnpm >/dev/null 2>&1 && PKG="pnpm"

$PKG install

# Sync the local content into the site (the `predev` hook only runs for dev).
GEARS_RUST_PATH="$REPO_ROOT" CARGO_GEARS_PATH="$CARGO_GEARS_PATH" BASE=/ $PKG run sync

if [ "$MODE" = "build" ]; then
  echo "==> Building docs site (content from $REPO_ROOT/docs/web-docs)"
  GEARS_RUST_PATH="$REPO_ROOT" CARGO_GEARS_PATH="$CARGO_GEARS_PATH" BASE=/ $PKG run build
  echo "==> Build complete: $CACHE_DIR/dist"
else
  echo "==> Starting docs site at http://localhost:4321 (content from $REPO_ROOT/docs/web-docs)"
  GEARS_RUST_PATH="$REPO_ROOT" CARGO_GEARS_PATH="$CARGO_GEARS_PATH" BASE=/ $PKG run dev
fi
