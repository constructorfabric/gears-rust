# Shared helpers for .githooks/pre-commit and .githooks/pre-push (sourced, POSIX sh).
# Same PATH tweak as the Makefile: pipx installs cfs into ~/.local/bin.
PATH="$HOME/.local/bin:$PATH"
failed=0

# have <tool>: succeeds if the tool works. clippy/rustfmt are probed by running them:
# the rustup proxies exist on PATH even when the component is not installed.
have() {
    case "$1" in
        "bash>=4") bash -c '[ "${BASH_VERSINFO[0]}" -ge 4 ]' 2>/dev/null ;;
        clippy) cargo clippy --version >/dev/null 2>&1 ;;
        rustfmt) rustfmt --version >/dev/null 2>&1 ;;
        *) command -v "$1" >/dev/null 2>&1 ;;
    esac
}

# need <check> <tool> <install cmd>: succeeds if present, else prints a skip notice.
need() {
    have "$2" && return 0
    echo "hook: skipping $1: $2 not found (install: $3)"
    return 1
}

# step <check>: announces a check; follow with `cmd || fail <check> <fix hint>`.
step() { echo "hook: $1"; }

# fail <check> <fix hint>: records the failure, never aborts.
fail() {
    echo "hook: FAILED $1 -- $2 (bypass: git $HOOK_CMD --no-verify)"
    failed=1
}

# finish: exit non-zero if any check failed.
finish() { exit "$failed"; }
