use std::sync::Arc;

use quota_enforcement_sdk::testing::InMemoryStorage;
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::testing_platform::{Call, Fault};

use super::PluginBinding;
use crate::domain::error::{DomainError, PluginKind};
use crate::test_support::{PluginFixture, hub_with, register_storage, storage_instance};

fn binding(hub: Arc<ClientHub>) -> PluginBinding {
    PluginBinding::new(hub, "acme".to_owned())
}

#[tokio::test]
async fn resolves_the_lowest_priority_instance_of_the_configured_vendor() {
    let storage_hi = storage_instance("cf.core._.qe_hi.v1", "acme", 50);
    let storage_lo = storage_instance("cf.core._.qe_lo.v1", "acme", 10);
    let other_vendor = storage_instance("cf.core._.qe_other.v1", "globex", 1);
    let hub = hub_with(&[&storage_hi, &storage_lo, &other_vendor]);
    let lo = Arc::new(InMemoryStorage::new());
    register_storage(&hub, &storage_hi, Arc::new(InMemoryStorage::new()));
    register_storage(&hub, &storage_lo, lo.clone());

    let binding = binding(hub);
    let storage = binding.resolve_storage().await.expect("storage resolved");
    storage
        .bootstrap(&quota_enforcement_sdk::BootstrapBundle::foundation())
        .await
        .expect("the resolved handle is the lowest-priority instance");
    assert_eq!(
        lo.bootstrap_calls(),
        1,
        "the priority-10 instance was chosen"
    );
}

#[tokio::test]
async fn a_vendor_without_instances_is_plugin_not_found() {
    let other_vendor = storage_instance("cf.core._.qe_other.v1", "globex", 1);
    let hub = hub_with(&[&other_vendor]);
    let err = binding(hub)
        .resolve_storage()
        .await
        .err()
        .expect("no acme instance");
    assert_eq!(
        err,
        DomainError::PluginNotFound {
            kind: PluginKind::Storage,
            vendor: "acme".to_owned(),
        }
    );
}

/// An instance without a scoped client in this process — a stale instance
/// of an earlier deployment — is never selected, whatever its priority.
#[tokio::test]
async fn an_unbound_instance_is_never_selected() {
    let stale = storage_instance("cf.core._.qe_stale.v1", "acme", 1);
    let bound = storage_instance("cf.core._.qe_db_storage.v1", "acme", 100);
    let hub = hub_with(&[&stale, &bound]);
    let storage = Arc::new(InMemoryStorage::new());
    register_storage(&hub, &bound, storage.clone());

    let resolved = binding(Arc::clone(&hub))
        .resolve_storage()
        .await
        .expect("the bound instance is selected");
    resolved
        .bootstrap(&quota_enforcement_sdk::BootstrapBundle::foundation())
        .await
        .expect("bootstrap");
    assert_eq!(
        storage.bootstrap_calls(),
        1,
        "the bound priority-100 instance won"
    );

    let only_stale = hub_with(&[&stale]);
    let err = binding(only_stale)
        .resolve_storage()
        .await
        .err()
        .expect("nothing bound");
    assert_eq!(
        err,
        DomainError::PluginNotFound {
            kind: PluginKind::Storage,
            vendor: "acme".to_owned(),
        }
    );
    assert_eq!(
        err.to_string(),
        "no bound storage plugin instance for vendor acme",
        "an advertised instance exists; none is bound"
    );
}

#[tokio::test]
async fn a_malformed_bound_instance_is_reported_with_its_id() {
    let broken = PluginFixture::malformed_storage("cf.core._.qe_broken.v1");
    let hub = hub_with(&[&broken]);
    register_storage(&hub, &broken, Arc::new(InMemoryStorage::new()));
    let err = binding(hub)
        .resolve_storage()
        .await
        .err()
        .expect("malformed");
    assert!(
        matches!(
            &err,
            DomainError::InvalidPluginInstance { kind: PluginKind::Storage, gts_id, .. }
                if gts_id == &broken.instance_id
        ),
        "{err:?}"
    );
}

/// Every page is read: the bound instance on a later page is selected, and a
/// failed later page is an unavailable registry, not a smaller listing.
#[tokio::test]
async fn every_page_is_read_and_a_failed_page_is_unavailability() {
    let mut fixtures: Vec<PluginFixture> = (0..60)
        .map(|n| storage_instance(&format!("cf.core._.qe_stale_{n:02}.v1"), "acme", 1))
        .collect();
    fixtures.push(storage_instance("cf.core._.qe_zz_bound.v1", "acme", 100));
    let published: Vec<_> = fixtures.iter().map(PluginFixture::published).collect();
    let registry = crate::test_support::platform_registry(&published);
    let hub = Arc::new(ClientHub::new());
    registry.install(&hub);
    let bound = fixtures.last().expect("bound");
    register_storage(&hub, bound, Arc::new(InMemoryStorage::new()));

    binding(Arc::clone(&hub))
        .resolve_storage()
        .await
        .expect("the bound instance on the second page is selected");
    assert_eq!(registry.calls(Call::List), 2, "two pages");

    registry.inject(
        Fault::on(Call::List)
            .nth(2)
            .fail(CanonicalError::service_unavailable().create()),
    );
    let err = binding(hub)
        .resolve_storage()
        .await
        .err()
        .expect("second page lost");
    assert!(
        matches!(err, DomainError::TypesRegistryUnavailable(_)),
        "{err:?}"
    );
}

#[tokio::test]
async fn registry_failures_and_a_missing_registry_are_unavailability() {
    let no_registry = Arc::new(ClientHub::new());
    let err = binding(no_registry)
        .resolve_storage()
        .await
        .err()
        .expect("no registry");
    assert!(
        matches!(err, DomainError::TypesRegistryUnavailable(_)),
        "{err:?}"
    );

    let hub = crate::test_support::hub_with_failing_registry(
        CanonicalError::service_unavailable()
            .with_detail("registry down")
            .create(),
    );
    let err = binding(hub)
        .resolve_storage()
        .await
        .err()
        .expect("registry error");
    assert!(
        matches!(err, DomainError::TypesRegistryUnavailable(_)),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_registry_call_that_never_answers_is_an_unavailable_registry() {
    let hanging = std::future::pending::<Result<(), CanonicalError>>();
    let err = super::bounded(std::time::Duration::from_millis(20), hanging)
        .await
        .expect_err("the deadline elapses");
    assert!(
        matches!(&err, DomainError::TypesRegistryUnavailable(detail) if detail.contains("no answer")),
        "{err:?}"
    );
}

#[tokio::test]
async fn the_selection_deadline_defaults_to_ten_seconds_and_can_be_tightened() {
    let hub = hub_with(&[]);
    assert_eq!(
        binding(hub.clone()).deadline,
        super::DEFAULT_SELECTION_DEADLINE
    );
    assert_eq!(
        super::DEFAULT_SELECTION_DEADLINE,
        std::time::Duration::from_secs(10)
    );
    let tightened = binding(hub).with_deadline(std::time::Duration::from_millis(5));
    assert_eq!(tightened.deadline, std::time::Duration::from_millis(5));
}
