Created: 2026-03-18 by Constructor Tech
Updated: 2026-03-18 by Constructor Tech

# DE1303: No Primitive Type Aliases in Contract

## What it does

Detects `pub type X = Y` aliases in **contract modules** where `Y` is a primitive-like type (Uuid, String, integers, etc.). Such aliases provide zero compile-time type safety and should be newtypes instead.

## Why is this bad?

A bare type alias is fully transparent: `TenantId` and `UserId` both resolve to `Uuid`, so the compiler accepts one where the other is expected. A newtype (`pub struct TenantId(Uuid)`) makes such confusion a hard compile error.

Type aliases are useful for generics or shortening complex types, not for wrapping a single primitive.

## Scope

This lint **only** enforces in **contract modules** (paths containing `contract/`). SDK and contract boundaries are where transparent primitive aliases cause API type-safety problems.

## Example

### Bad

```rust
// In contract/
pub type TenantId = Uuid;
pub type GtsId = String;
pub type Port = u16;
```

### Good

```rust
// In contract/
pub struct TenantId(pub Uuid);
pub struct GtsId(String);
pub struct Port(u16);
```

### Excluded (not flagged)

```rust
pub type Wrapper<T> = Vec<T>;           // Generic alias
pub type JsonValue = serde_json::Value; // Complex type, not primitive
```

## Primitive types flagged

- UUID/ID: `Uuid`, `Ulid`
- String: `String`
- Integers: `u8`, `u16`, `u32`, `u64`, `u128`, `usize`, `i8`, `i16`, `i32`, `i64`, `i128`, `isize`
- Floats: `f32`, `f64`
- Other: `bool`, `char`

## Configuration

This lint is configured to **deny** by default.

## See Also

- [Newtype pattern](https://doc.rust-lang.org/rust-by-example/generics/new_types.html)
- [DE0309](../../de03_domain_layer/de0309_must_have_domain_model) - Must Have Domain Model
- [Module layout and SDK pattern](../../../docs/modkit_unified_system/02_module_layout_and_sdk_pattern.md)
