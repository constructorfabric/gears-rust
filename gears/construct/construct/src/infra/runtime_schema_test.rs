//! A person's schema changed while the gear runs, through the gear's own wiring: the next message is planned,
//! written and read over the schema as it is then.

use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::PolicyEnforcer;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::pipeline::{Pipeline, assemble_with};
use super::storage::subject_schema_repo::SeaOrmSubjectSchemaRepository;
use crate::config::{CategoryCheckConfig, ConstructConfig};
use crate::domain::model_client::{ModelOutput, ModelResponse, ToolCall};
use crate::domain::profile_schema::SchemaChange;
use crate::domain::schema_service::{SchemaService, StoredSchemas};
use crate::domain::subject_settings::SubjectKey;
use crate::test_support::{
    AllowResolver, IntakeFixture, MemoryReviews, NoTenantResolver, Scripted, answer, chat_record,
    context_of, deletion_tables, fake_graph_storage, inmem_db, settings_service,
    unchecked_processing,
};

fn adds(kind: &str, value: &serde_json::Value) -> ModelResponse {
    answer(ModelOutput::ToolCalls(vec![ToolCall {
        id: "call_0".to_owned(),
        name: "add".to_owned(),
        arguments: json!({ "kind": kind, "value": value, "confidence": 0.9 }),
    }]))
}

fn complete() -> ModelResponse {
    answer(ModelOutput::Text("The plan is complete.".to_owned()))
}

fn model(answers: Vec<ModelResponse>) -> Arc<Scripted> {
    Arc::new(Scripted(std::sync::Mutex::new(answers.into())))
}

type Schemas = SchemaService<SeaOrmSubjectSchemaRepository>;

struct Gear {
    db: toolkit_db::Db,
    pipeline: Pipeline,
    schemas: Schemas,
    intake: crate::api::rest::types::ConcreteIntake,
    cancel: CancellationToken,
    running: tokio::task::JoinHandle<()>,
}

async fn gear(model: Arc<Scripted>) -> Gear {
    let db = inmem_db().await;
    let cfg = ConstructConfig {
        processing_identity: Some(Uuid::new_v4()),
        // One worker, so the messages of a test are processed in the order they are sent.
        processing: crate::config::ProcessingConfig {
            workers: std::num::NonZeroUsize::MIN,
            ..unchecked_processing()
        },
        profile_category_check: Some(CategoryCheckConfig::OneCall),
        ..ConstructConfig::default()
    }
    .validated()
    .unwrap();
    let repo = Arc::new(SeaOrmSubjectSchemaRepository::new());
    let provider = Arc::new(toolkit_db::DBProvider::new(db.clone()));
    let mut pipeline = assemble_with(
        &cfg,
        Some(model),
        fake_graph_storage(),
        Arc::new(settings_service(&db, Arc::new(AllowResolver), true)),
        Arc::new(MemoryReviews::default()),
        deletion_tables(&db),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
        Arc::new(NoTenantResolver),
        Arc::new(StoredSchemas::new(Arc::clone(&provider), Arc::clone(&repo))),
    )
    .expect("the pipeline assembles");
    let schemas = SchemaService::new(
        provider,
        repo,
        Arc::clone(&pipeline.schema_effects),
        Arc::new(crate::test_support::RecordingSchemaEvents::default()),
        PolicyEnforcer::new(Arc::new(AllowResolver)),
    );
    let worker = pipeline.worker.take().expect("processing is wired");
    let intake = IntakeFixture {
        hand_off_to: Some(Arc::clone(&pipeline.hand_off)),
        ..IntakeFixture::default()
    }
    .build(&db);
    let cancel = CancellationToken::new();
    let running = tokio::spawn(worker.run(cancel.clone()));
    Gear {
        db,
        pipeline,
        schemas,
        intake,
        cancel,
        running,
    }
}

impl Gear {
    async fn send(&self, tenant: Uuid, subject: Uuid, provenance: &str) {
        self.intake
            .submit(
                &context_of(Uuid::new_v4(), Uuid::new_v4()),
                tenant,
                chat_record(subject, provenance, "v1"),
            )
            .await
            .expect("intake answers");
    }

    /// The kinds with facts the profile read serves.
    async fn kinds(&self, tenant: Uuid, subject: Uuid) -> Vec<(String, serde_json::Value)> {
        let reader = context_of(Uuid::new_v4(), tenant);
        let view = self
            .pipeline
            .profile
            .read(&reader, None, subject)
            .await
            .unwrap();
        view.categories
            .into_iter()
            .flat_map(|group| {
                let name = group.category.name().to_owned();
                group
                    .facts
                    .into_iter()
                    .map(move |fact| (name.clone(), fact.value))
            })
            .collect()
    }

    async fn stop(self) {
        self.cancel.cancel();
        self.running.await.unwrap();
    }
}

async fn until<T>(what: &str, mut probe: impl AsyncFnMut() -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(found) = probe().await {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

use crate::test_support::add_pets as pets;

#[tokio::test]
async fn a_kind_added_without_a_restart_takes_a_node_from_the_next_message() {
    let rex = json!({ "name": "Rex", "species": "dog" });
    let model = model(vec![adds("pets", &rex), complete()]);
    let gear = gear(Arc::clone(&model)).await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());

    gear.schemas
        .change(&context_of(subject, tenant), subject, &[pets()])
        .await
        .expect("the host adds the kind");
    gear.send(tenant, subject, "chat/t/1").await;

    let found = until("the pets node", async || {
        Some(gear.kinds(tenant, subject).await).filter(|kinds| !kinds.is_empty())
    })
    .await;
    assert_eq!(found, [("pets".to_owned(), rex)]);
    gear.stop().await;
}

#[tokio::test]
async fn a_kind_the_schema_does_not_have_takes_no_node() {
    let model = model(vec![
        adds("pets", &json!({ "name": "Rex" })),
        complete(),
        adds("person", &json!({ "name": "Ann" })),
        complete(),
    ]);
    let gear = gear(Arc::clone(&model)).await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());

    // The first message names a kind the schema lacks and is refused; the second names a person. With one worker the
    // second is stored only after the first has been processed in full.
    gear.send(tenant, subject, "chat/t/1").await;
    gear.send(tenant, subject, "chat/t/2").await;
    let kinds = until("the person", async || {
        Some(gear.kinds(tenant, subject).await).filter(|kinds| !kinds.is_empty())
    })
    .await;

    assert_eq!(kinds, [("person".to_owned(), json!({ "name": "Ann" }))]);
    // Nothing was written to graph storage either: a node of "pets" that was merely hidden from the read would
    // show once the schema has the kind.
    gear.schemas
        .change(&context_of(subject, tenant), subject, &[pets()])
        .await
        .unwrap();
    assert_eq!(
        gear.kinds(tenant, subject).await,
        [("person".to_owned(), json!({ "name": "Ann" }))]
    );
    gear.stop().await;
}

#[tokio::test]
async fn a_kind_removed_without_a_restart_forgets_its_nodes_and_takes_no_more() {
    let model = model(vec![
        adds("project", &json!({ "name": "Rust" })),
        complete(),
        adds("project", &json!({ "name": "Go" })),
        complete(),
        adds("person", &json!({ "name": "Ann" })),
        complete(),
    ]);
    let gear = gear(Arc::clone(&model)).await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let owner = context_of(subject, tenant);

    gear.send(tenant, subject, "chat/t/1").await;
    until("the project", async || {
        Some(gear.kinds(tenant, subject).await).filter(|kinds| !kinds.is_empty())
    })
    .await;

    gear.schemas
        .change(
            &owner,
            subject,
            &[SchemaChange::RemoveNodeKind {
                name: "project".to_owned(),
            }],
        )
        .await
        .expect("the host removes the kind");
    assert!(
        gear.kinds(tenant, subject).await.is_empty(),
        "its nodes are gone"
    );

    // The second message names a project and is refused; the third is a person, which the schema still has. Once
    // the person is stored, the second has been processed in full, and left no project.
    gear.send(tenant, subject, "chat/t/2").await;
    gear.send(tenant, subject, "chat/t/3").await;
    let kinds = until("the person", async || {
        Some(gear.kinds(tenant, subject).await).filter(|kinds| !kinds.is_empty())
    })
    .await;
    assert_eq!(kinds, [("person".to_owned(), json!({ "name": "Ann" }))]);
    let view = gear.schemas.schema(&owner, subject).await.unwrap();
    assert!(view.schema.nodes.iter().all(|kind| kind.name != "project"));
    gear.stop().await;
}

#[tokio::test]
async fn the_nodes_of_a_removed_kind_are_forgotten_in_graph_storage_not_only_hidden() {
    let model = model(vec![
        adds("project", &json!({ "name": "Rust" })),
        complete(),
    ]);
    let gear = gear(Arc::clone(&model)).await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let owner = context_of(subject, tenant);
    gear.send(tenant, subject, "chat/t/1").await;
    until("the project", async || {
        Some(gear.kinds(tenant, subject).await).filter(|kinds| !kinds.is_empty())
    })
    .await;
    let remove = |name: &str| SchemaChange::RemoveNodeKind {
        name: name.to_owned(),
    };
    let doc = gear.schemas.schema(&owner, subject).await.unwrap().schema;
    let project = doc
        .nodes
        .iter()
        .find(|kind| kind.name == "project")
        .unwrap()
        .clone();
    gear.schemas
        .change(&owner, subject, &[remove("project")])
        .await
        .unwrap();

    // Added again, the kind is empty: a node merely hidden would come back with it.
    gear.schemas
        .change(
            &owner,
            subject,
            &[SchemaChange::AddNodeKind { kind: project }],
        )
        .await
        .expect("the same kind is added again");

    assert!(gear.kinds(tenant, subject).await.is_empty());
    gear.stop().await;
}

/// A schema without "project", stored the way a change stores it but without the removal that follows: the state a
/// crash or a failed removal leaves, with the nodes of the kind still in graph storage.
async fn store_without(gear: &Gear, tenant: Uuid, subject: Uuid, kind: &str) {
    use crate::domain::profile_schema::{SchemaChange, SubjectSchemaRepository, apply};
    let owner = context_of(subject, tenant);
    let before = gear.schemas.schema(&owner, subject).await.unwrap();
    let next = apply(
        &before.schema,
        &[SchemaChange::RemoveNodeKind {
            name: kind.to_owned(),
        }],
    )
    .unwrap();
    let expected = (before.version > 0).then_some(before.version);
    let conn = gear.db.conn().expect("connection");
    let stored = SeaOrmSubjectSchemaRepository::new()
        .store(
            &conn,
            &toolkit_security::AccessScope::allow_all(),
            SubjectKey {
                tenant_id: tenant,
                subject_id: subject,
            },
            expected,
            &next,
            0,
        )
        .await
        .unwrap();
    assert!(matches!(
        stored,
        crate::domain::profile_schema::SchemaStore::Stored(_)
    ));
}

#[tokio::test]
async fn nodes_of_a_kind_the_schema_lost_are_hidden_from_every_read_and_swept_by_the_next_change() {
    let model = model(vec![
        adds("project", &json!({ "name": "Rust" })),
        complete(),
        adds("person", &json!({ "name": "Ann" })),
        complete(),
    ]);
    let gear = gear(Arc::clone(&model)).await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let owner = context_of(subject, tenant);
    gear.send(tenant, subject, "chat/t/1").await;
    until("the project", async || {
        Some(gear.kinds(tenant, subject).await).filter(|kinds| !kinds.is_empty())
    })
    .await;

    store_without(&gear, tenant, subject, "project").await;

    // Hidden: neither the profile read nor the subject's own view fails or shows the node.
    assert!(gear.kinds(tenant, subject).await.is_empty());
    let own = gear
        .pipeline
        .subject_control
        .view(&owner, subject, &toolkit_odata::ODataQuery::new())
        .await
        .expect("the subject's view reads a profile with a node of a lost kind");
    assert!(own.page.items.is_empty(), "{own:?}");

    // A message to this person is planned, written and read over a profile that holds the node of the lost kind:
    // the planner's read skips it instead of failing, so the record is processed.
    gear.send(tenant, subject, "chat/t/2").await;
    let kinds = until("the person", async || {
        Some(gear.kinds(tenant, subject).await).filter(|kinds| !kinds.is_empty())
    })
    .await;
    assert_eq!(kinds, [("person".to_owned(), json!({ "name": "Ann" }))]);

    // Swept: a change that changes nothing forgets it, so the kind added again is empty.
    gear.schemas.change(&owner, subject, &[]).await.unwrap();
    let project = crate::domain::profile_schema::SchemaChange::AddNodeKind {
        kind: construct_sdk::schema_doc::SchemaDoc::of(
            &construct_sdk::linked_schema::default_schema(),
        )
        .nodes
        .into_iter()
        .find(|kind| kind.name == "project")
        .unwrap(),
    };
    gear.schemas
        .change(&owner, subject, &[project])
        .await
        .unwrap();
    assert_eq!(
        gear.kinds(tenant, subject).await,
        [("person".to_owned(), json!({ "name": "Ann" }))],
        "the swept node does not come back with its kind"
    );
    gear.stop().await;
}
