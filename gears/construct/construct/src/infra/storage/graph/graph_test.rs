//! The writer and the read against graph storage's own service over its
//! in-memory store: real type registration, schema validation, compare-and-set,
//! idempotency receipts and soft delete. A scripted wrapper around that client
//! loses answers, fails, hangs or holds calls where a case needs it.

use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    DeleteOutcome, EdgeKey, EdgeSpec, GraphRevision, GtsTypeId, IngestOutcome, IngestRequest,
    NeighborhoodRequest, NodeKey, NodeRow, NodeSpec, NodeView, Page, SearchRequest, SearchResponse,
    TraversalResponse, TraverseRequest, TruncationReason, TypeQuery, TypeRecord, TypeRegistration,
};
use serde_json::{Value, json};
use tokio::sync::Barrier;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_canonical_errors::resource_error;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use super::reader::{GraphProfileSource, MAX_FACTS, MAX_PROFILE_NODES};
use super::root::{PayloadCounter, Root, root_key};
use super::types::GraphTypes;
use super::writer::{
    DEFAULT_ATTEMPTS, FactRemoval, GraphPlanWriter, SoftDelete, WriterPorts, unknown_result,
};
use super::{DEFAULT_CALL_TIMEOUT, graph_error};
use crate::domain::error::DomainError;
use crate::domain::plan::{Change, NodeKey as FactKey, Plan, Property, Step};
use crate::domain::profile::{Category, PlanWriter, Profile, ProfileSource, Written};
use crate::domain::subject_settings::SubjectKey;
use crate::test_support::{add_value, connector_plan, replace_property};

/// Builds the canonical errors graph storage answers with.
#[resource_error(gts_id!("cf.construct.test.graph.v1~"))]
struct GraphTestError;

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

// --- A scripted graph storage ---------------------------------------------

/// What one ingest does.
enum Ingest {
    /// Hand it to graph storage.
    Forward,
    /// Hand it to graph storage, then lose the answer.
    LoseAnswer,
    /// Fail without reaching graph storage.
    Fail(fn() -> CanonicalError),
    /// Never answer.
    Hang,
}

fn unavailable() -> CanonicalError {
    CanonicalError::service_unavailable().create()
}

fn aborted(reason: &str) -> CanonicalError {
    GraphTestError::aborted("aborted")
        .with_reason(reason)
        .create()
}

fn serialization() -> CanonicalError {
    aborted("SERIALIZATION")
}

fn cas_conflict() -> CanonicalError {
    aborted("CAS_CONFLICT")
}

/// Graph storage that does, per ingest, what the case scripts, and otherwise
/// passes every call through.
struct Scripted {
    inner: Arc<dyn GraphStorageClientV1>,
    ingests: Mutex<VecDeque<Ingest>>,
    ingest_calls: AtomicU32,
    keys: Mutex<Vec<Option<String>>>,
    /// Holds every ingest until the given number of them wait, so writes that
    /// read the same profile both reach the compare-and-set.
    hold_ingests: Mutex<Option<Arc<Barrier>>>,
    /// The time of every ingest, in the order they were sent.
    sent_at: Mutex<Vec<tokio::time::Instant>>,
    /// Every traversal asked for.
    reads: Mutex<Vec<TraverseRequest>>,
    /// Marks every traversal as cut short for this reason.
    cut_reads: Mutex<Option<TruncationReason>>,
    hang_reads: AtomicBool,
    registrations: AtomicU32,
    fail_next_registration: AtomicBool,
}

impl Scripted {
    fn over(inner: Arc<dyn GraphStorageClientV1>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            ingests: Mutex::default(),
            ingest_calls: AtomicU32::new(0),
            keys: Mutex::default(),
            hold_ingests: Mutex::default(),
            sent_at: Mutex::default(),
            reads: Mutex::default(),
            cut_reads: Mutex::default(),
            hang_reads: AtomicBool::new(false),
            registrations: AtomicU32::new(0),
            fail_next_registration: AtomicBool::new(false),
        })
    }

    fn script(&self, ingests: impl IntoIterator<Item = Ingest>) {
        self.ingests.lock().unwrap().extend(ingests);
    }

    fn hold_ingests(&self, writers: usize) {
        *self.hold_ingests.lock().unwrap() = Some(Arc::new(Barrier::new(writers)));
    }
}

#[async_trait]
impl GraphStorageClientV1 for Scripted {
    async fn register_types(
        &self,
        ctx: &SecurityContext,
        batch: Vec<TypeRegistration>,
    ) -> Result<Vec<TypeRecord>, CanonicalError> {
        self.registrations.fetch_add(1, Ordering::SeqCst);
        if self.fail_next_registration.swap(false, Ordering::SeqCst) {
            return Err(unavailable());
        }
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
        self.ingest_calls.fetch_add(1, Ordering::SeqCst);
        self.sent_at
            .lock()
            .unwrap()
            .push(tokio::time::Instant::now());
        self.keys
            .lock()
            .unwrap()
            .push(request.idempotency_key.clone());
        let hold = self.hold_ingests.lock().unwrap().clone();
        if let Some(barrier) = hold {
            barrier.wait().await;
        }
        let step = self
            .ingests
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ingest::Forward);
        match step {
            Ingest::Forward => self.inner.ingest(ctx, request).await,
            Ingest::LoseAnswer => {
                let _committed = self.inner.ingest(ctx, request).await;
                Err(unavailable())
            }
            Ingest::Fail(error) => Err(error()),
            Ingest::Hang => std::future::pending().await,
        }
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
        self.reads.lock().unwrap().push(request.clone());
        if self.hang_reads.load(Ordering::SeqCst) {
            return std::future::pending().await;
        }
        let mut response = self.inner.traverse(ctx, request).await?;
        if let Some(reason) = *self.cut_reads.lock().unwrap() {
            response.truncated = Some(reason);
        }
        Ok(response)
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

// --- One subject's profile ----------------------------------------------------

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

    /// Another subject's profile in the same graph storage and tenant.
    fn neighbour(&self) -> Self {
        Self {
            client: Arc::clone(&self.client),
            ctx: self.ctx.clone(),
            tenant: self.tenant,
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

    /// A writer over the scripted graph storage; deletes go straight through.
    fn scripted_writer(&self, scripted: &Arc<Scripted>) -> GraphPlanWriter {
        Self::writer_with(
            Arc::clone(scripted) as Arc<dyn GraphStorageClientV1>,
            Arc::new(SoftDelete::new(Arc::clone(&self.client))),
        )
    }

    fn reader(&self) -> GraphProfileSource {
        GraphProfileSource::new(Arc::clone(&self.client), Arc::new(PayloadCounter))
    }

    async fn read(&self) -> Profile {
        self.reader()
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

    async fn write(&self, version: Option<u64>, steps: Vec<Step>) -> Written {
        self.writer()
            .write(&self.ctx, &self.plan(version, steps))
            .await
            .expect("the write runs")
    }

    /// A write a case starts from; it has to land.
    async fn given(&self, version: Option<u64>, steps: Vec<Step>) {
        assert_eq!(self.write(version, steps).await, Written::Stored);
    }

    /// Send `request` to graph storage as it is, past the writer.
    async fn ingest_directly(&self, request: IngestRequest) {
        self.client.ingest(&self.ctx, request).await.unwrap();
    }

    fn subject_key(&self) -> SubjectKey {
        SubjectKey {
            tenant_id: self.tenant,
            subject_id: self.subject,
        }
    }

    async fn root(&self) -> Option<Root> {
        let key = root_key(&self.subject_key());
        match self.client.get_node(&self.ctx, &key.0, None).await {
            Ok(node) => Some(Root::from_payload(&key, node.payload.as_ref()).unwrap()),
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

fn skill_values(names: &[&str]) -> Vec<(Category, String, Value)> {
    names
        .iter()
        .map(|name| (Category::Skill, "skills".to_owned(), skills(name)))
        .collect()
}

// --- Writing and reading -----------------------------------------------------

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

    let written = profiles
        .write(
            None,
            vec![
                add_value("identity", "full_name", json!("Ada Lovelace")),
                add_skill("Rust"),
            ],
        )
        .await;

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
    let first = add_skill("Rust");
    let old = key_of(&first);
    profiles.given(None, vec![first]).await;

    let written = profiles.write(Some(1), vec![replace(&old, "Go")]).await;

    assert_eq!(written, Written::Stored);
    let profile = profiles.read().await;
    assert_eq!(profile.version, Some(2));
    assert_eq!(values(&profile), skill_values(&["Go"]));
    assert!(
        !profiles.is_live(&old).await,
        "the old fact is soft-deleted"
    );
}

#[tokio::test]
async fn a_removal_that_is_done_drops_off_the_root_on_the_next_write() {
    let profiles = Profiles::new();
    let first = add_skill("Rust");
    let old = key_of(&first);
    profiles.given(None, vec![first]).await;
    profiles.given(Some(1), vec![replace(&old, "Go")]).await;
    assert_eq!(
        profiles.root().await.unwrap().removals,
        std::slice::from_ref(&old)
    );

    let written = profiles.write(Some(2), vec![add_skill("Zig")]).await;

    assert_eq!(written, Written::Stored);
    assert!(profiles.root().await.unwrap().removals.is_empty());
    assert_eq!(values(&profiles.read().await), skill_values(&["Go", "Zig"]));
}

#[tokio::test]
async fn a_removed_fact_is_gone_from_the_next_read() {
    let profiles = Profiles::new();
    let first = add_skill("Rust");
    let old = key_of(&first);
    profiles.given(None, vec![first, add_skill("Go")]).await;

    profiles.given(Some(1), vec![remove(&old)]).await;

    assert_eq!(values(&profiles.read().await), skill_values(&["Go"]));
}

#[tokio::test]
async fn a_plan_built_on_an_older_profile_conflicts_and_stores_nothing() {
    let profiles = Profiles::new();
    profiles.given(None, vec![add_skill("Rust")]).await;

    let stale_new_subject = profiles.write(None, vec![add_skill("Go")]).await;
    let stale_version = profiles.write(Some(7), vec![add_skill("Go")]).await;

    assert_eq!(stale_new_subject, Written::Conflict);
    assert_eq!(stale_version, Written::Conflict);
    let profile = profiles.read().await;
    assert_eq!(profile.version, Some(1));
    assert_eq!(profile.facts.len(), 1);
}

/// Two writes that both read `version` and both reach the ingest: the root's
/// compare-and-set lets only one land.
async fn two_writes_at_once(profiles: &Profiles, version: Option<u64>) -> [Written; 2] {
    let scripted = Scripted::over(Arc::clone(&profiles.client));
    scripted.hold_ingests(2);
    let (one, two) = (
        profiles.scripted_writer(&scripted),
        profiles.scripted_writer(&scripted),
    );
    let (go, zig) = (
        profiles.plan(version, vec![add_skill("Go")]),
        profiles.plan(version, vec![add_skill("Zig")]),
    );

    let (first, second) = tokio::join!(
        one.write(&profiles.ctx, &go),
        two.write(&profiles.ctx, &zig)
    );

    assert_eq!(
        scripted.ingest_calls.load(Ordering::SeqCst),
        2,
        "both writes reached the ingest"
    );
    let mut outcomes = [first.unwrap(), second.unwrap()];
    outcomes.sort_by_key(|written| *written == Written::Conflict);
    outcomes
}

#[tokio::test]
async fn two_plans_on_the_same_profile_never_both_land() {
    let profiles = Profiles::new();
    profiles.given(None, vec![add_skill("Rust")]).await;

    let outcomes = two_writes_at_once(&profiles, Some(1)).await;

    assert_eq!(outcomes, [Written::Stored, Written::Conflict]);
    let profile = profiles.read().await;
    assert_eq!(profile.version, Some(2));
    assert_eq!(profile.facts.len(), 2, "one plan only");
}

#[tokio::test]
async fn two_first_writes_create_the_root_once() {
    let profiles = Profiles::new();

    let outcomes = two_writes_at_once(&profiles, None).await;

    assert_eq!(outcomes, [Written::Stored, Written::Conflict]);
    let profile = profiles.read().await;
    assert_eq!(profile.version, Some(1));
    assert_eq!(profile.facts.len(), 1, "one plan only");
}

#[tokio::test]
async fn a_compare_and_set_refused_at_the_ingest_is_a_conflict() {
    let profiles = Profiles::new();
    let scripted = Scripted::over(Arc::clone(&profiles.client));
    scripted.script([Ingest::Fail(cas_conflict)]);

    let written = profiles
        .scripted_writer(&scripted)
        .write(&profiles.ctx, &profiles.plan(None, vec![add_skill("Rust")]))
        .await
        .unwrap();

    assert_eq!(written, Written::Conflict);
    assert_eq!(profiles.read().await.version, None);
}

// --- Removals ------------------------------------------------------------------

/// A removal that cannot run.
struct BrokenRemoval;

#[async_trait]
impl FactRemoval for BrokenRemoval {
    async fn remove(&self, _ctx: &SecurityContext, _key: &str) -> Result<(), DomainError> {
        Err(DomainError::Unavailable("graph storage is down".to_owned()))
    }
}

#[tokio::test]
async fn a_removal_that_fails_stays_listed_and_a_later_write_finishes_it() {
    let profiles = Profiles::new();
    let first = add_skill("Rust");
    let old = key_of(&first);
    profiles.given(None, vec![first]).await;
    let broken = Profiles::writer_with(Arc::clone(&profiles.client), Arc::new(BrokenRemoval));

    let written = broken
        .write(
            &profiles.ctx,
            &profiles.plan(Some(1), vec![replace(&old, "Go")]),
        )
        .await
        .unwrap();

    assert_eq!(written, Written::Stored);

    assert!(profiles.is_live(&old).await, "the soft delete did not run");
    assert_eq!(
        profiles.root().await.unwrap().removals,
        std::slice::from_ref(&old)
    );
    assert_eq!(
        values(&profiles.read().await),
        skill_values(&["Go"]),
        "a listed fact is not read"
    );

    profiles.given(Some(2), vec![add_skill("Zig")]).await;

    assert!(!profiles.is_live(&old).await, "the later write finished it");
}

#[tokio::test]
async fn a_listed_key_that_is_not_a_fact_of_the_root_is_never_deleted() {
    let profiles = Profiles::new();
    let other = profiles.neighbour();
    let theirs = add_skill("Rust");
    let their_key = key_of(&theirs);
    other.given(None, vec![theirs]).await;
    profiles.given(None, vec![add_skill("Go")]).await;
    // Someone else rewrites this subject's root to list the other subject's fact.
    let root = root_key(&profiles.subject_key());
    let planted = Root {
        removals: vec![their_key.clone()],
    }
    .payload(profiles.subject, &PayloadCounter, 2)
    .unwrap();
    profiles
        .ingest_directly(IngestRequest {
            nodes: vec![NodeSpec {
                node_key: root.0.clone(),
                type_id: construct_sdk::gts::PROFILE_ROOT_TYPE.to_owned(),
                name: None,
                payload: Some(planted),
                expected_version: Some(1),
            }],
            ..IngestRequest::default()
        })
        .await;

    profiles.given(Some(2), vec![add_skill("Zig")]).await;

    assert!(
        other.is_live(&their_key).await,
        "the other subject's fact stays"
    );
    assert!(profiles.root().await.unwrap().removals.is_empty());
}

#[tokio::test]
async fn a_soft_delete_of_a_fact_already_removed_is_done() {
    let profiles = Profiles::new();
    let first = add_skill("Rust");
    let key = key_of(&first);
    profiles.given(None, vec![first]).await;
    let removal = SoftDelete::new(Arc::clone(&profiles.client));

    removal.remove(&profiles.ctx, &key).await.unwrap();
    let again = removal.remove(&profiles.ctx, &key).await;

    assert!(again.is_ok(), "{again:?}");
    assert!(!profiles.is_live(&key).await);
}

#[tokio::test]
async fn a_soft_delete_of_a_fact_graph_storage_does_not_hold_fails_and_names_it() {
    let profiles = Profiles::new();

    let removed = SoftDelete::new(Arc::clone(&profiles.client))
        .remove(&profiles.ctx, "construct:never-written")
        .await;

    match removed {
        Err(DomainError::Internal(msg)) => {
            assert!(msg.contains("construct:never-written"), "{msg}");
        }
        other => panic!("{other:?}"),
    }
}

/// Removes nothing and fails after counting the keys it was asked for.
#[derive(Default)]
struct CountingBrokenRemoval {
    asked: AtomicU32,
}

#[async_trait]
impl FactRemoval for CountingBrokenRemoval {
    async fn remove(&self, _ctx: &SecurityContext, _key: &str) -> Result<(), DomainError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Err(DomainError::Unavailable("graph storage is down".to_owned()))
    }
}

#[tokio::test]
async fn the_cleanup_stops_at_the_first_failed_removal_and_every_key_stays_listed() {
    let profiles = Profiles::new();
    let (rust, go) = (add_skill("Rust"), add_skill("Go"));
    let mut keys = vec![key_of(&rust), key_of(&go)];
    keys.sort();
    profiles.given(None, vec![rust, go]).await;
    let removal = Arc::new(CountingBrokenRemoval::default());
    let writer = Profiles::writer_with(
        Arc::clone(&profiles.client),
        Arc::clone(&removal) as Arc<dyn FactRemoval>,
    );

    let written = writer
        .write(
            &profiles.ctx,
            &profiles.plan(Some(1), vec![remove(&keys[0]), remove(&keys[1])]),
        )
        .await
        .unwrap();

    assert_eq!(written, Written::Stored);
    assert_eq!(
        removal.asked.load(Ordering::SeqCst),
        1,
        "no removal after a failed one"
    );
    assert_eq!(profiles.root().await.unwrap().removals, keys);
    assert!(profiles.read().await.facts.is_empty());
}

#[tokio::test]
async fn a_node_with_an_edge_into_the_root_is_not_read_as_a_fact() {
    let profiles = Profiles::new();
    let other = profiles.neighbour();
    let theirs = add_skill("Rust");
    let their_key = key_of(&theirs);
    other.given(None, vec![theirs]).await;
    profiles.given(None, vec![add_skill("Go")]).await;
    // An edge of the has-fact type from the other subject's fact to this root:
    // the traversal walks it backwards, and it reaches the other fact.
    profiles
        .ingest_directly(IngestRequest {
            edges: vec![EdgeSpec {
                type_id: construct_sdk::gts::HAS_FACT_TYPE.to_owned(),
                src_node_key: their_key,
                dst_node_key: root_key(&profiles.subject_key()).0,
                discriminator: None,
                payload: None,
            }],
            ..IngestRequest::default()
        })
        .await;

    let profile = profiles.read().await;

    assert_eq!(values(&profile), skill_values(&["Go"]));
}

#[tokio::test]
async fn a_plan_that_removes_a_fact_not_in_its_profile_is_refused() {
    let profiles = Profiles::new();
    profiles.given(None, vec![add_skill("Rust")]).await;

    let result = profiles
        .writer()
        .write(
            &profiles.ctx,
            &profiles.plan(Some(1), vec![remove("construct:unknown")]),
        )
        .await;

    assert!(
        matches!(result, Err(DomainError::Internal(_))),
        "{result:?}"
    );
    assert_eq!(profiles.read().await.version, Some(1));
}

// --- The size of a profile -------------------------------------------------------

#[tokio::test]
async fn a_full_profile_takes_no_new_fact_but_takes_a_removal() {
    let profiles = Profiles::new();
    let first = add_skill("Rust");
    let old = key_of(&first);
    profiles.given(None, vec![first]).await;
    let writer = profiles.writer().with_max_facts(1);

    let added = writer
        .write(
            &profiles.ctx,
            &profiles.plan(Some(1), vec![add_skill("Go")]),
        )
        .await;
    let replaced = writer
        .write(
            &profiles.ctx,
            &profiles.plan(Some(1), vec![replace(&old, "Go")]),
        )
        .await;
    let removed = writer
        .write(&profiles.ctx, &profiles.plan(Some(1), vec![remove(&old)]))
        .await
        .unwrap();

    assert!(
        matches!(added, Err(DomainError::ProfileFull(_))),
        "{added:?}"
    );
    assert!(
        matches!(replaced, Err(DomainError::ProfileFull(_))),
        "the old fact counts until its delete has run: {replaced:?}"
    );
    assert_eq!(removed, Written::Stored);
    assert!(profiles.read().await.facts.is_empty());
}

#[tokio::test]
async fn a_profile_cut_short_by_the_read_limit_is_full_not_partial() {
    let profiles = Profiles::new();
    profiles.given(None, vec![add_skill("Rust")]).await;
    let scripted = Scripted::over(Arc::clone(&profiles.client));
    *scripted.cut_reads.lock().unwrap() = Some(TruncationReason::NodeBudget);

    let read = GraphProfileSource::new(
        Arc::clone(&scripted) as Arc<dyn GraphStorageClientV1>,
        Arc::new(PayloadCounter),
    )
    .read(&profiles.ctx, profiles.tenant, profiles.subject)
    .await;
    let written = profiles
        .scripted_writer(&scripted)
        .write(
            &profiles.ctx,
            &profiles.plan(Some(1), vec![add_skill("Go")]),
        )
        .await;

    assert!(matches!(read, Err(DomainError::ProfileFull(_))), "{read:?}");
    assert!(
        matches!(written, Err(DomainError::ProfileFull(_))),
        "{written:?}"
    );
}

#[tokio::test]
async fn a_profile_read_cut_short_for_another_reason_is_internal() {
    let profiles = Profiles::new();
    profiles.given(None, vec![add_skill("Rust")]).await;
    let scripted = Scripted::over(Arc::clone(&profiles.client));
    let reader = GraphProfileSource::new(
        Arc::clone(&scripted) as Arc<dyn GraphStorageClientV1>,
        Arc::new(PayloadCounter),
    );

    for reason in [
        TruncationReason::FrontierCap,
        TruncationReason::EdgeScanCap,
        TruncationReason::ResponseBytes,
    ] {
        *scripted.cut_reads.lock().unwrap() = Some(reason);
        let read = reader
            .read(&profiles.ctx, profiles.tenant, profiles.subject)
            .await;
        assert!(
            matches!(read, Err(DomainError::Internal(_))),
            "{reason:?}: {read:?}"
        );
    }
}

#[tokio::test]
async fn a_profile_read_asks_for_the_root_and_at_most_its_facts() {
    let profiles = Profiles::new();
    let scripted = Scripted::over(Arc::clone(&profiles.client));

    GraphProfileSource::new(
        Arc::clone(&scripted) as Arc<dyn GraphStorageClientV1>,
        Arc::new(PayloadCounter),
    )
    .read(&profiles.ctx, profiles.tenant, profiles.subject)
    .await
    .unwrap();

    let reads = scripted.reads.lock().unwrap().clone();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].max_nodes, Some(MAX_PROFILE_NODES));
    assert_eq!(reads[0].depth, 1);
    assert_eq!(reads[0].seeds, [root_key(&profiles.subject_key()).0]);
}

#[tokio::test]
async fn a_profile_at_the_limit_takes_no_new_fact_whatever_limit_the_writer_is_given() {
    let profiles = Profiles::new();
    let all: Vec<Step> = (0..MAX_FACTS)
        .map(|n| add_skill(&format!("skill {n}")))
        .collect();
    profiles.given(None, all).await;
    assert_eq!(
        profiles.read().await.facts.len(),
        MAX_FACTS,
        "the whole profile reads"
    );

    let added = profiles
        .writer()
        .with_max_facts(usize::MAX)
        .write(
            &profiles.ctx,
            &profiles.plan(Some(1), vec![add_skill("one more")]),
        )
        .await;

    assert!(
        matches!(added, Err(DomainError::ProfileFull(_))),
        "{added:?}"
    );
}

#[tokio::test]
async fn a_profile_past_what_one_read_takes_is_full() {
    let profiles = Profiles::new();
    let all: Vec<Step> = (0..MAX_FACTS)
        .map(|n| add_skill(&format!("skill {n}")))
        .collect();
    profiles.given(None, all).await;
    // One more fact on the root, written past the writer's limit.
    let one_more = add_skill("one more");
    let one_more_key = key_of(&one_more);
    let plan = profiles.plan(Some(1), vec![one_more]);
    let node = super::fact::fact_node(&plan, &plan.steps[0], time::OffsetDateTime::now_utc())
        .unwrap()
        .unwrap();
    profiles
        .ingest_directly(IngestRequest {
            nodes: vec![node],
            edges: vec![EdgeSpec {
                type_id: construct_sdk::gts::HAS_FACT_TYPE.to_owned(),
                src_node_key: root_key(&profiles.subject_key()).0,
                dst_node_key: one_more_key,
                discriminator: None,
                payload: None,
            }],
            ..IngestRequest::default()
        })
        .await;

    let read = profiles
        .reader()
        .read(&profiles.ctx, profiles.tenant, profiles.subject)
        .await;

    assert!(matches!(read, Err(DomainError::ProfileFull(_))), "{read:?}");
}

// --- Retries and timeouts --------------------------------------------------------

#[tokio::test]
async fn a_write_whose_answer_was_lost_is_sent_again_with_its_key_and_lands_once() {
    let profiles = Profiles::new();
    let scripted = Scripted::over(Arc::clone(&profiles.client));
    scripted.script([Ingest::LoseAnswer]);

    let written = profiles
        .scripted_writer(&scripted)
        .write(&profiles.ctx, &profiles.plan(None, vec![add_skill("Rust")]))
        .await
        .unwrap();

    assert_eq!(written, Written::Stored);
    let keys = scripted.keys.lock().unwrap().clone();
    assert_eq!(keys.len(), 2, "sent twice");
    assert!(keys[0].is_some() && keys[0] == keys[1], "with the same key");
    let profile = profiles.read().await;
    assert_eq!(profile.version, Some(1), "landed once");
    assert_eq!(profile.facts.len(), 1);
}

#[tokio::test]
async fn a_lost_serialization_race_is_sent_again() {
    let profiles = Profiles::new();
    let scripted = Scripted::over(Arc::clone(&profiles.client));
    scripted.script([Ingest::Fail(serialization)]);

    let written = profiles
        .scripted_writer(&scripted)
        .write(&profiles.ctx, &profiles.plan(None, vec![add_skill("Rust")]))
        .await
        .unwrap();

    assert_eq!(written, Written::Stored);
    assert_eq!(scripted.ingest_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn a_write_that_stays_unavailable_gives_up_after_its_attempts() {
    let profiles = Profiles::new();
    let scripted = Scripted::over(Arc::clone(&profiles.client));
    scripted.script((0..DEFAULT_ATTEMPTS.get()).map(|_| Ingest::Fail(unavailable)));

    let result = profiles
        .scripted_writer(&scripted)
        .write(&profiles.ctx, &profiles.plan(None, vec![add_skill("Rust")]))
        .await;

    assert!(
        matches!(result, Err(DomainError::Unavailable(_))),
        "{result:?}"
    );
    assert_eq!(
        scripted.ingest_calls.load(Ordering::SeqCst),
        DEFAULT_ATTEMPTS.get()
    );
    assert_eq!(profiles.read().await.version, None);
}

#[tokio::test(start_paused = true)]
async fn a_write_that_never_gets_an_answer_gives_up_after_its_attempts_as_unavailable() {
    let profiles = Profiles::new();
    let scripted = Scripted::over(Arc::clone(&profiles.client));
    scripted.script((0..DEFAULT_ATTEMPTS.get()).map(|_| Ingest::Hang));

    let result = profiles
        .scripted_writer(&scripted)
        .with_timeout(Duration::from_secs(1))
        .write(&profiles.ctx, &profiles.plan(None, vec![add_skill("Rust")]))
        .await;

    assert!(
        matches!(result, Err(DomainError::Unavailable(_))),
        "{result:?}"
    );
    assert_eq!(
        scripted.ingest_calls.load(Ordering::SeqCst),
        DEFAULT_ATTEMPTS.get()
    );
}

#[tokio::test(start_paused = true)]
async fn the_pause_between_sends_grows_and_carries_up_to_as_much_again() {
    let profiles = Profiles::new();
    let scripted = Scripted::over(Arc::clone(&profiles.client));
    scripted.script([Ingest::Fail(unavailable), Ingest::Fail(unavailable)]);

    let written = profiles
        .scripted_writer(&scripted)
        .write(&profiles.ctx, &profiles.plan(None, vec![add_skill("Rust")]))
        .await
        .unwrap();

    assert_eq!(written, Written::Stored);
    let sent_at = scripted.sent_at.lock().unwrap().clone();
    assert_eq!(sent_at.len(), 3);
    let (first, second) = (sent_at[1] - sent_at[0], sent_at[2] - sent_at[1]);
    let pause = Duration::from_millis(100);
    assert!(first >= pause && first <= pause * 2, "{first:?}");
    assert!(second >= pause * 2 && second <= pause * 4, "{second:?}");
}

#[tokio::test(start_paused = true)]
async fn a_writer_given_one_attempt_sends_once() {
    let profiles = Profiles::new();
    let scripted = Scripted::over(Arc::clone(&profiles.client));
    scripted.script([Ingest::Fail(unavailable)]);

    let result = profiles
        .scripted_writer(&scripted)
        .with_attempts(NonZeroU32::MIN)
        .write(&profiles.ctx, &profiles.plan(None, vec![add_skill("Rust")]))
        .await;

    assert!(
        matches!(result, Err(DomainError::Unavailable(_))),
        "{result:?}"
    );
    assert_eq!(scripted.ingest_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn a_send_without_an_answer_in_time_is_sent_again() {
    let profiles = Profiles::new();
    let scripted = Scripted::over(Arc::clone(&profiles.client));
    scripted.script([Ingest::Hang]);

    let written = profiles
        .scripted_writer(&scripted)
        .with_timeout(Duration::from_secs(1))
        .write(&profiles.ctx, &profiles.plan(None, vec![add_skill("Rust")]))
        .await
        .unwrap();

    assert_eq!(written, Written::Stored);
    assert_eq!(scripted.ingest_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn a_read_without_an_answer_in_time_is_unavailable() {
    let profiles = Profiles::new();
    let scripted = Scripted::over(Arc::clone(&profiles.client));
    scripted.hang_reads.store(true, Ordering::SeqCst);

    let read = GraphProfileSource::new(
        Arc::clone(&scripted) as Arc<dyn GraphStorageClientV1>,
        Arc::new(PayloadCounter),
    )
    .with_timeout(DEFAULT_CALL_TIMEOUT)
    .read(&profiles.ctx, profiles.tenant, profiles.subject)
    .await;

    assert!(matches!(read, Err(DomainError::Unavailable(_))), "{read:?}");
}

#[test]
fn only_a_write_whose_result_is_unknown_is_sent_again() {
    assert!(unknown_result(&serialization()));
    assert!(unknown_result(&unavailable()));
    assert!(unknown_result(
        &GraphTestError::deadline_exceeded("slow").create()
    ));
    assert!(
        !unknown_result(&GraphTestError::unknown("lost").create()),
        "an unknown error says nothing about the write"
    );
    assert!(!unknown_result(&cas_conflict()));
    assert!(!unknown_result(&aborted("IDEMPOTENCY_MISMATCH")));
    assert!(!unknown_result(
        &GraphTestError::invalid_argument()
            .with_field_violation("payload", "refused", "SCHEMA_VIOLATION")
            .create()
    ));
}

#[test]
fn graph_storage_failures_map_to_what_a_caller_can_act_on() {
    assert!(matches!(
        graph_error("call", &unavailable()),
        DomainError::Unavailable(_)
    ));
    assert!(matches!(
        graph_error("call", &GraphTestError::deadline_exceeded("slow").create()),
        DomainError::Unavailable(_)
    ));
    // Construct's own permission is missing: the deployment's defect, not a
    // refusal of the caller.
    assert!(matches!(
        graph_error(
            "call",
            &GraphTestError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create()
        ),
        DomainError::Internal(_)
    ));
    assert!(matches!(
        graph_error(
            "call",
            &CanonicalError::unauthenticated()
                .with_reason("TOKEN_EXPIRED")
                .create()
        ),
        DomainError::Internal(_)
    ));
    assert!(matches!(
        graph_error("call", &GraphTestError::unknown("lost").create()),
        DomainError::Internal(_)
    ));
    assert!(matches!(
        graph_error(
            "call",
            &GraphTestError::invalid_argument()
                .with_field_violation("payload", "refused", "SCHEMA_VIOLATION")
                .create()
        ),
        DomainError::Internal(_)
    ));
}

// --- Type registration -----------------------------------------------------------

#[tokio::test]
async fn the_types_register_again_after_a_restart_without_conflict() {
    let profiles = Profiles::new();
    let scripted = Scripted::over(Arc::clone(&profiles.client));
    let first = profiles
        .scripted_writer(&scripted)
        .write(&profiles.ctx, &profiles.plan(None, vec![add_skill("Rust")]))
        .await
        .unwrap();
    assert_eq!(first, Written::Stored);

    // A new writer has an empty registration cache, like a restarted instance.
    let written = profiles
        .scripted_writer(&scripted)
        .write(
            &profiles.ctx,
            &profiles.plan(Some(1), vec![add_skill("Go")]),
        )
        .await
        .unwrap();

    assert_eq!(written, Written::Stored);
    assert_eq!(scripted.registrations.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn registration_is_remembered_per_tenant_and_a_failed_one_is_not() {
    let scripted = Scripted::over(graph_storage());
    let types = GraphTypes::new(Arc::clone(&scripted) as Arc<dyn GraphStorageClientV1>);
    let (first, second, third) = (
        construct_in(Uuid::new_v4()),
        construct_in(Uuid::new_v4()),
        construct_in(Uuid::new_v4()),
    );

    types.ensure(&first).await.unwrap();
    types.ensure(&first).await.unwrap();
    types.ensure(&second).await.unwrap();
    scripted
        .fail_next_registration
        .store(true, Ordering::SeqCst);
    let failed = types.ensure(&third).await;
    types.ensure(&third).await.unwrap();

    assert!(
        matches!(failed, Err(DomainError::Unavailable(_))),
        "{failed:?}"
    );
    assert_eq!(scripted.registrations.load(Ordering::SeqCst), 4);
}

// --- Refused plans ---------------------------------------------------------------

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
async fn a_property_change_is_refused_and_stores_nothing() {
    let profiles = Profiles::new();

    let result = profiles
        .writer()
        .write(
            &profiles.ctx,
            &profiles.plan(None, vec![replace_property("skill", "skills", "Rust")]),
        )
        .await;

    assert!(
        matches!(result, Err(DomainError::Internal(_))),
        "{result:?}"
    );
    assert!(profiles.root().await.is_none());
}

#[tokio::test]
async fn an_empty_plan_writes_nothing() {
    let profiles = Profiles::new();

    let written = profiles.write(None, Vec::new()).await;

    assert_eq!(written, Written::Stored);
    assert!(profiles.root().await.is_none(), "no root for nothing");
}

#[tokio::test]
async fn a_profile_of_another_tenant_than_the_context_is_refused() {
    let profiles = Profiles::new();
    let mut plan = profiles.plan(None, vec![add_skill("Rust")]);
    plan.tenant_id = Uuid::new_v4();

    let written = profiles.writer().write(&profiles.ctx, &plan).await;
    let read = profiles
        .reader()
        .read(&profiles.ctx, plan.tenant_id, profiles.subject)
        .await;

    assert!(
        matches!(written, Err(DomainError::Internal(_))),
        "{written:?}"
    );
    assert!(matches!(read, Err(DomainError::Internal(_))), "{read:?}");
    assert_eq!(profiles.read().await.version, None);
}
