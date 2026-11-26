use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use quota_enforcement_sdk::testing::InMemoryStorage;

use super::Service;
use crate::domain::admission::Admission;
use crate::domain::bootstrap::Bound;
use crate::domain::error::{Dependency, DomainError};
use crate::domain::ports::metrics::NoopMetrics;
use crate::domain::quotas::QuotaLimits;
use crate::domain::readiness::Readiness;
use crate::test_support::{NoopCoordinator, PermitTenantsPdp, caller, tenant, test_catalog};

fn limits() -> QuotaLimits {
    QuotaLimits {
        metadata_max_bytes: 4096,
        list_max_limit: 500,
        list_max_ids: 100,
        bulk_max_items: 50,
    }
}

fn service() -> Service {
    let enforcer = PolicyEnforcer::new(Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()])));
    Service::new(
        Admission::new(enforcer, Arc::new(NoopMetrics)),
        Arc::new(Readiness::new()),
        limits(),
        crate::test_support::policy_limits(),
        crate::domain::service::OperationsRuntime {
            cache_entries: 16,
            cache_ttl: std::time::Duration::from_secs(5),
            preparation_max_attempts: std::num::NonZeroU32::new(3).expect("attempts"),
            leases: crate::domain::operations::LeaseLimits::default(),
            batch: crate::domain::operations::BatchLimits::default(),
            snapshot: crate::domain::operations::SnapshotLimits::default(),
        },
    )
}

#[test]
fn dependencies_are_not_ready_until_bound_and_bind_happens_once() {
    let svc = service();
    assert_eq!(
        svc.storage().err(),
        Some(DomainError::NotReady {
            dependency: Dependency::Storage
        })
    );
    assert_eq!(
        svc.coordinator().err(),
        Some(DomainError::NotReady {
            dependency: Dependency::Cluster
        })
    );
    assert_eq!(
        svc.catalog().err(),
        Some(DomainError::NotReady {
            dependency: Dependency::Catalog
        })
    );
    assert!(
        svc.operations(caller()).is_err(),
        "no storage, no operations"
    );
    assert_eq!(
        svc.quotas(caller()).err(),
        Some(DomainError::NotReady {
            dependency: Dependency::Storage
        }),
        "no storage, no lifecycle"
    );

    let bound = Bound {
        engines: Arc::new(crate::domain::engines::linked_registry().expect("engines")),
        artifacts: Arc::new(crate::domain::engines::PolicyArtifactCache::new(
            std::num::NonZeroUsize::new(256).expect("capacity"),
            std::num::NonZeroUsize::new(2).expect("permits"),
        )),
        storage: Arc::new(InMemoryStorage::new()),
        coordinator: Arc::new(NoopCoordinator),
        catalog: test_catalog().0,
    };
    svc.bind(bound.clone()).expect("first bind");
    assert!(svc.storage().is_ok());
    assert!(svc.coordinator().is_ok());
    assert!(svc.catalog().is_ok());
    assert!(svc.operations(caller()).is_ok());
    assert_eq!(svc.quotas(caller()).expect("lifecycle").limits(), limits());
    assert!(matches!(svc.bind(bound), Err(DomainError::Internal(_))));
    assert!(
        !svc.readiness().is_ready(),
        "binding does not imply readiness; bootstrap marks it"
    );
}

#[tokio::test]
async fn policy_management_over_trusted_admission_is_refused_before_storage() {
    let storage = crate::test_support::mrw_storage().await;
    let svc = crate::test_support::bound_service_over(
        Arc::new(crate::test_support::UncallablePdp),
        Arc::clone(&storage),
    )
    .await;
    let audit = storage.policy_audit().len();
    let policies = svc
        .policies_over(svc.admission().trusted(None))
        .expect("bound");
    let global = quota_enforcement_sdk::PolicyId::global();

    let refusals = [
        policies
            .create(quota_enforcement_sdk::PolicySpec {
                scope: quota_enforcement_sdk::PolicyScope::Global,
                engine_id: "most-restrictive-wins".to_owned(),
                engine_config: serde_json::json!({}),
                timeout_ms: None,
                description: None,
                comment: None,
            })
            .await
            .err(),
        policies.rollback(global.clone(), 1, None).await.err(),
        policies.delete(global.clone(), None).await.err(),
        policies.read(&global, None).await.err(),
    ];

    for refusal in refusals {
        assert!(
            matches!(refusal, Some(DomainError::PdpDenied { .. })),
            "{refusal:?}"
        );
    }
    assert_eq!(storage.policy_audit().len(), audit, "nothing was written");
}
