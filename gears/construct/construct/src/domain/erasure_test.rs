//! The subject's erasure request over an in-memory store, with stub policy
//! services.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use uuid::Uuid;

use crate::domain::deletion::DeletionEvent;
use crate::domain::erasure::ErasureService;
use crate::domain::error::DomainError;
use crate::domain::subject_settings::{SubjectKey, SubjectSettings};
use crate::test_support::{
    AllowResolver, DenyResolver, MemoryDeletionStore, RecordingDeletionEvents, RecordingResolver,
    context_of,
};

struct Fixture {
    key: SubjectKey,
    store: Arc<MemoryDeletionStore>,
    events: Arc<RecordingDeletionEvents>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            key: SubjectKey {
                tenant_id: Uuid::new_v4(),
                subject_id: Uuid::new_v4(),
            },
            store: Arc::default(),
            events: Arc::default(),
        }
    }

    fn service(&self, resolver: Arc<dyn AuthZResolverApi>) -> ErasureService {
        ErasureService::new(
            Arc::clone(&self.store) as _,
            Arc::clone(&self.events) as _,
            PolicyEnforcer::new(resolver),
        )
    }
}

#[tokio::test]
async fn the_subject_marks_its_erasure_which_is_said() {
    let fx = Fixture::new();

    let settings = fx
        .service(Arc::new(AllowResolver))
        .request(
            &context_of(fx.key.subject_id, fx.key.tenant_id),
            fx.key.subject_id,
        )
        .await
        .unwrap();

    assert_eq!(
        settings,
        SubjectSettings {
            personalization_enabled: false,
            erasure_in_progress: true,
        }
    );
    assert!(fx.store.is_marked(fx.key));
    assert_eq!(
        fx.events.taken(),
        [DeletionEvent::ErasureRequested {
            tenant_id: fx.key.tenant_id,
            subject_id: fx.key.subject_id,
        }]
    );
}

#[tokio::test]
async fn a_second_request_keeps_the_erasure_marked_and_is_recorded() {
    let fx = Fixture::new();
    let service = fx.service(Arc::new(AllowResolver));
    let owner = context_of(fx.key.subject_id, fx.key.tenant_id);

    service.request(&owner, fx.key.subject_id).await.unwrap();
    service.request(&owner, fx.key.subject_id).await.unwrap();

    assert!(fx.store.is_marked(fx.key));
    assert_eq!(fx.events.taken().len(), 2);
}

#[tokio::test]
async fn another_subject_is_refused_before_the_platform_is_asked_and_nothing_is_marked() {
    let fx = Fixture::new();
    let resolver = Arc::new(RecordingResolver::default());

    let err = fx
        .service(Arc::clone(&resolver) as _)
        .request(
            &context_of(Uuid::new_v4(), fx.key.tenant_id),
            fx.key.subject_id,
        )
        .await
        .unwrap_err();

    assert!(matches!(err, DomainError::Forbidden(_)), "{err:?}");
    assert!(resolver.asked.lock().unwrap().is_empty());
    assert!(!fx.store.is_marked(fx.key));
    assert!(fx.events.taken().is_empty());
}

#[tokio::test]
async fn the_subject_without_the_permission_is_refused_and_nothing_is_marked() {
    let fx = Fixture::new();

    let err = fx
        .service(Arc::new(DenyResolver))
        .request(
            &context_of(fx.key.subject_id, fx.key.tenant_id),
            fx.key.subject_id,
        )
        .await
        .unwrap_err();

    assert!(matches!(err, DomainError::Forbidden(_)), "{err:?}");
    assert!(!fx.store.is_marked(fx.key));
}

#[tokio::test]
async fn the_subject_is_asked_for_the_erase_permission_on_its_facts() {
    let fx = Fixture::new();
    let resolver = Arc::new(RecordingResolver::default());

    fx.service(Arc::clone(&resolver) as _)
        .request(
            &context_of(fx.key.subject_id, fx.key.tenant_id),
            fx.key.subject_id,
        )
        .await
        .unwrap();

    let asked = resolver.asked.lock().unwrap();
    let [(resource, action, _, owner_tenant)] = asked.as_slice() else {
        panic!("one policy request, got {asked:?}");
    };
    assert_eq!(resource, "gts.cf.construct.subject.fact.v1~");
    assert_eq!(action, "erase");
    assert_eq!(
        owner_tenant.as_deref(),
        Some(fx.key.tenant_id.to_string().as_str())
    );
}

#[tokio::test]
async fn a_settings_row_the_scope_leaves_out_is_refused_and_nothing_is_said() {
    let fx = Fixture::new();
    fx.store.out_of_scope.store(true, Ordering::SeqCst);

    let err = fx
        .service(Arc::new(AllowResolver))
        .request(
            &context_of(fx.key.subject_id, fx.key.tenant_id),
            fx.key.subject_id,
        )
        .await
        .unwrap_err();

    assert!(matches!(err, DomainError::OutOfScope(_)), "{err:?}");
    assert!(fx.events.taken().is_empty());
}
