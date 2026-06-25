---
name: starlark
description: Write, review, and validate Starlark — the small deterministic Python dialect (go.starlark.net runtime) embedded in kitsoki as the `host.starlark.run` glue capability. Use when authoring or debugging a `.star` glue script for a kitsoki story (the `main(ctx) -> dict` contract, the `.star.yaml` sidecar, `ctx.inputs`/`ctx.world`/`ctx.http`, HTTP cassettes), embedding the Starlark interpreter in Go (Thread, FileOptions, custom Value types, exposing Go builtins), choosing a validation toolchain (buildifier / starcheck / starlark CLI / `kitsoki test flows`), or diagnosing a "this is valid Python but errors in Starlark" surprise (strings aren't iterable, no while/recursion/classes/exceptions, globals can't be reassigned).
---

# Starlark

Starlark is a small, deterministic, hermetic **dialect of Python** built for
embedded configuration/scripting (it powers Bazel). kitsoki embeds it as the
**`host.starlark.run`** capability — the deterministic glue escape hatch for a
story — running on the canonical Go runtime, **`go.starlark.net`**.

Two layers, two readerships:

- **General Starlark** (the language + Go embedding + validation) — the
  reference files below.
- **The kitsoki `host.starlark.run` surface** (the `main(ctx)` contract, the
  sidecar, `ctx.inputs`/`world`/`http`, cassettes) — start at
  [`reference/kitsoki.md`](reference/kitsoki.md), whose authoritative contract
  reference is [`docs/architecture/hosts.md#hoststarlarkrun`](../../architecture/hosts.md#hoststarlarkrun).

## Reference (read on demand)

| File | When you need it |
|---|---|
| [`reference/kitsoki.md`](reference/kitsoki.md) | **Authoring a kitsoki glue script**: the `main(ctx) -> dict` contract, the `.star.yaml` sidecar, the `ctx` surface, `fail()` → `on_error:`, the no-LLM validation loop |
| [`reference/language.md`](reference/language.md) | Language semantics + the **Python-3 → Starlark divergence cheatsheet** (the gotchas) |
| [`reference/go-runtime.md`](reference/go-runtime.md) | Embedding API: `Thread`, `ExecFileOptions`, `Value`/custom types, exposing Go builtins, dialect flags, running untrusted code safely |
| [`reference/validation.md`](reference/validation.md) | The validation toolchain — buildifier, `starcheck` (incl. the `-kitsoki` profile), the `starlark` CLI, `kitsoki test flows` |

## The five things that bite a Python author

Reach for the cheatsheet, but these cause most "valid Python, broken Starlark":

1. **Strings are NOT iterable.** `for c in "ab"` / `list("ab")` are errors. Use
   `"ab".elems()` / `.codepoints()`. This is the #1 surprise.
2. **Globals can't be reassigned.** `X = 1; X = 2` is a *static* error. Mutate
   the contents of a mutable global, don't rebind the name.
3. **No `while`, no recursion** (by default), and loops must be over finite
   sequences — Starlark is deliberately not Turing-complete.
4. **No exceptions.** No `try`/`except`/`raise`. Errors abort; `fail(msg)` aborts
   on purpose. Validate inputs up front.
5. **No classes, no `import`.** Use plain `def`/dicts; cross-module sharing is
   `load("//pkg:file.star", "name")`, resolved by the host, not the filesystem.

> **Hermetic ≠ safe for untrusted code.** Determinism/hermeticity bound *what*
> the language reaches, not CPU/memory. Untrusted execution needs step limits +
> cancellation + a frozen allowlisted environment — see
> [`go-runtime.md`](reference/go-runtime.md#running-untrusted-code).

## The validation loop

Validate **without executing** — safe, side-effect-free, and it catches the
errors that matter for an embedded config language.

```bash
# 1. format + lint (if buildifier is installed)
buildifier -type=default -mode=check -lint=warn module.star

# 2. parse + resolve (no execution). From .agents/skills/starlark/tools/starcheck:
go run . module.star
go run . -r scripts/                          # a whole tree
go run . -predeclared=world,http,secret f.star # only these builtins are granted

# 2b. kitsoki glue script — pins the EXACT host.starlark.run sandbox surface
#     (predeclared={json,math}, strict dialect, requires def main(ctx)):
go run . -kitsoki scripts/derive.star

# format + starcheck over a path in one shot:
.agents/skills/starlark/tools/validate.sh scripts/
.agents/skills/starlark/tools/validate.sh scripts/derive.star -kitsoki  # flags pass through
```

`starcheck` is the tool to own: it wraps `syntax.Parse` + `resolve.File`, so by
restricting `-predeclared` to a capability's allowed names you can prove at
compile time that a script references nothing outside that surface. The
`-kitsoki` profile bundles the real `host.starlark.run` environment — see
[`reference/kitsoki.md`](reference/kitsoki.md) for how this fits the full no-LLM
validation loop (`kitsoki test flows`), and
[`reference/validation.md`](reference/validation.md) for all flags.

## Embedding it in Go (the short version)

```go
opts := &syntax.FileOptions{}                  // zero value = spec-strict dialect
thread := &starlark.Thread{Name: "load", Print: myPrint}
thread.SetMaxExecutionSteps(1_000_000)         // budget for untrusted code
globals, err := starlark.ExecFileOptions(opts, thread, "config.star", src, predeclared)
v, err := starlark.Call(thread, globals["fn"], starlark.Tuple{starlark.String("x")}, nil)
```

- Use the `*Options` entry points — `ExecFileOptions`/`EvalOptions`. Plain
  `ExecFile`/`Eval` are **deprecated** (they read legacy global flags).
- Expose Go functions with `starlark.NewBuiltin` + `starlark.UnpackArgs`.
- Hand structured data in via `starlarkstruct.Struct`, or a custom `Value` type
  implementing the optional interfaces (`HasAttrs`, `Mapping`, `Iterable`, …).
- Pass per-call context through `thread.SetLocal`/`Local`, not function args.

Full API surface, custom-type interface signatures, the `load()` caching
contract, and the dialect-flag table: [`reference/go-runtime.md`](reference/go-runtime.md).

## Using it in kitsoki (`host.starlark.run`)

A kitsoki glue script is a single file beside a story with a typed sidecar:

```python
# scripts/derive.star
def main(ctx):                                  # the ONE entry point the engine calls
    wid = ctx.inputs["widget_id"]               # ctx.inputs is a DICT (typed by the sidecar)
    resp = ctx.http.get("https://api.example.com/widgets/" + wid)
    if not resp:                                # truthy iff 2xx — branch, don't assume
        fail("lookup failed: %d" % resp.status) # fail() → Result.Error → effect's on_error:
    return {"name": resp.json()["name"]}        # outputs flow ONLY through this dict
```

```yaml
# scripts/derive.star.yaml — the AUTHORITATIVE interface (the engine ignores in-script docs)
inputs:  { widget_id: { type: string, required: true } }
outputs: { name: { type: string } }
```

The five things that bite a kitsoki glue author specifically — beyond the
language gotchas above:

1. **The sidecar is law.** Every declared output must be returned and every
   returned key must be declared, or the run is an `on_error:` domain failure.
   The `INPUTS`/`OUTPUTS` dicts some scripts write are documentation only.
2. **`ctx` is the whole world.** Exactly `ctx.inputs` (dict), `ctx.world.get(k)`
   (read-only), `ctx.http.get/post`. No `set`, no fs, no env, no clock, no
   random — `ctx.world` can't be written; outputs go through the return dict.
3. **Only `json` + `math`** are predeclared. No `time`, no `random` (they'd
   break determinism). `starcheck -kitsoki` enforces exactly this set.
4. **`fail()` is your error channel.** There are no exceptions; `fail(msg)` sets
   `world.last_error` and fires the effect's `on_error:` arc. Validate up front.
5. **Test with a cassette, never a live call.** A flow fixture replays the
   script's HTTP from a cassette — `kitsoki test flows <app.yaml>` runs the real
   script, no LLM, no network, no cost.

Authoring contract, sidecar types, the `ctx` surface, error mapping, and the
HTTP-cassette format are documented authoritatively in
[`docs/architecture/hosts.md#hoststarlarkrun`](../../architecture/hosts.md#hoststarlarkrun);
the skill-side authoring + validation loop is [`reference/kitsoki.md`](reference/kitsoki.md).
Runnable examples: [`stories/starlark-enrich/`](../../../stories/starlark-enrich/)
(minimal) and [`stories/weather-report/`](../../../stories/weather-report/)
(two chained HTTP calls, branch on mode, table outputs).

## Authoritative sources

- Language spec: <https://github.com/bazelbuild/starlark/blob/master/spec.md>
- **Go-runtime dialect spec** (the one that governs our runtime):
  <https://github.com/google/starlark-go/blob/master/doc/spec.md>
- godoc: <https://pkg.go.dev/go.starlark.net/starlark>
- Ecosystem index: <https://github.com/laurentlb/awesome-starlark>
