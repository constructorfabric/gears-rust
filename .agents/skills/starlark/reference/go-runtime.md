# Embedding Starlark in Go (`go.starlark.net`)

The canonical runtime is **`go.starlark.net`** (live GitHub mirror:
[google/starlark-go][repo]). It is a small, stable API. This page is the
embedder's reference: how to run a module, expose Go to Starlark, define custom
types, and run code safely. Source of truth is the [godoc][godoc] and the
package source — re-check signatures there before depending on them, the API
does evolve (e.g. `ExecFile` → `ExecFileOptions`).

```
go get go.starlark.net/starlark
```

## Packages

| Import path | Role |
|---|---|
| `go.starlark.net/starlark` | The interpreter: values, `Thread`, exec/eval/call, safety controls |
| `go.starlark.net/syntax` | Scanner/parser; `FileOptions` (dialect flags); `Parse` |
| `go.starlark.net/resolve` | Name-resolution pass; static checking without execution |
| `go.starlark.net/starlarkstruct` | Optional `struct` and `module` value types |
| `go.starlark.net/starlarktest` | Test helpers (`assert.star`-style assertions) |
| `go.starlark.net/repl` | The read-eval-print loop used by the CLI |
| `go.starlark.net/lib/json` `lib/math` `lib/time` `lib/proto` | Optional stdlib modules |
| `go.starlark.net/cmd/starlark` | The `starlark` CLI (REPL + run a file) |

## Running a module

The current entry points take a `*syntax.FileOptions` (dialect config). **The
older `starlark.ExecFile` / `starlark.Eval` are deprecated** — they read legacy
package-global flags. Prefer the `*Options` variants:

```go
import (
    "go.starlark.net/starlark"
    "go.starlark.net/syntax"
)

opts := &syntax.FileOptions{}            // zero value = spec-strict dialect
thread := &starlark.Thread{
    Name:  "load",
    Print: func(_ *starlark.Thread, msg string) { log.Println(msg) },
}

// predeclared: the names visible to the module beyond the universe builtins.
predeclared := starlark.StringDict{
    "greeting": starlark.String("hello"),
}

globals, err := starlark.ExecFileOptions(opts, thread, "config.star", srcBytes, predeclared)
// globals is a StringDict of the module's top-level bindings (now frozen).
```

`src` is `any`: a `string`, `[]byte`, `io.Reader`, or `nil` (read the file named
by `filename`). To evaluate a single expression instead of a file, use
`starlark.EvalOptions(opts, thread, filename, src, env)`.

### Calling a Starlark function from Go

```go
fn := globals["greet"]                                   // a *starlark.Function
v, err := starlark.Call(thread, fn,
    starlark.Tuple{starlark.String("ada")},              // positional args
    nil,                                                 // kwargs: []starlark.Tuple
)
```

## The `Value` interface

Every Starlark value implements:

```go
type Value interface {
    String() string        // Starlark source-like repr
    Type() string          // e.g. "int", "list"
    Freeze()               // make immutable; idempotent
    Truth() Bool           // truthiness
    Hash() (uint32, error) // err if unhashable (e.g. a list)
}
```

Built-in concrete types: `NoneType`, `Bool`, `Int`, `Float`, `String`, `Bytes`,
`*List`, `Tuple`, `*Dict`, `*Set`, `*Function`, `*Builtin`. Constructors:
`MakeInt`, `String(...)`, `NewList`, `NewDict`, `NewSet`, `Tuple{...}`.

## Exposing Go functions

```go
strlen := starlark.NewBuiltin("strlen", func(
    thread *starlark.Thread, b *starlark.Builtin,
    args starlark.Tuple, kwargs []starlark.Tuple,
) (starlark.Value, error) {
    var s string
    if err := starlark.UnpackArgs(b.Name(), args, kwargs, "s", &s); err != nil {
        return nil, err
    }
    return starlark.MakeInt(len(s)), nil
})
// expose it by putting it in the predeclared StringDict: {"strlen": strlen}
```

`UnpackArgs` is the idiomatic way to bind positional+keyword args into Go vars
(it understands `*starlark.Value`, `*int`, `*string`, `*bool`, etc., and `??`
optional markers). `UnpackPositionalArgs` is the positional-only variant.

## Custom Go value types

Implement `Value` (above) plus whichever **optional interfaces** give your type
behavior. Signatures (verbatim from godoc):

```go
Comparable   { CompareSameType(op syntax.Token, y Value, depth int) (bool, error) }
HasAttrs     { Attr(name string) (Value, error); AttrNames() []string }     // x.field
HasSetField  { HasAttrs; SetField(name string, val Value) error }           // x.field = v
Indexable    { Index(i int) Value; Len() int }                              // x[i]
HasSetIndex  { Indexable; SetIndex(index int, v Value) error }              // x[i] = v
Sequence     { Indexable; Iterate() Iterator }
Iterable     { Iterate() Iterator }                                         // for y in x
Mapping      { Get(k Value) (v Value, found bool, err error) }              // x[k] dict-like
HasSetKey    { Mapping; SetKey(k, v Value) error }                          // x[k] = v
Container    { Has(y Value) (bool, error) }                                 // y in x
Callable     { Name() string; CallInternal(thread *Thread, args Tuple, kwargs []Tuple) (Value, error) }
HasBinary    { Binary(op syntax.Token, y Value, side Side) (Value, error) } // x + y
HasUnary     { Unary(op syntax.Token) (Value, error) }                      // -x
```

`starlarkstruct.Struct` (an immutable attribute bag) is usually enough to hand
structured data to a script without writing a custom type — wire it in as
`starlark.NewBuiltin("struct", starlarkstruct.Make)`.

## The `Thread`

Carries per-execution state and two client hooks:

- `Print func(*Thread, string)` — where `print()` goes (defaults to stderr).
- `Load func(*Thread, module string) (StringDict, error)` — resolves `load()`
  statements. **Repeated calls with the same module name must return the same
  result**, so the host is responsible for caching/memoizing modules (and for
  detecting load cycles). The repo's `example_test.go` ships a caching loader to
  copy.
- `SetLocal(key, value)` / `Local(key)` — thread-local storage, the clean way to
  pass per-call context (a request, a DB handle, a capability set) into builtins
  without threading it through every Starlark signature.

## Dialect flags (`syntax.FileOptions`)

The zero value is the **spec-strict** dialect. Each flag relaxes it:

```go
type FileOptions struct {
    Set               bool // allow the 'set' builtin
    While             bool // allow 'while' statements
    TopLevelControl   bool // allow if/for/while at top level
    GlobalReassign    bool // allow reassigning top-level (global) names
    LoadBindsGlobally bool // load() binds globals, not file-locals (deprecated)
    Recursion         bool // disable the recursion check (allow recursive funcs)
}
```

Parse with options via the method form: `opts.Parse(filename, src, mode)`.
(The legacy `resolve` package globals — `AllowGlobalReassign`, `AllowRecursion`,
`LoadBindsGlobally`, and the now-always-true `AllowSet`/`AllowFloat`/
`AllowLambda`/`AllowNestedDef`/`AllowBitwise` — are deprecated in favor of
passing `FileOptions` explicitly.)

## Static checking without execution

Parse + resolve catches syntax errors, undefined names, scope/global violations,
and forbidden dialect features — **without running the code**:

```go
f, err := opts.Parse(filename, src, 0)            // syntax errors
if err != nil { /* ... */ }
err = resolve.File(f, isPredeclared, isUniversal) // resolve.ErrorList of name/scope errors
```

`isPredeclared` / `isUniversal` are `func(name string) bool` predicates. By
**narrowing `isPredeclared` to a capability's allowed names**, resolution fails
if the module references a builtin that surface doesn't grant. kitsoki's
`host.starlark.run` uses exactly this: `predeclared = {json, math}` and strict
`FileOptions` (see `internal/host/starlark/run.go`). The `starcheck` tool in
[`tools/starcheck`](../tools/starcheck) wraps this check in a CLI; its
`-kitsoki` profile reproduces that surface. See [validation.md](validation.md)
and [kitsoki.md](kitsoki.md).

## Running untrusted code

Hermeticity is not enough (see [language.md](language.md#the-two-guarantees-and-what-they-are-not)).
The actual controls:

- **Step budget.** `thread.SetMaxExecutionSteps(n)` caps computation steps;
  `thread.ExecutionSteps()` reads the counter; `thread.OnMaxSteps` runs when the
  cap is hit (default: `thread.Cancel("too many steps")`).
- **Cancellation.** `thread.Cancel(reason)` — callable from any goroutine —
  makes running code promptly fail with an `EvalError`. Wire it to a
  `context.Context` deadline. `thread.Uncancel()` resets. (Backed by an atomic
  pointer, so cross-goroutine cancel is safe.)
- **Recursion off.** Leave `FileOptions.Recursion` false (the default).
- **Freeze + allowlist.** Freeze the predeclared environment and expose only the
  builtins a script needs. There is **no built-in memory/allocation limit** —
  the step budget is the backstop.

[repo]: https://github.com/google/starlark-go
[godoc]: https://pkg.go.dev/go.starlark.net/starlark
