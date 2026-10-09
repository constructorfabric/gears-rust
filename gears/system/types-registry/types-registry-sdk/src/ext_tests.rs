use std::sync::Arc;
use std::time::Duration;

use gts::{GtsId, GtsInstanceId, GtsTypeId};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use uuid::Uuid;

use super::PlatformTypesRegistryApiExt;
use crate::contract::PlatformTypesRegistryApi;
use crate::models::{
    CandidateStatus, EntityKind, FieldSelection, IdempotencyKey, ListEntitiesRequest,
    OperationStatus, PageRequest, Projection, PublisherContext, RegisterEntitiesRequest,
    RegisterItem, RegistrationOperation,
};
use crate::testing_platform::{FakePlatformRegistry, ReadFault};

const TYPE: &str = "gts.cf.test.pkg.thing.v1~";
const INSTANCE: &str = "gts.cf.test.pkg.thing.v1~cf.test.pkg.one.v1";

fn ctx() -> PlatformSecurityContext {
    PlatformSecurityContext::outbound_marker()
}

fn id(s: &str) -> GtsId {
    GtsId::try_new(s).expect("valid identifier")
}

fn type_id(s: &str) -> GtsTypeId {
    GtsTypeId::try_new(s).expect("a Type Schema identifier")
}

fn instance_id(s: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(s).expect("an Instance identifier")
}

fn publisher() -> PublisherContext {
    PublisherContext {
        name: "ext-test".to_owned(),
        version: "1.0.0".parse().expect("version"),
    }
}

fn register_one(gts_id: &str, content: serde_json::Value) -> RegisterEntitiesRequest {
    RegisterEntitiesRequest {
        items: vec![RegisterItem {
            gts_id: id(gts_id),
            content,
            expected_resource_version: None,
            force: false,
        }],
        dry_run: false,
        publisher: publisher(),
    }
}

fn key(k: &str) -> IdempotencyKey {
    IdempotencyKey::new(k).expect("valid key")
}

/// Reconciliation's submit-and-poll step under a budget from now.
async fn submit_and_await<A: PlatformTypesRegistryApi + ?Sized>(
    api: &A,
    key: IdempotencyKey,
    request: RegisterEntitiesRequest,
    budget: Duration,
    cancel: &CancellationToken,
) -> Result<RegistrationOperation, CanonicalError> {
    let deadline = crate::submit::deadline_from_now(budget)?;
    crate::submit::await_registration(api, &ctx(), key, request, deadline, cancel).await
}

#[tokio::test(start_paused = true)]
async fn a_consumer_round_trips_submit_poll_and_read_through_the_trait() {
    let api: Arc<dyn PlatformTypesRegistryApi> =
        Arc::new(FakePlatformRegistry::new().completing_after(3));

    let operation = submit_and_await(
        &*api,
        key("k"),
        register_one(TYPE, json!({ "type": "object" })),
        Duration::from_secs(30),
        &CancellationToken::new(),
    )
    .await
    .expect("completes");

    assert_eq!(operation.status, OperationStatus::Completed);
    assert_eq!(operation.items[0].status, CandidateStatus::Succeeded);
    let snapshot = api
        .get_type_schema(
            &ctx(),
            &type_id(TYPE),
            Projection::Select(FieldSelection::with(&[crate::EntityField::Content])),
        )
        .await
        .expect("reads");
    assert_eq!(snapshot.content, Some(json!({ "type": "object" })));
}

#[tokio::test(start_paused = true)]
async fn an_operation_nothing_drains_fails_on_its_deadline_naming_the_operation() {
    let fake = Arc::new(FakePlatformRegistry::new().completing_after(u32::MAX));

    let error = submit_and_await(
        &*fake,
        key("k"),
        register_one(TYPE, json!({})),
        Duration::from_secs(5),
        &CancellationToken::new(),
    )
    .await
    .expect_err("never completes");

    assert!(
        matches!(error, CanonicalError::DeadlineExceeded { .. }),
        "{error:?}"
    );
    let operation_id = Uuid::parse_str(error.resource_name().expect("names it")).expect("uuid");
    let still = fake
        .get_operation(&ctx(), operation_id)
        .await
        .expect("the write stands");
    assert_eq!(still.status(), OperationStatus::Pending);
}

#[tokio::test]
async fn an_unrepresentable_deadline_is_refused_before_any_submit() {
    let fake = FakePlatformRegistry::new();

    let error = submit_and_await(
        &fake,
        key("k"),
        register_one(TYPE, json!({})),
        Duration::MAX,
        &CancellationToken::new(),
    )
    .await
    .expect_err("refused");

    assert!(
        matches!(error, CanonicalError::InvalidArgument { .. }),
        "{error:?}"
    );
    assert!(fake.submissions().is_empty());
}

#[tokio::test(start_paused = true)]
async fn cancellation_stops_waiting_without_cancelling_the_write() {
    let fake = Arc::new(FakePlatformRegistry::new().completing_after(u32::MAX));
    let cancel = CancellationToken::new();
    let waiter = {
        let fake = Arc::clone(&fake);
        let cancel = cancel.clone();
        tokio::spawn(async move {
            submit_and_await(
                &*fake,
                key("k"),
                register_one(TYPE, json!({})),
                Duration::from_secs(3600),
                &cancel,
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_secs(1)).await;

    cancel.cancel();

    let error = waiter.await.expect("joins").expect_err("cancelled");
    assert!(
        matches!(error, CanonicalError::Cancelled { .. }),
        "{error:?}"
    );
    assert_eq!(fake.submissions().len(), 1, "the accepted write stands");
}

#[tokio::test]
async fn an_unchecked_id_of_the_other_kind_is_refused_without_a_round_trip() {
    let fake = FakePlatformRegistry::new();

    let error = fake
        .get_type_schema(&ctx(), &GtsTypeId::new(INSTANCE), Projection::Default)
        .await
        .expect_err("an Instance identifier");
    assert!(matches!(error, CanonicalError::InvalidArgument { .. }));
    let error = fake
        .get_instance(&ctx(), &GtsInstanceId::new(TYPE, ""), Projection::Default)
        .await
        .expect_err("a Type Schema identifier");
    assert!(matches!(error, CanonicalError::InvalidArgument { .. }));

    assert_eq!(fake.batch_reads(), 0);
}

#[tokio::test]
async fn a_reference_to_the_other_kind_is_not_found() {
    let fake = FakePlatformRegistry::new();
    fake.seed(INSTANCE, json!({}));
    let uuid = id(INSTANCE).to_uuid();

    let error = fake
        .get_type_schema_by_uuid(&ctx(), uuid, Projection::Default)
        .await
        .expect_err("an Instance");
    assert!(
        matches!(error, CanonicalError::NotFound { .. }),
        "{error:?}"
    );
    assert!(
        fake.get_instance_by_uuid(&ctx(), uuid, Projection::Default)
            .await
            .is_ok()
    );

    let answers = fake
        .batch_get_type_schemas_by_uuid(&ctx(), &[uuid], Projection::Default)
        .await
        .expect("the read succeeds");
    assert_eq!(answers.get(&uuid), Some(&None), "absent among Type Schemas");
}

#[tokio::test]
async fn a_key_named_twice_gets_the_one_answer_under_each_spelling() {
    let fake = FakePlatformRegistry::new();
    fake.seed(TYPE, json!({}));

    let by_id = fake
        .batch_get_type_schemas(&ctx(), &[type_id(TYPE), type_id(TYPE)], Projection::Default)
        .await
        .expect("the reads succeed");
    assert_eq!(by_id.len(), 1);
    assert!(by_id[&type_id(TYPE)].is_some());

    let uuid = id(TYPE).to_uuid();
    let by_uuid = fake
        .batch_get_type_schemas_by_uuid(&ctx(), &[uuid, uuid], Projection::Default)
        .await
        .expect("the reads succeed");
    assert_eq!(by_uuid.len(), 1);
    assert!(by_uuid[&uuid].is_some());
}

#[tokio::test]
async fn every_asked_key_is_answered_and_absence_is_none() {
    let fake = FakePlatformRegistry::new();
    fake.seed(TYPE, json!({}));
    let absent = type_id("gts.cf.test.pkg.absent.v1~");

    let answers = fake
        .batch_get_type_schemas(
            &ctx(),
            &[type_id(TYPE), absent.clone()],
            Projection::Default,
        )
        .await
        .expect("the read succeeds");

    assert_eq!(answers.len(), 2);
    assert!(answers[&type_id(TYPE)].is_some());
    assert_eq!(answers.get(&absent), Some(&None));
}

#[tokio::test]
async fn a_large_read_is_split_into_bounded_batches_and_answers_every_key() {
    let fake = FakePlatformRegistry::new();
    let ids: Vec<GtsTypeId> = (0..150)
        .map(|n| type_id(&format!("gts.cf.test.pkg.thing{n}.v1~")))
        .collect();
    for id in &ids {
        fake.seed(id.as_ref(), json!({}));
    }

    let answers = fake
        .batch_get_type_schemas(&ctx(), &ids, Projection::Default)
        .await
        .expect("the reads succeed");

    assert_eq!(fake.batch_reads(), 2);
    assert_eq!(answers.len(), 150);
    assert!(answers.values().all(Option::is_some));
}

#[tokio::test]
async fn one_malformed_identifier_fails_the_call_before_any_read() {
    let fake = FakePlatformRegistry::new();
    let mut ids: Vec<GtsTypeId> = (0..150)
        .map(|n| type_id(&format!("gts.cf.test.pkg.thing{n}.v1~")))
        .collect();
    ids.push(GtsTypeId::new("not-an-id"));

    let error = fake
        .batch_get_type_schemas(&ctx(), &ids, Projection::Default)
        .await
        .expect_err("an id that does not parse");

    assert!(matches!(error, CanonicalError::InvalidArgument { .. }));
    assert_eq!(
        fake.batch_reads(),
        0,
        "not even the valid first batch is read"
    );
}

#[tokio::test]
async fn a_mislabeled_kind_is_a_protocol_fault_not_absence() {
    let fake = FakePlatformRegistry::new();
    fake.seed(TYPE, json!({}));
    fake.fault_reads(ReadFault::MislabelKind);
    let uuid = id(TYPE).to_uuid();

    // By UUID the kind is not known up front, so a mislabel could pass as "the other kind".
    let single = fake
        .get_instance_by_uuid(&ctx(), uuid, Projection::Default)
        .await
        .expect_err("a Type Schema labeled Instance");
    assert!(
        matches!(single, CanonicalError::Internal { .. }),
        "{single:?}"
    );
    let plural = fake
        .batch_get_type_schemas_by_uuid(&ctx(), &[uuid], Projection::Default)
        .await
        .expect_err("a Type Schema labeled Instance");
    assert!(
        matches!(plural, CanonicalError::Internal { .. }),
        "{plural:?}"
    );
    let by_id = fake
        .batch_get_type_schemas(&ctx(), &[type_id(TYPE)], Projection::Default)
        .await
        .expect_err("a Type Schema labeled Instance");
    assert!(
        matches!(by_id, CanonicalError::Internal { .. }),
        "{by_id:?}"
    );
}

#[tokio::test]
async fn an_unanswered_key_fails_the_call_rather_than_reading_as_absent() {
    let fake = FakePlatformRegistry::new();
    fake.seed(TYPE, json!({}));
    fake.fault_reads(ReadFault::DropAnswers);

    let error = fake
        .batch_get_type_schemas(&ctx(), &[type_id(TYPE)], Projection::Default)
        .await
        .expect_err("the registry left the key unanswered");

    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn list_helpers_select_their_documents_by_default_and_follow_every_page() {
    let fake = FakePlatformRegistry::new();
    fake.seed(TYPE, json!({ "type": "object" }));
    for n in 0..3 {
        fake.seed(
            &format!("gts.cf.test.pkg.thing.v1~cf.test.pkg.i{n}.v1"),
            json!({ "n": n }),
        );
    }
    let query = ListEntitiesRequest {
        page: PageRequest {
            limit: Some(1),
            cursor: None,
        },
        ..ListEntitiesRequest::default()
    };

    let instances = fake
        .list_instances(&ctx(), query.clone())
        .await
        .expect("lists");

    assert_eq!(instances.len(), 3, "every page is followed");
    assert!(
        instances.iter().all(|s| s.type_id == type_id(TYPE)),
        "each names the Type Schema it conforms to"
    );
    assert!(
        instances.iter().all(|s| s.content.is_some()),
        "content is selected"
    );

    let light = fake
        .list_instances(
            &ctx(),
            ListEntitiesRequest {
                projection: Projection::Select(FieldSelection::light()),
                ..query
            },
        )
        .await
        .expect("lists");
    assert!(
        light.iter().all(|s| s.content.is_none()),
        "an explicit selection is kept"
    );

    let schemas = fake
        .list_type_schemas(&ctx(), ListEntitiesRequest::default())
        .await
        .expect("lists");
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].content, Some(json!({ "type": "object" })));
    assert_eq!(
        schemas[0].resolved_schema,
        Some(json!({ "x-fake-resolved": { "type": "object" } })),
        "the default selection reads the resolved schema"
    );
    assert!(
        schemas[0].effective_traits.is_some(),
        "and the effective traits"
    );
    assert!(
        schemas[0].effective_traits_schema.is_some(),
        "and the effective traits schema"
    );
}

#[tokio::test]
async fn a_list_helper_refuses_a_page_that_repeats_its_cursor() {
    let fake = FakePlatformRegistry::new();
    for n in 0..3 {
        fake.seed(&format!("gts.cf.test.pkg.t{n}.v1~"), json!({}));
    }
    fake.repeat_list_cursor();
    let query = ListEntitiesRequest {
        page: PageRequest {
            limit: Some(1),
            cursor: None,
        },
        ..ListEntitiesRequest::default()
    };

    let error = fake
        .list_type_schemas(&ctx(), query)
        .await
        .expect_err("a repeated cursor is refused, not followed forever");

    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_list_helper_stops_after_its_page_bound() {
    let fake = FakePlatformRegistry::new();
    for n in 0..=super::MAX_LIST_PAGES {
        fake.seed(&format!("gts.cf.test.pkg.t{n}.v1~"), json!({}));
    }
    let query = ListEntitiesRequest {
        page: PageRequest {
            limit: Some(1),
            cursor: None,
        },
        ..ListEntitiesRequest::default()
    };

    let error = fake
        .list_type_schemas(&ctx(), query)
        .await
        .expect_err("more pages than the bound fail");

    assert!(
        matches!(error, CanonicalError::InvalidArgument { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_failed_batch_fails_the_whole_call_and_reads_no_further() {
    let fake = FakePlatformRegistry::new();
    let ids: Vec<GtsTypeId> = (0..250)
        .map(|n| type_id(&format!("gts.cf.test.pkg.t{n:03}.v1~")))
        .collect();
    for id in &ids {
        fake.seed(id.as_ref(), json!({}));
    }
    fake.fault_reads(ReadFault::FailOnly(2));

    let error = fake
        .batch_get_type_schemas(&ctx(), &ids, Projection::Default)
        .await
        .expect_err("the second batch failed");

    assert!(
        matches!(error, CanonicalError::ServiceUnavailable { .. }),
        "the call fails with the read's own error, once: {error:?}"
    );
    assert_eq!(fake.batch_reads(), 2, "no batch is read after the failure");
}

#[tokio::test]
async fn an_empty_read_needs_no_transport() {
    let fake = FakePlatformRegistry::new();

    let none = fake
        .batch_get_instances_by_uuid(&ctx(), &[], Projection::Default)
        .await
        .expect("an empty read reads nothing");
    assert!(none.is_empty());
    let none = fake
        .batch_get_type_schemas(&ctx(), &[], Projection::Default)
        .await
        .expect("an empty read reads nothing");
    assert!(none.is_empty());
    assert_eq!(fake.batch_reads(), 0);
}

#[tokio::test(start_paused = true)]
async fn polls_back_off_from_the_initial_interval_to_the_cap() {
    let fake = Arc::new(FakePlatformRegistry::new().completing_after(u32::MAX));
    let budget = Duration::from_secs(10);

    submit_and_await(
        &*fake,
        key("k-poll"),
        register_one(TYPE, json!({})),
        budget,
        &CancellationToken::new(),
    )
    .await
    .expect_err("never completes");

    // First five polls consume 1.55 s; eight 1 s polls fit in the remaining 8.45 s.
    let mut expected = 0;
    let mut elapsed = Duration::ZERO;
    let mut interval = super::POLL_INTERVAL_INITIAL;
    while elapsed + interval < budget {
        elapsed += interval;
        expected += 1;
        interval = (interval * 2).min(super::POLL_INTERVAL_MAX);
    }
    assert_eq!(fake.polls(), expected);
    assert_eq!(expected, 13);
}

#[tokio::test]
async fn a_list_helper_refuses_a_query_for_the_other_kind() {
    let fake = FakePlatformRegistry::new();
    let mut query = ListEntitiesRequest::default();
    query.filter.kind = Some(EntityKind::Instance);

    let error = fake
        .list_type_schemas(&ctx(), query)
        .await
        .expect_err("refused");

    assert!(matches!(error, CanonicalError::InvalidArgument { .. }));
}

#[tokio::test(start_paused = true)]
async fn a_slow_submit_spends_the_one_budget_and_the_deadline_still_names_the_operation() {
    let fake = Arc::new(FakePlatformRegistry::new().completing_after(u32::MAX));
    fake.delay_submits(Duration::from_secs(3));
    let started = tokio::time::Instant::now();

    let error = submit_and_await(
        &*fake,
        key("k"),
        register_one(TYPE, json!({})),
        Duration::from_secs(5),
        &CancellationToken::new(),
    )
    .await
    .expect_err("never completes");

    assert_eq!(
        started.elapsed(),
        Duration::from_secs(5),
        "the budget is not reset after submit"
    );
    assert!(
        matches!(error, CanonicalError::DeadlineExceeded { .. }),
        "{error:?}"
    );
    assert!(
        error.resource_name().is_some(),
        "the submit answered, so the id is known"
    );
}

#[tokio::test(start_paused = true)]
async fn a_submit_that_never_answers_returns_by_the_deadline() {
    let fake = FakePlatformRegistry::new();
    fake.delay_submits(Duration::from_secs(3600));
    let started = tokio::time::Instant::now();

    let error = submit_and_await(
        &fake,
        key("k"),
        register_one(TYPE, json!({})),
        Duration::from_secs(5),
        &CancellationToken::new(),
    )
    .await
    .expect_err("times out");

    assert_eq!(started.elapsed(), Duration::from_secs(5));
    assert!(matches!(error, CanonicalError::DeadlineExceeded { .. }));
    assert_eq!(error.resource_name(), None, "no operation is known yet");
}

#[tokio::test(start_paused = true)]
async fn a_poll_that_never_answers_returns_by_the_deadline_naming_the_operation() {
    let fake = Arc::new(FakePlatformRegistry::new().completing_after(u32::MAX));
    fake.delay_polls(Duration::from_secs(3600));
    let started = tokio::time::Instant::now();

    let error = submit_and_await(
        &*fake,
        key("k"),
        register_one(TYPE, json!({})),
        Duration::from_secs(5),
        &CancellationToken::new(),
    )
    .await
    .expect_err("times out");

    assert_eq!(started.elapsed(), Duration::from_secs(5));
    assert!(matches!(error, CanonicalError::DeadlineExceeded { .. }));
    let operation_id = Uuid::parse_str(error.resource_name().expect("named")).expect("uuid");
    assert_eq!(
        fake.submissions().len(),
        1,
        "{operation_id} is the one accepted operation"
    );
}

#[tokio::test(start_paused = true)]
async fn a_spent_budget_submits_nothing_even_to_an_instant_registry() {
    let fake = FakePlatformRegistry::new();

    let error = submit_and_await(
        &fake,
        key("k"),
        register_one(TYPE, json!({})),
        Duration::ZERO,
        &CancellationToken::new(),
    )
    .await
    .expect_err("no budget");

    assert!(
        matches!(error, CanonicalError::DeadlineExceeded { .. }),
        "{error:?}"
    );
    assert!(fake.submissions().is_empty());
}

// ---- TypesRegistryApiExt: the same helpers over the tenant contract ---------
//
// Called through `&dyn TypesRegistryApi`: the fake implements both contracts, and the
// trait object leaves only the tenant helpers in reach.

mod tenant {
    use serde_json::json;
    use toolkit_canonical_errors::CanonicalError;
    use toolkit_security::SecurityContext;

    use gts::{GtsInstanceId, GtsTypeId};

    use super::{FakePlatformRegistry, INSTANCE, TYPE, id, instance_id, type_id};
    use crate::TypesRegistryApiExt;
    use crate::contract::TypesRegistryApi;
    use crate::models::{
        EntityField, EntityKind, FieldSelection, ListEntitiesRequest, PageRequest, Projection,
    };

    fn tenant() -> SecurityContext {
        SecurityContext::anonymous()
    }

    #[tokio::test]
    async fn an_unchecked_id_of_the_other_kind_is_refused_without_a_round_trip() {
        let fake = FakePlatformRegistry::new();
        let api: &dyn TypesRegistryApi = &fake;

        let error = api
            .get_type_schema(&tenant(), &GtsTypeId::new(INSTANCE), Projection::Default)
            .await
            .expect_err("an Instance identifier");
        assert!(matches!(error, CanonicalError::InvalidArgument { .. }));
        let error = api
            .batch_get_instances(
                &tenant(),
                &[GtsInstanceId::new(TYPE, "")],
                Projection::Default,
            )
            .await
            .expect_err("a Type Schema identifier");
        assert!(matches!(error, CanonicalError::InvalidArgument { .. }));

        assert_eq!(fake.batch_reads(), 0);
    }

    #[tokio::test]
    async fn reads_answer_by_identifier_and_by_reference_within_their_kind() {
        let fake = FakePlatformRegistry::new();
        fake.seed(TYPE, json!({ "type": "object" }));
        fake.seed(INSTANCE, json!({ "n": 1 }));
        let api: &dyn TypesRegistryApi = &fake;
        let absent = type_id("gts.cf.test.pkg.absent.v1~");

        let schema = api
            .get_type_schema(&tenant(), &type_id(TYPE), Projection::Default)
            .await
            .expect("present");
        assert_eq!(schema.type_id, type_id(TYPE));
        let instance = api
            .get_instance_by_uuid(&tenant(), id(INSTANCE).to_uuid(), Projection::Default)
            .await
            .expect("present");
        assert_eq!(instance.id, instance_id(INSTANCE));
        assert_eq!(instance.type_id, type_id(TYPE));
        let other_kind = api
            .get_type_schema_by_uuid(&tenant(), id(INSTANCE).to_uuid(), Projection::Default)
            .await
            .expect_err("an Instance");
        assert!(matches!(other_kind, CanonicalError::NotFound { .. }));

        let schemas = api
            .batch_get_type_schemas(
                &tenant(),
                &[type_id(TYPE), absent.clone()],
                Projection::Default,
            )
            .await
            .expect("the reads succeed");
        assert!(schemas[&type_id(TYPE)].is_some());
        assert_eq!(schemas.get(&absent), Some(&None));
        let instances = api
            .batch_get_instances_by_uuid(&tenant(), &[id(INSTANCE).to_uuid()], Projection::Default)
            .await
            .expect("the reads succeed");
        assert!(instances[&id(INSTANCE).to_uuid()].is_some());
        assert_eq!(fake.batch_reads(), 5, "the two-key read is one batch");
    }

    #[tokio::test]
    async fn an_explicit_selection_reaches_the_read() {
        let fake = FakePlatformRegistry::new();
        fake.seed(INSTANCE, json!({ "n": 1 }));
        let api: &dyn TypesRegistryApi = &fake;

        let light = api
            .get_instance(
                &tenant(),
                &instance_id(INSTANCE),
                Projection::Select(FieldSelection::light()),
            )
            .await
            .expect("present");
        assert!(light.content.is_none(), "a light selection has no document");
        let with_content = api
            .get_instance(
                &tenant(),
                &instance_id(INSTANCE),
                Projection::Select(FieldSelection::with(&[EntityField::Content])),
            )
            .await
            .expect("present");
        assert_eq!(with_content.content, Some(json!({ "n": 1 })));
    }

    #[tokio::test]
    async fn list_helpers_select_their_documents_by_default_and_follow_every_page() {
        let fake = FakePlatformRegistry::new();
        fake.seed(TYPE, json!({ "type": "object" }));
        for n in 0..3 {
            fake.seed(
                &format!("gts.cf.test.pkg.thing.v1~cf.test.pkg.i{n}.v1"),
                json!({ "n": n }),
            );
        }
        let api: &dyn TypesRegistryApi = &fake;
        let query = ListEntitiesRequest {
            page: PageRequest {
                limit: Some(1),
                cursor: None,
            },
            ..ListEntitiesRequest::default()
        };

        let instances = api
            .list_instances(&tenant(), query.clone())
            .await
            .expect("lists");
        assert_eq!(instances.len(), 3, "every page is followed");
        assert!(instances.iter().all(|s| s.content.is_some()));

        let light = api
            .list_instances(
                &tenant(),
                ListEntitiesRequest {
                    projection: Projection::Select(FieldSelection::light()),
                    ..query
                },
            )
            .await
            .expect("lists");
        assert!(light.iter().all(|s| s.content.is_none()), "kept as asked");

        let schemas = api
            .list_type_schemas(&tenant(), ListEntitiesRequest::default())
            .await
            .expect("lists");
        assert_eq!(schemas.len(), 1);
        assert!(
            schemas[0].resolved_schema.is_some(),
            "materializations selected"
        );
    }

    #[tokio::test]
    async fn a_list_helper_refuses_a_query_for_the_other_kind() {
        let fake = FakePlatformRegistry::new();
        let api: &dyn TypesRegistryApi = &fake;
        let mut query = ListEntitiesRequest::default();
        query.filter.kind = Some(EntityKind::TypeSchema);

        let error = api
            .list_instances(&tenant(), query)
            .await
            .expect_err("refused");

        assert!(matches!(error, CanonicalError::InvalidArgument { .. }));
    }
}
