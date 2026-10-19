# Shared helpers for .githooks/pre-commit and .githooks/pre-push (sourced, POSIX sh).
# Same PATH tweak as the Makefile: pipx installs cfs into ~/.local/bin.
PATH="$HOME/.local/bin:$PATH"
failed=0

# have <tool>: succeeds if the tool is on PATH.
have() {
    if [ "$1" = "bash>=4" ]; then
        bash -c '[ "${BASH_VERSINFO[0]}" -ge 4 ]' 2>/dev/null
    else
        command -v "$1" >/dev/null 2>&1
    fi
}

# need <check> <tool> <install cmd>: succeeds if present, else prints a skip notice.
need() {
    have "$2" && return 0
    echo "hook: skipping $1: $2 not found (install: $3)"
    return 1
}

# run <check> <fix hint> <cmd...>: runs the command, records failure, never aborts.
run() {
    check=$1; hint=$2; shift 2
    echo "hook: $check"
    if ! "$@"; then
        echo "hook: FAILED $check -- $hint (bypass: git $HOOK_CMD --no-verify)"
        failed=1
    fi
}

# finish: exit non-zero if any check failed.
finish() { exit "$failed"; }
