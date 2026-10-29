//! Meter declarations (DESIGN [12 §4.7]; T-D-84).
//!
//! A meter is declared in types-registry: its fold, canonical unit and the closed set of metadata
//! keys its entries may carry. Nothing publishes a v2 meter declaration yet, so Rating resolves
//! them through the [`MeterDeclarations`] port (`FakeMeterDeclarations` in the tests) and caches
//! what it resolved: declarations are immutable.
//!
//! The reader resolves a page's meters **before** its transaction. An unavailable registry, or one
//! slower than the call timeout, is an error, so the page is not committed and nothing is
//! rejected; only a definitive "not declared" answer rejects an entry.
//!
//! [12 §4.7]: ../../../../docs/DESIGN.md#contract-12-4-7

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;
use crate::domain::ports::Dependency;
use crate::domain::usage::feed::MeterId;

/// The meter's `aggregation_fold`. Only `SUM` is chargeable; entries of another fold are still
/// captured and counted, and evaluation decides (12 §4.7, R-06).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fold {
    /// `SUM`.
    Sum,
    /// `COUNT`.
    Count,
    /// `MAX`.
    Max,
    /// `MIN`.
    Min,
    /// `LATEST`.
    Latest,
}

impl Fold {
    /// The fold a declared name denotes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "SUM" => Some(Self::Sum),
            "COUNT" => Some(Self::Count),
            "MAX" => Some(Self::Max),
            "MIN" => Some(Self::Min),
            "LATEST" => Some(Self::Latest),
            _ => None,
        }
    }
}

/// What Rating reads from a meter's declaration.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeterDeclaration {
    /// `aggregation_fold`.
    pub fold: Fold,
    /// `canonical_unit`.
    pub canonical_unit: String,
    /// The metadata keys the declaration closes `metadata` to.
    pub metadata_keys: BTreeSet<String>,
}

/// The declarations of a page's meters: `None` for a meter the registry answered as not declared.
pub type PageDeclarations = BTreeMap<MeterId, Option<MeterDeclaration>>;

/// The meter declaration read.
#[async_trait]
pub trait MeterDeclarations: Send + Sync {
    /// The declaration of `meter`, or `None` when the registry answers that it is not declared.
    ///
    /// # Errors
    ///
    /// [`DomainError::DependencyUnavailable`] when the registry cannot answer; the caller backs
    /// off and rejects nothing.
    async fn resolve(&self, meter: &MeterId) -> Result<Option<MeterDeclaration>, DomainError>;
}

/// The declarations already resolved, by meter. A "not declared" answer is not cached, so a meter
/// declared later is found on the next page.
#[domain_model]
#[derive(Clone)]
pub struct DeclarationCache {
    registry: Arc<dyn MeterDeclarations>,
    timeout: Duration,
    known: Arc<Mutex<HashMap<MeterId, MeterDeclaration>>>,
}

impl DeclarationCache {
    /// A cache in front of `registry`, each read of which is given up after `timeout`.
    #[must_use]
    pub fn new(registry: Arc<dyn MeterDeclarations>, timeout: Duration) -> Self {
        Self {
            registry,
            timeout,
            known: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The declarations of `meters`, from the cache where it holds them.
    ///
    /// # Errors
    ///
    /// The registry's error for the first meter it could not answer, or
    /// [`DomainError::DependencyUnavailable`] when it did not answer within the timeout.
    pub async fn resolve_page<'a>(
        &self,
        meters: impl IntoIterator<Item = &'a MeterId>,
    ) -> Result<PageDeclarations, DomainError> {
        let mut resolved = PageDeclarations::new();
        for meter in meters {
            if resolved.contains_key(meter) {
                continue;
            }
            let cached = self.cached(meter);
            let declaration = match cached {
                Some(declaration) => Some(declaration),
                None => self.fetch(meter).await?,
            };
            resolved.insert(meter.clone(), declaration);
        }
        Ok(resolved)
    }

    /// The declaration of `meter` read afresh, past the cache, which it then corrects: an operator
    /// retry uses it, so a declaration the cache held wrongly is fixed by the retry (V03).
    ///
    /// # Errors
    ///
    /// The registry's error.
    pub async fn resolve_fresh(
        &self,
        meter: &MeterId,
    ) -> Result<Option<MeterDeclaration>, DomainError> {
        self.forget(meter);
        self.fetch(meter).await
    }

    async fn fetch(&self, meter: &MeterId) -> Result<Option<MeterDeclaration>, DomainError> {
        let declaration = tokio::time::timeout(self.timeout, self.registry.resolve(meter))
            .await
            .map_err(|_elapsed| DomainError::DependencyUnavailable {
                dependency: Dependency::TypesRegistry,
                detail: format!(
                    "the declaration of {meter} took longer than {:?}",
                    self.timeout
                ),
                cause: None,
            })??;
        if let Some(declaration) = &declaration {
            self.known
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(meter.clone(), declaration.clone());
        }
        Ok(declaration)
    }

    fn cached(&self, meter: &MeterId) -> Option<MeterDeclaration> {
        self.known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(meter)
            .cloned()
    }

    fn forget(&self, meter: &MeterId) {
        self.known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(meter);
    }
}

#[cfg(test)]
#[path = "declarations_tests.rs"]
mod tests;
