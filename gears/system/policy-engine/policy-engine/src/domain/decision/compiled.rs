//! Compiled policy versions and the bounded cache in front of the compiler.
//!
//! Active versions are immutable, so a compiled version is cached by version
//! id and never invalidated; the least recently used entry leaves when the
//! cache is full. Concurrent misses of one version compile it once.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

use gts::{GtsId, GtsIdPattern};
use lru::LruCache;
use parking_lot::Mutex;
use policy_engine_sdk::POLICY_ENTRYPOINT;
use toolkit_macros::domain_model;
use toolkit_policy_evaluation::{
    CompiledDocument, RegoBackend, ScreenedCompileError, compile_screened,
};

use crate::domain::eval::document_applies;
use crate::domain::model::{BundleVersion, Document, DocumentId, VersionId};

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

/// Why a document could not be compiled for evaluation.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocumentCompileError {
    /// The content does not compile or references a denylisted builtin.
    #[error("document `{document}`: {error}")]
    Content {
        /// The document's name.
        document: String,
        /// What the backend or the denylist screen rejected.
        error: ScreenedCompileError,
    },
    /// A `resource_types` entry is not a GTS type pattern.
    #[error("document `{document}`: resource type pattern `{pattern}`: {message}")]
    Pattern {
        /// The document's name.
        document: String,
        /// The rejected pattern.
        pattern: String,
        /// Why it was rejected.
        message: String,
    },
}

/// Compiles every document of an active version. Content was validated at
/// activation; the denylist is screened again so stored content can never
/// bypass it.
///
/// # Errors
///
/// [`DocumentCompileError`] for the first document that does not compile.
pub fn compile_version(version: &BundleVersion) -> Result<CompiledVersion, DocumentCompileError> {
    compile_content(&version.documents)
}

/// Compiles the documents of an active version, as [`compile_version`].
///
/// # Errors
///
/// [`DocumentCompileError`] for the first document that does not compile.
pub fn compile_content(documents: &[Document]) -> Result<CompiledVersion, DocumentCompileError> {
    Ok(CompiledVersion {
        documents: compile_documents(documents)?,
    })
}

/// Compiles `documents`, screening each against the denylist.
///
/// # Errors
///
/// [`DocumentCompileError`] for the first document that does not compile.
pub fn compile_documents(
    documents: &[Document],
) -> Result<Vec<CompiledDocumentEntry>, DocumentCompileError> {
    let backend = RegoBackend::new();
    documents
        .iter()
        .map(|d| {
            let compiled = compile_screened(&backend, &d.name, &d.content, POLICY_ENTRYPOINT)
                .map_err(|error| DocumentCompileError::Content {
                    document: d.name.clone(),
                    error,
                })?;
            let resource_types = d
                .resource_types
                .iter()
                .map(|pattern| {
                    GtsIdPattern::try_new(pattern).map_err(|e| DocumentCompileError::Pattern {
                        document: d.name.clone(),
                        pattern: pattern.clone(),
                        message: e.to_string(),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CompiledDocumentEntry {
                document_id: d.id,
                name: d.name.clone(),
                resource_types,
                actions: d.actions.clone(),
                compiled,
            })
        })
        .collect()
}

/// Bounded cache of compiled versions, keyed by version id; the least
/// recently used version leaves when it is full.
#[domain_model]
pub struct CompileCache {
    entries: Mutex<LruCache<VersionId, Arc<CompiledVersion>>>,
    /// One gate per version being compiled, so concurrent misses of the same
    /// version wait for one compilation instead of repeating it.
    in_flight: Mutex<HashMap<VersionId, Arc<Mutex<()>>>>,
}

impl std::fmt::Debug for CompileCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompileCache")
            .field("capacity", &self.entries.lock().cap())
            .finish_non_exhaustive()
    }
}

impl CompileCache {
    /// A cache holding at most `capacity` compiled versions (at least one).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::MIN);
        Self {
            entries: Mutex::new(LruCache::new(capacity)),
            in_flight: Mutex::new(HashMap::new()),
        }
    }

    /// The cached compilation of `version`, if any.
    #[must_use]
    pub fn get(&self, id: VersionId) -> Option<Arc<CompiledVersion>> {
        self.entries.lock().get(&id).cloned()
    }

    /// The cached compilation of `id`, compiled with `compile` and cached on
    /// a miss. Concurrent callers missing the same version wait for one
    /// compilation and share it; a failed compilation is not cached, so a
    /// waiter tries again. Blocks while another caller compiles `id`: call it
    /// from a blocking task.
    ///
    /// # Errors
    ///
    /// What `compile` returns.
    pub fn get_or_compile<E>(
        &self,
        id: VersionId,
        compile: impl FnOnce() -> Result<CompiledVersion, E>,
    ) -> Result<Arc<CompiledVersion>, E> {
        if let Some(hit) = self.get(id) {
            return Ok(hit);
        }
        let gate = Arc::clone(self.in_flight.lock().entry(id).or_default());
        let result = {
            let _turn = gate.lock();
            match self.get(id) {
                Some(hit) => Ok(hit),
                None => compile().map(|compiled| {
                    let compiled = Arc::new(compiled);
                    self.insert(id, Arc::clone(&compiled));
                    compiled
                }),
            }
        };
        // The last caller through the gate removes it; one still waiting
        // holds a clone and removes it in turn.
        let mut in_flight = self.in_flight.lock();
        if in_flight
            .get(&id)
            .is_some_and(|g| Arc::ptr_eq(g, &gate) && Arc::strong_count(g) == 2)
        {
            in_flight.remove(&id);
        }
        result
    }

    /// Caches `compiled` for `id`, evicting the least recently used entry
    /// when full.
    pub fn insert(&self, id: VersionId, compiled: Arc<CompiledVersion>) {
        self.entries.lock().put(id, compiled);
    }

    /// Number of cached versions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
