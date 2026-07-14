# Gears Dylint Linters

Custom [dylint](https://github.com/trailofbits/dylint) linters enforcing Gears' architectural patterns, layer separation, and REST API conventions.

## Quick Start

```bash
# From workspace root
make dylint              # Run Dylint lints on Rust code (auto-rebuilds if changed)
make dylint-list         # Show all available Dylint lints
make dylint-test         # Test UI cases (compile & verify violations)
make gts-docs            # Validate GTS identifiers in docs (.md, .json, .yaml, .yml)
make gts-docs-test       # Run unit tests for GTS validator
```

## What This Checks

### Contract Layer (DE01xx)
- ✅ DE0101: No Serde in Contract
- ✅ DE0102: No ToSchema in Contract
- ✅ DE0103: No HTTP Types in Contract

### API Layer (DE02xx)
- ✅ DE0201: DTOs Only in API Rest Folder
- ✅ DE0202: DTOs Not Referenced Outside API
- ✅ DE0203: DTOs Must Have Serde Derives
- ✅ DE0204: DTOs Must Have ToSchema Derive
- ✅ DE0205: Operation builder must have tag and summary

### Domain Layer (DE03xx)
- ✅ DE0301: No Infra in Domain
- ✅ DE0308: No HTTP Types in Domain
- ✅ DE0309: Must Have Domain Model

### Infrastructure/storage Layer (DE04xx)
- TODO

### Client/gateway Layer (DE05xx)
- ✅ DE0503: Plugin Client Suffix

### Gear structure (DE06xx)
- TODO

### Security (DE07xx)
- ✅ DE0706: No Direct SQLx
- ✅ DE0707: Drop Zeroize (sensitive types)
- ✅ DE0708: No Non-FIPS Hasher Imports (sha2/sha1/md5 outside allow-list)

### REST Conventions (DE08xx)
- ✅ DE0801: API Endpoint Must Have Version
- ✅ DE0802: Use OData Extension Methods

### GTS (DE09xx)
- ✅ DE0901: GTS String Pattern Validator (Rust source code)
- ✅ DE0902: No `schema_for!` on GTS Structs (Rust source code)
- ✅ DE0903: GTS Documentation Validator (`.md`, `.json`, `.yaml`, `.yml` files)
- ✅ DE0904: No Hard-Coded GTS Prefix (Rust source code)

### Error handling (DE10xx)
- TODO

### Testing (DE11xx)
- TODO

### Documentation (DE12xx)
- ✅ DE1201: Publishable crates must set `package.metadata.docs.rs.all-features = true`

### Common patterns (DE13xx)
- ✅ DE1301: No Print/Debug Macros in libraries/gears
- ✅ DE1302: No `.to_string()` in Error From impls (preserve error chain)
- ✅ DE1303: No `pub type X = primitive`; use newtype for type safety

## Examples

Each lint includes bad/good examples in source comments. View them:

```bash
# Show lint implementation with examples
cat contract_lints/src/de01_contract_layer/de0101_no_serde_in_contract.rs
```

Example output:

```rust
//! ## Example: Bad
//!
//! // src/contract/user.rs - WRONG
//! #[derive(Serialize, Deserialize)]  // ❌ Serde in contract
//! pub struct User { ... }
//!
//! ## Example: Good
//!
//! // src/contract/user.rs - CORRECT
//! #[derive(Debug, Clone)]  // ✅ No serde
//! pub struct User { ... }
//!
//! // src/api/rest/dto.rs - CORRECT
//! #[derive(Serialize, Deserialize)]  // ✅ Serde in DTO
//! pub struct UserDto { ... }
```

## Development

### Project Structure

```text
dylint_lints/
├── contract_lints/           # Main lint crate
│   ├── src/
│   │   ├── de01_contract_layer/
│   │   ├── de02_api_layer/
│   │   ├── de08_rest_api_conventions/
│   │   ├── lib.rs            # Lint registration
│   │   └── utils.rs          # Helper functions
│   └── ui/                   # Test cases
│       ├── de0101_contract_serde.rs
│       ├── de0203_dto_serde_derives.rs
│       ├── de0801_api_versioning.rs
│       ├── good_contract.rs  # Correct patterns
│       └── ... (see ui/README.md)
├── Cargo.toml
├── rust-toolchain.toml       # Nightly required
└── README.md
```

### Adding a New Lint

1. Create file in appropriate category (e.g., `src/de02_api_layer/de0205_my_lint.rs`)

2. Implement the lint:

```rust
//! DE0205: My Lint Description
//!
//! ## Example: Bad
//! // ... bad code example
//!
//! ## Example: Good
//! // ... good code example

use rustc_hir::{Item, ItemKind};
use rustc_lint::{LateContext, LintContext};

rustc_session::declare_lint! {
    pub MY_LINT,
    Deny,
    "description of what this checks"
}

pub fn check<'tcx>(cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
    // Implementation
}
```

3. Register in `lib.rs`:

```rust
mod de02_api_layer {
    pub mod de0205_my_lint;
}

impl<'tcx> LateLintPass<'tcx> for ContractLints {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
        de02_api_layer::de0205_my_lint::check(cx, item);
    }
}
```

4. Add test case in `ui/` directory (optional but recommended):

```rust
// ui/de0205_my_lint.rs
mod api {
    // Should trigger - violation example
    pub struct BadPattern { }

    // Should NOT trigger - correct pattern
    pub struct GoodPattern { }
}
fn main() {}
```

5. Test:

```bash
make dylint       # Run on workspace code
make dylint-test  # List test cases - compare with your violations
```

### Useful Patterns

**Check if in specific gear:**

```rust
use crate::utils::is_in_api_rest_folder;

if !is_in_api_rest_folder(cx, item.owner_id.def_id) {
    return;
}
```

**Check derives:**

```rust
let attrs = cx.tcx.hir_attrs(item.hir_id());
for attr in attrs {
    if attr.has_name(Symbol::intern("derive")) {
        // Check derive attributes
    }
}
```

**Lint with help:**

```rust
cx.span_lint(MY_LINT, item.span, |diag| {
    diag.primary_message("Error message");
    diag.help("Suggestion on how to fix");
});
```

## GTS Validators (DE09xx)

GTS (Global Type System) identifiers are validated by complementary tools that cover different file types:

| Lint | Scope | Tool | Command |
|------|-------|------|---------|
| **DE0901** | GTS string patterns in Rust | Dylint (Rust) | `make dylint` |
| **DE0902** | No `schema_for!` on GTS structs | Dylint (Rust) | `make dylint` |
| **DE0903** | GTS in docs (`.md`, `.json`, `.yaml`, `.yml`) | Rust CLI | `make gts-docs` |
| **DE0904** | Hard-coded `gts.` prefix in Rust source | Dylint (pre-expansion) | `make dylint` |

### DE0901: GTS String Pattern Validator

A Dylint lint that validates GTS identifiers in Rust source files during compilation.

**What it checks:**
- `schema_id = "..."` in `#[struct_to_gts_schema(...)]` attributes
- Arguments to `gts_make_instance_id("...")`
- Any string literal starting with `gts.`
- GTS parts in permission strings (e.g., `"read:gts.cf.core.type.v1~"`)

**How to run:**
```bash
make dylint  # Runs DE0901 along with other Dylint lints
```

**Location:** [`de09_gts_layer/de0901_gts_string_pattern/`](de09_gts_layer/de0901_gts_string_pattern/)

### DE0902: No `schema_for!` on GTS Structs

A Dylint lint that prevents using `schemars::schema_for!()` on GTS-wrapped structs.

**Why:** GTS structs must use `gts_schema_with_refs_as_string()` for correct `$id` and `$ref` handling.

**Location:** [`de09_gts_layer/de0902_no_schema_for_on_gts_structs/`](de09_gts_layer/de0902_no_schema_for_on_gts_structs/)

### DE0903: Documentation Validator

A standalone CLI tool that validates GTS identifiers in documentation and configuration files. Distributed via crates.io as `gts-validator` (install with `cargo install gts-validator`).

**What it checks:**

- All `.md`, `.json`, `.yaml`, `.yml` files in `docs/`, `gears/`, `libs/`, `examples/`
- Skips intentionally invalid examples (marked with "bad", "invalid", "❌", etc.)
- Allows wildcards in pattern/filter contexts
- Optionally validates vendor consistency with `--vendor` flag

**How to run:**
```bash
# Quick check (from workspace root)
make gts-docs

# Direct CLI with options
gts-validator --vendor cf,vendor,example,fabrikam --exclude "target/*" docs gears libs examples
```

**Exit codes:**
- `0` - All GTS identifiers are valid
- `1` - Invalid GTS identifiers found (fails CI)

### DE0904: No Hard-Coded GTS Prefix

A pre-expansion Dylint lint that rejects user-authored GTS IDs beginning
with `gts.`. Use `gts_id!("<suffix>")` instead, so `GTS_ID_PREFIX` is
resolved in the consuming crate.

The pre-expansion phase is intentional: it checks a `gts_type_schema`,
`gts_instance`, or `resource_error` invocation before ToolKit wrapper macros
emit inventory registrations and error-builder methods. Generated code is not
reported as a false positive.

**Location:** [`de09_gts_layer/de0904_no_hardcoded_gts_prefix/`](de09_gts_layer/de0904_no_hardcoded_gts_prefix/)

### GTS Identifier Format

A GTS identifier follows this structure:
```text
gts.<segment>~[<segment>~]*

Where each segment = vendor.org.package.type.version
```

**Examples:**
```text
gts.cf.toolkit.plugins.plugin.v1~                             # Schema (type definition)
gts.cf.toolkit.plugins.plugin.v1~vendor.pkg.gear.plugin.v1  # Instance (chained)
gts.hx.core.errors.err.v1~hx.odata.errors.invalid.v1         # Error code
```

**Validation Rules:**

| Rule | Valid ✓ | Invalid ✗ |
|------|---------|-----------|
| Must start with `gts.` | `gts.cf.core.type.v1~` | `cf.core.type.v1~` |
| Schema IDs end with `~` | `gts.cf.core.type.v1~` | `gts.cf.core.type.v1` |
| 5 components per segment | `cf.core.pkg.type.v1` | `cf.core.type.v1` (4) |
| No hyphens | `my_type` | `my-type` |
| Version format | `v1`, `v1.0`, `v2.1` | `1.0`, `version1` |
| No wildcards (except patterns) | `gts.cf.core.type.v1~` | `gts.cf.*.type.v1~` |

**When wildcards ARE allowed:**
- In `$filter` queries: `$filter=type_id eq 'gts.cf.*'`
- In pattern methods: `.with_pattern("gts.cf.core.*")`
- In permission patterns: `.resource_pattern("gts.cf.core.type.v1~*")`

## Troubleshooting

**"dylint library not found"**
```bash
cd dylint_lints && cargo build --release
```

**"feature may not be used on stable"**
Dylint requires nightly. The `rust-toolchain.toml` in `dylint_lints/` sets this automatically.

**Lint not triggering**
- Check file path matches pattern (e.g., `*/api/rest/*`)
- Verify lint is registered in `lib.rs`
- Rebuild: `cd dylint_lints && cargo build --release`

**Changes not reflected**
Use `make dylint` - it auto-rebuilds if sources changed.

## Resources

- [Makefile](../../Makefile) - Tool comparison table (line 60)
- [Dylint Docs](https://github.com/trailofbits/dylint)
- [Clippy Lint Development](https://doc.rust-lang.org/nightly/clippy/development/index.html)

## License

Apache-2.0
