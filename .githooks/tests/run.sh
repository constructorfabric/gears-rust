#!/bin/sh
# Tests for .githooks (POSIX sh). Run from the repo root: sh .githooks/tests/run.sh
# Everything happens in a throwaway dir with its own global git config; the real hooks dir and
# this repo's .git are never touched. Tools are stubbed via PATH; rustfmt cases need the real one.
# Prints PASS|FAIL|SKIP per case and exits 1 if any case failed.
src=$(pwd)
[ -f "$src/.githooks/lib.sh" ] || { echo "run from the repo root: sh .githooks/tests/run.sh" >&2; exit 2; }
T=$(mktemp -d) || exit 2
trap 'chmod -R u+rwx "$T" 2>/dev/null; rm -rf "$T"' EXIT INT TERM
# lib.sh prepends $HOME/.local/bin (real cfs): use an empty HOME, keeping rustup's dirs
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
mkdir "$T/home"; HOME="$T/home"; export HOME
export GIT_CONFIG_GLOBAL="$T/gitconfig" GIT_CONFIG_NOSYSTEM=1 STUB_LOG="$T/stub.log"
unset CI GEARS_NO_HOOKS FAIL_CLIPPY
git config --global user.name hooktests
git config --global user.email hooktests@example.com
git config --global init.defaultBranch main
git config --global commit.gpgsign false

# Stub tools log their argv to $STUB_LOG.
mkdir "$T/stubs"
cat > "$T/stubs/cfs" <<'STUB'
#!/bin/sh
echo "cfs $*" >> "$STUB_LOG"
exit 0
STUB
cp "$T/stubs/cfs" "$T/stubs/jq"; cp "$T/stubs/cfs" "$T/stubs/cargo-gears"
cat > "$T/stubs/lychee" <<'STUB'
#!/bin/sh
for a in "$@"; do echo "lychee-arg $a" >> "$STUB_LOG"; done
exit 0
STUB
cat > "$T/stubs/cargo" <<'STUB'
#!/bin/sh
if [ "$1 $2" = "clippy --version" ]; then [ -z "${FAIL_CLIPPY:-}" ]; exit; fi
echo "cargo $*" >> "$STUB_LOG"
exit 0
STUB
chmod +x "$T/stubs/"*
PATH="$T/stubs:$PATH"

nfail=0
pass() { echo "PASS $1"; }
fail() { echo "FAIL $1"; nfail=$((nfail + 1)); }
skip() { echo "SKIP $1${2:+ ($2)}"; }
# assert <case> <cmd...>: PASS if the command succeeds.
assert() { name=$1; shift; if "$@"; then pass "$name"; else fail "$name"; fi; }

case $(uname -s) in MINGW* | MSYS* | CYGWIN*) windows=1 ;; *) windows="" ;; esac
have_rustfmt=""; rustfmt --version >/dev/null 2>&1 && have_rustfmt=1

# mkrepo <name>: fresh repo $r with a copy of .githooks (no tests dir).
mkrepo() {
    r="$T/$1"
    rm -rf "$r"; mkdir -p "$r/.githooks"
    git -C "$r" init -q
    for f in "$src"/.githooks/*; do [ -f "$f" ] && cp "$f" "$r/.githooks/"; done
    cp "$src/rustfmt.toml" "$r/" 2>/dev/null || :
}
# hook <repo> <hook> [args]: runs a hook script of the repo; output in $T/out, status in $rc.
hook() {
    hr=$1; hh=$2; shift 2
    rc=0
    (cd "$hr" && sh ".githooks/$hh" "$@") > "$T/out" 2>&1 || rc=$?
}
marker() { git -C "$1" config --local --get gears.hooks; }
is_true() { [ "$(marker "$1")" = true ]; }
no_marker() { [ -z "$(marker "$1")" ]; }
# link_hook <hook> <where>: symlink to the source hook, falling back to a copy (Git Bash).
link_hook() { ln -sf "$1" "$2" 2>/dev/null || cp "$1" "$2"; }

# ---- install ----
mkrepo i1; mkdir "$r/sub"
(cd "$r/sub" && sh ../.githooks/install.sh) > "$T/out" 2>&1
assert "install from subdir: pre-commit" test -e "$r/.git/hooks/pre-commit"
assert "install from subdir: pre-push" test -e "$r/.git/hooks/pre-push"
assert "install sets marker" is_true "$r"

git -C "$r" config core.hooksPath rel-hooks
(cd "$r/sub" && sh ../.githooks/install.sh) > "$T/out" 2>&1
assert "install with relative hooksPath" test -e "$r/rel-hooks/pre-commit"

git -C "$r" config core.hooksPath "$T/abs-hooks"
(cd "$r/sub" && sh ../.githooks/install.sh) > "$T/out" 2>&1
assert "install with absolute hooksPath" test -e "$T/abs-hooks/pre-push"

mkrepo i2
hook "$r" install.sh --quiet
assert "install --quiet exits 0" test "$rc" = 0
assert "install --quiet installs hooks" test -e "$r/.git/hooks/pre-push"

# ---- uninstall ----
mkrepo u1; hook "$r" install.sh
hook "$r" install.sh --uninstall
assert "uninstall removes own link" test ! -e "$r/.git/hooks/pre-commit"
assert "uninstall unsets marker" no_marker "$r"

mkrepo u2; mkdir -p "$r/.git/hooks"; hook "$r" install.sh
# Foreign target must exist: Git Bash `ln -s` copies the target and fails on a missing one.
mkdir -p "$T/other/.githooks"; echo '#!/bin/sh' > "$T/other/.githooks/pre-commit"
ln -sf "$T/other/.githooks/pre-commit" "$r/.git/hooks/pre-commit"
hook "$r" install.sh --uninstall
assert "uninstall keeps foreign link" test -e "$r/.git/hooks/pre-commit"
if [ -L "$r/.git/hooks/pre-commit" ]; then
    assert "uninstall reports foreign link" grep -q 'not this clone' "$T/out"
else
    skip "uninstall reports foreign link" "ln -s made a copy here"
fi

mkrepo u3; mkdir -p "$r/.git/hooks"
cp "$r/.githooks/pre-commit" "$r/.git/hooks/pre-commit"
hook "$r" install.sh
assert "install treats identical copy as ours" grep -q "^ok: .*pre-commit" "$T/out"
assert "install does not skip identical copy" test -z "$(grep SKIP "$T/out")"
hook "$r" install.sh --uninstall
assert "uninstall removes identical copy" test ! -e "$r/.git/hooks/pre-commit"

mkrepo u4; mkdir -p "$r/.git/hooks"; echo '#!/bin/sh' > "$r/.git/hooks/pre-commit"
hook "$r" install.sh
assert "install skips foreign regular file" grep -q '^SKIP: .*pre-commit' "$T/out"
hook "$r" install.sh --uninstall
assert "uninstall keeps foreign regular file" test -f "$r/.git/hooks/pre-commit"

# ---- unwritable hooks dir ----
mkrepo w1; mkdir -p "$r/.git/hooks"; chmod 555 "$r/.git/hooks"
if [ -n "$windows" ] || [ "$(id -u)" = 0 ]; then
    skip "unwritable hooks dir: plain exits 1" "chmod does not restrict here"
    skip "unwritable hooks dir: --quiet exits 0" "chmod does not restrict here"
else
    hook "$r" install.sh
    assert "unwritable hooks dir: plain exits 1" test "$rc" = 1
    hook "$r" install.sh --quiet
    assert "unwritable hooks dir: --quiet exits 0" test "$rc" = 0
fi
chmod 755 "$r/.git/hooks"

# ---- untrusted repo behind a shared (global) hooks dir ----
mkrepo e1; mkdir "$T/shared"
git config --global core.hooksPath "$T/shared"
link_hook "$src/.githooks/pre-commit" "$T/shared/pre-commit"
printf '#!/bin/sh\necho ran > "%s/pwned"\n' "$T" > "$r/.githooks/pre-commit"
chmod +x "$r/.githooks/pre-commit"
(cd "$r" && echo x > f && git add f && git commit -qm a) > "$T/out" 2>&1
assert "untrusted repo without marker: own hook not run" test ! -e "$T/pwned"
git -C "$r" config --local gears.hooks true
(cd "$r" && echo y > f && git add f && git commit -qm b) > "$T/out" 2>&1
assert "repo with marker: own hook runs" test -e "$T/pwned"
git config --global --unset core.hooksPath

# ---- pre-commit ----
mkrepo f1; git -C "$r" config --local gears.hooks true
if [ -z "$have_rustfmt" ]; then
    skip "rustfmt: unformatted staged fails" "no rustfmt"
    skip "rustfmt: formatted staged passes" "no rustfmt"
else
    printf 'fn main(){\n1+1;\n}\n' > "$r/a.rs"; git -C "$r" add a.rs
    printf 'fn main() {\n    1 + 1;\n}\n' > "$r/a.rs"
    hook "$r" pre-commit
    assert "rustfmt: unformatted staged fails (worktree formatted)" test "$rc" != 0
    git -C "$r" add a.rs
    printf 'fn main(){\n1+1;\n}\n' > "$r/a.rs"
    hook "$r" pre-commit
    assert "rustfmt: formatted staged passes (worktree unformatted)" test "$rc" = 0
fi

mkrepo f2; git -C "$r" config --local gears.hooks true
: > "$STUB_LOG"
echo hi > "$r/résumé.md"; git -C "$r" add .
hook "$r" pre-commit
assert "non-ASCII path reaches lychee" grep -Fqx 'lychee-arg résumé.md' "$STUB_LOG"

mkrepo f3; git -C "$r" config --local gears.hooks true
mkdir -p "$r/docs"; echo hi > "$r/docs/a.md"; git -C "$r" add .
: > "$STUB_LOG"
hook "$r" pre-commit
assert "docs change runs cfs validate" grep -qx 'cfs validate' "$STUB_LOG"

mkrepo f4; git -C "$r" config --local gears.hooks true
printf 'a\r\nb\r\n' > "$r/crlf.txt"; git -C "$r" add crlf.txt
hook "$r" pre-commit
assert "CRLF staged file fails pre-commit" test "$rc" != 0
assert "CRLF failure names the check" grep -q 'FAILED crlf' "$T/out"

mkrepo f5; git -C "$r" config --local gears.hooks true
hook "$r" pre-commit
assert "nothing staged exits 0" test "$rc" = 0

# ---- pre-push ----
mkrepo p1; r="$T/p1"
mkdir -p "$r/.github/scripts" "$r/pk/src"
printf '#!/bin/sh\nexit 0\n' > "$r/.github/scripts/check-forbidden-words.sh"
chmod +x "$r/.github/scripts/check-forbidden-words.sh"
printf '[package]\nname = "pk"\n' > "$r/pk/Cargo.toml"
echo 'fn a() {}' > "$r/pk/src/lib.rs"; printf '[workspace]\n' > "$r/Cargo.toml"
git -C "$r" add -A; git -C "$r" commit -qm base
git -C "$r" branch -M main; git -C "$r" config --local gears.hooks true
git -C "$r" checkout -qb feat0
zeros=0000000000000000000000000000000000000000
# push <sha>: runs pre-push as git would; output in $T/out, cargo calls in $STUB_LOG.
push() {
    : > "$STUB_LOG"; rc=0
    echo "refs/heads/feat $1 refs/heads/feat $zeros" |
        (cd "$r" && sh .githooks/pre-push) > "$T/out" 2>&1 || rc=$?
}
head_sha() { git -C "$r" rev-parse HEAD; }

echo '# x' >> "$r/Cargo.toml"; git -C "$r" commit -qam c1
push "$(head_sha)"
assert "pre-push: root Cargo.toml -> clippy --workspace" grep -q 'cargo clippy --workspace' "$STUB_LOG"
assert "pre-push: exits 0" test "$rc" = 0

git -C "$r" checkout -q main; git -C "$r" checkout -qb feat
old=$(head_sha)
echo 'fn b() {}' >> "$r/pk/src/lib.rs"; git -C "$r" commit -qam c2
new=$(head_sha)
git -C "$r" checkout -q --detach "$old"
push "$new"
assert "pre-push: sha != HEAD skips clippy" grep -q 'not the checked-out HEAD' "$T/out"
assert "pre-push: sha != HEAD runs no clippy" test -z "$(grep clippy "$STUB_LOG")"
git -C "$r" checkout -q feat

push "$(head_sha)"
assert "pre-push: package change -> -p pk" grep -q 'cargo clippy -p pk ' "$STUB_LOG"

echo 'fn c() {}' >> "$r/pk/src/lib.rs"
push "$(head_sha)"
assert "pre-push: dirty .rs -> note" grep -q 'uncommitted Rust/Cargo changes' "$T/out"
git -C "$r" checkout -q -- pk/src/lib.rs

FAIL_CLIPPY=1 push "$(head_sha)"
assert "pre-push: cargo without clippy -> skip notice" grep -q 'skipping clippy: clippy not found' "$T/out"
assert "pre-push: cargo without clippy exits 0" test "$rc" = 0

[ "$nfail" = 0 ] || { echo "$nfail case(s) failed"; exit 1; }
