//! The round trip through the gear's own wiring: a connector's record goes
//! through intake, the queue, the planner (over a scripted model), the checks,
//! Admission and the writer into graph storage's in-memory store, and the
//! profile read reads it back.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::models::{EvaluationRequest, EvaluationResponse};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio_util::sync::CancellationToken;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

use super::pipeline::{MANAGE_BUDGET, assemble};
use crate::config::{CategoryCheckConfig, ConstructConfig};
use crate::domain::model_client::{
    ModelClient, ModelError, ModelOutput, ModelRequest, ModelResponse, ToolCall, Usage,
};
use crate::domain::profile::Category;
use crate::domain::profile_read::OriginKind;
use crate::domain::record_intake::IntakeOutcome;
use crate::test_support::{
    AllowResolver, IntakeFixture, chat_record, fake_graph_storage, inmem_db, settings_service,
    unchecked_processing,
};

/// A model that answers with the given answers in turn, then the last again.
struct Scripted(Mutex<VecDeque<ModelResponse>>);

#[async_trait]
impl ModelClient for Scripted {
    async fn complete(
        &self,
        _ctx: &SecurityContext,
        _request: &ModelRequest,
    ) -> Result<ModelResponse, ModelError> {
        let mut answers = self.0.lock().unwrap();
        Ok(if answers.len() > 1 {
            answers.pop_front().unwrap()
        } else {
            answers.front().cloned().unwrap()
        })
    }
}

fn answer(output: ModelOutput) -> ModelResponse {
    ModelResponse {
        output,
        usage: Some(Usage {
            input_tokens: 100,
            output_tokens: 20,
        }),
    }
}

/// The model adds one skill, then says the plan is complete.
fn adds_a_skill() -> Arc<Scripted> {
    Arc::new(Scripted(Mutex::new(VecDeque::from([
        answer(ModelOutput::ToolCalls(vec![ToolCall {
            id: "call_0".to_owned(),
            name: "add".to_owned(),
            arguments: json!({ "kind": "project", "value": { "name": "Rust" }, "confidence": 0.9 }),
        }])),
        answer(ModelOutput::Text("The plan is complete.".to_owned())),
    ]))))
}

fn in_tenant(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant)
        .build()
        .unwrap()
}

#[tracing_test::traced_test]
#[tokio::test]
async fn a_record_is_planned_checked_admitted_written_read_back_and_deleted_by_its_subject() {
    let db = inmem_db().await;
    let cfg = ConstructConfig {
        processing_identity: Some(Uuid::new_v4()),
        processing: unchecked_processing(),
        profile_category_check: Some(CategoryCheckConfig::OneCall),
        ..ConstructConfig::default()
    }
    .validated()
    .unwrap();
    let pipeline = assemble(
        &cfg,
        Some(adds_a_skill()),
        fake_graph_storage(),
        Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
        Arc::new(crate::test_support::MemoryReviews::default()),
        crate::test_support::deletion_tables(&db),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
        Arc::new(crate::test_support::NoTenantResolver),
    )
    .expect("the pipeline assembles");
    let worker = pipeline.worker.expect("processing is wired");
    let intake = IntakeFixture {
        hand_off_to: Some(Arc::clone(&pipeline.hand_off)),
        ..IntakeFixture::default()
    }
    .build(&db);
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let cancel = CancellationToken::new();
    let running = tokio::spawn(worker.run(cancel.clone()));

    let outcome = intake
        .submit(
            &in_tenant(Uuid::new_v4()),
            tenant,
            chat_record(subject, "chat/t/1", "v1"),
        )
        .await
        .expect("intake answers");
    assert_eq!(outcome, IntakeOutcome::Received);

    let reader = in_tenant(tenant);
    let read_back = async {
        loop {
            let view = pipeline.profile.read(&reader, None, subject).await.unwrap();
            if !view.categories.is_empty() {
                return view;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let view = tokio::time::timeout(Duration::from_secs(30), read_back)
        .await
        .expect("the record is processed");
    cancel.cancel();
    running.await.unwrap();

    // The subject deletes the fact through Subject Control, over the same
    // writer and graph storage, and the profile read no longer serves it.
    let owner = crate::test_support::context_of(subject, tenant);
    let own = pipeline
        .subject_control
        .view(&owner, subject, &toolkit_odata::ODataQuery::new())
        .await
        .expect("the subject's view");
    let [stored] = own.page.items.as_slice() else {
        panic!("one fact in the subject's view, got {own:?}");
    };
    pipeline
        .subject_control
        .delete(&owner, subject, &stored.fact.node_key)
        .await
        .expect("the subject deletes its fact");
    let after = pipeline.profile.read(&reader, None, subject).await.unwrap();
    assert!(
        after.categories.is_empty(),
        "deleted, so not served: {after:?}"
    );
    let own_after = pipeline
        .subject_control
        .view(&owner, subject, &toolkit_odata::ODataQuery::new())
        .await
        .unwrap();
    assert!(own_after.page.items.is_empty(), "{own_after:?}");
    assert!(
        own_after.version > own.version,
        "the delete moved the profile's version on: {:?} then {:?}",
        own.version,
        own_after.version
    );

    let [skills] = view.categories.as_slice() else {
        panic!("one category, got {view:?}");
    };
    assert_eq!(skills.category, Category::PROJECT);
    let [fact] = skills.facts.as_slice() else {
        panic!("one fact, got {:?}", skills.facts);
    };
    assert_eq!(fact.property, "project");
    assert_eq!(fact.value, json!({ "name": "Rust" }));
    assert_eq!(fact.origin, OriginKind::Connector);
    assert_eq!(
        fact.produced_at,
        Some(OffsetDateTime::parse("2026-09-10T09:13:05Z", &Rfc3339).unwrap()),
        "the record's observed_at"
    );
    assert!(
        logs_contain("no sensitive-data check"),
        "the operator is told that no check runs"
    );
}

/// A model whose call panics; it counts its calls.
#[tokio::test]
async fn manage_facts_gets_the_manage_budget() {
    let db = inmem_db().await;
    let cfg = ConstructConfig {
        processing_identity: Some(Uuid::new_v4()),
        processing: unchecked_processing(),
        profile_category_check: Some(CategoryCheckConfig::OneCall),
        ..ConstructConfig::default()
    }
    .validated()
    .unwrap();

    let pipeline = assemble(
        &cfg,
        Some(adds_a_skill()),
        fake_graph_storage(),
        Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
        Arc::new(crate::test_support::MemoryReviews::default()),
        crate::test_support::deletion_tables(&db),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
        Arc::new(crate::test_support::NoTenantResolver),
    )
    .expect("the pipeline assembles");

    assert!(pipeline.worker.is_some(), "processing is wired");
    assert_eq!(pipeline.manage.budget, MANAGE_BUDGET);
}

#[derive(Default)]
struct Panics(std::sync::atomic::AtomicUsize);

#[async_trait]
impl ModelClient for Panics {
    async fn complete(
        &self,
        _ctx: &SecurityContext,
        _request: &ModelRequest,
    ) -> Result<ModelResponse, ModelError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        panic!("the model client broke")
    }
}

#[tracing_test::traced_test]
#[tokio::test]
async fn a_record_whose_processing_panics_is_dropped_and_the_worker_goes_on() {
    let db = inmem_db().await;
    let model = Arc::new(Panics::default());
    let cfg = ConstructConfig {
        processing_identity: Some(Uuid::new_v4()),
        processing: unchecked_processing(),
        profile_category_check: Some(CategoryCheckConfig::OneCall),
        ..ConstructConfig::default()
    }
    .validated()
    .unwrap();
    let pipeline = assemble(
        &cfg,
        Some(Arc::clone(&model) as _),
        fake_graph_storage(),
        Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
        Arc::new(crate::test_support::MemoryReviews::default()),
        crate::test_support::deletion_tables(&db),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
        Arc::new(crate::test_support::NoTenantResolver),
    )
    .expect("the pipeline assembles");
    let worker = pipeline.worker.expect("processing is wired");
    let load = worker.load();
    let intake = IntakeFixture {
        hand_off_to: Some(Arc::clone(&pipeline.hand_off)),
        ..IntakeFixture::default()
    }
    .build(&db);
    let cancel = CancellationToken::new();
    let running = tokio::spawn(worker.run(cancel.clone()));

    intake
        .submit(
            &in_tenant(Uuid::new_v4()),
            Uuid::new_v4(),
            chat_record(Uuid::new_v4(), "chat/t/1", "v1"),
        )
        .await
        .expect("intake answers");
    let dropped = async {
        while !logs_contain("processing a record panicked") || load.in_flight() > 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), dropped)
        .await
        .expect("the panic is caught");

    assert!(logs_contain("the model client broke"));
    assert!(logs_contain(r#"stage="worker""#));
    assert!(logs_contain(r#"reason="processing_failed""#));
    // The worker goes on: a second record reaches the model too.
    intake
        .submit(
            &in_tenant(Uuid::new_v4()),
            Uuid::new_v4(),
            chat_record(Uuid::new_v4(), "chat/t/2", "v1"),
        )
        .await
        .expect("intake answers");
    let second = async {
        while model.0.load(std::sync::atomic::Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), second)
        .await
        .expect("the worker takes the next record after a panic");
    cancel.cancel();
    running.await.unwrap();
}

#[tracing_test::traced_test]
#[tokio::test]
async fn without_a_model_records_are_dropped_with_an_event_and_no_worker_runs() {
    let db = inmem_db().await;
    let cfg = ConstructConfig {
        processing_identity: Some(Uuid::new_v4()),
        processing: unchecked_processing(),
        profile_category_check: Some(CategoryCheckConfig::OneCall),
        ..ConstructConfig::default()
    }
    .validated()
    .unwrap();
    let pipeline = assemble(
        &cfg,
        None,
        fake_graph_storage(),
        Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
        Arc::new(crate::test_support::MemoryReviews::default()),
        crate::test_support::deletion_tables(&db),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
        Arc::new(crate::test_support::NoTenantResolver),
    )
    .unwrap();
    let intake = IntakeFixture {
        hand_off_to: Some(Arc::clone(&pipeline.hand_off)),
        ..IntakeFixture::default()
    }
    .build(&db);

    let outcome = intake
        .submit(
            &in_tenant(Uuid::new_v4()),
            Uuid::new_v4(),
            chat_record(Uuid::new_v4(), "chat/t/1", "v1"),
        )
        .await
        .unwrap();

    assert!(pipeline.worker.is_none());
    assert_eq!(outcome, IntakeOutcome::Received);
    assert!(logs_contain("event=\"record_dropped\""));
    assert!(logs_contain("reason=\"no_planner\""));
    assert!(
        logs_contain("partly configured"),
        "a model is missing, so it warns"
    );
}

#[tokio::test]
async fn without_a_processing_identity_the_profile_read_is_not_configured() {
    let db = inmem_db().await;

    let pipeline = assemble(
        &ConstructConfig::default().validated().unwrap(),
        Some(adds_a_skill()),
        fake_graph_storage(),
        Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
        Arc::new(crate::test_support::MemoryReviews::default()),
        crate::test_support::deletion_tables(&db),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
        Arc::new(crate::test_support::NoTenantResolver),
    )
    .unwrap();
    let tenant = Uuid::new_v4();

    let result = pipeline
        .profile
        .read(&in_tenant(tenant), None, Uuid::new_v4())
        .await;

    assert!(
        pipeline.worker.is_none(),
        "no processing without an identity"
    );
    assert!(
        matches!(
            result,
            Err(crate::domain::error::DomainError::NotConfigured(_))
        ),
        "{result:?}"
    );
}

/// [`AllowResolver`] that records the category each request asks about.
#[derive(Default)]
struct CategoriesAsked(Mutex<Vec<Option<String>>>);

#[async_trait]
impl AuthZResolverApi for CategoriesAsked {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        if request.resource.resource_type == crate::domain::profile_read::PROFILE_TYPE {
            self.0.lock().unwrap().push(
                request
                    .resource
                    .properties
                    .get(crate::domain::profile_read::CATEGORY_PROPERTY)
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
            );
        }
        AllowResolver.evaluate(ctx, request).await
    }
}

#[tokio::test]
async fn the_configured_category_check_reaches_the_profile_read() {
    for (check, per_category) in [
        (CategoryCheckConfig::OneCall, 0),
        (CategoryCheckConfig::PerCategory, Category::all().len()),
    ] {
        let db = inmem_db().await;
        let asked = Arc::new(CategoriesAsked::default());
        let cfg = ConstructConfig {
            processing_identity: Some(Uuid::new_v4()),
            processing: unchecked_processing(),
            profile_category_check: Some(check),
            ..ConstructConfig::default()
        }
        .validated()
        .unwrap();
        let pipeline = assemble(
            &cfg,
            None,
            fake_graph_storage(),
            Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
            Arc::new(crate::test_support::MemoryReviews::default()),
            crate::test_support::deletion_tables(&db),
            PolicyEnforcer::new(Arc::clone(&asked) as Arc<dyn AuthZResolverApi>),
            Arc::new(crate::test_support::NoTenantResolver),
        )
        .unwrap();

        pipeline
            .profile
            .read(&in_tenant(Uuid::new_v4()), None, Uuid::new_v4())
            .await
            .unwrap();

        let asked = asked.0.lock().unwrap().clone();
        assert_eq!(
            asked.iter().filter(|category| category.is_some()).count(),
            per_category,
            "{check:?}: {asked:?}"
        );
    }
}

#[tokio::test]
async fn without_a_processing_identity_even_an_empty_profile_is_not_configured() {
    let db = inmem_db().await;
    let pipeline = assemble(
        &ConstructConfig::default().validated().unwrap(),
        None,
        fake_graph_storage(),
        // Personalization off by default: the read would serve nothing.
        Arc::new(settings_service(&db, Arc::new(AllowResolver), false)),
        Arc::new(crate::test_support::MemoryReviews::default()),
        crate::test_support::deletion_tables(&db),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
        Arc::new(crate::test_support::NoTenantResolver),
    )
    .unwrap();

    let result = pipeline
        .profile
        .read(&in_tenant(Uuid::new_v4()), None, Uuid::new_v4())
        .await;

    assert!(
        matches!(
            result,
            Err(crate::domain::error::DomainError::NotConfigured(_))
        ),
        "{result:?}"
    );
}

#[tracing_test::traced_test]
#[tokio::test]
async fn without_the_opt_in_to_run_unchecked_no_worker_runs_and_it_warns() {
    let db = inmem_db().await;
    let cfg = ConstructConfig {
        processing_identity: Some(Uuid::new_v4()),
        profile_category_check: Some(CategoryCheckConfig::OneCall),
        ..ConstructConfig::default()
    }
    .validated()
    .unwrap();

    let pipeline = assemble(
        &cfg,
        Some(adds_a_skill()),
        fake_graph_storage(),
        Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
        Arc::new(crate::test_support::MemoryReviews::default()),
        crate::test_support::deletion_tables(&db),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
        Arc::new(crate::test_support::NoTenantResolver),
    )
    .unwrap();

    assert!(
        pipeline.worker.is_none(),
        "an identity and a model are not enough while no sensitive-data check exists"
    );
    assert!(logs_contain("run_without_sensitive_data_checks"));
}

#[tokio::test]
async fn without_a_model_the_subject_still_deletes_its_facts() {
    use crate::domain::planner::processing::{PlanWriter as _, ProcessingContexts as _, Written};
    use crate::infra::processing_contexts::ServiceIdentity;
    use crate::infra::storage::graph::root::PayloadCounter;
    use crate::infra::storage::graph::types::GraphTypes;
    use crate::infra::storage::graph::writer::{GraphPlanWriter, SoftDelete, WriterPorts};

    let db = inmem_db().await;
    let identity = Uuid::new_v4();
    let cfg = ConstructConfig {
        processing_identity: Some(identity),
        profile_category_check: Some(CategoryCheckConfig::OneCall),
        ..ConstructConfig::default()
    }
    .validated()
    .unwrap();
    let graph = fake_graph_storage();
    let pipeline = assemble(
        &cfg,
        None,
        Arc::clone(&graph),
        Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
        Arc::new(crate::test_support::MemoryReviews::default()),
        crate::test_support::deletion_tables(&db),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
        Arc::new(crate::test_support::NoTenantResolver),
    )
    .unwrap();
    assert!(pipeline.worker.is_none(), "no model, no processing");
    // A fact stored earlier, by a deployment that had a model.
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let (construct, _) = ServiceIdentity::new(identity)
        .for_tenant(tenant)
        .await
        .unwrap();
    let mut plan = crate::test_support::connector_plan(vec![crate::test_support::add_value(
        "project",
        "project",
        json!({ "name": "Rust" }),
    )]);
    plan.tenant_id = tenant;
    plan.subject_id = subject;
    plan.profile_version = None;
    let writer = GraphPlanWriter::new(WriterPorts {
        client: Arc::clone(&graph),
        types: Arc::new(GraphTypes::new(Arc::clone(&graph))),
        version: Arc::new(PayloadCounter),
        removal: Arc::new(SoftDelete::new(Arc::clone(&graph))),
        generations: Arc::new(crate::infra::storage::graph::root::FirstGeneration),
        tenants: Arc::new(crate::test_support::MemoryDeletionStore::default()),
    });
    assert_eq!(
        writer.write(&construct, &plan).await.unwrap(),
        Written::Stored
    );
    let owner = crate::test_support::context_of(subject, tenant);
    let everything = toolkit_odata::ODataQuery::new();
    let before = pipeline
        .subject_control
        .view(&owner, subject, &everything)
        .await
        .unwrap();
    let [fact] = before.page.items.as_slice() else {
        panic!("one fact, got {before:?}");
    };

    pipeline
        .subject_control
        .delete(&owner, subject, &fact.fact.node_key)
        .await
        .expect("a delete needs no model");

    let after = pipeline
        .subject_control
        .view(&owner, subject, &everything)
        .await
        .unwrap();
    assert!(after.page.items.is_empty(), "{after:?}");
}

#[tokio::test]
async fn a_correction_needs_the_opt_in_to_store_values_unchecked() {
    use crate::domain::error::DomainError;
    use crate::domain::profile::Category;
    use crate::domain::review_queue::Resolution;
    use crate::domain::subject_settings::SubjectKey;

    let db = inmem_db().await;
    let resolve = |unchecked: bool| {
        let db = db.clone();
        async move {
            let cfg = ConstructConfig {
                processing_identity: Some(Uuid::new_v4()),
                processing: crate::config::ProcessingConfig {
                    run_without_sensitive_data_checks: unchecked,
                    ..crate::config::ProcessingConfig::default()
                },
                profile_category_check: Some(CategoryCheckConfig::OneCall),
                ..ConstructConfig::default()
            }
            .validated()
            .unwrap();
            let key = SubjectKey {
                tenant_id: Uuid::new_v4(),
                subject_id: Uuid::new_v4(),
            };
            let reviews = Arc::new(crate::test_support::MemoryReviews::default());
            let request = reviews.open_on_at(
                key,
                &crate::test_support::stored_fact(
                    Category::PROJECT,
                    "project",
                    json!({ "name": "Rust" }),
                    10,
                ),
                100,
            );
            let pipeline = assemble(
                &cfg,
                None,
                fake_graph_storage(),
                Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
                reviews,
                crate::test_support::deletion_tables(&db),
                PolicyEnforcer::new(Arc::new(AllowResolver)),
                Arc::new(crate::test_support::NoTenantResolver),
            )
            .unwrap();
            pipeline
                .review_queue
                .resolve(
                    &crate::test_support::context_of(Uuid::new_v4(), key.tenant_id),
                    request.id,
                    Resolution::Corrected {
                        property: "project".to_owned(),
                        value: json!({ "name": "Go" }),
                        reason: None,
                    },
                )
                .await
        }
    };

    let without = resolve(false).await;
    let with = resolve(true).await;

    assert!(
        matches!(without, Err(DomainError::NotConfigured(_))),
        "{without:?}"
    );
    assert!(
        with.is_ok(),
        "with the opt-in it runs; the fact is not in graph storage, so it closes as deleted: {with:?}"
    );
}

#[tokio::test]
async fn an_administrators_add_needs_the_opt_in_to_store_values_unchecked() {
    use crate::domain::error::DomainError;

    let db = inmem_db().await;
    let add = |unchecked: bool| {
        let db = db.clone();
        async move {
            let cfg = ConstructConfig {
                processing_identity: Some(Uuid::new_v4()),
                processing: crate::config::ProcessingConfig {
                    run_without_sensitive_data_checks: unchecked,
                    ..crate::config::ProcessingConfig::default()
                },
                profile_category_check: Some(CategoryCheckConfig::OneCall),
                ..ConstructConfig::default()
            }
            .validated()
            .unwrap();
            let pipeline = assemble(
                &cfg,
                None,
                fake_graph_storage(),
                Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
                Arc::new(crate::test_support::MemoryReviews::default()),
                crate::test_support::deletion_tables(&db),
                PolicyEnforcer::new(Arc::new(AllowResolver)),
                Arc::new(crate::test_support::NoTenantResolver),
            )
            .unwrap();
            pipeline
                .admin_facts
                .add(
                    &crate::test_support::context_of(Uuid::new_v4(), Uuid::new_v4()),
                    Uuid::new_v4(),
                    "project",
                    &json!({ "name": "Go" }),
                )
                .await
        }
    };

    let without = add(false).await;
    let with = add(true).await;

    assert!(
        matches!(without, Err(DomainError::NotConfigured(_))),
        "{without:?}"
    );
    assert!(
        !matches!(with, Err(DomainError::NotConfigured(_))),
        "with the opt-in the add runs: {with:?}"
    );
}

/// The model adds one skill and completes the plan, for each of two records.
fn adds_a_skill_twice() -> Arc<Scripted> {
    let once = adds_a_skill();
    let answers: Vec<ModelResponse> = once.0.lock().unwrap().iter().cloned().collect();
    Arc::new(Scripted(Mutex::new(
        answers.iter().chain(answers.iter()).cloned().collect(),
    )))
}

#[tokio::test]
async fn an_erased_subject_is_served_nothing_at_once_keeps_only_its_setting_and_later_starts_anew()
{
    use crate::domain::deletion::DeletionStore;
    use crate::domain::subject_settings::{SubjectKey, SubjectSettings};

    let db = inmem_db().await;
    let cfg = ConstructConfig {
        processing_identity: Some(Uuid::new_v4()),
        processing: unchecked_processing(),
        profile_category_check: Some(CategoryCheckConfig::OneCall),
        ..ConstructConfig::default()
    }
    .validated()
    .unwrap();
    let settings = Arc::new(settings_service(&db, Arc::new(AllowResolver), true));
    let graph = fake_graph_storage();
    let pipeline = assemble(
        &cfg,
        Some(adds_a_skill_twice()),
        Arc::clone(&graph),
        Arc::clone(&settings) as _,
        Arc::new(crate::test_support::MemoryReviews::default()),
        crate::test_support::deletion_tables(&db),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
        Arc::new(crate::test_support::NoTenantResolver),
    )
    .expect("the pipeline assembles");
    let worker = pipeline.worker.expect("processing is wired");
    let intake = IntakeFixture {
        hand_off_to: Some(Arc::clone(&pipeline.hand_off)),
        generations_from: Some(Arc::clone(&pipeline.generations)),
        ..IntakeFixture::default()
    }
    .build(&db);
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let key = SubjectKey {
        tenant_id: tenant,
        subject_id: subject,
    };
    let (connector, reader, owner) = (
        in_tenant(Uuid::new_v4()),
        in_tenant(tenant),
        crate::test_support::context_of(subject, tenant),
    );
    let cancel = CancellationToken::new();
    let running = tokio::spawn(worker.run(cancel.clone()));
    let served = |provenance: &'static str| {
        let (intake, connector, reader, profile) =
            (&intake, &connector, &reader, &pipeline.profile);
        async move {
            let outcome = intake
                .submit(connector, tenant, chat_record(subject, provenance, "v1"))
                .await
                .expect("intake answers");
            assert_eq!(outcome, IntakeOutcome::Received);
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    let view = profile.read(reader, None, subject).await.unwrap();
                    if !view.categories.is_empty() {
                        return view;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("the record is processed")
        }
    };
    served("chat/t/1").await;
    let before = pipeline
        .subject_control
        .view(&owner, subject, &toolkit_odata::ODataQuery::new())
        .await
        .unwrap();
    let old_facts: Vec<String> = before
        .page
        .items
        .iter()
        .map(|item| item.fact.node_key.0.clone())
        .collect();
    assert_eq!(old_facts.len(), 1, "{before:?}");
    let first_root = crate::infra::storage::graph::root::generation_root(&key, 0);
    assert!(
        graph.get_node(&reader, &first_root.0, None).await.is_ok(),
        "the first profile has its root"
    );

    // The subject asked an hour ago, and that run stopped; it asks again.
    crate::test_support::deletion_tables(&db)
        .mark(
            &toolkit_security::AccessScope::for_tenant(tenant),
            key,
            OffsetDateTime::now_utc() - time::Duration::hours(1),
        )
        .await
        .unwrap();
    let marked = pipeline.erasure.request(&owner, subject).await.unwrap();
    assert_eq!(
        marked,
        SubjectSettings {
            personalization_enabled: false,
            erasure_in_progress: true,
        }
    );
    let at_once = pipeline.profile.read(&reader, None, subject).await.unwrap();
    assert!(at_once.categories.is_empty(), "nothing is served at once");
    let refused = intake
        .submit(&connector, tenant, chat_record(subject, "chat/t/2", "v1"))
        .await;
    let Err(crate::domain::error::DomainError::Refused(refusal)) = refused else {
        panic!("a record is refused meanwhile: {refused:?}");
    };
    assert_eq!(
        refusal.reason,
        crate::domain::refusal::RefusalReason::ErasureInProgress
    );

    // The leader's job runs the stopped erasure to its end.
    pipeline.deletion_job.run_once().await;
    let own = pipeline
        .subject_control
        .view(&owner, subject, &toolkit_odata::ODataQuery::new())
        .await
        .unwrap();
    assert!(own.page.items.is_empty(), "no fact is left: {own:?}");
    assert_eq!(own.version, None, "nor a profile root");
    // The view reads the new generation; graph storage shows the old one is
    // gone too.
    for node_key in old_facts.iter().chain([&first_root.0]) {
        let gone = graph.get_node(&reader, node_key, None).await;
        assert!(
            matches!(gone, Err(CanonicalError::NotFound { .. })),
            "{node_key} is removed from graph storage: {gone:?}"
        );
    }
    assert_eq!(
        settings.owner_settings(&owner, subject).await.unwrap(),
        SubjectSettings {
            personalization_enabled: false,
            erasure_in_progress: false,
        },
        "only the setting, off, is kept"
    );
    assert_eq!(
        crate::test_support::stored_record_ids(&db).await,
        0,
        "no record identity is left"
    );

    // The subject turns personalization on again; a new record starts a new
    // profile, under a root of its own.
    settings
        .set_personalization(&owner, subject, true)
        .await
        .unwrap();
    let anew = served("chat/t/3").await;
    let [skills] = anew.categories.as_slice() else {
        panic!("one category, got {anew:?}");
    };
    assert_eq!(skills.facts.len(), 1);
    cancel.cancel();
    running.await.unwrap();
}

#[tokio::test]
async fn a_tenant_the_platform_deleted_leaves_construct_and_graph_storage_through_the_assembled_job()
 {
    use crate::domain::deletion::DeletionStore;
    use crate::domain::subject_settings::SubjectKey;
    use crate::domain::tenant_exit::TenantStatus;

    let db = inmem_db().await;
    let cfg = ConstructConfig {
        processing_identity: Some(Uuid::new_v4()),
        profile_category_check: Some(CategoryCheckConfig::OneCall),
        ..ConstructConfig::default()
    }
    .validated()
    .unwrap();
    let (graph, graph_store) = crate::test_support::fake_graph_storage_where_every_tenant_is(true);
    let statuses = Arc::new(crate::test_support::ScriptedStatuses::default());
    let tables = crate::test_support::deletion_tables(&db);
    let pipeline = assemble(
        &cfg,
        None,
        graph,
        Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
        Arc::new(crate::test_support::MemoryReviews::default()),
        Arc::clone(&tables),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
        Arc::clone(&statuses) as _,
    )
    .expect("the pipeline assembles");
    let (leaving, staying) = (
        SubjectKey {
            tenant_id: Uuid::new_v4(),
            subject_id: Uuid::new_v4(),
        },
        SubjectKey {
            tenant_id: Uuid::new_v4(),
            subject_id: Uuid::new_v4(),
        },
    );
    statuses.set(leaving.tenant_id, TenantStatus::Deleted);
    for key in [leaving, staying] {
        tables
            .mark(
                &toolkit_security::AccessScope::for_tenant(key.tenant_id),
                key,
                OffsetDateTime::now_utc(),
            )
            .await
            .unwrap();
    }
    let marked = |key: SubjectKey| {
        let tables = Arc::clone(&tables);
        async move {
            tables
                .marked(
                    &toolkit_security::AccessScope::for_tenant(key.tenant_id),
                    key,
                )
                .await
                .unwrap()
        }
    };

    // Graph storage fences the tenant and starts removing its graph; the
    // tenant's rows here go, and it stays kept until the graph is gone.
    pipeline.deletion_job.run_once().await;
    assert_eq!(
        marked(leaving).await,
        None,
        "the leaving tenant's rows are gone"
    );
    assert!(
        marked(staying).await.is_some(),
        "another tenant keeps its rows"
    );
    assert!(tables.tenants().await.unwrap().contains(&leaving.tenant_id));

    crate::test_support::run_graph_offboarding(&graph_store).await;
    pipeline.deletion_job.run_once().await;
    let kept = tables.tenants().await.unwrap();
    assert!(
        !kept.contains(&leaving.tenant_id),
        "the tenant is no longer kept"
    );
    assert!(kept.contains(&staying.tenant_id));
}

/// A model that learns the story's message: Priya works on the project and promised to send a script.
fn learns_priya() -> Arc<Scripted> {
    let call = |id: &str, name: &str, arguments: serde_json::Value| ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments,
    };
    Arc::new(Scripted(Mutex::new(VecDeque::from([
        answer(ModelOutput::ToolCalls(vec![
            call(
                "c1",
                "add",
                json!({ "kind": "person", "value": { "name": "Priya Shah", "space": "work" }, "confidence": 1.0 }),
            ),
            call(
                "c2",
                "add",
                json!({ "kind": "project", "value": { "name": "Construct Browser onboarding" }, "confidence": 1.0 }),
            ),
            call(
                "c3",
                "link",
                json!({ "kind": "works_on", "from": 1, "to": 2, "confidence": 1.0 }),
            ),
            call(
                "c4",
                "add",
                json!({
                    "kind": "commitment",
                    "value": { "kind": "promise", "text": "send the interview script" },
                    "owner": 2,
                    "confidence": 1.0
                }),
            ),
            call(
                "c5",
                "link",
                json!({ "kind": "owner", "from": 3, "to": 1, "confidence": 1.0 }),
            ),
        ])),
        answer(ModelOutput::Text(
            "Added Priya, the project and the promise.".to_owned(),
        )),
    ]))))
}

#[tokio::test]
async fn a_message_is_learned_as_linked_nodes_shown_in_one_call_and_forgotten_with_what_the_node_owns()
 {
    let db = inmem_db().await;
    let cfg = ConstructConfig {
        processing_identity: Some(Uuid::new_v4()),
        processing: unchecked_processing(),
        profile_category_check: Some(CategoryCheckConfig::OneCall),
        ..ConstructConfig::default()
    }
    .validated()
    .unwrap();
    let pipeline = assemble(
        &cfg,
        Some(learns_priya()),
        fake_graph_storage(),
        Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
        Arc::new(crate::test_support::MemoryReviews::default()),
        crate::test_support::deletion_tables(&db),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
        Arc::new(crate::test_support::NoTenantResolver),
    )
    .expect("the pipeline assembles");
    let worker = pipeline.worker.expect("processing is wired");
    let intake = IntakeFixture {
        hand_off_to: Some(Arc::clone(&pipeline.hand_off)),
        ..IntakeFixture::default()
    }
    .build(&db);
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let cancel = CancellationToken::new();
    let running = tokio::spawn(worker.run(cancel.clone()));
    intake
        .submit(
            &in_tenant(Uuid::new_v4()),
            tenant,
            chat_record(subject, "chat/t/1", "v1"),
        )
        .await
        .expect("intake answers");

    let reader = in_tenant(tenant);
    let read_back = async {
        loop {
            let view = pipeline.profile.read(&reader, None, subject).await.unwrap();
            if view
                .categories
                .iter()
                .map(|group| group.facts.len())
                .sum::<usize>()
                == 3
            {
                return view;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let view = tokio::time::timeout(Duration::from_secs(30), read_back)
        .await
        .expect("the record is processed");
    cancel.cancel();
    running.await.unwrap();

    // The profile shows a person, a project and a commitment.
    let kinds: Vec<&str> = view
        .categories
        .iter()
        .map(|group| group.category.name())
        .collect();
    assert_eq!(kinds, ["person", "project", "commitment"]);
    let node = |kind: &str| {
        view.categories
            .iter()
            .find(|group| group.category.name() == kind)
            .and_then(|group| group.facts.first())
            .cloned()
            .unwrap()
    };
    let (priya, project, promise) = (node("person"), node("project"), node("commitment"));
    assert_eq!(promise.owner.as_ref(), Some(&project.node_key));
    // The tool is the connector that sent the message; the message is the record.
    assert_eq!(project.learned[0].message.as_deref(), Some("chat/t/1"));

    // One call gives the project, its commitment, the person who works on it and where it was learned from.
    let details = pipeline
        .profile
        .read_node(&reader, None, subject, &project.node_key)
        .await
        .unwrap();
    assert_eq!(
        details.node.value,
        json!({ "name": "Construct Browser onboarding" })
    );
    assert_eq!(details.owned.len(), 1);
    assert_eq!(details.owned[0].node_key, promise.node_key);
    let works_on = details
        .neighbours
        .iter()
        .find(|group| group.link == "works_on")
        .expect("the person who works on it");
    assert_eq!(works_on.neighbours[0].node.node_key, priya.node_key);
    assert_eq!(details.learned_from[0].tool, project.learned[0].tool);
    assert!(details.learned_from[0].count >= 1);

    // The subject forgets the project: it goes with its commitment and its links, and Priya stays.
    let owner = crate::test_support::context_of(subject, tenant);
    pipeline
        .subject_control
        .delete(&owner, subject, &project.node_key)
        .await
        .expect("the subject forgets the project");
    let after = pipeline.profile.read(&reader, None, subject).await.unwrap();
    let left: Vec<&str> = after
        .categories
        .iter()
        .map(|group| group.category.name())
        .collect();
    assert_eq!(left, ["person"], "Priya stays: {after:?}");
    let priya_after = pipeline
        .profile
        .read_node(&reader, None, subject, &priya.node_key)
        .await
        .unwrap();
    assert!(
        priya_after.neighbours.is_empty(),
        "no link of the project is left: {priya_after:?}"
    );
}

/// A subject whose message was learned as a project that owns a commitment, over reviews kept in memory.
struct Learned {
    pipeline: super::pipeline::Pipeline,
    reviews: Arc<crate::test_support::MemoryReviews>,
    tenant: Uuid,
    subject: Uuid,
    project: crate::domain::profile_read::StoredFact,
    promise: crate::domain::profile_read::StoredFact,
}

async fn learned_project_with_a_commitment() -> Learned {
    let db = inmem_db().await;
    let cfg = ConstructConfig {
        processing_identity: Some(Uuid::new_v4()),
        processing: unchecked_processing(),
        profile_category_check: Some(CategoryCheckConfig::OneCall),
        ..ConstructConfig::default()
    }
    .validated()
    .unwrap();
    let reviews = Arc::new(crate::test_support::MemoryReviews::default());
    let mut pipeline = assemble(
        &cfg,
        Some(learns_priya()),
        fake_graph_storage(),
        Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
        Arc::clone(&reviews) as _,
        crate::test_support::deletion_tables(&db),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
        Arc::new(crate::test_support::NoTenantResolver),
    )
    .expect("the pipeline assembles");
    let worker = pipeline.worker.take().expect("processing is wired");
    let intake = IntakeFixture {
        hand_off_to: Some(Arc::clone(&pipeline.hand_off)),
        ..IntakeFixture::default()
    }
    .build(&db);
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let cancel = CancellationToken::new();
    let running = tokio::spawn(worker.run(cancel.clone()));
    intake
        .submit(
            &in_tenant(Uuid::new_v4()),
            tenant,
            chat_record(subject, "chat/t/1", "v1"),
        )
        .await
        .expect("intake answers");
    let reader = in_tenant(tenant);
    let read_back = async {
        loop {
            let view = pipeline.profile.read(&reader, None, subject).await.unwrap();
            if view.categories.iter().map(|g| g.facts.len()).sum::<usize>() == 3 {
                return view;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let view = tokio::time::timeout(Duration::from_secs(30), read_back)
        .await
        .expect("the record is processed");
    cancel.cancel();
    running.await.unwrap();
    let node = |kind: &str| {
        view.categories
            .iter()
            .find(|group| group.category.name() == kind)
            .and_then(|group| group.facts.first())
            .cloned()
            .unwrap()
    };
    let (project, promise) = (node("project"), node("commitment"));
    assert_eq!(promise.owner.as_ref(), Some(&project.node_key));

    Learned {
        pipeline,
        reviews,
        tenant,
        subject,
        project,
        promise,
    }
}

impl Learned {
    /// The subject flags `node`.
    async fn flags(&self, node: &crate::domain::profile_read::StoredFact) -> Uuid {
        let owner = crate::test_support::context_of(self.subject, self.tenant);
        self.pipeline
            .review_queue
            .flag(&owner, self.subject, &node.node_key, None)
            .await
            .expect("the subject flags the node")
            .id
    }

    /// The commitment and the project that owns it, both flagged: the commitment's request, then the project's.
    async fn flags_both(&self) -> (Uuid, Uuid) {
        (
            self.flags(&self.promise).await,
            self.flags(&self.project).await,
        )
    }

    /// Who closed the request on `node` as deleted: none when it has no request, or it is open or closed otherwise.
    fn closed_as_deleted_by(&self, node: &crate::domain::profile_read::StoredFact) -> Option<Uuid> {
        use crate::domain::review_queue::{ReviewOutcome, ReviewStatus};
        let requests = self.reviews.requests.lock().unwrap();
        let [(request, _)] = requests
            .iter()
            .filter(|(request, _)| request.fact_id == node.node_key)
            .collect::<Vec<_>>()
            .try_into()
            .ok()?;
        match &request.status {
            ReviewStatus::Closed(done) if done.outcome == ReviewOutcome::Deleted => Some(done.by),
            _ => None,
        }
    }
}

#[tokio::test]
async fn a_forget_closes_the_review_request_of_every_node_it_removes() {
    let learned = learned_project_with_a_commitment().await;
    learned.flags_both().await;
    let owner = crate::test_support::context_of(learned.subject, learned.tenant);

    learned
        .pipeline
        .subject_control
        .delete(&owner, learned.subject, &learned.project.node_key)
        .await
        .expect("the subject forgets the project");

    let closers = (
        learned.closed_as_deleted_by(&learned.promise),
        learned.closed_as_deleted_by(&learned.project),
    );
    assert_eq!(closers, (Some(learned.subject), Some(learned.subject)));
    assert_eq!(
        learned
            .reviews
            .close_open_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        2,
        "one close for the project, one for the commitment"
    );
}

#[tokio::test]
async fn an_administrators_delete_closes_the_requests_of_the_owner_and_of_what_it_owns() {
    let learned = learned_project_with_a_commitment().await;
    learned.flags_both().await;
    let administrator = Uuid::new_v4();

    learned
        .pipeline
        .admin_facts
        .delete(
            &crate::test_support::context_of(administrator, learned.tenant),
            learned.subject,
            &learned.project.node_key,
        )
        .await
        .expect("the administrator deletes the project");

    let closers = (
        learned.closed_as_deleted_by(&learned.promise),
        learned.closed_as_deleted_by(&learned.project),
    );
    assert_eq!(closers, (Some(administrator), Some(administrator)));
}

#[tokio::test]
async fn an_administrators_delete_closes_the_request_of_an_owned_node_when_the_owner_has_none() {
    let learned = learned_project_with_a_commitment().await;
    learned.flags(&learned.promise).await;
    let administrator = Uuid::new_v4();

    learned
        .pipeline
        .admin_facts
        .delete(
            &crate::test_support::context_of(administrator, learned.tenant),
            learned.subject,
            &learned.project.node_key,
        )
        .await
        .expect("the administrator deletes the project");

    assert_eq!(
        learned.closed_as_deleted_by(&learned.promise),
        Some(administrator)
    );
    assert_eq!(learned.closed_as_deleted_by(&learned.project), None);
}

#[tokio::test]
async fn a_reviewer_deleting_the_owner_closes_the_requests_of_what_it_owns() {
    use crate::domain::review_queue::Resolution;
    let learned = learned_project_with_a_commitment().await;
    let (_, project_request) = learned.flags_both().await;
    let reviewer = Uuid::new_v4();

    learned
        .pipeline
        .review_queue
        .resolve(
            &crate::test_support::context_of(reviewer, learned.tenant),
            project_request,
            Resolution::Deleted { reason: None },
        )
        .await
        .expect("the reviewer deletes the project");

    let closers = (
        learned.closed_as_deleted_by(&learned.promise),
        learned.closed_as_deleted_by(&learned.project),
    );
    assert_eq!(closers, (Some(reviewer), Some(reviewer)));
}

#[tokio::test]
async fn a_close_that_fails_for_the_owned_node_only_is_finished_by_the_repeat() {
    use crate::domain::error::DomainError;
    let learned = learned_project_with_a_commitment().await;
    learned.flags_both().await;
    let owner = crate::test_support::context_of(learned.subject, learned.tenant);
    *learned.reviews.close_open_fails_on.lock().unwrap() = Some(learned.promise.node_key.clone());

    let first = learned
        .pipeline
        .subject_control
        .delete(&owner, learned.subject, &learned.project.node_key)
        .await;

    assert!(
        matches!(first, Err(DomainError::Unavailable(_))),
        "{first:?}"
    );
    assert_eq!(
        learned.closed_as_deleted_by(&learned.project),
        Some(learned.subject),
        "the named request closed"
    );
    assert_eq!(
        learned.closed_as_deleted_by(&learned.promise),
        None,
        "still open"
    );
    *learned.reviews.close_open_fails_on.lock().unwrap() = None;

    learned
        .pipeline
        .subject_control
        .delete(&owner, learned.subject, &learned.project.node_key)
        .await
        .expect("the repeat finishes the delete, though the fact is gone");

    assert_eq!(
        learned.closed_as_deleted_by(&learned.promise),
        Some(learned.subject)
    );
}

#[tokio::test]
async fn a_delete_whose_every_close_failed_is_finished_by_its_repeat() {
    use crate::domain::error::DomainError;
    use std::sync::atomic::Ordering;
    let learned = learned_project_with_a_commitment().await;
    learned.flags_both().await;
    let owner = crate::test_support::context_of(learned.subject, learned.tenant);
    learned
        .reviews
        .close_open_fails
        .store(true, Ordering::SeqCst);

    let first = learned
        .pipeline
        .subject_control
        .delete(&owner, learned.subject, &learned.project.node_key)
        .await;
    assert!(
        matches!(first, Err(DomainError::Unavailable(_))),
        "{first:?}"
    );
    assert_eq!(learned.closed_as_deleted_by(&learned.promise), None);
    assert_eq!(learned.closed_as_deleted_by(&learned.project), None);
    learned
        .reviews
        .close_open_fails
        .store(false, Ordering::SeqCst);

    learned
        .pipeline
        .subject_control
        .delete(&owner, learned.subject, &learned.project.node_key)
        .await
        .expect("the repeat finishes the delete");

    let closers = (
        learned.closed_as_deleted_by(&learned.promise),
        learned.closed_as_deleted_by(&learned.project),
    );
    assert_eq!(closers, (Some(learned.subject), Some(learned.subject)));
}
