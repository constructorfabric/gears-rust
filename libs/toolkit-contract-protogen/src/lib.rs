//! Generate idiomatic `.proto` files from `ContractIr` + `GrpcBindingIr`
//! plus caller-supplied `JsonSchema` definitions.
//!
//! This is the "transport-projection" of the schemars-based pipeline used
//! elsewhere by `toolkit-contract::openapi`. The same `&[(name, Schema)]`
//! input list yields either an `OpenAPI` document or a `.proto` file.
//!
//! Scope:
//! - object → message
//! - string enum (top-level OR inline) → proto enum
//! - primitives (string, int32/64/uint64, bool, float/double, uuid → string,
//!   byte/base64 → bytes)
//! - `Option<T>` → `optional T`
//! - `Vec<T>` → `repeated T`
//! - `HashMap<String, T>` / `additionalProperties: T` → `map<string, T>` (field-level)
//! - `$ref` → message/enum reference
//! - `oneOf` of `{type: "string", const: "..."}` branches → proto enum
//! - `oneOf` of single-property object branches (externally-tagged enums)
//!   → proto3 `oneof` inside a wrapper message
//!
//! Out of scope (returns [`ProtoGenError`]):
//! - `allOf` composition (no single base)
//! - `not` schemas
//! - nested arrays (`array<array<T>>`)
//! - non-string map keys
//! - internally-tagged enums (use externally-tagged)
//! - heterogeneous `anyOf` / type-arrays with multiple non-null types
//!
//! See [`generate_proto_file`] for the entry point.

pub mod lockfile;

pub use lockfile::{LockfileError, MessageLock, ProtoLockfile};

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use heck::{ToShoutySnakeCase, ToSnakeCase, ToUpperCamelCase};
use toolkit_contract::ir::contract::{
    ContractIr, FieldIr, FieldRole, MethodIr, PrimitiveType, TypeRef,
};
use toolkit_contract::ir::grpc::{GrpcBindingIr, GrpcIdempotency};
use serde_json::Value;

/// Strongly-typed taxonomy of features the generator does not (yet) support.
/// Replaces the previous `&'static str` `feature` field — variants are
/// machine-readable (assertions can match on them without string comparison)
/// and self-documenting via `Display`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProtoGenFeature {
    // --- JSON Schema compositions -----------------------------------------
    /// `allOf` composition (no single base).
    AllOf,
    /// `anyOf` composition (heterogeneous union without nullable-ref shortcut).
    AnyOf,
    /// `oneOf` composition that doesn't match the externally-tagged or
    /// string-const enum shortcuts.
    OneOf,
    /// `not` schema.
    Not,

    // --- Object shape -----------------------------------------------------
    /// Object schema lacking `properties` (and not the
    /// `additionalProperties`-only map shape).
    ObjectWithoutProperties,
    /// Object combining `properties` and `additionalProperties`.
    PropertiesAndAdditionalProperties,
    /// Top-level schema that is neither an object nor a string-enum.
    NonObjectNonStringEnumTopLevel,

    // --- Maps -------------------------------------------------------------
    /// Map key must be `String` (proto3 limitation).
    NonStringMapKey,
    /// Map value cannot itself be `repeated` or another `map`.
    MapValueRepeatedOrNested,

    // --- Arrays / lists ---------------------------------------------------
    /// `array` schema without `items`.
    ArrayWithoutItems,
    /// Nested arrays (`array<array<T>>`) — proto3 has no equivalent.
    NestedArrays,

    // --- Type arrays / nullability ---------------------------------------
    /// `Option<Vec<_>>` — proto3 cannot mix `repeated` with `optional`.
    OptionOfVec,
    /// `Option<HashMap<_,_>>` — proto3 cannot mix `optional` with `map`.
    OptionOfMap,
    /// `Vec<HashMap<_,_>>` — proto3 cannot nest map inside repeated.
    VecOfMap,
    /// JSON Schema type-array with two or more non-null entries.
    TypeArrayMultipleNonNull,
    /// Field whose only declared type is `"null"`.
    NullOnlyField,
    /// Field with no `type` annotation at all.
    UntypedField,
    /// Field declared with a JSON Schema primitive type that has no proto3
    /// equivalent. The variant carries the offending type name verbatim.
    UnknownPrimitiveType(String),

    // --- Method I/O ------------------------------------------------------
    /// Method return type is a primitive (must be wrapped in a Named struct).
    PrimitiveMethodReturn,
    /// Method return type is non-Named (Option/List/Map at the boundary).
    NonNamedMethodReturn,

    // --- Enums -----------------------------------------------------------
    /// Top-level string enum with zero variants.
    EmptyEnum,
    /// Inline string enum (in a field) with zero variants.
    EmptyInlineEnum,

    // --- oneof payloads --------------------------------------------------
    /// `oneof` variant payload tries to be repeated/map/optional.
    OneofVariantPayloadInvalid,
}

impl std::fmt::Display for ProtoGenFeature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtoGenFeature::AllOf => f.write_str("allOf"),
            ProtoGenFeature::AnyOf => f.write_str("anyOf"),
            ProtoGenFeature::OneOf => f.write_str("oneOf"),
            ProtoGenFeature::Not => f.write_str("not"),
            ProtoGenFeature::ObjectWithoutProperties => f.write_str("object without properties"),
            ProtoGenFeature::PropertiesAndAdditionalProperties => {
                f.write_str("object combining `properties` and `additionalProperties`")
            }
            ProtoGenFeature::NonObjectNonStringEnumTopLevel => {
                f.write_str("non-object, non-string-enum top-level schema")
            }
            ProtoGenFeature::NonStringMapKey => {
                f.write_str("map key must be `String` (proto3 map limitation)")
            }
            ProtoGenFeature::MapValueRepeatedOrNested => {
                f.write_str("map value cannot be `repeated` or another `map`")
            }
            ProtoGenFeature::ArrayWithoutItems => f.write_str("array without items"),
            ProtoGenFeature::NestedArrays => f.write_str("nested arrays (repeated repeated)"),
            ProtoGenFeature::OptionOfVec => f.write_str("Option<Vec<_>> nested optional/repeated"),
            ProtoGenFeature::OptionOfMap => {
                f.write_str("Option<HashMap<_,_>> not representable in proto3")
            }
            ProtoGenFeature::VecOfMap => {
                f.write_str("Vec<HashMap<_,_>> not representable in proto3")
            }
            ProtoGenFeature::TypeArrayMultipleNonNull => {
                f.write_str("type-array with multiple non-null types")
            }
            ProtoGenFeature::NullOnlyField => f.write_str("null-only field"),
            ProtoGenFeature::UntypedField => f.write_str("untyped field"),
            ProtoGenFeature::UnknownPrimitiveType(name) => {
                write!(f, "unknown primitive type `{name}`")
            }
            ProtoGenFeature::PrimitiveMethodReturn => {
                f.write_str("primitive method return type (wrap in a Named struct)")
            }
            ProtoGenFeature::NonNamedMethodReturn => f.write_str("non-Named method return type"),
            ProtoGenFeature::EmptyEnum => f.write_str("empty enum"),
            ProtoGenFeature::EmptyInlineEnum => f.write_str("empty inline enum"),
            ProtoGenFeature::OneofVariantPayloadInvalid => {
                f.write_str("oneof variant payload cannot be repeated/map/optional")
            }
        }
    }
}

/// Errors produced while translating contract + schemas into `.proto` text.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProtoGenError {
    /// A schema feature is not yet supported by the generator.
    #[error("unsupported schema feature in `{schema_name}`: {feature}")]
    UnsupportedSchemaFeature {
        schema_name: String,
        feature: ProtoGenFeature,
    },
    /// A `$ref` referenced an unknown definition.
    #[error("unknown type reference `{ref_path}` in `{schema_name}`")]
    UnknownTypeReference {
        schema_name: String,
        ref_path: String,
    },
    /// The contract method binding referred to a method missing in `ContractIr`.
    #[error("binding/method drift: `{method_name}` present in binding but not in contract IR")]
    BindingDrift { method_name: String },
    /// Synthesized `<Method>Request` collides with a user-provided schema of the same name.
    #[error(
        "synthesized request type `{request_name}` for method `{method_name}` collides with a user-provided schema of the same name"
    )]
    SynthesizedNameCollision {
        method_name: String,
        request_name: String,
    },
    /// Two JSON property names collapse to the same proto `snake_case` identifier.
    #[error(
        "schema `{schema_name}` has properties `{a}` and `{b}` that both map to proto field `{snake}`"
    )]
    FieldNameCollision {
        schema_name: String,
        a: String,
        b: String,
        snake: String,
    },
    /// Internal invariant violation: a field/variant that was just assigned a
    /// number via the lockfile is no longer found when looking it up.
    #[error("internal lockfile invariant violation: {detail}")]
    LockfileInvariant { detail: String },
}

/// Generate a complete `.proto` file from the contract, gRPC binding, and
/// a list of schemas covering every `TypeRef::Named` referenced by methods.
///
/// `schemas` follows the same convention used by
/// `toolkit_contract::openapi::generate_openapi_spec`: a list of
/// `(name, schema)` pairs where the name matches `TypeRef::Named(name)` in
/// `ContractIr`.
///
/// `lock` is mutated in place: existing `(message, field) → number`
/// mappings are preserved verbatim; new fields receive the smallest unused
/// number; fields removed from the schema move to
/// `reserved_numbers`/`reserved_names`. Caller is responsible for
/// persisting `lock` back to disk after this call returns. Pass a fresh
/// [`ProtoLockfile::empty()`] only for one-shot ad-hoc generation —
/// anything externally consumed needs a persisted lock so wire numbers
/// stay stable across schema mutations.
///
/// # Errors
///
/// Returns [`ProtoGenError`] when an input schema uses an unsupported
/// `JsonSchema` feature, references an undefined type, or the contract IR
/// disagrees with the gRPC binding.
pub fn generate_proto_file(
    contract: &ContractIr,
    binding: &GrpcBindingIr,
    schemas: &[(&str, schemars::Schema)],
    lock: &mut ProtoLockfile,
) -> Result<String, ProtoGenError> {
    let schema_map: BTreeMap<&str, &schemars::Schema> =
        schemas.iter().map(|(n, s)| (*n, s)).collect();

    let mut messages: BTreeMap<String, MessageDef> = BTreeMap::new();
    let mut enums: BTreeMap<String, EnumDef> = BTreeMap::new();
    let mut emitted_for_methods: BTreeSet<String> = BTreeSet::new();
    let mut visit_queue: Vec<String> = Vec::new();

    // Walk each method and record their input/output proto types.
    let mut method_proto_io: Vec<MethodProtoIo> = Vec::new();
    for binding_method in &binding.methods {
        let contract_method = contract
            .methods
            .iter()
            .find(|m| m.name == binding_method.method_name)
            .ok_or_else(|| ProtoGenError::BindingDrift {
                method_name: binding_method.method_name.clone(),
            })?;

        let input = method_input_type(contract_method, &mut messages, lock, &schema_map)?;
        let output = method_output_type(contract_method)?;
        method_proto_io.push(MethodProtoIo {
            rpc_name: binding_method.rpc_name.clone(),
            server_streaming: binding_method.server_streaming,
            idempotency: binding_method.idempotency_level,
            input_type: input.clone(),
            output_type: output.clone(),
        });

        for ty in [&input, &output] {
            let ProtoTypeName::Message(name) = ty;
            visit_queue.push(name.clone());
        }
        emitted_for_methods.insert(contract_method.name.clone());
    }

    // Walk every Named schema referenced from methods and emit message/enum
    // defs, following nested $refs/$defs.
    let mut visited: BTreeSet<String> = BTreeSet::new();
    while let Some(name) = visit_queue.pop() {
        if !visited.insert(name.clone()) {
            continue;
        }
        if messages.contains_key(&name) || enums.contains_key(&name) {
            // Already emitted (e.g., synthesized request).
            continue;
        }
        let Some(schema) = schema_map.get(name.as_str()) else {
            return Err(ProtoGenError::UnknownTypeReference {
                schema_name: name.clone(),
                ref_path: name.clone(),
            });
        };

        translate_schema(
            &name,
            schema.as_value(),
            &mut messages,
            &mut enums,
            &mut visit_queue,
            lock,
        )?;
    }

    // Reap fields that vanished from the schema across all messages we
    // touched, then propagate the lock's reserved tombstones into the
    // MessageDef so the renderer emits `reserved` clauses.
    for (name, def) in &mut messages {
        let entry = lock.messages.entry(name.clone()).or_default();
        let mut current: BTreeSet<String> = def.fields.iter().map(|f| f.name.clone()).collect();
        if let Some(oneof) = &def.oneof {
            for v in &oneof.variants {
                current.insert(v.name.to_snake_case());
            }
        }
        entry.reap_removed(&current);
        def.reserved_numbers.clone_from(&entry.reserved_numbers);
        def.reserved_names.clone_from(&entry.reserved_names);
    }

    // Same lifecycle for enums: pre-assign numbers under the lock so they
    // stay stable across regenerations, then reap removed variants. The
    // renderer always synthesizes `<ENUM_NAME>_UNSPECIFIED = 0` — user
    // variants live at 1..N.
    let mut enum_numbers: BTreeMap<String, BTreeMap<String, u32>> = BTreeMap::new();
    for (name, def) in &mut enums {
        let entry = lock.enums.entry(name.clone()).or_default();
        for variant in &def.variants {
            entry.assign(variant);
        }
        let current: BTreeSet<String> = def.variants.iter().cloned().collect();
        entry.reap_removed(&current);
        def.reserved_numbers = entry.reserved_numbers.clone();
        def.reserved_names = entry.reserved_names.clone();
        enum_numbers.insert(name.clone(), entry.variants.clone());
    }

    Ok(render_proto(
        &binding.package,
        &binding.service,
        contract,
        &method_proto_io,
        &messages,
        &enums,
        &enum_numbers,
    ))
}

// --- internal types -------------------------------------------------------

#[derive(Debug, Clone)]
enum ProtoTypeName {
    /// Reference to a generated message or enum by name.
    Message(String),
}

impl ProtoTypeName {
    fn render_ref(&self) -> String {
        match self {
            ProtoTypeName::Message(n) => n.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct MessageDef {
    fields: Vec<MessageField>,
    /// Externally-tagged enum projected as `oneof` — when present, the
    /// message body contains a single `oneof` block instead of regular fields.
    oneof: Option<OneofDef>,
    /// Field numbers that were once assigned but are now removed. Emitted as
    /// `reserved <numbers>;` so the wire numbers can never be re-used.
    reserved_numbers: Vec<u32>,
    /// Field names that were once assigned but are now removed. Emitted as
    /// `reserved "<name>";` so the wire names can never be re-used.
    reserved_names: Vec<String>,
}

#[derive(Debug, Clone)]
struct MessageField {
    name: String,
    /// Either a primitive ("string"), a referenced type name ("`ChargeRequest`"),
    /// or a fully formatted `map<string, V>` literal.
    proto_type: String,
    repeated: bool,
    optional: bool,
    /// `true` when [`proto_type`] is a literal `map<...>` and modifiers like
    /// `repeated`/`optional` must be suppressed at render time.
    is_map: bool,
    field_number: u32,
}

/// An `oneof` block inside a message — emitted for externally-tagged Rust
/// enums where each variant carries a payload referenced by `$ref`.
#[derive(Debug, Clone)]
struct OneofDef {
    /// The label inside the message body, e.g. `oneof variant { ... }`.
    label: String,
    variants: Vec<OneofVariant>,
}

#[derive(Debug, Clone)]
struct OneofVariant {
    /// The discriminator key as it appears in JSON — used for the proto field
    /// name (`snake_case`).
    name: String,
    /// Proto type for the variant payload: a referenced message name or a
    /// primitive type literal.
    proto_type: String,
    field_number: u32,
}

#[derive(Debug, Clone, Default)]
struct EnumDef {
    /// User-declared variants in source order. Numbers are assigned at
    /// render time via [`ProtoLockfile::enums`] so they stay stable across
    /// regenerations. Number 0 is reserved for the synthetic
    /// `<ENUM_NAME>_UNSPECIFIED` sentinel emitted by the renderer.
    variants: Vec<String>,
    /// Tombstoned numbers from the lockfile, propagated into the `EnumDef`
    /// just before rendering so the renderer emits `reserved <N>;`.
    reserved_numbers: Vec<u32>,
    /// Tombstoned names from the lockfile, ditto.
    reserved_names: Vec<String>,
}

#[derive(Debug, Clone)]
struct MethodProtoIo {
    rpc_name: String,
    server_streaming: bool,
    idempotency: GrpcIdempotency,
    input_type: ProtoTypeName,
    output_type: ProtoTypeName,
}

// --- method I/O resolution ------------------------------------------------

fn method_input_type(
    method: &MethodIr,
    messages: &mut BTreeMap<String, MessageDef>,
    lock: &mut ProtoLockfile,
    schema_map: &BTreeMap<&str, &schemars::Schema>,
) -> Result<ProtoTypeName, ProtoGenError> {
    // Drop server-side parameters (SecurityContext) — they don't belong on
    // the gRPC wire; the macro injects them via metadata at the client and
    // the server handler reconstructs them from inbound trailers.
    let wire_fields: Vec<&FieldIr> = method
        .input
        .fields
        .iter()
        .filter(|f| f.role == FieldRole::Wire)
        .collect();

    // Single Named wire-param → reuse it.
    if wire_fields.len() == 1
        && let TypeRef::Named(name) = &wire_fields[0].ty
    {
        return Ok(ProtoTypeName::Message(name.clone()));
    }

    // Otherwise synthesize <MethodName>Request from the wire fields.
    let request_name = format!("{}Request", method.name.to_upper_camel_case());
    if messages.contains_key(&request_name) || schema_map.contains_key(request_name.as_str()) {
        return Err(ProtoGenError::SynthesizedNameCollision {
            method_name: method.name.clone(),
            request_name,
        });
    }
    let mut sorted_fields = wire_fields;
    sorted_fields.sort_by(|a, b| a.name.cmp(&b.name));

    let lock_entry = lock.messages.entry(request_name.clone()).or_default();
    let mut fields = Vec::with_capacity(sorted_fields.len());
    for field in &sorted_fields {
        let snake_name = field.name.to_snake_case();
        let field_number = lock_entry.assign(&snake_name);
        let (proto_type, repeated, _optional, is_map) = type_ref_to_proto(&field.ty)?;
        fields.push(MessageField {
            name: snake_name,
            proto_type,
            repeated,
            optional: field.optional && !is_map,
            is_map,
            field_number,
        });
    }
    messages.insert(
        request_name.clone(),
        MessageDef {
            fields,
            oneof: None,
            ..MessageDef::default()
        },
    );
    Ok(ProtoTypeName::Message(request_name))
}

fn method_output_type(method: &MethodIr) -> Result<ProtoTypeName, ProtoGenError> {
    match &method.output {
        TypeRef::Named(name) => Ok(ProtoTypeName::Message(name.clone())),
        TypeRef::Primitive(p) => {
            // Primitive output: most realistic mapping is a Named wrapper.
            // Out-of-scope for Phase 1 — return error so the user knows.
            let _ = p;
            Err(ProtoGenError::UnsupportedSchemaFeature {
                schema_name: method.name.clone(),
                feature: ProtoGenFeature::PrimitiveMethodReturn,
            })
        }
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            Err(ProtoGenError::UnsupportedSchemaFeature {
                schema_name: method.name.clone(),
                feature: ProtoGenFeature::NonNamedMethodReturn,
            })
        }
    }
}

/// Returned tuple is `(proto_type, repeated, optional, is_map)`. `is_map`
/// signals that the `proto_type` already includes a `map<...>` literal and the
/// renderer must suppress `repeated`/`optional` modifiers.
fn type_ref_to_proto(ty: &TypeRef) -> Result<(String, bool, bool, bool), ProtoGenError> {
    match ty {
        TypeRef::Primitive(p) => Ok((primitive_proto(*p).to_owned(), false, false, false)),
        TypeRef::Named(name) => Ok((name.clone(), false, false, false)),
        TypeRef::Optional(inner) => {
            let (t, repeated, _, is_map) = type_ref_to_proto(inner)?;
            if repeated {
                return Err(ProtoGenError::UnsupportedSchemaFeature {
                    schema_name: format!("{ty:?}"),
                    feature: ProtoGenFeature::OptionOfVec,
                });
            }
            if is_map {
                return Err(ProtoGenError::UnsupportedSchemaFeature {
                    schema_name: format!("{ty:?}"),
                    feature: ProtoGenFeature::OptionOfMap,
                });
            }
            Ok((t, false, true, false))
        }
        TypeRef::List(inner) => {
            let (t, repeated, _, is_map) = type_ref_to_proto(inner)?;
            if repeated {
                return Err(ProtoGenError::UnsupportedSchemaFeature {
                    schema_name: format!("{ty:?}"),
                    feature: ProtoGenFeature::NestedArrays,
                });
            }
            if is_map {
                return Err(ProtoGenError::UnsupportedSchemaFeature {
                    schema_name: format!("{ty:?}"),
                    feature: ProtoGenFeature::VecOfMap,
                });
            }
            Ok((t, true, false, false))
        }
        TypeRef::Map(key, value) => {
            // proto3 maps require a string-or-integer key; we restrict to
            // string for the common Rust `HashMap<String, V>` shape.
            let key_proto = match key.as_ref() {
                TypeRef::Primitive(PrimitiveType::String) => "string".to_owned(),
                _ => {
                    return Err(ProtoGenError::UnsupportedSchemaFeature {
                        schema_name: format!("{ty:?}"),
                        feature: ProtoGenFeature::NonStringMapKey,
                    });
                }
            };
            let (val_proto, repeated, _, is_map) = type_ref_to_proto(value)?;
            if repeated || is_map {
                return Err(ProtoGenError::UnsupportedSchemaFeature {
                    schema_name: format!("{ty:?}"),
                    feature: ProtoGenFeature::MapValueRepeatedOrNested,
                });
            }
            Ok((format!("map<{key_proto}, {val_proto}>"), false, false, true))
        }
    }
}

fn primitive_proto(p: PrimitiveType) -> &'static str {
    match p {
        PrimitiveType::String | PrimitiveType::Uuid => "string",
        PrimitiveType::I32 => "int32",
        PrimitiveType::I64 => "int64",
        PrimitiveType::U64 => "uint64",
        PrimitiveType::F64 => "double",
        PrimitiveType::Bool => "bool",
        PrimitiveType::Bytes => "bytes",
    }
}

// --- schemars Schema → MessageDef / EnumDef --------------------------------

fn translate_schema(
    schema_name: &str,
    schema: &Value,
    messages: &mut BTreeMap<String, MessageDef>,
    enums: &mut BTreeMap<String, EnumDef>,
    queue: &mut Vec<String>,
    lock: &mut ProtoLockfile,
) -> Result<(), ProtoGenError> {
    // String enum: { type: "string", enum: ["a", "b", ...] }
    if let Some(enum_array) = schema.get("enum").and_then(|v| v.as_array())
        && schema
            .get("type")
            .and_then(|v| v.as_str())
            .is_some_and(|t| t == "string")
    {
        let variants: Vec<String> = enum_array
            .iter()
            .filter_map(|v| v.as_str().map(std::borrow::ToOwned::to_owned))
            .collect();
        if variants.is_empty() {
            return Err(ProtoGenError::UnsupportedSchemaFeature {
                schema_name: schema_name.to_owned(),
                feature: ProtoGenFeature::EmptyEnum,
            });
        }
        enums.insert(
            schema_name.to_owned(),
            EnumDef {
                variants,
                ..EnumDef::default()
            },
        );
        return Ok(());
    }

    // schemars 1.x rename_all-tagged unit enum: { oneOf: [
    //   { type: "string", const: "pending" }, { type: "string", const: "completed" }, ...
    // ] }
    // Promote to a proto enum if every branch is a string-typed const.
    if let Some(one_of) = schema.get("oneOf").and_then(|v| v.as_array())
        && let Some(variants) = collect_string_consts(one_of)
    {
        enums.insert(
            schema_name.to_owned(),
            EnumDef {
                variants,
                ..EnumDef::default()
            },
        );
        return Ok(());
    }

    // Externally-tagged enum projected as a proto3 `oneof`.
    // schemars emits `enum E { Variant1(X), Variant2(Y) }` as
    // `{ oneOf: [{type:"object", required:["Variant1"], properties:{Variant1: schema}}, ...] }`.
    if let Some(branches) = schema.get("oneOf").and_then(|v| v.as_array())
        && let Some(oneof) =
            collect_externally_tagged_oneof(schema_name, branches, enums, queue, lock)?
    {
        messages.insert(
            schema_name.to_owned(),
            MessageDef {
                fields: Vec::new(),
                oneof: Some(oneof),
                ..MessageDef::default()
            },
        );
        return Ok(());
    }

    // Forbidden compositions (after the oneOf-of-consts and oneOf-of-objects
    // shortcuts above). `anyOf` of `[{$ref}, {type:null}]` for nullable refs
    // is handled at field level.
    for (key, feature) in [
        ("allOf", ProtoGenFeature::AllOf),
        ("anyOf", ProtoGenFeature::AnyOf),
        ("oneOf", ProtoGenFeature::OneOf),
        ("not", ProtoGenFeature::Not),
    ] {
        if schema.get(key).is_some() {
            return Err(ProtoGenError::UnsupportedSchemaFeature {
                schema_name: schema_name.to_owned(),
                feature,
            });
        }
    }

    // Object: { type: "object", properties: {...}, required: [...] }
    if schema
        .get("type")
        .and_then(|v| v.as_str())
        .is_some_and(|t| t == "object")
    {
        // Top-level `HashMap<String, V>` projection: an object whose only
        // schema descriptor is `additionalProperties`. Rendered as a wrapper
        // message with a single `entries` map field.
        if let Some(addl) = schema.get("additionalProperties")
            && !addl.is_boolean()
            && schema
                .get("properties")
                .and_then(|p| p.as_object())
                .is_none_or(serde_json::Map::is_empty)
        {
            let value_shape =
                json_schema_to_proto_field(schema_name, "entries", addl, false, queue, enums)?;
            if value_shape.repeated || value_shape.is_map {
                return Err(ProtoGenError::UnsupportedSchemaFeature {
                    schema_name: schema_name.to_owned(),
                    feature: ProtoGenFeature::MapValueRepeatedOrNested,
                });
            }
            messages.insert(
                schema_name.to_owned(),
                MessageDef {
                    fields: vec![MessageField {
                        name: "entries".to_owned(),
                        proto_type: format!("map<string, {}>", value_shape.proto_type),
                        repeated: false,
                        optional: false,
                        is_map: true,
                        field_number: 1,
                    }],
                    oneof: None,
                    ..MessageDef::default()
                },
            );
            return Ok(());
        }

        if schema.get("additionalProperties").is_some()
            && schema
                .get("additionalProperties")
                .is_some_and(|v| !v.is_boolean())
        {
            return Err(ProtoGenError::UnsupportedSchemaFeature {
                schema_name: schema_name.to_owned(),
                feature: ProtoGenFeature::PropertiesAndAdditionalProperties,
            });
        }

        let properties = schema
            .get("properties")
            .and_then(|v| v.as_object())
            .ok_or_else(|| ProtoGenError::UnsupportedSchemaFeature {
                schema_name: schema_name.to_owned(),
                feature: ProtoGenFeature::ObjectWithoutProperties,
            })?;
        let required: BTreeSet<&str> = schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        // Sorted field names → deterministic *iteration order* for the
        // generated `.proto`. Numbers themselves come from the lockfile so
        // they stay stable across schema mutations (per § Field-number
        // stability).
        let mut sorted_keys: Vec<&str> =
            properties.keys().map(std::string::String::as_str).collect();
        sorted_keys.sort_unstable();

        let mut snake_seen: BTreeMap<String, &str> = BTreeMap::new();
        for key in &sorted_keys {
            let snake = key.to_snake_case();
            if let Some(prev) = snake_seen.insert(snake.clone(), *key)
                && prev != *key
            {
                return Err(ProtoGenError::FieldNameCollision {
                    schema_name: schema_name.to_owned(),
                    a: prev.to_owned(),
                    b: (*key).to_owned(),
                    snake,
                });
            }
        }
        let lock_entry = lock.messages.entry(schema_name.to_owned()).or_default();
        // Pre-assign all numbers first so iteration order doesn't affect
        // which numbers new fields get (deterministic across runs).
        for key in &sorted_keys {
            let snake = key.to_snake_case();
            lock_entry.assign(&snake);
        }

        let mut fields = Vec::with_capacity(sorted_keys.len());
        for key in &sorted_keys {
            let prop_schema = &properties[*key];
            let field_proto = json_schema_to_proto_field(
                schema_name,
                key,
                prop_schema,
                !required.contains(key),
                queue,
                enums,
            )?;
            let snake = key.to_snake_case();
            let Some(field_number) = lock
                .messages
                .get(schema_name)
                .and_then(|m| m.fields.get(&snake))
                .copied()
            else {
                return Err(ProtoGenError::LockfileInvariant {
                    detail: format!(
                        "field number for `{schema_name}.{snake}` was not assigned by lockfile"
                    ),
                });
            };
            fields.push(MessageField {
                name: snake,
                proto_type: field_proto.proto_type,
                repeated: field_proto.repeated,
                optional: field_proto.optional,
                is_map: field_proto.is_map,
                field_number,
            });
        }
        // Sort fields by field_number for deterministic .proto output. This
        // matches the conventional rendering of human-written .proto files
        // where fields are listed in number order.
        fields.sort_by_key(|f| f.field_number);
        messages.insert(
            schema_name.to_owned(),
            MessageDef {
                fields,
                oneof: None,
                ..MessageDef::default()
            },
        );
        return Ok(());
    }

    Err(ProtoGenError::UnsupportedSchemaFeature {
        schema_name: schema_name.to_owned(),
        feature: ProtoGenFeature::NonObjectNonStringEnumTopLevel,
    })
}

struct ProtoFieldShape {
    proto_type: String,
    repeated: bool,
    optional: bool,
    /// `true` when [`proto_type`] is a `map<...>` literal — modifiers must be
    /// suppressed at render time.
    is_map: bool,
}

fn json_schema_to_proto_field(
    parent_schema: &str,
    field_name: &str,
    schema: &Value,
    declared_optional: bool,
    queue: &mut Vec<String>,
    extra_enums: &mut BTreeMap<String, EnumDef>,
) -> Result<ProtoFieldShape, ProtoGenError> {
    // Inline string-enum: `{ type: "string", enum: ["a", "b"] }` — synthesize
    // a sibling proto enum named `<ParentMessage><FieldName>` (UpperCamelCase)
    // and reference it. The synthesized enum is later merged into the global
    // enum table.
    if let (Some(enum_array), Some("string")) = (
        schema.get("enum").and_then(|v| v.as_array()),
        schema.get("type").and_then(|v| v.as_str()),
    ) && schema.get("$ref").is_none()
    {
        let variants: Vec<String> = enum_array
            .iter()
            .filter_map(|v| v.as_str().map(std::borrow::ToOwned::to_owned))
            .collect();
        if variants.is_empty() {
            return Err(ProtoGenError::UnsupportedSchemaFeature {
                schema_name: format!("{parent_schema}.{field_name}"),
                feature: ProtoGenFeature::EmptyInlineEnum,
            });
        }
        let enum_name = synthesize_enum_name(parent_schema, field_name);
        extra_enums.entry(enum_name.clone()).or_insert(EnumDef {
            variants,
            ..EnumDef::default()
        });
        return Ok(ProtoFieldShape {
            proto_type: enum_name,
            repeated: false,
            optional: declared_optional || schema_indicates_optional(schema),
            is_map: false,
        });
    }

    // $ref → Foo
    if let Some(ref_str) = schema.get("$ref").and_then(|v| v.as_str()) {
        let referent = ref_to_name(parent_schema, ref_str)?;
        queue.push(referent.clone());
        let optional = declared_optional || schema_indicates_optional(schema);
        return Ok(ProtoFieldShape {
            proto_type: referent,
            repeated: false,
            optional,
            is_map: false,
        });
    }

    // $ref nested inside `anyOf` for nullable refs:
    //  { anyOf: [ { $ref: "..." }, { type: "null" } ] }
    if let Some(any_of) = schema.get("anyOf").and_then(|v| v.as_array()) {
        let mut ref_target: Option<String> = None;
        let mut has_null = false;
        for entry in any_of {
            if entry
                .get("type")
                .and_then(|v| v.as_str())
                .is_some_and(|t| t == "null")
            {
                has_null = true;
            } else if let Some(r) = entry.get("$ref").and_then(|v| v.as_str()) {
                ref_target = Some(ref_to_name(parent_schema, r)?);
            }
        }
        if let Some(target) = ref_target {
            queue.push(target.clone());
            return Ok(ProtoFieldShape {
                proto_type: target,
                repeated: false,
                optional: declared_optional || has_null,
                is_map: false,
            });
        }
    }

    // type: "object" with only `additionalProperties` — map<string, V>.
    // Schemars emits `HashMap<String, V>` as `{ type: "object",
    // additionalProperties: <V-schema> }`. Proto3 only supports string keys
    // (and a handful of integer keys); Rust HashMap<String, V> is the common
    // case we model here.
    if schema
        .get("type")
        .and_then(|v| v.as_str())
        .is_some_and(|t| t == "object")
        && let Some(addl) = schema.get("additionalProperties")
        && !addl.is_boolean()
    {
        // Reject objects that mix `properties` with `additionalProperties` —
        // that's a JSON Schema feature with no proto3 equivalent.
        if schema
            .get("properties")
            .and_then(|p| p.as_object())
            .is_some_and(|p| !p.is_empty())
        {
            return Err(ProtoGenError::UnsupportedSchemaFeature {
                schema_name: format!("{parent_schema}.{field_name}"),
                feature: ProtoGenFeature::PropertiesAndAdditionalProperties,
            });
        }
        let value_shape =
            json_schema_to_proto_field(parent_schema, field_name, addl, false, queue, extra_enums)?;
        if value_shape.repeated || value_shape.is_map {
            return Err(ProtoGenError::UnsupportedSchemaFeature {
                schema_name: format!("{parent_schema}.{field_name}"),
                feature: ProtoGenFeature::MapValueRepeatedOrNested,
            });
        }
        return Ok(ProtoFieldShape {
            proto_type: format!("map<string, {}>", value_shape.proto_type),
            repeated: false,
            optional: false,
            is_map: true,
        });
    }

    // type: "array", items: ...
    if schema
        .get("type")
        .and_then(|v| v.as_str())
        .is_some_and(|t| t == "array")
    {
        let items = schema
            .get("items")
            .ok_or_else(|| ProtoGenError::UnsupportedSchemaFeature {
                schema_name: format!("{parent_schema}.{field_name}"),
                feature: ProtoGenFeature::ArrayWithoutItems,
            })?;
        let inner = json_schema_to_proto_field(
            parent_schema,
            field_name,
            items,
            false,
            queue,
            extra_enums,
        )?;
        if inner.repeated {
            return Err(ProtoGenError::UnsupportedSchemaFeature {
                schema_name: format!("{parent_schema}.{field_name}"),
                feature: ProtoGenFeature::NestedArrays,
            });
        }
        return Ok(ProtoFieldShape {
            proto_type: inner.proto_type,
            repeated: true,
            optional: false,
            is_map: false,
        });
    }

    // schemars 1.x emits `Option<Primitive>` as `{ type: ["string", "null"] }`.
    // Detect this and unwrap to a single type with implied optional.
    let mut implied_optional = false;
    let normalized_type: Option<&str> =
        if let Some(arr) = schema.get("type").and_then(|v| v.as_array()) {
            let mut non_null: Option<&str> = None;
            let mut saw_null = false;
            for v in arr {
                match v.as_str() {
                    Some("null") => saw_null = true,
                    Some(t) => {
                        if non_null.is_some() {
                            return Err(ProtoGenError::UnsupportedSchemaFeature {
                                schema_name: format!("{parent_schema}.{field_name}"),
                                feature: ProtoGenFeature::TypeArrayMultipleNonNull,
                            });
                        }
                        non_null = Some(t);
                    }
                    None => {}
                }
            }
            if saw_null {
                implied_optional = true;
            }
            non_null
        } else {
            schema.get("type").and_then(|v| v.as_str())
        };

    // Primitive types.
    let ty = normalized_type;
    let format = schema.get("format").and_then(|v| v.as_str());
    #[allow(
        clippy::match_same_arms,
        reason = "Each arm encodes a distinct OpenAPI/JsonSchema `format` semantic (uuid/date-time → string, int64/uint64 vs default → int64, double/float vs default → double); merging them would erase the documentation of which formats we explicitly recognize versus which fall through to the type's default proto representation."
    )]
    let proto_ty = match (ty, format) {
        (Some("string"), Some("uuid")) => "string", // string with comment
        (Some("string"), Some("date-time")) => "string",
        // `format: "byte"` (OpenAPI/JsonSchema convention for base64-encoded
        // bytes) and `format: "base64"` (some encoders) → proto3 `bytes`.
        // schemars emits `Vec<u8>` as `array<integer>` by default; use
        // `serde_with` / `serde_bytes` to opt in to the `bytes` representation.
        (Some("string"), Some("byte" | "base64")) => "bytes",
        (Some("string"), _) => "string",
        (Some("integer"), Some("int32")) => "int32",
        (Some("integer"), Some("int64")) => "int64",
        (Some("integer"), Some("uint64")) => "uint64",
        (Some("integer"), _) => "int64",
        (Some("number"), Some("double")) => "double",
        (Some("number"), Some("float")) => "float",
        (Some("number"), _) => "double",
        (Some("boolean"), _) => "bool",
        (Some("null"), _) => {
            return Err(ProtoGenError::UnsupportedSchemaFeature {
                schema_name: format!("{parent_schema}.{field_name}"),
                feature: ProtoGenFeature::NullOnlyField,
            });
        }
        (None, _) => {
            // No `type` key — schemars often drops it for boolean true/false
            // schemas. Treat as unsupported for safety.
            return Err(ProtoGenError::UnsupportedSchemaFeature {
                schema_name: format!("{parent_schema}.{field_name}"),
                feature: ProtoGenFeature::UntypedField,
            });
        }
        (Some(other), _) => {
            return Err(ProtoGenError::UnsupportedSchemaFeature {
                schema_name: format!("{parent_schema}.{field_name}"),
                feature: ProtoGenFeature::UnknownPrimitiveType(other.to_owned()),
            });
        }
    };

    Ok(ProtoFieldShape {
        proto_type: proto_ty.to_owned(),
        repeated: false,
        optional: declared_optional || implied_optional || schema_indicates_optional(schema),
        is_map: false,
    })
}

fn schema_indicates_optional(schema: &Value) -> bool {
    // schemars 1.x emits `nullable: true` for Option<T> at field level, or
    // wraps in `anyOf` (handled above). Treat both as optional.
    schema.get("nullable") == Some(&Value::Bool(true))
}

/// Detect schemars-1.x's `rename_all`-style unit enums:
/// `{ oneOf: [{ type: "string", const: "pending" }, ...] }`.
/// Returns the list of string constants if every branch matches; otherwise
/// `None` and the caller falls through to the generic `oneOf` rejection.
fn collect_string_consts(branches: &[Value]) -> Option<Vec<String>> {
    let mut out = Vec::with_capacity(branches.len());
    for branch in branches {
        let is_string = branch
            .get("type")
            .and_then(|v| v.as_str())
            .is_some_and(|t| t == "string");
        if !is_string {
            return None;
        }
        let const_value = branch.get("const")?.as_str()?;
        out.push(const_value.to_owned());
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Detect schemars' externally-tagged enum representation:
/// `{ oneOf: [{type:"object", required:["VariantName"], properties:{"VariantName": payload}}, ...] }`.
/// Returns a [`OneofDef`] when every branch matches the pattern; `Ok(None)`
/// otherwise (caller falls back to other oneOf shortcuts or rejects).
fn collect_externally_tagged_oneof(
    schema_name: &str,
    branches: &[Value],
    extra_enums: &mut BTreeMap<String, EnumDef>,
    queue: &mut Vec<String>,
    lock: &mut ProtoLockfile,
) -> Result<Option<OneofDef>, ProtoGenError> {
    if branches.is_empty() {
        return Ok(None);
    }
    let mut variants: Vec<OneofVariant> = Vec::with_capacity(branches.len());
    for branch in branches {
        let is_object = branch
            .get("type")
            .and_then(|v| v.as_str())
            .is_some_and(|t| t == "object");
        if !is_object {
            return Ok(None);
        }
        let required = branch
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(std::borrow::ToOwned::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if required.len() != 1 {
            return Ok(None);
        }
        let Some(key) = required.into_iter().next() else {
            return Ok(None);
        };
        let properties = branch.get("properties").and_then(|v| v.as_object());
        let Some(properties) = properties else {
            return Ok(None);
        };
        if properties.len() != 1 {
            return Ok(None);
        }
        let Some(payload_schema) = properties.get(&key) else {
            return Ok(None);
        };
        let shape = json_schema_to_proto_field(
            schema_name,
            &key,
            payload_schema,
            false,
            queue,
            extra_enums,
        )?;
        if shape.repeated || shape.is_map || shape.optional {
            return Err(ProtoGenError::UnsupportedSchemaFeature {
                schema_name: format!("{schema_name}.{key}"),
                feature: ProtoGenFeature::OneofVariantPayloadInvalid,
            });
        }
        let snake_variant_name = key.to_snake_case();
        let field_number = lock
            .messages
            .entry(schema_name.to_owned())
            .or_default()
            .assign(&snake_variant_name);
        variants.push(OneofVariant {
            name: key,
            proto_type: shape.proto_type,
            field_number,
        });
    }
    Ok(Some(OneofDef {
        label: "variant".to_owned(),
        variants,
    }))
}

fn synthesize_enum_name(parent_schema: &str, field_name: &str) -> String {
    format!(
        "{}{}",
        parent_schema.to_upper_camel_case(),
        field_name.to_upper_camel_case()
    )
}

fn ref_to_name(parent_schema: &str, ref_str: &str) -> Result<String, ProtoGenError> {
    // Accept "#/$defs/Name" or "#/definitions/Name" or "#/components/schemas/Name".
    for prefix in &["#/$defs/", "#/definitions/", "#/components/schemas/"] {
        if let Some(rest) = ref_str.strip_prefix(prefix) {
            return Ok(rest.to_owned());
        }
    }
    Err(ProtoGenError::UnknownTypeReference {
        schema_name: parent_schema.to_owned(),
        ref_path: ref_str.to_owned(),
    })
}

// --- rendering -------------------------------------------------------------

fn render_proto(
    package: &str,
    service: &str,
    contract: &ContractIr,
    methods: &[MethodProtoIo],
    messages: &BTreeMap<String, MessageDef>,
    enums: &BTreeMap<String, EnumDef>,
    enum_numbers: &BTreeMap<String, BTreeMap<String, u32>>,
) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str("// GENERATED by toolkit-contract-protogen \u{2014} DO NOT EDIT BY HAND.\n");
    out.push_str("// Regenerate via the SDK's `gen_grpc_proto` example.\n\n");
    out.push_str("syntax = \"proto3\";\n");
    // write! on a String cannot fail (fmt::Write impl is infallible).
    _ = writeln!(out, "package {package};\n");
    _ = writeln!(
        out,
        "// {service} \u{2014} gear: {gear}, version: {version}",
        gear = contract.gear,
        version = contract.version,
    );
    _ = writeln!(out, "service {service} {{");
    for m in methods {
        let returns = if m.server_streaming {
            format!("stream {}", m.output_type.render_ref())
        } else {
            m.output_type.render_ref()
        };
        let leading_comment = match (m.server_streaming, &m.idempotency) {
            (true, GrpcIdempotency::NoSideEffects) => "  // server-streaming, NO_SIDE_EFFECTS",
            (true, GrpcIdempotency::Idempotent) => "  // server-streaming, IDEMPOTENT",
            (true, _) => "  // server-streaming",
            (false, GrpcIdempotency::NoSideEffects) => "  // NO_SIDE_EFFECTS",
            (false, GrpcIdempotency::Idempotent) => "  // IDEMPOTENT",
            (false, _) => "  // unary",
        };
        _ = writeln!(out, "{leading_comment}");
        _ = writeln!(
            out,
            "  rpc {rpc}({input}) returns ({returns}) {{",
            rpc = m.rpc_name,
            input = m.input_type.render_ref(),
        );
        let level = m.idempotency.proto_variant();
        _ = writeln!(out, "    option idempotency_level = {level};");
        out.push_str("  }\n");
    }
    out.push_str("}\n\n");

    for (name, def) in messages {
        _ = writeln!(out, "message {name} {{");
        for f in &def.fields {
            // `map<...>` fields render verbatim — proto3 disallows `repeated`
            // / `optional` on map fields. Otherwise prepend the modifier.
            let prefix = if f.is_map {
                ""
            } else if f.repeated {
                "repeated "
            } else if f.optional {
                "optional "
            } else {
                ""
            };
            _ = writeln!(
                out,
                "  {prefix}{ty} {name} = {num};",
                ty = f.proto_type,
                name = f.name,
                num = f.field_number,
            );
        }
        if let Some(oneof) = &def.oneof {
            _ = writeln!(out, "  oneof {label} {{", label = oneof.label);
            for v in &oneof.variants {
                _ = writeln!(
                    out,
                    "    {ty} {name} = {num};",
                    ty = v.proto_type,
                    name = v.name.to_snake_case(),
                    num = v.field_number,
                );
            }
            out.push_str("  }\n");
        }
        // Emit `reserved` clauses for tombstoned numbers and names. Proto3
        // requires these on every message that has ever lost a field, so
        // wire compat is preserved across schema mutations.
        if !def.reserved_numbers.is_empty() {
            let nums: Vec<String> = def
                .reserved_numbers
                .iter()
                .map(std::string::ToString::to_string)
                .collect();
            _ = writeln!(out, "  reserved {};", nums.join(", "));
        }
        if !def.reserved_names.is_empty() {
            let names: Vec<String> = def
                .reserved_names
                .iter()
                .map(|n| format!("\"{n}\""))
                .collect();
            _ = writeln!(out, "  reserved {};", names.join(", "));
        }
        out.push_str("}\n\n");
    }

    for (name, def) in enums {
        _ = writeln!(out, "enum {name} {{");
        // Proto3 enums must carry an explicit zero-valued sentinel so the
        // wire-default for absent / newly-added fields can't silently decode
        // as a real meaningful variant. Per Google proto3 style guide the
        // sentinel is named `<ENUM_NAME>_UNSPECIFIED`.
        let sentinel = format!("{}_UNSPECIFIED", name.to_shouty_snake_case());
        _ = writeln!(out, "  {sentinel} = 0;");

        // Pull stable per-variant numbers from the lockfile-projected map.
        // Variants render in number order so the .proto matches conventional
        // hand-written layout.
        let numbers = enum_numbers.get(name);
        #[allow(
            clippy::expect_used,
            reason = "Invariant established earlier in generate_proto_file: every variant in def.variants is passed through enum_lock.assign() and the resulting number is mirrored into enum_numbers before this renderer runs. A missing entry would indicate a bug in the generator pipeline itself, not user input."
        )]
        let mut numbered: Vec<(String, u32)> = def
            .variants
            .iter()
            .map(|v| {
                let shouty = v.to_shouty_snake_case();
                let num = numbers.and_then(|m| m.get(v).copied()).expect(
                    "every variant in EnumDef must have been pre-assigned via the lockfile",
                );
                (shouty, num)
            })
            .collect();
        numbered.sort_by_key(|(_, n)| *n);
        for (shouty, num) in numbered {
            _ = writeln!(out, "  {shouty} = {num};");
        }

        if !def.reserved_numbers.is_empty() {
            let nums: Vec<String> = def
                .reserved_numbers
                .iter()
                .map(std::string::ToString::to_string)
                .collect();
            _ = writeln!(out, "  reserved {};", nums.join(", "));
        }
        if !def.reserved_names.is_empty() {
            let names: Vec<String> = def
                .reserved_names
                .iter()
                .map(|n| format!("\"{}\"", n.to_shouty_snake_case()))
                .collect();
            _ = writeln!(out, "  reserved {};", names.join(", "));
        }
        out.push_str("}\n\n");
    }

    out
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use toolkit_contract::ir::contract::{
        FieldIr, Idempotency, InputShape, MethodIr, MethodKind, ServiceIr, TypeRef,
    };
    use toolkit_contract::ir::grpc::{GrpcBindingIr, GrpcIdempotency, GrpcMethodBindingIr};

    fn sample_contract() -> ContractIr {
        ServiceIr {
            name: "PaymentApi".into(),
            gear: "service-hub-demo".into(),
            version: "v1".into(),
            methods: vec![MethodIr {
                name: "charge".into(),
                kind: MethodKind::Unary,
                input: InputShape {
                    fields: vec![FieldIr {
                        name: "req".into(),
                        ty: TypeRef::Named("ChargeRequest".into()),
                        optional: false,
                        role: toolkit_contract::ir::contract::FieldRole::Wire,
                    }],
                },
                output: TypeRef::Named("ChargeResponse".into()),
                error: None,
                idempotency: Idempotency::NonIdempotentWrite,
                optional: false,
            }],
        }
    }

    fn sample_binding() -> GrpcBindingIr {
        GrpcBindingIr {
            package: "demo.payment.v1".into(),
            service: "PaymentApi".into(),
            methods: vec![GrpcMethodBindingIr {
                method_name: "charge".into(),
                rpc_name: "Charge".into(),
                client_streaming: false,
                server_streaming: false,
                idempotency_level: GrpcIdempotency::NotIdempotent,
                retryable: false,
                optional: false,
            }],
        }
    }

    fn req_schema() -> schemars::Schema {
        let v = serde_json::json!({
            "type": "object",
            "required": ["amount_cents", "currency"],
            "properties": {
                "amount_cents": { "type": "integer", "format": "int64" },
                "currency": { "type": "string" },
                "description": { "type": "string", "nullable": true }
            }
        });
        schemars::Schema::try_from(v).expect("valid schema")
    }

    fn resp_schema() -> schemars::Schema {
        let v = serde_json::json!({
            "type": "object",
            "required": ["payment_id", "status"],
            "properties": {
                "payment_id": { "type": "string", "format": "uuid" },
                "status": { "$ref": "#/$defs/PaymentStatus" }
            }
        });
        schemars::Schema::try_from(v).expect("valid schema")
    }

    fn status_schema() -> schemars::Schema {
        let v = serde_json::json!({
            "type": "string",
            "enum": ["pending", "completed", "failed"]
        });
        schemars::Schema::try_from(v).expect("valid schema")
    }

    #[test]
    fn generates_minimal_proto() {
        let contract = sample_contract();
        let binding = sample_binding();
        let req = req_schema();
        let resp = resp_schema();
        let status = status_schema();
        let proto = generate_proto_file(
            &contract,
            &binding,
            &[
                ("ChargeRequest", req),
                ("ChargeResponse", resp),
                ("PaymentStatus", status),
            ],
            &mut ProtoLockfile::empty(),
        )
        .expect("generated");
        assert!(proto.contains("syntax = \"proto3\";"));
        assert!(proto.contains("package demo.payment.v1;"));
        assert!(proto.contains("service PaymentApi"));
        assert!(proto.contains("rpc Charge(ChargeRequest) returns (ChargeResponse)"));
        assert!(proto.contains("message ChargeRequest"));
        assert!(proto.contains("message ChargeResponse"));
        assert!(proto.contains("enum PaymentStatus"));
        // Synthetic UNSPECIFIED sentinel always at 0; user variants start at 1.
        assert!(proto.contains("PAYMENT_STATUS_UNSPECIFIED = 0"));
        assert!(proto.contains("PENDING = 1"));
        assert!(proto.contains("FAILED = 3"));
    }

    #[test]
    fn synthesizes_request_for_multi_param_methods() {
        let mut contract = sample_contract();
        contract.methods[0].input.fields = vec![
            FieldIr {
                name: "invoice_id".into(),
                ty: TypeRef::Primitive(toolkit_contract::ir::contract::PrimitiveType::String),
                optional: false,
                role: toolkit_contract::ir::contract::FieldRole::Wire,
            },
            FieldIr {
                name: "include_history".into(),
                ty: TypeRef::Primitive(toolkit_contract::ir::contract::PrimitiveType::Bool),
                optional: false,
                role: toolkit_contract::ir::contract::FieldRole::Wire,
            },
        ];
        let proto = generate_proto_file(
            &contract,
            &sample_binding(),
            &[
                ("ChargeResponse", resp_schema()),
                ("PaymentStatus", status_schema()),
            ],
            &mut ProtoLockfile::empty(),
        )
        .expect("generated");
        assert!(proto.contains("message ChargeRequest"));
        assert!(proto.contains("string invoice_id"));
        assert!(proto.contains("bool include_history"));
    }

    #[test]
    fn rejects_oneof() {
        let bad = schemars::Schema::try_from(serde_json::json!({
            "oneOf": [
                { "type": "string" },
                { "type": "integer" }
            ]
        }))
        .unwrap();
        let err = generate_proto_file(
            &sample_contract(),
            &sample_binding(),
            &[
                ("ChargeRequest", bad),
                ("ChargeResponse", resp_schema()),
                ("PaymentStatus", status_schema()),
            ],
            &mut ProtoLockfile::empty(),
        )
        .unwrap_err();
        match err {
            ProtoGenError::UnsupportedSchemaFeature { feature, .. } => {
                assert_eq!(feature, ProtoGenFeature::OneOf);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn additional_properties_at_field_level_emits_map() {
        let req = schemars::Schema::try_from(serde_json::json!({
            "type": "object",
            "required": ["amount_cents", "tags"],
            "properties": {
                "amount_cents": { "type": "integer", "format": "int64" },
                "tags": {
                    "type": "object",
                    "additionalProperties": { "type": "string" }
                }
            }
        }))
        .unwrap();
        let proto = generate_proto_file(
            &sample_contract(),
            &sample_binding(),
            &[
                ("ChargeRequest", req),
                ("ChargeResponse", resp_schema()),
                ("PaymentStatus", status_schema()),
            ],
            &mut ProtoLockfile::empty(),
        )
        .expect("generated");
        assert!(proto.contains("map<string, string> tags"));
        // Map fields must NOT carry `repeated`/`optional` modifiers.
        assert!(!proto.contains("repeated map<"));
        assert!(!proto.contains("optional map<"));
    }

    #[test]
    fn top_level_additional_properties_emits_wrapper_message() {
        let map_top = schemars::Schema::try_from(serde_json::json!({
            "type": "object",
            "additionalProperties": { "type": "integer", "format": "int32" }
        }))
        .unwrap();
        let resp = schemars::Schema::try_from(serde_json::json!({
            "type": "object",
            "required": ["counts"],
            "properties": {
                "counts": { "$ref": "#/$defs/CountMap" }
            }
        }))
        .unwrap();
        let proto = generate_proto_file(
            &sample_contract(),
            &sample_binding(),
            &[
                ("ChargeRequest", req_schema()),
                ("ChargeResponse", resp),
                ("CountMap", map_top),
            ],
            &mut ProtoLockfile::empty(),
        )
        .expect("generated");
        assert!(proto.contains("message CountMap"));
        assert!(proto.contains("map<string, int32> entries = 1"));
    }

    #[test]
    fn rejects_object_combining_properties_and_additional_properties() {
        let bad = schemars::Schema::try_from(serde_json::json!({
            "type": "object",
            "required": ["amount_cents"],
            "properties": {
                "amount_cents": { "type": "integer", "format": "int64" }
            },
            "additionalProperties": { "type": "string" }
        }))
        .unwrap();
        let err = generate_proto_file(
            &sample_contract(),
            &sample_binding(),
            &[
                ("ChargeRequest", bad),
                ("ChargeResponse", resp_schema()),
                ("PaymentStatus", status_schema()),
            ],
            &mut ProtoLockfile::empty(),
        )
        .unwrap_err();
        match err {
            ProtoGenError::UnsupportedSchemaFeature { feature, .. } => {
                assert_eq!(feature, ProtoGenFeature::PropertiesAndAdditionalProperties);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn one_of_of_objects_emits_proto_oneof() {
        // Externally-tagged `enum E { Charge(ChargeRequest), Refund(RefundRequest) }`
        let action = schemars::Schema::try_from(serde_json::json!({
            "oneOf": [
                {
                    "type": "object",
                    "required": ["Charge"],
                    "properties": { "Charge": { "$ref": "#/$defs/ChargeRequest" } }
                },
                {
                    "type": "object",
                    "required": ["Refund"],
                    "properties": { "Refund": { "$ref": "#/$defs/RefundRequest" } }
                }
            ]
        }))
        .unwrap();
        let refund = schemars::Schema::try_from(serde_json::json!({
            "type": "object",
            "required": ["amount_cents"],
            "properties": {
                "amount_cents": { "type": "integer", "format": "int64" }
            }
        }))
        .unwrap();
        let resp = schemars::Schema::try_from(serde_json::json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": { "$ref": "#/$defs/PaymentAction" }
            }
        }))
        .unwrap();
        let proto = generate_proto_file(
            &sample_contract(),
            &sample_binding(),
            &[
                ("ChargeRequest", req_schema()),
                ("ChargeResponse", resp),
                ("PaymentAction", action),
                ("RefundRequest", refund),
            ],
            &mut ProtoLockfile::empty(),
        )
        .expect("generated");
        assert!(proto.contains("message PaymentAction"));
        assert!(proto.contains("oneof variant"));
        assert!(proto.contains("ChargeRequest charge = 1"));
        assert!(proto.contains("RefundRequest refund = 2"));
    }

    #[test]
    fn inline_string_enum_is_synthesized_into_named_enum() {
        let req = schemars::Schema::try_from(serde_json::json!({
            "type": "object",
            "required": ["amount_cents", "currency"],
            "properties": {
                "amount_cents": { "type": "integer", "format": "int64" },
                "currency": { "type": "string", "enum": ["usd", "eur", "gbp"] }
            }
        }))
        .unwrap();
        let proto = generate_proto_file(
            &sample_contract(),
            &sample_binding(),
            &[
                ("ChargeRequest", req),
                ("ChargeResponse", resp_schema()),
                ("PaymentStatus", status_schema()),
            ],
            &mut ProtoLockfile::empty(),
        )
        .expect("generated");
        // The inline enum should be promoted to a top-level proto enum named
        // `<Parent><Field>` and the field references it.
        assert!(proto.contains("enum ChargeRequestCurrency"));
        assert!(proto.contains("CHARGE_REQUEST_CURRENCY_UNSPECIFIED = 0"));
        assert!(proto.contains("USD = 1"));
        assert!(proto.contains("ChargeRequestCurrency currency"));
    }

    #[test]
    fn format_byte_emits_bytes() {
        let req = schemars::Schema::try_from(serde_json::json!({
            "type": "object",
            "required": ["payload"],
            "properties": {
                "payload": { "type": "string", "format": "byte" }
            }
        }))
        .unwrap();
        let proto = generate_proto_file(
            &sample_contract(),
            &sample_binding(),
            &[
                ("ChargeRequest", req),
                ("ChargeResponse", resp_schema()),
                ("PaymentStatus", status_schema()),
            ],
            &mut ProtoLockfile::empty(),
        )
        .expect("generated");
        assert!(proto.contains("bytes payload"));
    }

    #[test]
    fn rejects_all_of() {
        let bad = schemars::Schema::try_from(serde_json::json!({
            "allOf": [
                { "$ref": "#/$defs/ChargeResponse" },
                { "type": "object", "properties": { "extra": { "type": "string" } } }
            ]
        }))
        .unwrap();
        let err = generate_proto_file(
            &sample_contract(),
            &sample_binding(),
            &[
                ("ChargeRequest", bad),
                ("ChargeResponse", resp_schema()),
                ("PaymentStatus", status_schema()),
            ],
            &mut ProtoLockfile::empty(),
        )
        .unwrap_err();
        match err {
            ProtoGenError::UnsupportedSchemaFeature { feature, .. } => {
                assert_eq!(feature, ProtoGenFeature::AllOf);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn unknown_referenced_type_errors() {
        let resp = schemars::Schema::try_from(serde_json::json!({
            "type": "object",
            "required": ["status"],
            "properties": {
                "status": { "$ref": "#/$defs/UnknownEnum" }
            }
        }))
        .unwrap();
        let err = generate_proto_file(
            &sample_contract(),
            &sample_binding(),
            &[("ChargeRequest", req_schema()), ("ChargeResponse", resp)],
            &mut ProtoLockfile::empty(),
        )
        .unwrap_err();
        assert!(matches!(err, ProtoGenError::UnknownTypeReference { .. }));
    }

    /// `method_input_type` must drop `FieldRole::SecurityContext` fields:
    /// they are server-injected and never appear on the proto wire. With one
    /// `Wire` field and one `SecurityContext` field, the single remaining
    /// wire field is reused (the synthesized request fast-path).
    #[test]
    fn method_input_type_filters_security_context() {
        let method = MethodIr {
            name: "charge".into(),
            kind: MethodKind::Unary,
            input: InputShape {
                fields: vec![
                    FieldIr {
                        name: "ctx".into(),
                        ty: TypeRef::Named("SecurityContext".into()),
                        optional: false,
                        role: toolkit_contract::ir::contract::FieldRole::SecurityContext,
                    },
                    FieldIr {
                        name: "req".into(),
                        ty: TypeRef::Named("ChargeRequest".into()),
                        optional: false,
                        role: toolkit_contract::ir::contract::FieldRole::Wire,
                    },
                ],
            },
            output: TypeRef::Named("ChargeResponse".into()),
            error: None,
            idempotency: Idempotency::NonIdempotentWrite,
            optional: false,
        };
        let mut messages: BTreeMap<String, MessageDef> = BTreeMap::new();
        let mut lock = ProtoLockfile::empty();
        let schema_map: BTreeMap<&str, &schemars::Schema> = BTreeMap::new();
        let resolved = method_input_type(&method, &mut messages, &mut lock, &schema_map)
            .expect("input type resolved");
        // Single Wire field of TypeRef::Named → reused directly, no
        // synthesized request type emitted.
        match resolved {
            ProtoTypeName::Message(name) => assert_eq!(name, "ChargeRequest"),
            other => panic!("unexpected ProtoTypeName: {other:?}"),
        }
        assert!(
            !messages.contains_key("ChargeRequest"),
            "no synthesized type expected; existing Named type is reused"
        );
    }

    /// When the input has two wire fields plus a `SecurityContext`, the
    /// `SecurityContext` must not appear in the synthesized request type.
    #[test]
    fn synthesized_request_excludes_security_context() {
        let method = MethodIr {
            name: "search".into(),
            kind: MethodKind::Unary,
            input: InputShape {
                fields: vec![
                    FieldIr {
                        name: "ctx".into(),
                        ty: TypeRef::Named("SecurityContext".into()),
                        optional: false,
                        role: toolkit_contract::ir::contract::FieldRole::SecurityContext,
                    },
                    FieldIr {
                        name: "query".into(),
                        ty: TypeRef::Primitive(PrimitiveType::String),
                        optional: false,
                        role: toolkit_contract::ir::contract::FieldRole::Wire,
                    },
                    FieldIr {
                        name: "limit".into(),
                        ty: TypeRef::Primitive(PrimitiveType::I32),
                        optional: false,
                        role: toolkit_contract::ir::contract::FieldRole::Wire,
                    },
                ],
            },
            output: TypeRef::Named("Hits".into()),
            error: None,
            idempotency: Idempotency::SafeRead,
            optional: false,
        };
        let mut messages: BTreeMap<String, MessageDef> = BTreeMap::new();
        let mut lock = ProtoLockfile::empty();
        let schema_map: BTreeMap<&str, &schemars::Schema> = BTreeMap::new();
        let resolved = method_input_type(&method, &mut messages, &mut lock, &schema_map)
            .expect("input type resolved");
        match resolved {
            ProtoTypeName::Message(name) => assert_eq!(name, "SearchRequest"),
            other => panic!("unexpected ProtoTypeName: {other:?}"),
        }
        let req = messages
            .get("SearchRequest")
            .expect("synthesized SearchRequest message");
        let names: Vec<&str> = req.fields.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"query"));
        assert!(names.contains(&"limit"));
        assert!(!names.contains(&"ctx"));
    }
}
