//! Compiled policy versions and the bounded cache in front of the compiler.
//!
//! Active versions are immutable, so a compiled version is cached by version
//! id and never invalidated; the oldest entry leaves when the cache is full.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use gts::{GtsId, GtsIdPattern};
use parking_lot::Mutex;
use policy_engine_sdk::POLICY_ENTRYPOINT;
use toolkit_macros::domain_model;
use toolkit_policy_evaluation::{
    CompiledDocument, EvaluationBackend, RegoBackend, screen_denylist,
};

use crate::domain::eval::document_applies;
use crate::domain::model::{BundleVersion, DocumentId, VersionId};

/// One compiled document with its applicability.
#[domain_model]
#[derive(Debug, Clone)]
pub struct CompiledDocumentEntry {
    /// The document.
    pub document_id: DocumentId,
    /// Its name.
    pub name: String,
    /// Parsed `resource_types` patterns.
    pub resource_types: Vec<GtsIdPattern>,
    /// Actions it applies to; empty means all.
    pub actions: Vec<String>,
    /// The compiled Rego.
    pub compiled: Arc<dyn CompiledDocument>,
}

/// A version with every document compiled.
#[domain_model]
#[derive(Debug, Clone)]
pub struct CompiledVersion {
    /// The documents, in stored order.
    pub documents: Vec<CompiledDocumentEntry>,
}

impl CompiledVersion {
    /// The documents that apply to `action` on `type_id`.
    pub fn applicable<'a>(
        &'a self,
        type_id: &'a GtsId,
        action: &'a str,
    ) -> impl Iterator<Item = &'a CompiledDocumentEntry> + 'a {
        self.documents
            .iter()
            .filter(move |d| document_applies(&d.resource_types, &d.actions, type_id, action))
    }
}

/// Compiles every document of an active version. Content was validated at
/// activation; the denylist is screened again so stored content can never
/// bypass it.
///
/// # Errors
///
/// A description of the first document that does not compile.
pub fn compile_version(version: &BundleVersion) -> Result<CompiledVersion, String> {
    let backend = RegoBackend::new();
    let documents = version
        .documents
        .iter()
        .map(|d| {
            let compiled = backend
                .compile(&d.name, &d.content, POLICY_ENTRYPOINT)
                .map_err(|e| format!("document `{}`: {e}", d.name))?;
            if !screen_denylist(compiled.as_ref()).is_empty() {
                return Err(format!("document `{}` uses a denylisted builtin", d.name));
            }
            let resource_types = d
                .resource_types
                .iter()
                .map(|p| GtsIdPattern::try_new(p).map_err(|e| format!("pattern `{p}`: {e}")))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CompiledDocumentEntry {
                document_id: d.id,
                name: d.name.clone(),
                resource_types,
                actions: d.actions.clone(),
                compiled,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(CompiledVersion { documents })
}

#[derive(Default)]
struct Entries {
    by_id: HashMap<VersionId, Arc<CompiledVersion>>,
    order: VecDeque<VersionId>,
}

/// Bounded cache of compiled versions, keyed by version id.
#[domain_model]
pub struct CompileCache {
    capacity: usize,
    entries: Mutex<Entries>,
}

impl std::fmt::Debug for CompileCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompileCache")
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

impl CompileCache {
    /// A cache holding at most `capacity` compiled versions.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Mutex::new(Entries::default()),
        }
    }

    /// The cached compilation of `version`, if any.
    #[must_use]
    pub fn get(&self, id: VersionId) -> Option<Arc<CompiledVersion>> {
        self.entries.lock().by_id.get(&id).cloned()
    }

    /// Caches `compiled` for `id`, evicting the oldest entry when full.
    pub fn insert(&self, id: VersionId, compiled: Arc<CompiledVersion>) {
        let mut entries = self.entries.lock();
        if entries.by_id.insert(id, compiled).is_none() {
            entries.order.push_back(id);
        }
        while entries.by_id.len() > self.capacity {
            let Some(oldest) = entries.order.pop_front() else {
                break;
            };
            entries.by_id.remove(&oldest);
        }
    }

    /// Number of cached versions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().by_id.len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
