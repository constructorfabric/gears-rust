//! The writer and the reader against graph storage's own service over its
//! in-memory store: real type registration, schema validation, compare-and-set,
//! idempotency receipts and soft delete.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use graph_storage::config::GraphStorageConfig;
use graph_storage::domain::embedding::{EmbeddingCoordinator, SpaceState};
use graph_storage::domain::local_client::GraphStorageLocalClient;
use graph_storage::domain::service::GraphServices;
use graph_storage::infra::embedding::fake::FakeEmbeddingProvider;
use graph_storage::infra::fake_store::{FakeGraphEngine, FakeGraphStore};
use graph_storage_sdk::GraphStorageClientV1;
use graph_storage_sdk::models::{
    DeleteOutcome, EdgeKey, GraphRevision, GtsTypeId, IngestOutcome, IngestRequest,
    NeighborhoodRequest, NodeKey, NodeRow, NodeView, Page, SearchRequest, SearchResponse,
    TraversalResponse, TraverseRequest, TypeQuery, TypeRecord, TypeRegistration,
};
use serde_json::{Value, json};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use super::reader::GraphProfileSource;
use super::root::{PayloadCounter, Root, root_key};
use super::types::GraphTypes;
use super::writer::{FactRemoval, GraphPlanWriter, SoftDelete, WriterPorts};
use crate::domain::error::DomainError;
use crate::domain::plan::{Change, NodeKey as FactKey, Plan, Property, Step};
use crate::domain::profile::{Category, PlanWriter, Profile, ProfileSource, Written};
use crate::test_support::{add_value, connector_plan};

/// Admits every call, scoped to the caller's own tenant, like a real PDP.
struct AllowInOwnTenant;

#[async_trait]
impl AuthZResolverApi for AllowInOwnTenant {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let tenant = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(Value::as_str)
            .and_then(|raw| Uuid::parse_str(raw).ok())
            .unwrap_or_else(Uuid::nil);
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [tenant],
                    ))],
                }],
                ..Default::default()
            },
        })
    }
}

/// Graph storage's real service over its in-memory store.
fn graph_storage() -> Arc<dyn GraphStorageClientV1> {
    let store = Arc::new(FakeGraphStore::new());
    let engine = Arc::new(FakeGraphEngine::new(Arc::clone(&store)));
    let embedding = EmbeddingCoordinator::new(
        Arc::new(FakeEmbeddingProvider::new(384)),
        SpaceState::Active { epoch: 1 },
        8 * 1024,
    );
    let services = Arc::new(GraphServices::new(
        GraphStorageConfig::default()
            .validated()
            .expect("the default configuration is valid"),
        store,
        engine,
        PolicyEnforcer::new(Arc::new(AllowInOwnTenant)),
        embedding,
    ));
    Arc::new(GraphStorageLocalClient::new(services))
}

/// Construct's processing identity in one tenant.
fn construct_in(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant)
        .build()
        .unwrap()
}

struct Profiles {
    client: Arc<dyn GraphStorageClientV1>,
    ctx: SecurityContext,
    tenant: Uuid,
    subject: Uuid,
}

impl Profiles {
    fn new() -> Self {
        let tenant = Uuid::new_v4();
        Self {
            client: graph_storage(),
            ctx: construct_in(tenant),
            tenant,
            subject: Uuid::new_v4(),
        }
    }

    fn writer_with(
        client: Arc<dyn GraphStorageClientV1>,
        removal: Arc<dyn FactRemoval>,
    ) -> GraphPlanWriter {
        GraphPlanWriter::new(WriterPorts {
            client: Arc::clone(&client),
            types: Arc::new(GraphTypes::new(client)),
            version: Arc::new(PayloadCounter),
            removal,
        })
    }

    fn writer(&self) -> GraphPlanWriter {
        Self::writer_with(
            Arc::clone(&self.client),
            Arc::new(SoftDelete::new(Arc::clone(&self.client))),
        )
    }

    async fn read(&self) -> Profile {
        GraphProfileSource::new(Arc::clone(&self.client), Arc::new(PayloadCounter))
            .read(&self.ctx, self.tenant, self.subject)
            .await
            .expect("the profile reads")
    }

    /// A record's plan for this subject, built on `version`.
    fn plan(&self, version: Option<u64>, steps: Vec<Step>) -> Plan {
        let mut plan = connector_plan(steps);
        plan.tenant_id = self.tenant;
        plan.subject_id = self.subject;
        plan.profile_version = version;
        plan
    }

    async fn root(&self) -> Option<Root> {
        match self
            .client
            .get_node(&self.ctx, &root_key(self.tenant, self.subject), None)
            .await
        {
            Ok(node) => Some(Root::from_payload(node.payload.as_ref()).unwrap()),
            Err(CanonicalError::NotFound { .. }) => None,
            Err(err) => panic!("root read: {err}"),
        }
    }

    async fn is_live(&self, key: &str) -> bool {
        self.client
            .get_node(&self.ctx, &key.to_owned(), None)
            .await
            .is_ok()
    }
}

fn skills(name: &str) -> Value {
    json!({ "name": name, "level": "expert" })
}

fn add_skill(name: &str) -> Step {
    add_value("skill", "skills", skills(name))
}

fn key_of(step: &Step) -> String {
    match &step.change {
        Change::AddEntity { key, .. } | Change::ReplaceEntity { key, .. } => key.0.clone(),
        other => panic!("no new key in {other:?}"),
    }
}

fn replace(old: &str, name: &str) -> Step {
    Step {
        entity_kind: "skill".to_owned(),
        change: Change::ReplaceEntity {
            old: FactKey(old.to_owned()),
            key: FactKey(format!("construct:{}", Uuid::new_v4())),
            properties: vec![Property::new("skills", skills(name))],
        },
        confidence: Some(0.9),
    }
}

fn remove(old: &str) -> Step {
    Step {
        entity_kind: "skill".to_owned(),
        change: Change::RemoveEntity {
            entity: FactKey(old.to_owned()),
        },
        confidence: Some(0.9),
    }
}

fn values(profile: &Profile) -> Vec<(Category, String, Value)> {
    let mut values: Vec<_> = profile
        .facts
        .iter()
        .map(|fact| (fact.category, fact.property.clone(), fact.value.clone()))
        .collect();
    values.sort_by_key(|(_, property, value)| format!("{property}{value}"));
    values
}

#[tokio::test]
async fn a_subject_without_a_profile_reads_empty() {
    let profiles = Profiles::new();

    assert_eq!(
        profiles.read().await,
        Profile {
            version: None,
            facts: Vec::new()
        }
    );
}

#[tokio::test]
async fn a_first_plan_creates_the_root_and_reads_back_whole() {
    let profiles = Profiles::new();
    let plan = profiles.plan(
        None,
        vec![
            add_value("identity", "full_name", json!("Ada Lovelace")),
            add_skill("Rust"),
        ],
    );

    let written = profiles.writer().write(&profiles.ctx, &plan).await.unwrap();

    assert_eq!(written, Written::Stored);
    let profile = profiles.read().await;
    assert_eq!(profile.version, Some(1));
    assert_eq!(
        values(&profile),
        [
            (
                Category::Identity,
                "full_name".to_owned(),
                json!("Ada Lovelace")
            ),
            (Category::Skill, "skills".to_owned(), skills("Rust")),
        ]
    );
}

#[tokio::test]
async fn a_replace_stores_the_new_fact_and_removes_the_old_in_one_write() {
    let profiles = Profiles::new();
    let writer = profiles.writer();
    let first = add_skill("Rust");
    let old = key_of(&first);
    writer
        .write(&profiles.ctx, &profiles.plan(None, vec![first]))
        .await
        .unwrap();

    let written = writer
        .write(
            &profiles.ctx,
            &profiles.plan(Some(1), vec![replace(&old, "Go")]),
        )
        .await
        .unwrap();

    assert_eq!(written, Written::Stored);
    let profile = profiles.read().await;
    assert_eq!(profile.version, Some(2));
    assert_eq!(
        values(&profile),
        [(Category::Skill, "skills".to_owned(), skills("Go"))]
    );
    assert!(
        !profiles.is_live(&old).await,
        "the old fact is soft-deleted"
    );
}

#[tokio::test]
async fn a_removed_fact_is_gone_from_the_next_read() {
    let profiles = Profiles::new();
    let writer = profiles.writer();
    let first = add_skill("Rust");
    let old = key_of(&first);
    writer
        .write(
            &profiles.ctx,
            &profiles.plan(None, vec![first, add_skill("Go")]),
        )
        .await
        .unwrap();

    writer
        .write(&profiles.ctx, &profiles.plan(Some(1), vec![remove(&old)]))
        .await
        .unwrap();

    assert_eq!(
        values(&profiles.read().await),
        [(Category::Skill, "skills".to_owned(), skills("Go"))]
    );
}

#[tokio::test]
async fn a_plan_built_on_an_older_profile_conflicts_and_stores_nothing() {
    let profiles = Profiles::new();
    let writer = profiles.writer();
    writer
        .write(&profiles.ctx, &profiles.plan(None, vec![add_skill("Rust")]))
        .await
        .unwrap();

    let stale_new_subject = writer
        .write(&profiles.ctx, &profiles.plan(None, vec![add_skill("Go")]))
        .await
        .unwrap();
    let stale_version = writer
        .write(
            &profiles.ctx,
            &profiles.plan(Some(7), vec![add_skill("Go")]),
        )
        .await
        .unwrap();

    assert_eq!(stale_new_subject, Written::Conflict);
    assert_eq!(stale_version, Written::Conflict);
    let profile = profiles.read().await;
    assert_eq!(profile.version, Some(1));
    assert_eq!(profile.facts.len(), 1);
}

#[tokio::test]
async fn two_plans_on_the_same_profile_never_both_land() {
    let profiles = Profiles::new();
    profiles
        .writer()
        .write(&profiles.ctx, &profiles.plan(None, vec![add_skill("Rust")]))
        .await
        .unwrap();
    let (one, two) = (profiles.writer(), profiles.writer());

    let (go, zig) = (
        profiles.plan(Some(1), vec![add_skill("Go")]),
        profiles.plan(Some(1), vec![add_skill("Zig")]),
    );

    let (first, second) = tokio::join!(
        one.write(&profiles.ctx, &go),
        two.write(&profiles.ctx, &zig),
    );

    let mut outcomes = [first.unwrap(), second.unwrap()];
    outcomes.sort_by_key(|written| *written == Written::Conflict);
    assert_eq!(outcomes, [Written::Stored, Written::Conflict]);
    let profile = profiles.read().await;
    assert_eq!(profile.version, Some(2));
    assert_eq!(profile.facts.len(), 2, "one plan only");
}

#[tokio::test]
async fn the_types_register_again_after_a_restart_without_conflict() {
    let profiles = Profiles::new();
    profiles
        .writer()
        .write(&profiles.ctx, &profiles.plan(None, vec![add_skill("Rust")]))
        .await
        .unwrap();

    // A new writer has an empty registration cache, like a restarted instance.
    let written = profiles
        .writer()
        .write(
            &profiles.ctx,
            &profiles.plan(Some(1), vec![add_skill("Go")]),
        )
        .await
        .unwrap();

    assert_eq!(written, Written::Stored);
}

/// A removal that cannot run until it is told to.
#[derive(Default)]
struct BrokenRemoval;

#[async_trait]
impl FactRemoval for BrokenRemoval {
    async fn remove(&self, _ctx: &SecurityContext, _key: &str) -> Result<(), DomainError> {
        Err(DomainError::Unavailable("graph storage is down".to_owned()))
    }
}

#[tokio::test]
async fn a_removal_that_fails_stays_listed_and_the_next_write_finishes_it() {
    let profiles = Profiles::new();
    let first = add_skill("Rust");
    let old = key_of(&first);
    profiles
        .writer()
        .write(&profiles.ctx, &profiles.plan(None, vec![first]))
        .await
        .unwrap();
    let broken = Profiles::writer_with(Arc::clone(&profiles.client), Arc::new(BrokenRemoval));

    broken
        .write(
            &profiles.ctx,
            &profiles.plan(Some(1), vec![replace(&old, "Go")]),
        )
        .await
        .unwrap();

    assert!(profiles.is_live(&old).await, "the soft delete did not run");
    assert_eq!(
        profiles.root().await.unwrap().removals,
        std::slice::from_ref(&old)
    );
    assert_eq!(
        values(&profiles.read().await),
        [(Category::Skill, "skills".to_owned(), skills("Go"))],
        "a listed fact is not read"
    );

    profiles
        .writer()
        .write(
            &profiles.ctx,
            &profiles.plan(Some(2), vec![add_skill("Zig")]),
        )
        .await
        .unwrap();

    assert!(!profiles.is_live(&old).await, "the next write finished it");
    assert!(profiles.root().await.unwrap().removals.is_empty());
}

/// Graph storage whose first ingest commits but loses its answer.
struct LosesFirstAnswer {
    inner: Arc<dyn GraphStorageClientV1>,
    ingests: AtomicU32,
    keys: std::sync::Mutex<Vec<Option<String>>>,
}

#[async_trait]
impl GraphStorageClientV1 for LosesFirstAnswer {
    async fn register_types(
        &self,
        ctx: &SecurityContext,
        batch: Vec<TypeRegistration>,
    ) -> Result<Vec<TypeRecord>, CanonicalError> {
        self.inner.register_types(ctx, batch).await
    }
    async fn get_type(
        &self,
        ctx: &SecurityContext,
        type_id: &GtsTypeId,
    ) -> Result<TypeRecord, CanonicalError> {
        self.inner.get_type(ctx, type_id).await
    }
    async fn list_types(
        &self,
        ctx: &SecurityContext,
        query: TypeQuery,
    ) -> Result<Page<TypeRecord>, CanonicalError> {
        self.inner.list_types(ctx, query).await
    }
    async fn ingest(
        &self,
        ctx: &SecurityContext,
        request: IngestRequest,
    ) -> Result<IngestOutcome, CanonicalError> {
        self.keys
            .lock()
            .unwrap()
            .push(request.idempotency_key.clone());
        let outcome = self.inner.ingest(ctx, request).await;
        if self.ingests.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(CanonicalError::service_unavailable().create());
        }
        outcome
    }
    async fn delete_node(
        &self,
        ctx: &SecurityContext,
        node_key: &NodeKey,
    ) -> Result<DeleteOutcome, CanonicalError> {
        self.inner.delete_node(ctx, node_key).await
    }
    async fn delete_edge(
        &self,
        ctx: &SecurityContext,
        edge_key: &EdgeKey,
    ) -> Result<DeleteOutcome, CanonicalError> {
        self.inner.delete_edge(ctx, edge_key).await
    }
    async fn get_node(
        &self,
        ctx: &SecurityContext,
        node_key: &NodeKey,
        adjacency_limit: Option<u32>,
    ) -> Result<NodeView, CanonicalError> {
        self.inner.get_node(ctx, node_key, adjacency_limit).await
    }
    async fn project_nodes(
        &self,
        ctx: &SecurityContext,
        type_patterns: &[String],
        query: toolkit_odata::ODataQuery,
    ) -> Result<toolkit_odata::Page<NodeRow>, CanonicalError> {
        self.inner.project_nodes(ctx, type_patterns, query).await
    }
    async fn search(
        &self,
        ctx: &SecurityContext,
        request: SearchRequest,
    ) -> Result<SearchResponse, CanonicalError> {
        self.inner.search(ctx, request).await
    }
    async fn traverse(
        &self,
        ctx: &SecurityContext,
        request: TraverseRequest,
    ) -> Result<TraversalResponse, CanonicalError> {
        self.inner.traverse(ctx, request).await
    }
    async fn neighborhood(
        &self,
        ctx: &SecurityContext,
        request: NeighborhoodRequest,
    ) -> Result<TraversalResponse, CanonicalError> {
        self.inner.neighborhood(ctx, request).await
    }
    async fn revision(&self, ctx: &SecurityContext) -> Result<GraphRevision, CanonicalError> {
        self.inner.revision(ctx).await
    }
}

#[tokio::test]
async fn a_write_whose_answer_was_lost_is_sent_again_with_its_key_and_lands_once() {
    let profiles = Profiles::new();
    let lossy = Arc::new(LosesFirstAnswer {
        inner: Arc::clone(&profiles.client),
        ingests: AtomicU32::new(0),
        keys: std::sync::Mutex::default(),
    });
    let writer = Profiles::writer_with(
        Arc::clone(&lossy) as Arc<dyn GraphStorageClientV1>,
        Arc::new(SoftDelete::new(Arc::clone(&profiles.client))),
    );

    let written = writer
        .write(&profiles.ctx, &profiles.plan(None, vec![add_skill("Rust")]))
        .await
        .unwrap();

    assert_eq!(written, Written::Stored);
    let keys = lossy.keys.lock().unwrap().clone();
    assert_eq!(keys.len(), 2, "sent twice");
    assert!(keys[0].is_some() && keys[0] == keys[1], "with the same key");
    let profile = profiles.read().await;
    assert_eq!(profile.version, Some(1), "landed once");
    assert_eq!(profile.facts.len(), 1);
}

#[tokio::test]
async fn a_value_its_type_refuses_stores_nothing() {
    let profiles = Profiles::new();
    let plan = profiles.plan(
        None,
        vec![
            add_skill("Rust"),
            add_value("skill", "skills", json!({ "name": "Go", "level": "guru" })),
        ],
    );

    let result = profiles.writer().write(&profiles.ctx, &plan).await;

    assert!(
        matches!(result, Err(DomainError::Internal(_))),
        "{result:?}"
    );
    assert_eq!(profiles.read().await.version, None, "nothing landed");
}

#[tokio::test]
async fn an_empty_plan_writes_nothing() {
    let profiles = Profiles::new();

    let written = profiles
        .writer()
        .write(&profiles.ctx, &profiles.plan(None, Vec::new()))
        .await
        .unwrap();

    assert_eq!(written, Written::Stored);
    assert!(profiles.root().await.is_none(), "no root for nothing");
}

#[tokio::test]
async fn a_plan_for_another_tenant_than_the_context_is_refused_and_stores_nothing() {
    let profiles = Profiles::new();
    let mut plan = profiles.plan(None, vec![add_skill("Rust")]);
    plan.tenant_id = Uuid::new_v4();

    let result = profiles.writer().write(&profiles.ctx, &plan).await;

    assert!(
        matches!(result, Err(DomainError::Internal(_))),
        "{result:?}"
    );
    assert_eq!(profiles.read().await.version, None);
}
