//! The in-process client's profile read.

use std::sync::Arc;

use construct_sdk::{ConstructClientV1, FactCategory, FactOrigin};
use serde_json::json;
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::local_client::LocalClient;
use crate::domain::plan::NodeKey;
use crate::domain::profile::Category;
use crate::domain::profile_read::{CategoryCheck, OriginKind, StoredFact};
use crate::test_support::{
    AllowResolver, DenyResolver, IntakeFixture, RecordingResolver, inmem_db, profile_service,
};

fn caller() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .build()
        .unwrap()
}

fn reviewed_skill() -> StoredFact {
    StoredFact {
        node_key: NodeKey("construct:t:fact-1".to_owned()),
        category: Category::Skill,
        property: "skills".to_owned(),
        value: json!({ "name": "Rust" }),
        origin: OriginKind::Reviewer,
        stored_at: OffsetDateTime::UNIX_EPOCH,
        produced_at: None,
    }
}

async fn client(
    resolver: Arc<dyn authz_resolver_sdk::AuthZResolverApi>,
) -> LocalClient<
    crate::infra::storage::record_ids_repo::SeaOrmRecordIdRepository,
    crate::infra::storage::subject_settings_repo::SeaOrmSubjectSettingsRepository,
> {
    let db = inmem_db().await;
    let intake = Arc::new(IntakeFixture::default().build(&db));
    let profile = Arc::new(profile_service(
        &db,
        resolver,
        vec![reviewed_skill()],
        Arc::default(),
        CategoryCheck::OneCall,
    ));
    LocalClient::new(intake, profile)
}

#[tokio::test]
async fn read_profile_answers_the_profile_in_the_sdk_shape() {
    let subject = Uuid::new_v4();

    let profile = client(Arc::new(AllowResolver))
        .await
        .read_profile(&caller(), None, subject)
        .await
        .unwrap();

    assert_eq!(profile.subject_id, subject);
    let [skills] = profile.categories.as_slice() else {
        panic!("one category: {profile:?}");
    };
    assert_eq!(skills.category, FactCategory::Skill);
    let [fact] = skills.facts.as_slice() else {
        panic!("one fact: {skills:?}");
    };
    assert_eq!(fact.fact_id.as_str(), "construct:t:fact-1");
    assert_eq!(
        (fact.property.as_str(), &fact.value),
        ("skills", &json!({ "name": "Rust" }))
    );
    assert_eq!(fact.origin, FactOrigin::Reviewer);
    assert_eq!(fact.produced_at, None);
}

#[tokio::test]
async fn read_profile_answers_a_subject_the_caller_may_not_act_for_as_not_found() {
    let result = client(Arc::new(DenyResolver))
        .await
        .read_profile(&caller(), None, Uuid::new_v4())
        .await;

    assert!(
        matches!(result, Err(CanonicalError::NotFound { .. })),
        "{result:?}"
    );
}

#[tokio::test]
async fn read_profile_asks_about_the_named_tenant() {
    let resolver = Arc::new(RecordingResolver::default());
    let tenant = Uuid::new_v4();

    client(Arc::clone(&resolver) as _)
        .await
        .read_profile(&caller(), Some(tenant), Uuid::new_v4())
        .await
        .unwrap();

    let asked: Vec<_> = resolver
        .asked
        .lock()
        .unwrap()
        .iter()
        .filter(|(resource, ..)| resource == crate::domain::profile_read::PROFILE_TYPE)
        .map(|(_, _, context, owner)| (*context, owner.clone()))
        .collect();
    assert_eq!(asked, [(Some(tenant), Some(tenant.to_string()))]);
}

#[tracing_test::traced_test]
#[tokio::test]
async fn a_refused_read_is_logged_with_the_caller_the_subject_and_the_tenant() {
    let ctx = caller();
    let (subject, named) = (Uuid::new_v4(), Uuid::new_v4());
    let client = client(Arc::new(DenyResolver)).await;

    let own = client.read_profile(&ctx, None, subject).await;
    let other = client.read_profile(&ctx, Some(named), subject).await;

    assert!(own.is_err() && other.is_err());
    assert!(logs_contain("construct profile read refused"));
    assert!(logs_contain(&format!("subject_id={subject}")));
    assert!(logs_contain(&format!("caller={}", ctx.subject_id())));
    assert!(
        logs_contain(&format!("tenant_id={}", ctx.subject_tenant_id())),
        "the caller's own tenant"
    );
    assert!(
        logs_contain(&format!("tenant_id={named}")),
        "the tenant the read named"
    );
}
