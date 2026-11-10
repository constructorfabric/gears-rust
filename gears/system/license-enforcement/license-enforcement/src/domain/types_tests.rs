use super::*;

#[test]
fn type_key_drops_minor_versions() {
    assert_eq!(
        type_key("gts.cf.core.lic.subj.v1.2~acme.app.iam.user.v3.10~"),
        "gts.cf.core.lic.subj.v1~acme.app.iam.user.v3~"
    );
    assert_eq!(
        type_key("gts.cf.core.lic.subj.v1~acme.app.iam.user.v1~"),
        "gts.cf.core.lic.subj.v1~acme.app.iam.user.v1~"
    );
    assert_eq!(type_key("not a gts id"), "not a gts id");
}

#[test]
fn covers_follows_derivation() {
    let user = GtsTypeId::new("gts.cf.core.lic.subj.v1~acme.app.iam.user.v1~");
    let admin =
        GtsTypeId::new("gts.cf.core.lic.subj.v1~acme.app.iam.user.v1.1~acme.app.iam.admin.v1~");
    let service = GtsTypeId::new("gts.cf.core.lic.subj.v1~acme.app.iam.service.v1~");
    assert!(covers(&user, &user));
    assert!(covers(&user, &admin));
    assert!(!covers(&admin, &user));
    assert!(!covers(&user, &service));
    let (bad_base, bad_subject) = (GtsTypeId::new("acme.us"), GtsTypeId::new("acme.user"));
    assert!(!covers(&bad_base, &bad_subject));
}

#[tokio::test]
async fn ensure_registered_fails_on_a_missing_type() {
    use types_registry_sdk::testing::MockTypesRegistryClient;

    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TypesRegistryClient>(Arc::new(MockTypesRegistryClient::new()));
    let resolver = TypeResolver::new(hub);
    let missing = GtsTypeId::new("gts.cf.core.lic.subj.v1~test.le.app.user.v1~");
    assert!(matches!(
        resolver.ensure_registered(&[missing]).await,
        Err(DomainError::Internal(_))
    ));
    assert!(resolver.ensure_registered(&[]).await.is_ok());
}

#[tokio::test]
async fn conditions_address_the_subject_and_resource_contracts() {
    let env = crate::test_support::env().await;
    let resolver = TypeResolver::new(Arc::clone(&env.hub));
    let terms = crate::test_support::terms();
    let (mut violations, mut seen) = (Vec::new(), Schemas::new());
    resolver
        .validate_terms("g", &terms, &mut violations, &mut seen)
        .await
        .unwrap();
    assert!(violations.is_empty(), "{violations:?}");
    let schema = conditions_schema(&terms, &seen).unwrap();
    for (part, t) in [
        ("subject", &terms.subject_type),
        ("resource", &terms.resource.gts_type),
    ] {
        let expected = seen[t.as_ref()].as_ref().unwrap().effective_schema();
        assert_eq!(schema["properties"][part], expected, "{part}");
    }

    let mut unknown = terms.clone();
    unknown.subject_type = GtsTypeId::new("gts.cf.core.lic.subj.v1~test.le.app.nope.v1~");
    assert!(conditions_schema(&unknown, &seen).is_none());
}
