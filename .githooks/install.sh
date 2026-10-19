#!/bin/sh
# Symlinks .githooks/{pre-commit,pre-push} into the git hooks dir (honours core.hooksPath)
# and records the opt-in marker `gears.hooks=true` in this clone's local git config.
# Usage: install.sh [--quiet | --uninstall]
# Installs missing links only; never overwrites a real file. No-op when GEARS_NO_HOOKS=1,
# in CI, or outside a git work tree. --quiet never fails (the Makefile auto-install runs it).
mode=${1:-}
[ -z "${GEARS_NO_HOOKS:-}" ] && [ -z "${CI:-}" ] || [ "$mode" = "--uninstall" ] || exit 0
top=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
hooks=$(git rev-parse --path-format=absolute --git-path hooks 2>/dev/null) || exit 0
cd "$top" || exit 0
[ -f .githooks/lib.sh ] || exit 0

fail=0
# failed <link>: report (unless --quiet) and remember the failure.
failed() {
    [ "$mode" = "--quiet" ] || echo "install.sh: failed to install $1" >&2
    fail=1
}

if [ "$mode" = "--uninstall" ]; then
    git config --local --unset gears.hooks 2>/dev/null || true
else
    git config --local gears.hooks true || failed "gears.hooks marker"
fi

for h in pre-commit pre-push; do
    link="$hooks/$h"
    if [ "$mode" = "--uninstall" ]; then
        target=$(readlink "$link" 2>/dev/null) || continue
        case "$target" in
            "$top/.githooks/$h") rm "$link" && echo "removed $link" || failed "$link" ;;
            */.githooks/"$h") echo "kept $link -> $target (not this clone)" ;;
        esac
        continue
    fi
    if [ -e "$link" ] && [ ! -L "$link" ]; then
        [ "$mode" = "--quiet" ] || echo "SKIP: $link exists and is not a symlink; call $top/.githooks/$h from it manually"
        continue
    fi
    # Present (live) link: leave it; dangling link (clone removed): replace.
    [ -e "$link" ] && { [ "$mode" = "--quiet" ] || echo "ok: $link"; continue; }
    if mkdir -p "$hooks" && ln -sf "$top/.githooks/$h" "$link"; then
        installed=1
        [ "$mode" = "--quiet" ] || echo "installed $link"
    else
        failed "$link"
    fi
done
[ "$mode" != "--quiet" ] || [ -z "${installed:-}" ] ||
    echo "git hooks installed (.githooks); bypass with --no-verify, opt out with GEARS_NO_HOOKS=1"
[ "$mode" = "--quiet" ] && exit 0
exit "$fail"
