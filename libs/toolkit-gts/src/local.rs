//! Crate-local GTS declaration collectors.
//!
//! The crate-root macros (`toolkit_gts::{gts_type_schema, gts_instance,
//! gts_instance_raw}`) submit into one process-wide inventory: whoever reads
//! it sees every declaration linked into the process, whichever crate made
//! it. A gear publishing its own declarations to a remote types-registry
//! needs to know which ones are its own, so a crate opts into a collector of
//! its own:
//!
//! ```ignore
//! // At the crate root:
//! toolkit_gts::declare_gts_inventory!();
//!
//! // Anywhere in the crate, instead of the crate-root macros:
//! #[toolkit_gts::local::gts_type_schema(/* same arguments */)]
//! pub struct MySpecV1 { /* ... */ }
//! toolkit_gts::local::gts_instance_raw!({ "id": "...", /* ... */ });
//! ```
//!
//! The `local` macros take the same input as the crate-root ones and submit
//! each declaration twice: into the crate's own collector, read by the
//! generated `gts_declarations()`, and into the process-wide inventory, so
//! readers of the latter — an embedded types-registry — keep seeing it.
//!
//! `declare_gts_inventory!()` defines the collector (`__toolkit_gts_inventory`)
//! and `pub fn gts_declarations() -> Vec<(String, String)>` at the crate root:
//! the crate's declarations as `(GTS id, JSON text)`, std types only, so no
//! `toolkit_gts` or `gts` type crosses the crate boundary. A `local` macro in
//! a crate without the declaration fails to compile with
//! "could not find `__toolkit_gts_inventory` in the crate root".
//!
//! A gear merges its own crates' declarations with [`DeclarationMerge`]
//! ([`merge_declarations`]).

use std::collections::BTreeMap;

use gts::GtsId;

pub use toolkit_gts_macros::{
    local_gts_instance as gts_instance, local_gts_instance_raw as gts_instance_raw,
    local_gts_type_schema as gts_type_schema,
};

pub use crate::declare_gts_inventory;

/// Defines this crate's GTS declaration collector and its
/// `pub fn gts_declarations() -> Vec<(String, String)>`; see [`crate::local`].
/// Invoke once, at the crate root.
#[macro_export]
macro_rules! declare_gts_inventory {
    () => {
        #[doc(hidden)]
        pub mod __toolkit_gts_inventory {
            /// A Type Schema declared by this crate.
            pub struct TypeSchema(pub $crate::InventoryTypeSchema);
            /// An Instance declared by this crate.
            pub struct Instance(pub $crate::InventoryInstance);
            $crate::inventory::collect!(TypeSchema);
            $crate::inventory::collect!(Instance);
        }

        /// The GTS Type Schemas and Instances this crate declares, as
        /// `(GTS id, JSON text)` sorted by id.
        #[must_use]
        pub fn gts_declarations() -> ::std::vec::Vec<(::std::string::String, ::std::string::String)>
        {
            $crate::local::__collect(
                $crate::inventory::iter::<__toolkit_gts_inventory::TypeSchema>
                    .into_iter()
                    .map(|entry| &entry.0),
                $crate::inventory::iter::<__toolkit_gts_inventory::Instance>
                    .into_iter()
                    .map(|entry| &entry.0),
            )
        }
    };
}

/// The body of a generated `gts_declarations()`.
#[doc(hidden)]
pub fn __collect<'a>(
    schemas: impl Iterator<Item = &'a crate::InventoryTypeSchema>,
    instances: impl Iterator<Item = &'a crate::InventoryInstance>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = schemas
        .map(|entry| (entry.type_id.to_owned(), (entry.schema_fn)()))
        .chain(instances.map(|entry| {
            (
                entry.instance_id.to_owned(),
                (entry.payload_fn)().to_string(),
            )
        }))
        .collect();
    out.sort();
    out
}

/// One source of unparsed declarations for [`DeclarationMerge::add_source`]: a
/// crate's `gts_declarations()`.
pub struct DeclarationSource {
    /// Names the source in diagnostics, e.g. the crate name.
    pub origin: String,
    /// `(GTS id, JSON text)` pairs.
    pub declarations: Vec<(String, String)>,
}

impl DeclarationSource {
    #[must_use]
    pub fn new(origin: impl Into<String>, declarations: Vec<(String, String)>) -> Self {
        Self {
            origin: origin.into(),
            declarations,
        }
    }
}

/// Merges declarations into one list of `(GTS id, content)` sorted by id: crate
/// collectors' `(GTS id, JSON text)` ([`Self::add_source`]) and typed documents a gear
/// builds at runtime ([`Self::add_documents`]).
///
/// Each id is parsed once, where it enters: a [`GtsId`] keeps its spelling and accepts
/// one spelling per identifier, so ids compare as text. An id declared more than once
/// with the same canonical content (see [`same_canonical_content`]) is kept once.
/// [`Self::finish`] reports every problem together: an invalid id or JSON text, naming
/// its origin, and ids declared with different content, naming both origins.
#[derive(Debug, Default)]
pub struct DeclarationMerge {
    merged: BTreeMap<String, (GtsId, serde_json::Value, String)>,
    problems: Vec<String>,
}

impl DeclarationMerge {
    /// Adds a crate collector's declarations, parsing each id and JSON text.
    pub fn add_source(&mut self, source: DeclarationSource) -> &mut Self {
        for (id, text) in source.declarations {
            let parsed = match GtsId::try_new(&id) {
                Ok(parsed) => parsed,
                Err(e) => {
                    self.problems
                        .push(format!("{id} from {}: invalid GTS id: {e}", source.origin));
                    continue;
                }
            };
            match serde_json::from_str(&text) {
                Ok(content) => self.insert(parsed, content, &source.origin),
                Err(e) => self
                    .problems
                    .push(format!("{id} from {}: invalid JSON: {e}", source.origin)),
            }
        }
        self
    }

    /// Adds documents whose ids are already parsed, e.g. a gear's `entities`.
    pub fn add_documents(
        &mut self,
        origin: &str,
        documents: impl IntoIterator<Item = (GtsId, serde_json::Value)>,
    ) -> &mut Self {
        for (id, content) in documents {
            self.insert(id, content, origin);
        }
        self
    }

    fn insert(&mut self, id: GtsId, content: serde_json::Value, origin: &str) {
        match self.merged.get(id.id()) {
            None => {
                self.merged
                    .insert(id.id().to_owned(), (id, content, origin.to_owned()));
            }
            Some((_, kept, _)) if same_canonical_content(kept, &content) => {}
            Some((_, _, kept_origin)) => self.problems.push(format!(
                "{id} is declared with different content by {kept_origin} and {origin}"
            )),
        }
    }

    /// The merged `(GTS id, content)`, sorted by id.
    ///
    /// # Errors
    /// Lists every invalid id or JSON text and every conflicting id.
    pub fn finish(self) -> anyhow::Result<Vec<(GtsId, serde_json::Value)>> {
        if !self.problems.is_empty() {
            anyhow::bail!(
                "conflicting or invalid GTS declarations:\n- {}",
                self.problems.join("\n- ")
            );
        }
        Ok(self
            .merged
            .into_values()
            .map(|(id, content, _)| (id, content))
            .collect())
    }
}

/// Merges `sources` with a [`DeclarationMerge`].
///
/// # Errors
/// As [`DeclarationMerge::finish`].
pub fn merge_declarations(
    sources: impl IntoIterator<Item = DeclarationSource>,
) -> anyhow::Result<Vec<(GtsId, serde_json::Value)>> {
    let mut merge = DeclarationMerge::default();
    for source in sources {
        merge.add_source(source);
    }
    merge.finish()
}

/// Whether two documents have the same canonical content, as types-registry
/// stores and compares it: object key order aside, array order kept, every
/// number keeps its spelling. `Value` equality would call `0.0` and `-0.0`
/// equal.
#[must_use]
pub fn same_canonical_content(left: &serde_json::Value, right: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (left, right) {
        (Value::Number(l), Value::Number(r)) => l.to_string() == r.to_string(),
        (Value::Array(l), Value::Array(r)) => {
            l.len() == r.len() && l.iter().zip(r).all(|(l, r)| same_canonical_content(l, r))
        }
        (Value::Object(l), Value::Object(r)) => {
            l.len() == r.len()
                && l.iter()
                    .all(|(key, l)| r.get(key).is_some_and(|r| same_canonical_content(l, r)))
        }
        _ => left == right,
    }
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod local_tests;
