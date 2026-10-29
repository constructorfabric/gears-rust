#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use super::{DeclarationCache, Fold, MeterDeclaration, MeterDeclarations};
use crate::domain::error::DomainError;
use crate::domain::ports::Dependency;
use crate::domain::usage::feed::MeterId;

const TIMEOUT: Duration = Duration::from_secs(5);

struct Registry {
    declaration: Mutex<Option<MeterDeclaration>>,
    down: AtomicBool,
    reads: AtomicUsize,
}

#[async_trait]
impl MeterDeclarations for Registry {
    async fn resolve(&self, _meter: &MeterId) -> Result<Option<MeterDeclaration>, DomainError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.down.load(Ordering::SeqCst) {
            return Err(DomainError::DependencyUnavailable {
                dependency: Dependency::TypesRegistry,
                detail: "down".to_owned(),
                cause: None,
            });
        }
        Ok(self.declaration.lock().unwrap().clone())
    }
}

fn declaration(keys: &[&str]) -> MeterDeclaration {
    MeterDeclaration {
        fold: Fold::Sum,
        canonical_unit: "h".to_owned(),
        metadata_keys: keys.iter().map(|k| (*k).to_owned()).collect(),
    }
}

fn registry(declared: Option<MeterDeclaration>) -> Arc<Registry> {
    Arc::new(Registry {
        declaration: Mutex::new(declared),
        down: AtomicBool::new(false),
        reads: AtomicUsize::new(0),
    })
}

fn meter() -> MeterId {
    MeterId::parse("gts.cf.core.uc.usage_record.v2~cf.test.hours.v1~").unwrap()
}

#[test]
fn every_declared_fold_parses() {
    for (name, fold) in [
        ("SUM", Fold::Sum),
        ("COUNT", Fold::Count),
        ("MAX", Fold::Max),
        ("MIN", Fold::Min),
        ("LATEST", Fold::Latest),
    ] {
        assert_eq!(Fold::parse(name), Some(fold));
    }
    assert_eq!(Fold::parse("sum"), None);
}

#[tokio::test]
async fn a_declaration_is_read_once_and_cached() {
    let registry = registry(Some(declaration(&[])));
    let cache = DeclarationCache::new(Arc::clone(&registry) as Arc<dyn MeterDeclarations>, TIMEOUT);
    let meters = [meter(), meter()];
    for _ in 0..3 {
        let page = cache.resolve_page(meters.iter()).await.unwrap();
        assert_eq!(page.get(&meter()), Some(&Some(declaration(&[]))));
    }
    assert_eq!(registry.reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_not_declared_answer_is_asked_again_on_the_next_page() {
    let registry = registry(None);
    let cache = DeclarationCache::new(Arc::clone(&registry) as Arc<dyn MeterDeclarations>, TIMEOUT);
    assert_eq!(
        cache
            .resolve_page([meter()].iter())
            .await
            .unwrap()
            .get(&meter()),
        Some(&None)
    );
    *registry.declaration.lock().unwrap() = Some(declaration(&[]));
    assert_eq!(
        cache
            .resolve_page([meter()].iter())
            .await
            .unwrap()
            .get(&meter()),
        Some(&Some(declaration(&[])))
    );
}

#[tokio::test]
async fn an_unavailable_registry_is_an_error_not_a_rejection() {
    let registry = registry(Some(declaration(&[])));
    registry.down.store(true, Ordering::SeqCst);
    let cache = DeclarationCache::new(Arc::clone(&registry) as Arc<dyn MeterDeclarations>, TIMEOUT);
    let error = cache.resolve_page([meter()].iter()).await.unwrap_err();
    assert!(matches!(error, DomainError::DependencyUnavailable { .. }));
}

#[tokio::test]
async fn a_fresh_read_corrects_a_declaration_the_cache_held_wrongly() {
    let registry = registry(Some(declaration(&[])));
    let cache = DeclarationCache::new(Arc::clone(&registry) as Arc<dyn MeterDeclarations>, TIMEOUT);
    cache.resolve_page([meter()].iter()).await.unwrap();
    *registry.declaration.lock().unwrap() = Some(declaration(&["region"]));
    // The cache still answers what it held ...
    assert_eq!(
        cache
            .resolve_page([meter()].iter())
            .await
            .unwrap()
            .get(&meter()),
        Some(&Some(declaration(&[])))
    );
    // ... until a retry reads the meter afresh, which also corrects the cache.
    assert_eq!(
        cache.resolve_fresh(&meter()).await.unwrap(),
        Some(declaration(&["region"]))
    );
    assert_eq!(
        cache
            .resolve_page([meter()].iter())
            .await
            .unwrap()
            .get(&meter()),
        Some(&Some(declaration(&["region"])))
    );
}

/// A registry that accepts the call and never answers.
struct Stalled;

#[async_trait]
impl MeterDeclarations for Stalled {
    async fn resolve(&self, _meter: &MeterId) -> Result<Option<MeterDeclaration>, DomainError> {
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn a_registry_that_never_answers_is_given_up_as_unavailable() {
    let cache = DeclarationCache::new(Arc::new(Stalled), Duration::from_secs(2));
    let started = tokio::time::Instant::now();
    let error = cache.resolve_page([&meter()]).await.unwrap_err();
    assert!(
        matches!(
            error,
            DomainError::DependencyUnavailable {
                dependency: Dependency::TypesRegistry,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(started.elapsed(), Duration::from_secs(2));
    assert!(
        cache.resolve_fresh(&meter()).await.is_err(),
        "the retry path too"
    );
}
