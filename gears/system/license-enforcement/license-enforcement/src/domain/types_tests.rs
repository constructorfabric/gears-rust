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
    use types_registry_sdk::testing_platform::MockTypesRegistry;

    let hub = Arc::new(ClientHub::new());
    Arc::new(MockTypesRegistry::new()).install(&hub);
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
    let caller = crate::test_support::ctx(uuid::Uuid::new_v4());
    resolver
        .validate_terms(&caller, "g", &terms, &mut violations, &mut seen)
        .await
        .unwrap();
    assert!(violations.is_empty(), "{violations:?}");
    let schema = conditions_schema(&terms, &seen).unwrap();
    for (part, t) in [
        ("subject", &terms.subject_type),
        ("resource", &terms.resource.gts_type),
    ] {
        let expected = seen[t.as_ref()]
            .as_ref()
            .unwrap()
            .resolved_schema
            .clone()
            .unwrap();
        assert_eq!(schema["properties"][part], expected, "{part}");
    }

    let mut unknown = terms.clone();
    unknown.subject_type = GtsTypeId::new("gts.cf.core.lic.subj.v1~test.le.app.nope.v1~");
    assert!(conditions_schema(&unknown, &seen).is_none());
}

/// A grant request's lookups go through the tenant contract under the
/// caller's context; the start check through the platform contract.
#[tokio::test]
async fn requests_read_as_the_caller_and_the_start_check_as_the_platform() {
    use types_registry_sdk::testing_platform::Call;

    let registry = crate::test_support::registry();
    let hub = Arc::new(ClientHub::new());
    registry.install(&hub);
    let resolver = TypeResolver::new(hub);
    let caller = crate::test_support::ctx(uuid::Uuid::new_v4());
    let terms = crate::test_support::terms();
    let (mut violations, mut seen) = (Vec::new(), Schemas::new());
    resolver
        .validate_terms(&caller, "g", &terms, &mut violations, &mut seen)
        .await
        .unwrap();
    assert!(violations.is_empty(), "{violations:?}");
    let contexts = registry.caller_contexts(Call::BatchGet);
    assert!(!contexts.is_empty());
    assert!(
        contexts
            .iter()
            .all(|c| c.subject_id() == caller.subject_id()
                && c.subject_tenant_id() == caller.subject_tenant_id())
    );
    assert_eq!(registry.platform_calls(Call::BatchGet), 0);

    resolver
        .ensure_registered(std::slice::from_ref(&terms.subject_type))
        .await
        .unwrap();
    assert_eq!(registry.platform_calls(Call::BatchGet), 1);
}

fn snapshot(type_id: &str) -> TypeSchema {
    TypeSchema {
        type_id: GtsTypeId::new(type_id),
        type_uuid: uuid::Uuid::nil(),
        lifecycle_status: LifecycleStatus::Active,
        ownership: types_registry_sdk::Ownership::Global,
        origin: None,
        content: Some(serde_json::json!({ "type": "object" })),
        resolved_schema: Some(serde_json::json!({ "type": "object" })),
        effective_traits: Some(serde_json::json!({})),
        effective_traits_schema: None,
        provenance: None,
    }
}

/// A registered contract without a selected artifact is unusable: an internal
/// fault, not an outage a retry cures. A deleted one is not registered.
#[test]
fn a_contract_missing_an_artifact_is_unusable_and_a_deleted_one_unregistered() {
    let id = crate::test_support::RESOURCE;
    assert!(matches!(found(id, snapshot(id)), Ok(Lookup::Found(_))));
    for strip in [
        |s: &mut TypeSchema| s.content = None,
        |s: &mut TypeSchema| s.resolved_schema = None,
        |s: &mut TypeSchema| s.effective_traits = None,
    ] {
        let mut incomplete = snapshot(id);
        strip(&mut incomplete);
        assert!(
            matches!(found(id, incomplete), Err(DomainError::Internal(ref d)) if d.contains("unusable")),
        );
    }
    let mut deleted = snapshot(id);
    deleted.lifecycle_status = LifecycleStatus::Deleted;
    assert!(matches!(found(id, deleted), Ok(Lookup::Unregistered)));
}

/// A grant request naming a deleted type reports it as not registered.
#[tokio::test]
async fn a_deleted_type_is_not_registered() {
    use types_registry_sdk::testing_platform::Seed;

    let registry = crate::test_support::registry();
    let resource = crate::test_support::RESOURCE;
    let content = registry.content(resource).expect("seeded");
    registry.seed_entity(Seed::new(resource, content).deleted());
    let hub = Arc::new(ClientHub::new());
    registry.install(&hub);
    let resolver = TypeResolver::new(hub);
    let caller = crate::test_support::ctx(uuid::Uuid::new_v4());
    let (mut violations, mut seen) = (Vec::new(), Schemas::new());
    resolver
        .validate_terms(
            &caller,
            "g",
            &crate::test_support::terms(),
            &mut violations,
            &mut seen,
        )
        .await
        .unwrap();
    let reasons: Vec<_> = violations.iter().map(|v| v.reason.as_str()).collect();
    assert_eq!(reasons, [reason::TYPE_NOT_REGISTERED], "{violations:?}");
}

/// A registry that refuses a request's caller answered: the refusal is the
/// caller's, not an outage. The start check keeps the outage classification.
#[tokio::test]
async fn a_refused_request_is_the_callers_refusal_not_an_outage() {
    use types_registry_sdk::testing_platform::{Call, Fault};

    let registry = crate::test_support::registry();
    let hub = Arc::new(ClientHub::new());
    registry.install(&hub);
    let resolver = TypeResolver::new(hub);
    let caller = crate::test_support::ctx(uuid::Uuid::new_v4());
    let terms = crate::test_support::terms();
    let denied = || {
        crate::domain::error::GrantResourceError::permission_denied()
            .with_reason("FORBIDDEN")
            .create()
    };
    let unauthenticated = CanonicalError::unauthenticated()
        .with_reason("TOKEN_EXPIRED")
        .create();
    for (refusal, is_expected) in [
        (
            denied(),
            (|e: &DomainError| matches!(e, DomainError::PermissionDenied))
                as fn(&DomainError) -> bool,
        ),
        (unauthenticated, |e| {
            matches!(e, DomainError::Unauthenticated)
        }),
    ] {
        registry.inject(Fault::on(Call::BatchGet).first(1).fail(refusal));
        let (mut violations, mut seen) = (Vec::new(), Schemas::new());
        let err = resolver
            .validate_terms(&caller, "g", &terms, &mut violations, &mut seen)
            .await
            .expect_err("refused");
        assert!(is_expected(&err), "{err:?}");
    }

    registry.inject(Fault::on(Call::BatchGet).first(1).fail(denied()));
    let err = resolver
        .ensure_registered(std::slice::from_ref(&terms.subject_type))
        .await
        .expect_err("refused");
    assert!(
        matches!(err, DomainError::DependencyUnavailable(_)),
        "{err:?}"
    );
}

/// A refused credential answers 401 with the listener's token.
#[test]
fn an_unauthenticated_caller_is_a_401() {
    use toolkit_canonical_errors::Problem;

    let problem = serde_json::to_value(Problem::from(CanonicalError::from(
        DomainError::Unauthenticated,
    )))
    .expect("json");
    assert_eq!(problem["status"], 401, "{problem}");
    assert_eq!(problem["context"]["reason"], "AUTHN_FAILED", "{problem}");
}
