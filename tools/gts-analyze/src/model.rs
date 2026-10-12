//! Data structures: types/instances/references found in a module, plus the aggregate Report.

use serde::Serialize;
use std::collections::BTreeMap;

/// A GTS Type definition found in the module (Rust macro, JSON schema, or `struct_to_gts_schema!`).
#[derive(Serialize, Debug, Clone)]
pub struct TypeDef {
    /// The GTS type identifier.
    pub gts_id: String,
    /// Path of the defining file, relative to the module root.
    pub file: String,
    /// 1-based line number of the definition.
    pub line: usize,
    /// How the definition was found (for example `rust_macro` or `json_schema`).
    pub source_kind: &'static str,
    /// Location class of the file (`sdk`, `main`, `plugin`, `doc` or `other`).
    pub location: String,
    /// Name of the annotated Rust struct, when applicable.
    pub struct_name: Option<String>,
    /// Base type identifier the definition derives from, if any.
    pub base: Option<String>,
    /// `dir_path` macro argument, if present.
    pub dir_path: Option<String>,
    /// `properties` macro argument, if present.
    pub properties: Option<String>,
    /// Description from the macro argument or schema, if present.
    pub description: Option<String>,
}

/// A GTS Instance declared with `gts_instance!` or `gts_instance_raw!`.
#[derive(Serialize, Debug, Clone)]
pub struct InstanceDef {
    /// The GTS instance identifier.
    pub gts_id: String,
    /// Path of the declaring file, relative to the module root.
    pub file: String,
    /// 1-based line number of the declaration.
    pub line: usize,
    /// Declaring macro kind (`rust_macro` or `rust_macro_raw`).
    pub source_kind: &'static str,
    /// Location class of the file (`sdk`, `main`, `plugin`, `doc` or `other`).
    pub location: String,
    /// Rust type the instance is declared as, when the macro names one.
    pub typed_as: Option<String>,
}

/// Any GTS-shaped string literal found in source / docs.
#[derive(Serialize, Debug, Clone)]
pub struct Reference {
    /// The GTS identifier found in the literal.
    pub gts_id: String,
    /// Path of the file, relative to the module root.
    pub file: String,
    /// 1-based line number of the literal.
    pub line: usize,
    /// Location class of the file (`sdk`, `main`, `plugin`, `doc` or `other`).
    pub location: String,
    /// Source line containing the literal, shortened for display.
    pub context: String,
}

/// Aggregate scan result. `verbose` controls rendering only; not part of JSON payload.
#[derive(Serialize, Debug, Default)]
pub struct Report {
    /// Root directory of the scanned module.
    pub module_root: String,
    /// Whether test files were included in the scan.
    pub include_tests: bool,
    /// Whether Markdown and `docs/` files were skipped.
    pub skip_docs: bool,
    /// Whether to expand reference listings per line when rendering.
    #[serde(skip_serializing)]
    pub verbose: bool,
    /// Number of scanned files per extension.
    pub file_counts: BTreeMap<String, usize>,
    /// Number of scanned files per location class.
    pub location_counts: BTreeMap<String, usize>,
    /// Type definitions found.
    pub types: Vec<TypeDef>,
    /// Instance declarations found.
    pub instances: Vec<InstanceDef>,
    /// Other GTS-shaped references found.
    pub references: Vec<Reference>,
}
