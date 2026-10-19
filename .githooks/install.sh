#!/bin/sh
# Symlinks .githooks/{pre-commit,pre-push} into the git hooks dir (honours core.hooksPath).
# Usage: install.sh [--quiet | --uninstall]
# Installs missing links only; never overwrites a real file. No-op when GEARS_NO_HOOKS=1,
# in CI, or outside a git work tree.
mode=${1:-}
[ -z "${GEARS_NO_HOOKS:-}" ] && [ -z "${CI:-}" ] || [ "$mode" = "--uninstall" ] || exit 0
top=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
hooks=$(git rev-parse --git-path hooks 2>/dev/null) || exit 0
cd "$top" || exit 0
[ -f .githooks/lib.sh ] || exit 0

for h in pre-commit pre-push; do
    link="$hooks/$h"
    if [ "$mode" = "--uninstall" ]; then
        case "$(readlink "$link" 2>/dev/null)" in
            */.githooks/"$h") rm "$link" && echo "removed $link" ;;
        esac
        continue
    fi
    if [ -e "$link" ] && [ ! -L "$link" ]; then
        [ "$mode" = "--quiet" ] || echo "SKIP: $link exists and is not a symlink; call $top/.githooks/$h from it manually"
        continue
    fi
    # Present (live) link: leave it; dangling link (clone removed): replace.
    [ -e "$link" ] && { [ "$mode" = "--quiet" ] || echo "ok: $link"; continue; }
    mkdir -p "$hooks" && ln -sf "$top/.githooks/$h" "$link" || continue
    installed=1
    [ "$mode" = "--quiet" ] || echo "installed $link"
done
[ "$mode" != "--quiet" ] || [ -z "${installed:-}" ] ||
    echo "git hooks installed (.githooks); bypass with --no-verify, opt out with GEARS_NO_HOOKS=1"
exit 0
