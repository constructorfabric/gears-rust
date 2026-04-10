//! Compile-time proto-representability check.
//!
//! [`GrpcRepr`] and [`GrpcReprScalar`] are marker traits used by the
//! `#[toolkit::grpc_contract]` macro to verify that every method parameter
//! and return type can be represented in proto3 — without running the
//! schema-to-proto generator.
//!
//! Opt-in for user DTOs: add `#[derive(toolkit::ProtoBridge)]`. The derive
//! emits both `impl GrpcRepr for YourType {}` and
//! `impl GrpcReprScalar for YourType {}`.
//!
//! Built-in primitive impls are provided here. Composite shapes
//! (`Vec<T>`, `Option<T>`, `HashMap<String, V>`, `BTreeMap<String, V>`) are
//! accepted automatically when their element type implements
//! [`GrpcReprScalar`]. Nested maps and `Vec<Vec<_>>` are intentionally NOT
//! representable — proto3 has no equivalent.
//!
//! This module is feature-gate-free so the macro can emit static assertions
//! regardless of which features the downstream crate enables.

use std::collections::{BTreeMap, HashMap};

/// Marker for "any type that can appear in a gRPC method signature, either
/// as a parameter or as the success type of `Result<T, E>` returned from a
/// method." Composite shapes (`Vec<T>`, `Option<T>`, maps) are
/// `GrpcRepr` when their inner type is [`GrpcReprScalar`].
pub trait GrpcRepr {}

/// Marker for "scalar" types — anything that can sit inside a `Vec<>`,
/// `Option<>`, or as the value type of a `HashMap<String, V>`.
///
/// Maps and lists are deliberately NOT scalars: proto3 disallows nesting
/// `repeated repeated` and `map<K, map<...>>`.
pub trait GrpcReprScalar: GrpcRepr {}

// --- primitive scalar impls -------------------------------------------------

macro_rules! impl_primitive_repr {
    ($($t:ty),* $(,)?) => {
        $(
            impl GrpcRepr for $t {}
            impl GrpcReprScalar for $t {}
        )*
    };
}

// Numeric and string primitives that have a 1:1 proto3 mapping.
// `String` ↔ `string`, `i32` ↔ `int32`, `i64` ↔ `int64`, `u32` ↔ `uint32`,
// `u64` ↔ `uint64`, `f32` ↔ `float`, `f64` ↔ `double`, `bool` ↔ `bool`.
//
// `i8`, `i16`, `u8`, `u16` are intentionally NOT included: proto3 has no
// narrower-than-32-bit integer types and silently widening would lose
// validation. Use `i32`/`u32` explicitly.
//
// `i128`, `u128`, `f128`, `char`, `isize`, `usize` are NOT included: they
// have no proto3 representation.
impl_primitive_repr!(String, i32, i64, u32, u64, f32, f64, bool);

// --- composite impls --------------------------------------------------------

/// `Vec<T>` → `repeated T`. Disallows `Vec<Vec<T>>` because `Vec<T>` does
/// not implement [`GrpcReprScalar`].
impl<T: GrpcReprScalar> GrpcRepr for Vec<T> {}

/// `Option<T>` → `optional T` (proto3). Disallows `Option<Vec<T>>` and
/// `Option<HashMap<_,_>>` for the same reason maps and lists aren't scalar.
impl<T: GrpcReprScalar> GrpcRepr for Option<T> {}

/// `HashMap<String, V>` → `map<string, V>`. Restricted to string keys —
/// proto3 also allows integer keys but the common Rust idiom is string
/// keys, and admitting more would defeat the guard's clarity.
impl<V: GrpcReprScalar, S: ::std::hash::BuildHasher> GrpcRepr for HashMap<String, V, S> {}

/// `BTreeMap<String, V>` mirrors `HashMap<String, V>` for code that prefers
/// deterministic iteration order in serialized output.
impl<V: GrpcReprScalar> GrpcRepr for BTreeMap<String, V> {}

// --- byte-buffer impls ------------------------------------------------------
//
// `Vec<u8>` is *not* a `GrpcReprScalar`: `u8` itself is not in the impl list
// above (proto3 has no 8-bit integer), so `Vec<u8>` would fail anyway. Users
// who want a `bytes` field should derive `ProtoBridge` on a wrapper struct
// or annotate their DTO field — both flow through `#[derive(ProtoBridge)]`.

// --- compile-time assert helper --------------------------------------------

/// Static assertion helper used by `#[toolkit::grpc_contract]` to fail
/// compilation when a method parameter or return type cannot be represented
/// in proto3.
///
/// The macro emits a `const _: () = { ... };` block calling this for every
/// non-context method parameter and the `Ok` half of every `Result<T, E>`
/// return type.
///
/// Naming is intentionally awkward — this is not a public API; treat it as
/// an internal helper that happens to live in `pub mod` so generated code
/// can reach it.
#[doc(hidden)]
pub const fn assert_grpc_repr<T: GrpcRepr + ?Sized>() {}

// ---------------------------------------------------------------------------
// SecurityContext marker
// ---------------------------------------------------------------------------

/// Marker trait for "this type carries an in-process security context that
/// must be projected onto gRPC metadata at the client and reconstructed on
/// the server" — i.e. a parameter the wire payload synthesizer must skip.
///
/// `#[toolkit::grpc_contract]` and `#[toolkit::rest_contract]` detect such
/// parameters by type name (`*SecurityContext`-suffixed) and emit a static
/// assertion `assert_security_context::<T>()` so accidentally naming a DTO
/// `SecurityContext` without implementing this marker fails to compile.
///
/// Lives in this module (always-on, feature-gate-free) so the macro can
/// emit unconditional assertions regardless of which features downstream
/// crates enable. The default impl for `toolkit_security::SecurityContext`
/// lives in [`crate::grpc`] under the `grpc-client` feature.
pub trait SecurityContextMarker {}

/// Compile-time helper used by generated code. Calling
/// `assert_security_context::<T>()` requires `T: SecurityContextMarker` —
/// so any type the macro classifies as "security context" must explicitly
/// opt into the marker trait.
#[doc(hidden)]
pub const fn assert_security_context<T: SecurityContextMarker + ?Sized>() {}

/// Error returned by the generated `try_from_i32` inherent method on a
/// `ProtoBridge` enum when the wire value does not correspond to any known
/// Rust variant. The parallel infallible `From<i32>` impl silently falls
/// back to `Default::default()` for unknown discriminants — use
/// `try_from_i32` when callers need to detect the unknown-variant case.
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[error("unknown enum discriminant: {0}")]
pub struct UnknownEnumDiscriminant(pub i32);

/// Error returned by the generated `try_from_proto` inherent method on a
/// `ProtoBridge` struct when a `#[proto_bridge(via_string)]` field carries a
/// value that fails to parse via `FromStr`. The parallel infallible
/// `From<Proto>` impl panics with `.expect(...)` on malformed input — use
/// `try_from_proto` to convert wire input from peers without exposing a
/// remote-DoS surface.
#[derive(Debug, thiserror::Error)]
#[error("proto bridge: invalid `{field}` value (could not parse from string): {source}")]
pub struct ViaStringParseError {
    pub field: &'static str,
    #[source]
    pub source: Box<dyn std::error::Error + Send + Sync + 'static>,
}

/// Logging hook called from generated `From<i32>` impls when the wire value
/// does not correspond to any known Rust variant.
///
/// Centralized here (not inlined into the macro expansion) so SDK crates
/// that derive `ProtoBridge` do not need a direct `tracing` dependency, and
/// so the observability behavior can evolve in one place without
/// re-expanding every macro consumer.
#[doc(hidden)]
pub fn log_unknown_enum_discriminant(discriminant: i32, rust_type: &'static str) {
    tracing::warn!(
        discriminant,
        rust_type,
        "proto bridge: unknown enum discriminant; falling back to Default. \
         Use `try_from_i32` if the caller needs to detect this case."
    );
}
