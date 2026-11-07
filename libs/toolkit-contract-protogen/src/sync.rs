//! Keep a committed `.proto` in step with its contract, from a test.
//!
//! The `.proto` an SDK commits is generated from the contract IR, the gRPC
//! binding and the JSON schemas of the wire types. [`SchemaCollector`] gathers
//! those schemas through the `<base>_grpc_schemas` visitor that
//! `#[toolkit::grpc_contract]` generates, and [`check_proto`] compares what
//! the contract generates today with the committed files:
//!
//! ```ignore
//! #[test]
//! fn the_committed_proto_is_up_to_date() {
//!     let mut schemas = toolkit_contract_protogen::SchemaCollector::default();
//!     my_sdk::grpc::payment_api_grpc_schemas(&mut schemas);
//!     toolkit_contract_protogen::check_proto(&toolkit_contract_protogen::ProtoSpec {
//!         contract: my_sdk::contract::payment_api_ir(),
//!         binding: my_sdk::grpc::payment_api_grpc_binding(),
//!         schemas,
//!         proto: concat!(env!("CARGO_MANIFEST_DIR"), "/proto/payments/v1/payment.proto").into(),
//!         lock: concat!(env!("CARGO_MANIFEST_DIR"), "/proto.lock.toml").into(),
//!     });
//! }
//! ```
//!
//! On a difference the test fails and names the first differing line. Run it
//! with `TOOLKIT_PROTO_UPDATE=1` to rewrite the `.proto` and the lockfile,
//! then review and commit them like any other change to the wire contract.

use std::path::PathBuf;

use schemars::{JsonSchema, Schema};
use toolkit_contract::ir::contract::ContractIr;
use toolkit_contract::ir::grpc::{GrpcBindingIr, VisitType};

use crate::{ProtoLockfile, generate_proto_file};

/// Environment variable that makes [`check_proto`] rewrite instead of fail.
pub const UPDATE_ENV: &str = "TOOLKIT_PROTO_UPDATE";

/// Named schemas of a projection's wire types, collected through the
/// generated `<base>_grpc_schemas` visitor.
///
/// Each visited type contributes its own schema when it is a message or an
/// enum (primitives and containers contribute none) and every type it
/// references, from the schema's `$defs`, so nested types never have to be
/// listed.
#[derive(Debug, Default)]
pub struct SchemaCollector {
    schemas: Vec<(String, Schema)>,
}

impl SchemaCollector {
    /// The collected `(name, schema)` pairs, in first-seen order.
    #[must_use]
    pub fn schemas(&self) -> &[(String, Schema)] {
        &self.schemas
    }

    fn add(&mut self, name: &str, schema: Schema) {
        if !self.schemas.iter().any(|(n, _)| n == name) {
            self.schemas.push((name.to_owned(), schema));
        }
    }

    fn add_root(&mut self, mut root: Schema) {
        let defs = root
            .as_object_mut()
            .and_then(|o| o.remove("$defs"))
            .and_then(|d| match d {
                serde_json::Value::Object(map) => Some(map),
                _ => None,
            })
            .unwrap_or_default();
        if let Some(obj) = root.as_object_mut() {
            obj.remove("$schema");
        }
        let named = root
            .as_object()
            .and_then(|o| o.get("title"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        if let Some(name) = named
            && is_named_type(&root)
        {
            self.add(&name, root);
        }
        for (name, def) in defs {
            if let Ok(schema) = Schema::try_from(def) {
                self.add(&name, schema);
            }
        }
    }
}

/// A message or an enum: what becomes a named proto type.
fn is_named_type(schema: &Schema) -> bool {
    let Some(obj) = schema.as_object() else {
        return false;
    };
    let ty = obj.get("type").and_then(serde_json::Value::as_str);
    ty == Some("object")
        || obj.contains_key("enum")
        || obj.contains_key("oneOf")
        || (ty == Some("string") && obj.contains_key("enum"))
}

impl<T: JsonSchema + ?Sized> VisitType<T> for SchemaCollector {
    fn visit(&mut self) {
        self.add_root(schemars::schema_for!(T));
    }
}

/// What [`check_proto`] checks: one contract's `.proto` and lockfile.
pub struct ProtoSpec {
    /// The contract IR (`<trait>_ir()`).
    pub contract: ContractIr,
    /// The gRPC binding (`<trait>_grpc_binding()`).
    pub binding: GrpcBindingIr,
    /// The wire-type schemas.
    pub schemas: SchemaCollector,
    /// The committed `.proto`.
    pub proto: PathBuf,
    /// The committed `proto.lock.toml`.
    pub lock: PathBuf,
}

/// Generate the `.proto` and lockfile for `spec` and write them. What an SDK's
/// `gen_grpc_proto` example (run by `cargo xtask proto-regen`) calls.
///
/// # Errors
/// Generation or I/O failures, described.
pub fn write_proto(spec: &ProtoSpec) -> Result<(), String> {
    let (generated, lock, _) = generate(spec)?;
    if let Some(parent) = spec.proto.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&spec.proto, &generated)
        .map_err(|e| format!("write {}: {e}", spec.proto.display()))?;
    lock.save(&spec.lock)
        .map_err(|e| format!("save {}: {e}", spec.lock.display()))
}

/// What the contract generates now, the lockfile after it, and the committed
/// lockfile.
fn generate(spec: &ProtoSpec) -> Result<(String, ProtoLockfile, ProtoLockfile), String> {
    let committed_lock = ProtoLockfile::load(&spec.lock)
        .map_err(|e| format!("load {}: {e}", spec.lock.display()))?;
    let mut lock = committed_lock.clone();
    let schemas: Vec<(&str, Schema)> = spec
        .schemas
        .schemas()
        .iter()
        .map(|(n, s)| (n.as_str(), s.clone()))
        .collect();
    let generated = generate_proto_file(&spec.contract, &spec.binding, &schemas, &mut lock)
        .map_err(|e| format!("generate {}: {e}", spec.proto.display()))?;
    Ok((generated, lock, committed_lock))
}

/// Panic unless the committed `.proto` and lockfile are what the contract
/// generates now; with `TOOLKIT_PROTO_UPDATE=1`, rewrite them instead.
///
/// # Panics
/// On a difference (without the update variable), or when generation or I/O
/// fails.
pub fn check_proto(spec: &ProtoSpec) {
    if std::env::var_os(UPDATE_ENV).is_some_and(|v| v == "1") {
        if let Err(e) = write_proto(spec) {
            panic!("{e}");
        }
        return;
    }
    let (generated, lock, committed_lock) = generate(spec).unwrap_or_else(|e| panic!("{e}"));
    let committed = std::fs::read_to_string(&spec.proto).unwrap_or_default();
    if generated != committed {
        let line = first_difference(&committed, &generated);
        panic!(
            "{} is out of date with its contract (first difference at line {line}).\n\
             Regenerate with `{UPDATE_ENV}=1 cargo test` and review the diff.",
            spec.proto.display(),
        );
    }
    assert!(
        lock == committed_lock,
        "{} is missing field numbers the contract needs; regenerate with `{UPDATE_ENV}=1 cargo test`",
        spec.lock.display(),
    );
}

/// 1-based number of the first line where `committed` and `generated` differ.
fn first_difference(committed: &str, generated: &str) -> usize {
    committed
        .lines()
        .zip(generated.lines())
        .take_while(|(c, g)| c == g)
        .count()
        + 1
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    enum Status {
        Pending,
        Done,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Item {
        id: String,
        status: Status,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Page {
        items: Vec<Item>,
    }

    fn names(c: &SchemaCollector) -> Vec<&str> {
        c.schemas().iter().map(|(n, _)| n.as_str()).collect()
    }

    #[test]
    fn nested_types_are_collected_once() {
        let mut c = SchemaCollector::default();
        <SchemaCollector as VisitType<Page>>::visit(&mut c);
        <SchemaCollector as VisitType<Item>>::visit(&mut c);
        assert_eq!(names(&c), ["Page", "Item", "Status"]);
    }

    #[test]
    fn primitives_and_containers_add_only_what_they_reference() {
        let mut c = SchemaCollector::default();
        <SchemaCollector as VisitType<String>>::visit(&mut c);
        <SchemaCollector as VisitType<Vec<Item>>>::visit(&mut c);
        assert_eq!(names(&c), ["Item", "Status"]);
    }

    #[test]
    fn first_difference_names_the_line() {
        assert_eq!(first_difference("a\nb\nc", "a\nx\nc"), 2);
        assert_eq!(first_difference("a\nb", "a\nb\nc"), 3);
    }
}
