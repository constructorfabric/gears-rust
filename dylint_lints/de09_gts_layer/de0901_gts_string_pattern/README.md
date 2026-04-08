# DE0901 – GTS string pattern validator

## What it does

`DE0901_GTS_STRING_PATTERN` validates every string literal that looks like a
Global Type Schema (GTS) identifier. It ensures that:

1. `schema_id = "..."` inside `#[struct_to_gts_schema]` attributes is a valid
   **type schema** (must end with `~`, no wildcards).
2. Arguments passed to `gts_make_instance_id("...")` are valid **instance
   segment identifiers** (single segment, no wildcards, no `:` or `~`).
3. Any other string literal that starts with `gts.` or appears inside a
   colon-separated permission string contains a valid schema/instance chain.
4. `const`/`static` items holding GTS wildcard strings (`*`) **must** have
   names ending with `_WILDCARD` — otherwise the lint reports an error.

Wildcards (`*`) are only allowed in contexts where they are used as patterns:
permission strings, `resource_pattern(...)`, `with_pattern(...)`,
`resolve_to_uuids(...)`, `GtsWildcard::new(...)`, and `str.starts_with(...)`.
Everywhere else the lint rejects wildcard tokens.

Use `#[allow(de0901_gts_string_pattern)]` to suppress the lint:
```rust
#[allow(unknown_lints)]
#[allow(de0901_gts_string_pattern)]
let schema = "gts.acme.core.events.*";
```

## Why is this bad?

* Invalid identifiers break contract generation, registry lookups, or instance
  resolution at runtime.
* Wildcards in schema identifiers create ambiguous or insecure behavior, e.g.,
  allowing access to whole type families.
* Providing schemas to APIs that expect instance segments (or vice versa) leads
  to confusing errors buried deep inside infrastructure crates.

By catching the issues early, the lint prevents accidental schema typos and
protects security-critical permission checks.

## Known exceptions

* Permission strings (anything containing `:`) allow wildcards in the GTS
  segment, but the lint still validates each GTS component.
* `resource_pattern("...")`, `with_pattern("...")`, and
  `resolve_to_uuids(&["..."])` calls also allow wildcards, since they represent
  pattern matching or resolution contexts.
* `GtsWildcard::new("...")` — arguments are allowed to contain wildcards, since
  `GtsWildcard` is explicitly typed to hold pattern values.
* `const`/`static` items whose names end with `_WILDCARD` may hold GTS wildcard
  strings (they are allowed and marked as intentional wildcard constants).
* Strings passed to `str.starts_with("gts.")` are ignored.
* Inline suppressions are supported through `#[allow(de0901_gts_string_pattern)]`
  on a binding or expression when a wildcard must be hard-coded outside the
  recognised helper APIs.

## `_WILDCARD` naming convention for constants

When a wildcard GTS pattern must be stored in a `const` or `static`, the item
name **must** end with `_WILDCARD`:

```rust
// ✅ Allowed — name ends with _WILDCARD
const SRR_WILDCARD: &str = "gts.x.core.srr.resource.v1~*";
GtsWildcard::new(SRR_WILDCARD).unwrap();

// ❌ DE0901: name does not end with _WILDCARD
const SRR_PATTERN: &str = "gts.x.core.srr.resource.v1~*";
//  → rename to `SRR_PATTERN_WILDCARD` or use a non-wildcard value
```

## Example

```rust
// ❌ Triggers DE0901: wildcard inside a plain schema string
let schema = "gts.acme.core.events.*";

// ❌ Triggers DE0901: schema (with `~`) used in gts_make_instance_id
let _id = Product::gts_make_instance_id("vendor.package.sku.some.v1~");

// ❌ Triggers DE0901: const named without _WILDCARD suffix holds a wildcard
const BAD_PATTERN: &str = "gts.x.core.srr.resource.v1~*";
```

Use instead:

```rust
// ✅ Explicit type schema
let schema = "gts.acme.core.events.type.v1~";

// ✅ Instance id segment
let _id = Product::gts_make_instance_id("vendor.package.sku.some.v1");

// ✅ Wildcard allowed inside permission/resource patterns
let pattern = Permission::builder()
    .resource_pattern("gts.acme.core.events.topic.v1~vendor.*")
    .action("publish")
    .build()
    .unwrap();

// ✅ Wildcard constant with _WILDCARD suffix
const ALL_SRR_WILDCARD: &str = "gts.x.core.srr.resource.v1~*";
let wc = GtsWildcard::new(ALL_SRR_WILDCARD).unwrap();

// ✅ Inline wildcard passed directly to GtsWildcard::new()
let wc = GtsWildcard::new("gts.x.core.srr.resource.v1~*").unwrap();
```
