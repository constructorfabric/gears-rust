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
unset CI GEARS_NO_HOOKS FAIL_CLIPPY STUB_METADATA_FAIL STUB_DRAIN GEARS_HOOKS_BASE
export STUB_METADATA="$T/metadata.json"
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
cp "$T/stubs/cfs" "$T/stubs/cargo-gears"
cat > "$T/stubs/lychee" <<'STUB'
#!/bin/sh
for a in "$@"; do echo "lychee-arg $a" >> "$STUB_LOG"; done
exit 0
STUB
cat > "$T/stubs/cargo" <<'STUB'
#!/bin/sh
if [ "$1 $2" = "clippy --version" ]; then [ -z "${FAIL_CLIPPY:-}" ]; exit; fi
[ -z "${STUB_DRAIN:-}" ] || cat > /dev/null
if [ "$1" = metadata ]; then [ -z "${STUB_METADATA_FAIL:-}" ] || exit 101; cat "$STUB_METADATA"; exit 0; fi
echo "cargo $*" >> "$STUB_LOG"
exit 0
STUB
chmod +x "$T/stubs/"*
# No jq stub: the pre-push package logic needs the real one (cases needing it skip when it is absent).
real_path=$PATH
PATH="$T/stubs:$PATH"

nfail=0
# pass <case>: print a PASS line for the case.
pass() { echo "PASS $1"; }
fail() { echo "FAIL $1"; nfail=$((nfail + 1)); }
# skip <case> [reason]: print a SKIP line for the case, with the reason in parentheses if given.
skip() { echo "SKIP $1${2:+ ($2)}"; }
# assert <case> <cmd...>: PASS if the command succeeds.
assert() { name=$1; shift; if "$@"; then pass "$name"; else fail "$name"; fi; }

case $(uname -s) in MINGW* | MSYS* | CYGWIN*) windows=1 ;; *) windows="" ;; esac
have_rustfmt=""; rustfmt --version >/dev/null 2>&1 && have_rustfmt=1
# Real tools, as seen before the stubs were put on PATH.
have_bash4=""; PATH=$real_path bash -c '[ "${BASH_VERSINFO[0]}" -ge 4 ]' 2>/dev/null && have_bash4=1
have_jq=""; PATH=$real_path command -v jq >/dev/null 2>&1 && have_jq=1
have_pcre=""
{ printf 'a\n' | PATH=$real_path grep -qP a || printf 'a\n' | PATH=$real_path ggrep -qP a; } 2>/dev/null && have_pcre=1

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
# marker <repo>: print the repo's local gears.hooks config value (empty if unset).
marker() ( repo=$1; git -C "$repo" config --local --get gears.hooks )
is_true() ( repo=$1; [ "$(marker "$repo")" = true ] )
# is_false <repo>: succeed if the repo's gears.hooks marker is exactly false.
is_false() ( repo=$1; [ "$(marker "$repo")" = false ] )
# link_hook <hook> <where>: symlink to the source hook, falling back to a copy (Git Bash).
link_hook() ( hook_src=$1 hook_dst=$2; ln -sf "$hook_src" "$hook_dst" 2>/dev/null || cp "$hook_src" "$hook_dst" )

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
assert "install --quiet on fresh repo sets marker" is_true "$r"

# ---- uninstall ----
mkrepo u1; hook "$r" install.sh
hook "$r" install.sh --uninstall
assert "uninstall removes own link" test ! -e "$r/.git/hooks/pre-commit"
assert "uninstall sets marker to false" is_false "$r"
hook "$r" install.sh --quiet
assert "--quiet after uninstall creates no pre-commit" test ! -e "$r/.git/hooks/pre-commit"
assert "--quiet after uninstall creates no pre-push" test ! -e "$r/.git/hooks/pre-push"
assert "--quiet after uninstall keeps marker false" is_false "$r"
hook "$r" install.sh
assert "plain install after uninstall re-creates pre-commit" test -e "$r/.git/hooks/pre-commit"
assert "plain install after uninstall re-creates pre-push" test -e "$r/.git/hooks/pre-push"
assert "plain install after uninstall sets marker true" is_true "$r"

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

# A copy of an older hook version (Git Bash) is still ours: install refreshes it, uninstall removes it.
mkrepo u5; mkdir -p "$r/.git/hooks"
cp "$r/.githooks/pre-commit" "$r/.git/hooks/pre-commit"; echo '# old version' >> "$r/.git/hooks/pre-commit"
hook "$r" install.sh
assert "install reports updated stale copy" grep -q "^updated .*pre-commit" "$T/out"
assert "install refreshes stale copy" cmp -s "$r/.git/hooks/pre-commit" "$r/.githooks/pre-commit"
assert "install does not skip stale copy" test -z "$(grep SKIP "$T/out")"
rm -f "$r/.git/hooks/pre-commit"
cp "$r/.githooks/pre-commit" "$r/.git/hooks/pre-commit"; echo '# old version' >> "$r/.git/hooks/pre-commit"
hook "$r" install.sh --uninstall
assert "uninstall removes stale copy" test ! -e "$r/.git/hooks/pre-commit"

mkrepo u6; mkdir -p "$r/.git/hooks"; printf '#!/bin/sh\necho mine\n' > "$r/.git/hooks/pre-push"
cp "$r/.git/hooks/pre-push" "$T/foreign-copy"
hook "$r" install.sh
assert "install skips foreign file" grep -q '^SKIP: .*pre-push' "$T/out"
assert "install leaves foreign file untouched" cmp -s "$r/.git/hooks/pre-push" "$T/foreign-copy"
hook "$r" install.sh --uninstall
assert "uninstall leaves foreign file untouched" cmp -s "$r/.git/hooks/pre-push" "$T/foreign-copy"

# Another clone may use a copy in a shared hooks dir: uninstall keeps it.
mkrepo u3s; mkdir -p "$T/shared-copy"; git -C "$r" config core.hooksPath "$T/shared-copy"
cp "$r/.githooks/pre-commit" "$T/shared-copy/pre-commit"
hook "$r" install.sh --uninstall
assert "uninstall keeps copy in shared hooks dir" test -e "$T/shared-copy/pre-commit"
assert "uninstall reports kept shared copy" grep -q 'shared hooks dir' "$T/out"

mkrepo u4; mkdir -p "$r/.git/hooks"; echo '#!/bin/sh' > "$r/.git/hooks/pre-commit"
hook "$r" install.sh
assert "install skips foreign regular file" grep -q '^SKIP: .*pre-commit' "$T/out"
hook "$r" install.sh --uninstall
assert "uninstall keeps foreign regular file" test -f "$r/.git/hooks/pre-commit"

# ---- make auto-install (the parse-time lines of gears-rust.mk, verbatim) ----
if ! command -v make >/dev/null 2>&1; then
    skip "make auto-install: installs hooks" "no make"
    skip "make auto-install: respects uninstall" "no make"
else
    mkrepo m1
    grep -E '^(_HOOKS_MSG :=|\$\(if \$\(_HOOKS_MSG\))' "$src/gears-rust.mk" > "$r/Makefile"
    printf 'all:\n\t@:\n' >> "$r/Makefile"
    (cd "$r" && make) > "$T/out" 2>&1
    assert "make auto-install: installs hooks" test -e "$r/.git/hooks/pre-push"
    assert "make auto-install: sets marker" is_true "$r"
    hook "$r" install.sh --uninstall
    (cd "$r" && make) > "$T/out" 2>&1
    assert "make auto-install: respects uninstall (no links)" test ! -e "$r/.git/hooks/pre-commit"
    assert "make auto-install: respects uninstall (marker)" is_false "$r"
fi

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
# app depends on pk; fz is outside the workspace; cf-gears-oagw sits in gears/oagw
for pn in app:app fz:fz cf-gears-oagw:gears/oagw; do
    mkdir -p "$r/${pn#*:}/src"
    printf '[package]\nname = "%s"\n' "${pn%%:*}" > "$r/${pn#*:}/Cargo.toml"
    echo 'fn a() {}' > "$r/${pn#*:}/src/lib.rs"
done
cat > "$T/metadata.json" <<'JSON'
{"packages":[{"id":"pk","name":"pk"},{"id":"app","name":"app"},{"id":"fz","name":"fz"},{"id":"cf-gears-oagw","name":"cf-gears-oagw"}],
 "workspace_members":["pk","app","cf-gears-oagw"],
 "resolve":{"nodes":[{"id":"pk","deps":[]},{"id":"app","deps":[{"pkg":"pk"}]},{"id":"cf-gears-oagw","deps":[]}]}}
JSON
git -C "$r" add -A; git -C "$r" commit -qm base
git -C "$r" branch -M main; git -C "$r" config --local gears.hooks true
git -C "$r" checkout -qb feat0
zeros=0000000000000000000000000000000000000000
# push <sha>: runs pre-push as git would; output in $T/out, cargo calls in $STUB_LOG.
push() {
    push_sha=$1
    : > "$STUB_LOG"; rc=0
    echo "refs/heads/feat $push_sha refs/heads/feat $zeros" |
        (cd "$r" && sh .githooks/pre-push) > "$T/out" 2>&1 || rc=$?
}
# head_sha: print the HEAD commit SHA of the test repo $r.
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
if [ -n "$have_jq" ]; then
    assert "pre-push: package change -> -p pk" grep -q 'cargo clippy .*-p pk ' "$STUB_LOG"
else
    skip "pre-push: package change -> -p pk" "needs jq"
fi

echo 'fn c() {}' >> "$r/pk/src/lib.rs"
push "$(head_sha)"
assert "pre-push: dirty .rs -> note" grep -q 'uncommitted Rust/Cargo changes' "$T/out"
git -C "$r" checkout -q -- pk/src/lib.rs

FAIL_CLIPPY=1; export FAIL_CLIPPY
push "$(head_sha)"
unset FAIL_CLIPPY
assert "pre-push: cargo without clippy -> skip notice" grep -q 'skipping clippy: clippy not found' "$T/out"
assert "pre-push: cargo without clippy exits 0" test "$rc" = 0

# ---- pre-push: workspace members, dependents, dylint, base, stdin ----
# on_branch <name> <file>: new branch off main with one commit touching <file>; push it; back on feat.
on_branch() {
    git -C "$r" checkout -q main; git -C "$r" checkout -qb "$1"
    echo 'fn z() {}' >> "$r/$2"; git -C "$r" commit -qam "$1"
    push "$(head_sha)"
    git -C "$r" checkout -q feat; git -C "$r" branch -qD "$1"
}
# nojq <case-less>: runs push with a PATH that has the stubs and git/sed/grep etc. but no jq.
nojq() {
    mkdir -p "$T/nojq"
    for t in git sh bash grep sed sort dirname head cat env tr uname basename wc awk id; do
        tp=$(PATH=$real_path command -v "$t") && link_hook "$tp" "$T/nojq/$t"
    done
    saved_path=$PATH; PATH="$T/stubs:$T/nojq"
    "$@"
    PATH=$saved_path
}
if [ -z "$have_jq" ]; then
    for c in "change outside the workspace is skipped" "change in pk also checks dependents" \
        "no jq -> clippy --workspace" "metadata failure -> clippy --workspace" "oagw change runs the cluster dylint" \
        "metadata failure + oagw change runs the cluster dylint"; do skip "pre-push: $c" "needs jq"; done
else
    on_branch feat-fz fz/src/lib.rs
    assert "pre-push: non-member package: no -p fz" test -z "$(grep -e '-p fz' "$STUB_LOG")"
    assert "pre-push: non-member package: no clippy" test -z "$(grep 'cargo clippy' "$STUB_LOG")"
    assert "pre-push: non-member package: skip line" grep -q 'hook: clippy: skipping fz (not a workspace member)' "$T/out"
    assert "pre-push: non-member package: exits 0" test "$rc" = 0

    push "$(head_sha)"
    assert "pre-push: pk change -> -p pk" grep -q 'cargo clippy .*-p pk ' "$STUB_LOG"
    assert "pre-push: pk change -> dependent -p app" grep -q 'cargo clippy .*-p app ' "$STUB_LOG"
    assert "pre-push: pk change -> summary" grep -q 'clippy: 2 package(s): 1 changed + 1 dependent(s)' "$T/out"
    assert "pre-push: pk only -> no OAGW cluster dylint" test -z "$(grep -e '-P cf-gears-oagw' "$STUB_LOG")"
    assert "pre-push: pk only -> workspace dylint" grep -q 'cargo gears lint --dylint$' "$STUB_LOG"

    nojq push "$(head_sha)"
    assert "pre-push: no jq -> clippy --workspace" grep -q 'cargo clippy --workspace' "$STUB_LOG"
    assert "pre-push: no jq -> note" grep -q 'note: jq not found (install: brew install jq / apt install jq); running clippy on the whole workspace' "$T/out"

    STUB_METADATA_FAIL=1; export STUB_METADATA_FAIL
    push "$(head_sha)"
    unset STUB_METADATA_FAIL
    assert "pre-push: metadata failure -> clippy --workspace" grep -q 'cargo clippy --workspace' "$STUB_LOG"
    assert "pre-push: metadata failure -> note" grep -q 'note: cargo metadata failed; running clippy on the whole workspace' "$T/out"
    assert "pre-push: metadata failure exits 0" test "$rc" = 0

    on_branch feat-oagw gears/oagw/src/lib.rs
    assert "pre-push: oagw change -> cluster dylint" grep -q 'cargo gears lint --dylint -P cf-gears-oagw -F cluster' "$STUB_LOG"
    assert "pre-push: oagw change -> workspace dylint too" grep -q 'cargo gears lint --dylint$' "$STUB_LOG"
    STUB_METADATA_FAIL=1; export STUB_METADATA_FAIL
    on_branch feat-oagw gears/oagw/src/lib.rs
    unset STUB_METADATA_FAIL
    assert "pre-push: unknown package set + oagw files -> cluster dylint" grep -q 'cargo gears lint --dylint -P cf-gears-oagw -F cluster' "$STUB_LOG"
fi

# base ref: upstream/main beats origin/main; GEARS_HOOKS_BASE overrides; an unknown override skips
git -C "$r" update-ref refs/remotes/origin/main "$(head_sha)"
git -C "$r" update-ref refs/remotes/upstream/main main
push "$(head_sha)"
assert "pre-push: base prefers upstream/main" grep -q 'hook: base upstream/main (merge-base ' "$T/out"
GEARS_HOOKS_BASE=origin/main; export GEARS_HOOKS_BASE
push "$(head_sha)"
assert "pre-push: GEARS_HOOKS_BASE overrides" grep -q 'hook: base origin/main (merge-base ' "$T/out"
GEARS_HOOKS_BASE=nope; export GEARS_HOOKS_BASE
push "$(head_sha)"
unset GEARS_HOOKS_BASE
assert "pre-push: unknown GEARS_HOOKS_BASE says so" grep -q 'skipping all checks: base nope not found' "$T/out"
assert "pre-push: unknown GEARS_HOOKS_BASE exits 0" test "$rc" = 0
git -C "$r" update-ref -d refs/remotes/origin/main; git -C "$r" update-ref -d refs/remotes/upstream/main

# stdin belongs to the hook: a tool that reads stdin must not swallow the other pushed refs
: > "$STUB_LOG"; rc=0
STUB_DRAIN=1; export STUB_DRAIN
printf 'refs/heads/a %s refs/heads/a %s\nrefs/heads/b %s refs/heads/b %s\n' "$(head_sha)" "$zeros" "$(head_sha)" "$zeros" |
    (cd "$r" && sh .githooks/pre-push) > "$T/out" 2>&1 || rc=$?
unset STUB_DRAIN
assert "pre-push: tools reading stdin do not eat later refs" test "$(grep -c '^hook: base ' "$T/out")" = 2

# Stubs standing in for BSD grep (stock macOS): -P is rejected, anything else runs the real grep.
mkdir "$T/nopgrep"
cat > "$T/nopgrep/grep" <<STUB
#!/bin/sh
for a in "\$@"; do
    case \$a in --) break ;; -P | -[a-zA-Z]*P*) exit 2 ;; esac
done
exec "$(command -v grep)" "\$@"
STUB
printf '#!/bin/sh\nexit 2\n' > "$T/nopgrep/ggrep"
chmod +x "$T/nopgrep/"*

# pre-push: without a working grep -P the forbidden-words step is skipped, not run or failed
if [ -z "$have_bash4" ] || [ -z "$have_jq" ]; then
    skip "pre-push: no grep -P skips forbidden-words" "needs bash >= 4 and jq"
else
    printf '#!/bin/sh\necho ran > "%s/fw-ran"\n' "$T" > "$r/.github/scripts/check-forbidden-words.sh"
    saved_path=$PATH; PATH="$T/nopgrep:$PATH"
    push "$(head_sha)"
    PATH=$saved_path
    assert "pre-push: no grep -P prints skip notice" grep -q 'skipping forbidden-words: grep -P not found' "$T/out"
    assert "pre-push: no grep -P does not run the script" test ! -e "$T/fw-ran"
    assert "pre-push: no grep -P exits 0" test "$rc" = 0
fi

# ---- check-forbidden-words.sh (the real script) ----
fw_ok=1
[ -n "$have_bash4" ] || { fw_ok=""; fw_why="needs bash >= 4"; }
[ -n "$have_jq" ] || { fw_ok=""; fw_why="needs jq"; }
mkrepo fw; r="$T/fw"
mkdir -p "$r/.github/scripts"
cp "$src/.github/scripts/check-forbidden-words.sh" "$r/.github/scripts/"
cp "$src/.github/forbidden-words.json" "$r/.github/"
echo base > "$r/a.txt"; git -C "$r" add -A; git -C "$r" commit -qm base
# split so this file itself passes the forbidden-words check
echo "cyber""fabric" >> "$r/a.txt"; git -C "$r" commit -qam bad
# fw_run <PATH>: runs the script on the last commit; status in $rc.
fw_run() {
    rc=0
    (cd "$r" && PATH=$1 bash .github/scripts/check-forbidden-words.sh HEAD~1 HEAD) > "$T/out" 2>&1 || rc=$?
}
if [ -z "$fw_ok" ]; then
    skip "forbidden-words: violation found with PCRE grep" "$fw_why"
    skip "forbidden-words: grep without -P fails loudly" "$fw_why"
else
    if [ -n "$have_pcre" ]; then
        fw_run "$real_path"
        assert "forbidden-words: violation found with PCRE grep" test "$rc" = 1
    else
        skip "forbidden-words: violation found with PCRE grep" "no grep -P here"
    fi
    fw_run "$T/nopgrep:$real_path"
    assert "forbidden-words: grep without -P exits 2, not 0" test "$rc" = 2
    assert "forbidden-words: grep without -P names the problem" grep -q 'needs grep -P' "$T/out"
fi

[ "$nfail" = 0 ] || { echo "$nfail case(s) failed"; exit 1; }
