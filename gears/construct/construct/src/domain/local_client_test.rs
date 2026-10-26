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
use crate::test_support::{AllowResolver, DenyResolver, IntakeFixture, inmem_db, profile_service};

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
    assert_eq!(fact.fact_id, "construct:t:fact-1");
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
