# Starlark: the language

Starlark is a small, dynamically-typed **dialect of Python** (formerly "Skylark")
created at Google for the Bazel build tool and now used by many projects as an
embedded configuration / scripting language. It has Python-3 syntax, first-class
functions with lexical scope, and garbage collection — but it deliberately drops
most of what makes Python a general-purpose language, in service of two
guarantees.

Sources for everything here: the [bazelbuild/starlark spec][spec] and the
[go.starlark.net dialect spec][gospec] (the authoritative one for our runtime).

## The two guarantees (and what they are *not*)

- **Deterministic.** Executing the same file with the same interpreter yields
  the same result. There are no random numbers, no clocks, no unspecified
  iteration order in the core language.
- **Hermetic.** By default user code cannot touch the filesystem, network, or
  system clock. I/O exists only if the *host* injects a builtin for it.

> **Hermetic is not the same as "safe for untrusted code."** Hermeticity bounds
> *what* the language can reach; it says nothing about CPU, memory, or
> allocation. Running untrusted Starlark safely is a separate problem solved
> with execution-step limits, cancellation, and a frozen, allowlisted
> environment — see [go-runtime.md](go-runtime.md#running-untrusted-code).

A consequence of the design: recursion is banned by default and every loop
iterates a finite sequence, so **Starlark programs are not Turing-complete**
(an embedder can opt into recursion; see `Recursion` in
[go-runtime.md](go-runtime.md#dialect-flags-fileoptions)).

## What Starlark removes from Python

No classes or inheritance · no `try`/`except`/`raise` (no in-language error
handling) · no `while` (by default) · no `yield`/generators · no `is` operator ·
no `lambda` restrictions aside, no reflection · no `import` (use `load()`) · no
`global`/`nonlocal` · no set literals by default · no string formatting via
`%` is fine but no f-strings · no `del` on variables (only dict/list elements).
There are also **no user-defined types** in the language — though host Go code
*can* define custom value types (see [go-runtime.md](go-runtime.md)).

## Python-3 → Starlark divergence cheatsheet

These are the things that trip up a Python author. The first one is by far the
most common bug.

| Topic | Python 3 | Starlark | Why it bites |
|---|---|---|---|
| **Strings are iterable** | Yes — `for c in "ab"`, `list("ab")` | **No.** Strings are an immutable byte sequence and are *not* iterable. Use `.elems()` / `.codepoints()` / `.elem_ords()` / `.codepoint_ords()` | A single string silently exploding into characters where a list of strings was meant — Starlark removes the footgun by making it an error |
| **Global reassignment** | Allowed | **Static error** to bind a global name twice (`X = 1; X = 2`) | Globals are write-once; mutate *contents* of a mutable global instead |
| **Augmented assign on globals** | Allowed | Spec-strict: error. (go.starlark.net relaxes `+=` on a *mutable* global because it's a mutation, not a rebind — gate with `GlobalReassign`) | Dialect-dependent; don't rely on it |
| **Conditional expr** | `a if c else b` | Same | — |
| **Sets** | `{1,2}` literal | No literal; `set()` builtin only, and only if the host enables it (`Set` option) | `{...}` is always a dict |
| **Recursion** | Allowed | **Dynamic error** by default | Rewrite as iteration over a finite sequence |
| **`while`** | Allowed | Disallowed by default (host may enable) | Use `for` over a bounded range |
| **Exceptions** | `try/except` | None. Errors abort; use `fail(msg)` to abort deliberately | No recovery — validate inputs up front |
| **Top-level control flow** | Allowed | `if`/`for`/`while` at module top level disallowed by default (host may enable) | Put logic inside `def`s |
| **Integers** | `int` arbitrary precision | Same (arbitrary precision) | — |
| **Floats** | Always available | Available; `int`/`float` distinct, `/` is float div, `//` floor | — |
| **`load()`** | n/a (`import`) | `load("//path:file.star", "name", alias = "orig")` — binds names from another module; resolution is host-supplied | Not a filesystem import; the host decides what a module path means |
| **Dict order** | Insertion-ordered | Insertion-ordered (guaranteed) | — |
| **`del x`** | Deletes variable | Only `del d[k]` / list element; can't delete a variable | — |

## Freezing

Any mutable value (list, dict, set, and module globals after a module loads) can
be **frozen**, after which any attempt to mutate it fails with a dynamic error.
Embedders freeze a module's globals once it has loaded so that loaded modules are
safely shareable across threads. In practice: treat anything you `load()` — and
your own module's globals once defined — as read-only.

## Built-in types

`NoneType`, `bool`, `int` (arbitrary precision), `float`, `string` (immutable
bytes), `bytes`, `list` (mutable), `tuple` (immutable), `dict` (insertion
ordered), `set` (optional). Functions and builtins are first-class values.

## Standard builtins (the "universe")

Always available unless the host removes them: `len`, `range`, `enumerate`,
`zip`, `min`, `max`, `sum`, `sorted`, `reversed`, `all`, `any`, `str`, `int`,
`float`, `bool`, `list`, `dict`, `tuple`, `type`, `dir`, `getattr`, `setattr`,
`hasattr`, `hash`, `repr`, `print`, `fail`. A host can add more (e.g. `json`,
`math`, `time`) or remove any of these.

## Optional standard library modules

The Go runtime ships deterministic-friendly modules an embedder may opt into:
`json` (`lib/json`), `math` (`lib/math`), `time` (`lib/time` — *can* break
determinism unless a fixed clock is set), `proto` (`lib/proto`). See
[go-runtime.md](go-runtime.md#optional-standard-library).

[spec]: https://github.com/bazelbuild/starlark/blob/master/spec.md
[gospec]: https://github.com/google/starlark-go/blob/master/doc/spec.md
