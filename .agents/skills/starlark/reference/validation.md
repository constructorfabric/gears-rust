# Validating Starlark

Three tools, in increasing fidelity. Use all three; they catch different
classes of problem. There is **no type-checker** for Starlark (it is dynamically
typed) — the closest thing to static analysis is the parse+resolve pass, which
checks names and scope but not types.

## 1. `starcheck` — parse + resolve, no execution (the embedder check)

Lives at [`tools/starcheck`](../tools/starcheck) (its own Go module so it adds no
dependency to the main repo until the host package lands). It parses and
resolves each file **without running it**, so it is safe on untrusted input and
free of side effects. It catches: syntax errors, undefined names, illegal global
rebinding, forbidden dialect features (`while`, top-level control flow,
recursion), and — via `-predeclared` — references to a builtin a capability
level does not grant.

```bash
# from .agents/skills/starlark/tools/starcheck
go run . path/to/module.star            # spec-strict, universe builtins only
go run . -r path/to/scripts/            # recurse a directory

# simulate a capability level: only these names are predeclared
go run . -predeclared=world,http,secret enrich.star

# relax the dialect
go run . -while -recursion -global-reassign legacy.star

go build -o starcheck . && ./starcheck ...   # or build a binary
```

Flags: `-predeclared=a,b,c` (allowed non-universe names), `-universe=false`
(drop the standard builtins too), `-while` / `-toplevel-control` / `-set` /
`-global-reassign` / `-recursion` (dialect relaxations), `-require-def=NAME`
(fail unless the file defines a top-level `def NAME`), `-r` (recurse), `-q`
(errors only). Exit 0 = all clean, 1 = at least one error.

This is the tool that mirrors what an embedder's loader does at load time. When
you want to prove *"a function can't reference `http`"*, run it with a
`-predeclared` set that omits `http` and confirm it fails.

### The `-kitsoki` profile

`-kitsoki` pins the exact `host.starlark.run` sandbox surface so a single
command answers *"would this load and dispatch in kitsoki?"* without booting an
app. It is equivalent to `-predeclared=json,math -require-def=main` with every
dialect relaxation off (strict), mirroring `internal/host/starlark/run.go`:

```bash
go run . -kitsoki stories/<story>/scripts/derive.star
go run . -kitsoki -r stories/<story>/scripts/        # whole scripts dir
```

It catches the two kitsoki authoring mistakes a plain resolve misses: an entry
point named anything but `main`, and a reference to a name outside `{json,
math}` (e.g. `time`, `random`) — both reported in one pass. It does **not**
check the `.star.yaml` sidecar or run the script; for those, use `kitsoki test
flows` (below). Full authoring loop: [kitsoki.md](kitsoki.md).

## 2. `buildifier` — format + lint

The Bazel formatter/linter. Works on generic Starlark via `-type=default`.

```bash
go install github.com/bazelbuild/buildtools/buildifier@latest

buildifier -type=default file.star                       # format in place
buildifier -type=default -mode=check file.star           # CI: nonzero if unformatted
buildifier -type=default -mode=diff  file.star           # show the diff it would make
buildifier -type=default -lint=warn  file.star           # report lint findings
buildifier -type=default -lint=fix   file.star           # auto-fix what it can
buildifier -r -type=default -lint=warn scripts/          # recurse a tree
buildifier -type=default -format=json -mode=check f.star # machine-readable diagnostics
```

`-mode`: `check` (CI gate) · `diff` · `fix` (default, rewrites in place) ·
`print_if_changed`. `-lint`: `off` · `warn` · `fix`. `-warnings=+a,-b` tunes
which lint categories fire.

Note buildifier is BUILD-file-oriented; some warnings assume Bazel semantics, so
review findings rather than blindly `-lint=fix`-ing on a non-Bazel DSL.

## 3. `starlark` CLI — REPL + run

For interactive exploration and a quick "does it even run" smoke check. Note
this **executes** the file (unlike `starcheck`), so only run trusted code.

```bash
go install go.starlark.net/cmd/starlark@latest

starlark                 # REPL (Ctrl-D to exit)
starlark module.star     # run a file
```

## 4. `kitsoki test flows` — the native, behavioural check (no LLM, no cost)

For a kitsoki glue script the authoritative end-to-end check is a flow fixture:
it loads the app (validating the script path + `.star.yaml` sidecar), then runs
the **real** `main(ctx)` with its HTTP served from a cassette. No network, no
LLM, no cost, fully deterministic.

```bash
kitsoki test flows stories/<story>/app.yaml      # one app
make test-flows                                   # every story's fixtures
```

The fixture names its cassette via `starlark_http_cassette:`. See
[kitsoki.md §The validation loop](kitsoki.md#the-validation-loop-fast--thorough-all-no-llm)
for the fixture shape and the cassette record/replay workflow, and
[hosts.md](../../../architecture/hosts.md#record--replay-http-cassettes) for the
authoritative cassette format.

## Recommended toolchain

| Goal | Tool | Command |
|---|---|---|
| Format (CI gate) | buildifier | `buildifier -type=default -mode=check -lint=warn` |
| Auto-format | buildifier | `buildifier -type=default` |
| Static validity + surface enforcement | starcheck | `starcheck -predeclared=<names> -r scripts/` |
| **kitsoki glue pre-flight** | starcheck | `starcheck -kitsoki -r stories/<story>/scripts/` |
| **kitsoki glue end-to-end** | kitsoki CLI | `kitsoki test flows stories/<story>/app.yaml` |
| Interactive / smoke run | starlark CLI | `starlark module.star` |

The first two are wired together in [`tools/validate.sh`](../tools/validate.sh)
— it runs buildifier (if installed) then `starcheck` over a path, and exits
nonzero if either complains:

```bash
./validate.sh scripts/                          # whole tree, spec-strict
./validate.sh enrich.star -predeclared=http      # extra flags pass through to starcheck
```

## Other tools (situational)

- **`starpls`** ([withered-magic/starpls](https://github.com/withered-magic/starpls)) —
  a Starlark **LSP** server (hovers, go-to-def, diagnostics) for editor
  integration. Not a CI gate and not a real type-checker, but useful while
  authoring.
- **`facebook/starlark-rust`** — an independent Rust implementation with its own
  linter/typechecker experiments; relevant only if you target that runtime
  (kitsoki uses the Go runtime).
