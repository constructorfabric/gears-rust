use super::*;
use crate::domain::test_support::{
    FailureKind, FakeContractRegistry, OPERATOR_PRINCIPAL, PRINCIPAL_BASE, SEAT_SUBJECT,
    TENANT_SUBJECT, USER_PRINCIPAL, principal_mapping,
};

fn mapper(
    entries: &[(&str, &str)],
    registry: FakeContractRegistry,
) -> (PrincipalMapper, Arc<FakeContractRegistry>) {
    let registry = Arc::new(registry);
    let mapper = PrincipalMapper::new(
        principal_mapping(entries),
        registry.clone() as Arc<dyn ContractRegistry>,
    );
    (mapper, registry)
}

#[tokio::test]
async fn an_exact_entry_maps_without_the_registry() {
    let (mapper, registry) = mapper(&[("user", SEAT_SUBJECT)], FakeContractRegistry::new());

    let subject_type = mapper.subject_type(Some("user")).await.unwrap();

    assert_eq!(subject_type.as_ref().map(AsRef::as_ref), Some(SEAT_SUBJECT));
    assert_eq!(registry.calls(), 0);
}

#[tokio::test]
async fn a_subtype_takes_the_nearest_mapped_ancestor() {
    let (mapper, _) = mapper(
        &[
            (PRINCIPAL_BASE, TENANT_SUBJECT),
            (USER_PRINCIPAL, SEAT_SUBJECT),
        ],
        FakeContractRegistry::with_test_contracts(),
    );

    let subject_type = mapper.subject_type(Some(OPERATOR_PRINCIPAL)).await.unwrap();

    assert_eq!(subject_type.as_ref().map(AsRef::as_ref), Some(SEAT_SUBJECT));
}

#[tokio::test]
async fn an_unmapped_chain_is_not_mapped() {
    let (mapper, _) = mapper(
        &[("user", SEAT_SUBJECT)],
        FakeContractRegistry::with_test_contracts(),
    );

    assert_eq!(
        mapper.subject_type(Some(OPERATOR_PRINCIPAL)).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn an_absent_principal_type_is_not_mapped() {
    let (mapper, _) = mapper(&[("user", SEAT_SUBJECT)], FakeContractRegistry::new());

    assert_eq!(mapper.subject_type(None).await.unwrap(), None);
}

#[tokio::test]
async fn an_unregistered_or_malformed_principal_type_is_not_mapped() {
    let (unregistered, _) = mapper(&[("user", SEAT_SUBJECT)], FakeContractRegistry::new());
    let (malformed, _) = mapper(
        &[("user", SEAT_SUBJECT)],
        FakeContractRegistry::failing(FailureKind::Malformed),
    );

    assert_eq!(unregistered.subject_type(Some("app")).await.unwrap(), None);
    assert_eq!(malformed.subject_type(Some("app")).await.unwrap(), None);
}

#[tokio::test]
async fn without_entries_the_registry_is_not_asked() {
    let (mapper, registry) = mapper(&[], FakeContractRegistry::with_test_contracts());

    assert_eq!(
        mapper.subject_type(Some(USER_PRINCIPAL)).await.unwrap(),
        None
    );
    assert_eq!(registry.calls(), 0);
}

#[tokio::test]
async fn an_unavailable_registry_is_an_error() {
    let (mapper, _) = mapper(
        &[("user", SEAT_SUBJECT)],
        FakeContractRegistry::failing(FailureKind::Unavailable),
    );

    let err = mapper
        .subject_type(Some(OPERATOR_PRINCIPAL))
        .await
        .expect_err("cannot determine the mapping");
    assert!(
        matches!(err, DomainError::TypesRegistryUnavailable(_)),
        "got: {err:?}"
    );
}
