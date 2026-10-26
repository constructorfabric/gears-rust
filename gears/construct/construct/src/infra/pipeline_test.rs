//! The round trip through the gear's own wiring: a connector's record goes
//! through intake, the queue, the planner (over a scripted model), the checks,
//! Admission and the writer into graph storage's in-memory store, and the
//! profile read reads it back.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::pipeline::assemble;
use crate::config::ConstructConfig;
use crate::domain::model_client::{
    ModelClient, ModelError, ModelOutput, ModelRequest, ModelResponse, ToolCall, Usage,
};
use crate::domain::profile::Category;
use crate::domain::profile_read::OriginKind;
use crate::domain::record_intake::IntakeOutcome;
use crate::test_support::{
    AllowResolver, IntakeFixture, chat_record, fake_graph_storage, inmem_db, settings_service,
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
            arguments: json!({ "property": "skills", "value": { "name": "Rust" }, "confidence": 0.9 }),
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

#[tokio::test]
async fn a_record_is_planned_checked_admitted_written_and_read_back() {
    let db = inmem_db().await;
    let cfg = ConstructConfig {
        processing_identity: Some(Uuid::new_v4()),
        ..ConstructConfig::default()
    };
    let pipeline = assemble(
        &cfg,
        Some(adds_a_skill()),
        Some(fake_graph_storage()),
        Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
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

    let [skills] = view.categories.as_slice() else {
        panic!("one category, got {view:?}");
    };
    assert_eq!(skills.category, Category::Skill);
    let [fact] = skills.facts.as_slice() else {
        panic!("one fact, got {:?}", skills.facts);
    };
    assert_eq!(fact.property, "skills");
    assert_eq!(fact.value, json!({ "name": "Rust" }));
    assert_eq!(fact.origin, OriginKind::Connector);
    assert_eq!(
        fact.produced_at,
        Some(OffsetDateTime::parse("2026-09-10T09:13:05Z", &Rfc3339).unwrap()),
        "the record's observed_at"
    );
}

#[tracing_test::traced_test]
#[tokio::test]
async fn without_a_model_records_are_dropped_with_an_event_and_no_worker_runs() {
    let db = inmem_db().await;
    let cfg = ConstructConfig {
        processing_identity: Some(Uuid::new_v4()),
        ..ConstructConfig::default()
    };
    let pipeline = assemble(
        &cfg,
        None,
        Some(fake_graph_storage()),
        Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
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
async fn without_a_processing_identity_the_profile_read_is_unavailable() {
    let db = inmem_db().await;

    let pipeline = assemble(
        &ConstructConfig::default(),
        Some(adds_a_skill()),
        Some(fake_graph_storage()),
        Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
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
            Err(crate::domain::error::DomainError::Unavailable(_))
        ),
        "{result:?}"
    );
}
