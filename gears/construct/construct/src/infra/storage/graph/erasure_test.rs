//! A profile's root over graph storage's real service and in-memory store:
//! the erasure removes it with the facts it still lists, a profile written
//! after it gets the root of the next generation, and retention lists the
//! subjects that have a root.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use super::directory::GraphProfileDirectory;
use super::reader::GraphProfileSource;
use super::root::{PayloadCounter, generation_root, root_key};
use super::types::GraphTypes;
use super::writer::{FactRemoval, GraphPlanWriter, SoftDelete, WriterPorts};
use crate::domain::deletion::{ProfileDirectory, ProfileGenerations, ProfileRoots, RootRemoval};
use crate::domain::error::DomainError;
use crate::domain::plan::{Change, NodeKey, Plan, Step};
use crate::domain::planner::processing::{PlanWriter, Written};
use crate::domain::profile_writes::SubjectProfiles;
use crate::domain::subject_settings::SubjectKey;
use crate::infra::processing_contexts::ServiceIdentity;
use crate::test_support::{add_value, connector_plan, fake_graph_storage};
use graph_storage_sdk::GraphStorageClientV1;
use toolkit_security::SecurityContext;

/// A generation the test moves on by hand, as an erasure's finish does.
#[derive(Default)]
struct Generation(AtomicU64);

#[async_trait]
impl ProfileGenerations for Generation {
    async fn generation(&self, _key: SubjectKey) -> Result<u64, DomainError> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}

/// Graph storage's soft delete, which fails while `failing` is set and
/// keeps every key it removed.
struct FlakyRemoval {
    inner: SoftDelete,
    failing: AtomicBool,
    removed: Mutex<Vec<NodeKey>>,
}

#[async_trait]
impl FactRemoval for FlakyRemoval {
    async fn remove(&self, ctx: &SecurityContext, key: &NodeKey) -> Result<(), DomainError> {
        if self.failing.load(Ordering::SeqCst) {
            return Err(DomainError::Unavailable("graph storage is down".to_owned()));
        }
        self.inner.remove(ctx, key).await?;
        self.removed.lock().unwrap().push(key.clone());
        Ok(())
    }
}

struct Graph {
    key: SubjectKey,
    ctx: toolkit_security::SecurityContext,
    client: Arc<dyn GraphStorageClientV1>,
    removal: Arc<FlakyRemoval>,
    generation: Arc<Generation>,
    /// The tenants the writer noted.
    tenants: Arc<crate::test_support::MemoryDeletionStore>,
    reader: GraphProfileSource,
    writer: GraphPlanWriter,
    directory: GraphProfileDirectory,
}

impl Graph {
    fn new() -> Self {
        let client = fake_graph_storage();
        let key = SubjectKey {
            tenant_id: Uuid::new_v4(),
            subject_id: Uuid::new_v4(),
        };
        let ctx = ServiceIdentity::new(Uuid::new_v4())
            .context(key.tenant_id)
            .unwrap();
        let generation = Arc::new(Generation::default());
        let tenants = Arc::new(crate::test_support::MemoryDeletionStore::default());
        let reader = GraphProfileSource::new(
            Arc::clone(&client),
            Arc::new(PayloadCounter),
            Arc::clone(&generation) as _,
            crate::test_support::no_retention(),
        );
        let removal = Arc::new(FlakyRemoval {
            inner: SoftDelete::new(Arc::clone(&client)),
            failing: AtomicBool::new(false),
            removed: Mutex::default(),
        });
        let writer = GraphPlanWriter::new(WriterPorts {
            client: Arc::clone(&client),
            types: Arc::new(GraphTypes::new(Arc::clone(&client))),
            version: Arc::new(PayloadCounter),
            removal: Arc::clone(&removal) as _,
            generations: Arc::clone(&generation) as _,
            tenants: Arc::clone(&tenants) as _,
        });
        Self {
            key,
            ctx,
            client: Arc::clone(&client),
            removal,
            generation,
            tenants,
            reader,
            writer,
            directory: GraphProfileDirectory::new(client),
        }
    }

    /// Add a skill to the profile as it is now.
    async fn add_skill(&self, name: &str) {
        self.add_skill_of(&self.ctx, self.key, name).await;
    }

    /// Add a skill to the profile of `key`, read and written as `ctx`.
    async fn add_skill_of(&self, ctx: &SecurityContext, key: SubjectKey, name: &str) {
        let version = self.reader.facts(ctx, key).await.unwrap().version;
        let plan = Plan {
            tenant_id: key.tenant_id,
            subject_id: key.subject_id,
            profile_version: version,
            ..connector_plan(vec![add_value("skill", "skills", json!({ "name": name }))])
        };
        assert_eq!(
            self.writer.write(ctx, &plan).await.unwrap(),
            Written::Stored
        );
    }

    /// Remove every live fact, as the erasure's plan does.
    async fn remove_all(&self) {
        let facts = self.reader.facts(&self.ctx, self.key).await.unwrap();
        let plan = Plan {
            tenant_id: self.key.tenant_id,
            subject_id: self.key.subject_id,
            profile_version: facts.version,
            steps: facts
                .facts
                .iter()
                .map(|fact| Step {
                    entity_kind: fact.category.name().to_owned(),
                    change: Change::RemoveEntity {
                        entity: fact.node_key.clone(),
                    },
                    confidence: None,
                })
                .collect(),
            ..connector_plan(Vec::new())
        };
        assert_eq!(
            self.writer.write(&self.ctx, &plan).await.unwrap(),
            Written::Stored
        );
    }
}

#[tokio::test]
async fn the_root_goes_once_its_facts_are_gone_and_the_next_generation_starts_a_new_profile() {
    let graph = Graph::new();
    graph.add_skill("Rust").await;
    assert_eq!(
        graph
            .directory
            .subjects(&graph.ctx, None)
            .await
            .unwrap()
            .subjects,
        [graph.key.subject_id],
        "a subject with a root is listed"
    );

    // A live fact keeps the root.
    assert_eq!(
        graph
            .writer
            .remove_root(&graph.ctx, graph.key, 0)
            .await
            .unwrap(),
        RootRemoval::FactsLeft(1)
    );

    graph.remove_all().await;
    assert_eq!(
        graph
            .writer
            .remove_root(&graph.ctx, graph.key, 0)
            .await
            .unwrap(),
        RootRemoval::Removed
    );
    let gone = graph.reader.facts(&graph.ctx, graph.key).await.unwrap();
    assert_eq!((gone.version, gone.facts.len()), (None, 0), "no profile");
    assert_eq!(
        graph
            .writer
            .remove_root(&graph.ctx, graph.key, 0)
            .await
            .unwrap(),
        RootRemoval::Gone,
        "removing again is safe"
    );
    assert!(
        graph
            .directory
            .subjects(&graph.ctx, None)
            .await
            .unwrap()
            .subjects
            .is_empty(),
        "a removed root is not listed"
    );

    // The erasure's finish moves the generation on: the next profile has a
    // root of its own, since the removed key is never given out again.
    graph.generation.0.store(1, Ordering::SeqCst);
    graph.add_skill("Go").await;
    let again = graph.reader.facts(&graph.ctx, graph.key).await.unwrap();
    assert_eq!(
        again.version,
        Some((1 << 32) | 1),
        "a new root, created at counter 1 in generation 1"
    );
    // A plan built on the first root at its first version, before the
    // erasure, does not pass for one built on the new root.
    let stale = Plan {
        tenant_id: graph.key.tenant_id,
        subject_id: graph.key.subject_id,
        profile_version: Some(1),
        ..connector_plan(vec![add_value("skill", "skills", json!({ "name": "C" }))])
    };
    assert_eq!(
        graph.writer.write(&graph.ctx, &stale).await.unwrap(),
        Written::Conflict,
        "planned again on the new profile"
    );
    let [fact] = again.facts.as_slice() else {
        panic!("one fact in the new profile, got {:?}", again.facts);
    };
    assert_eq!(fact.value, json!({ "name": "Go" }));
    assert_ne!(generation_root(&graph.key, 1), root_key(&graph.key));
    assert_eq!(
        graph
            .directory
            .subjects(&graph.ctx, None)
            .await
            .unwrap()
            .subjects,
        [graph.key.subject_id]
    );
}

/// A run whose erasure another run finished removes the root it was
/// started on, not the next generation's.
#[tokio::test]
async fn a_root_removal_leaves_the_root_of_a_later_generation() {
    let graph = Graph::new();
    graph.add_skill("Rust").await;
    graph.remove_all().await;
    assert_eq!(
        graph
            .writer
            .remove_root(&graph.ctx, graph.key, 0)
            .await
            .unwrap(),
        RootRemoval::Removed
    );
    graph.generation.0.store(1, Ordering::SeqCst);
    graph.add_skill("Go").await;

    assert_eq!(
        graph
            .writer
            .remove_root(&graph.ctx, graph.key, 0)
            .await
            .unwrap(),
        RootRemoval::Gone,
        "generation 0 has no root left"
    );
    let new = graph.reader.facts(&graph.ctx, graph.key).await.unwrap();
    assert_eq!(
        (new.generation, new.version, new.facts.len()),
        (1, Some((1 << 32) | 1), 1),
        "the new profile is untouched"
    );
}

#[tokio::test]
async fn the_root_deletes_the_facts_it_still_lists_before_it_goes() {
    let graph = Graph::new();
    graph.add_skill("Rust").await;
    let [fact] = graph
        .reader
        .facts(&graph.ctx, graph.key)
        .await
        .unwrap()
        .facts
        .try_into()
        .unwrap();
    // The removal is written, but its delete fails: the root lists the fact.
    graph.removal.failing.store(true, Ordering::SeqCst);
    graph.remove_all().await;
    assert!(graph.removal.removed.lock().unwrap().is_empty());
    graph.removal.failing.store(false, Ordering::SeqCst);

    assert_eq!(
        graph
            .writer
            .remove_root(&graph.ctx, graph.key, 0)
            .await
            .unwrap(),
        RootRemoval::Removed
    );
    assert_eq!(
        *graph.removal.removed.lock().unwrap(),
        std::slice::from_ref(&fact.node_key),
        "the listed fact is deleted before the root"
    );
    let gone = graph
        .client
        .get_node(&graph.ctx, &fact.node_key.0, None)
        .await;
    assert!(gone.is_err(), "the fact node is gone: {gone:?}");
}

#[tokio::test]
async fn a_root_whose_listed_fact_cannot_be_deleted_stays() {
    let graph = Graph::new();
    graph.add_skill("Rust").await;
    graph.removal.failing.store(true, Ordering::SeqCst);
    graph.remove_all().await;

    let err = graph.writer.remove_root(&graph.ctx, graph.key, 0).await;

    assert!(matches!(err, Err(DomainError::Unavailable(_))), "{err:?}");
    let still = graph.reader.facts(&graph.ctx, graph.key).await.unwrap();
    assert!(still.version.is_some(), "the root stays for the next run");
}

/// The in-memory store answers one page with no cursor; the cursor is shown
/// by the graph test over a scripted listing.
#[tokio::test]
async fn the_subjects_with_a_root_are_listed_only_in_the_tenant() {
    let graph = Graph::new();
    let mut subjects: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
    for subject_id in &subjects {
        let key = SubjectKey {
            tenant_id: graph.key.tenant_id,
            subject_id: *subject_id,
        };
        graph.add_skill_of(&graph.ctx, key, "Rust").await;
    }
    let elsewhere = SubjectKey {
        tenant_id: Uuid::new_v4(),
        subject_id: Uuid::new_v4(),
    };
    let elsewhere_ctx = ServiceIdentity::new(Uuid::new_v4())
        .context(elsewhere.tenant_id)
        .unwrap();
    graph.add_skill_of(&elsewhere_ctx, elsewhere, "Go").await;

    let page = graph.directory.subjects(&graph.ctx, None).await.unwrap();

    let mut listed = page.subjects;
    listed.sort();
    subjects.sort();
    assert_eq!(
        listed, subjects,
        "each subject once, none of another tenant"
    );
    assert_eq!(page.next, None);
}

#[tokio::test]
async fn a_cursor_that_cannot_be_read_is_a_defect() {
    let graph = Graph::new();

    let err = graph
        .directory
        .subjects(&graph.ctx, Some("not a cursor".to_owned()))
        .await;

    assert!(matches!(err, Err(DomainError::Internal(_))), "{err:?}");
}

/// A fixed time a day after this test run started, as the reader's clock.
fn a_day_later() -> OffsetDateTime {
    static AT: std::sync::OnceLock<OffsetDateTime> = std::sync::OnceLock::new();
    *AT.get_or_init(|| OffsetDateTime::now_utc() + time::Duration::days(1))
}

/// Retention periods that give every tenant `.0`.
struct EveryTenant(std::time::Duration);

#[async_trait]
impl crate::domain::deletion::RetentionPeriods for EveryTenant {
    async fn period(&self, _tenant_id: Uuid) -> Result<Option<std::time::Duration>, DomainError> {
        Ok(Some(self.0))
    }

    fn listed(&self) -> std::collections::BTreeSet<Uuid> {
        std::collections::BTreeSet::new()
    }

    fn has_default(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn the_planner_reads_no_fact_past_the_retention_period_while_the_subject_still_sees_it() {
    use crate::domain::planner::processing::ProfileSource;

    let graph = Graph::new();
    graph.add_skill("Rust").await;
    // Only so the two facts get distinct stored times; the cut does not
    // depend on how long anything takes.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    graph.add_skill("Go").await;
    let stored = graph.reader.facts(&graph.ctx, graph.key).await.unwrap();
    let [old, fresh] = stored.facts.as_slice() else {
        panic!("two facts, got {stored:?}");
    };
    let (old, fresh) = if old.stored_at <= fresh.stored_at {
        (old, fresh)
    } else {
        (fresh, old)
    };
    assert!(old.stored_at < fresh.stored_at, "{stored:?}");
    // The reader counts the period back from a fixed time, a day after the
    // facts, so the period starts exactly halfway between them.
    let cut = old.stored_at + (fresh.stored_at - old.stored_at) / 2;
    let period = std::time::Duration::try_from(a_day_later() - cut).unwrap();
    let reader = GraphProfileSource::new(
        Arc::clone(&graph.client),
        Arc::new(PayloadCounter),
        Arc::clone(&graph.generation) as _,
        Arc::new(EveryTenant(period)),
    )
    .with_clock(a_day_later);

    let planned = ProfileSource::read(
        &reader,
        &graph.ctx,
        graph.key.tenant_id,
        graph.key.subject_id,
    )
    .await
    .unwrap();
    let own = reader.facts(&graph.ctx, graph.key).await.unwrap();

    let planned_keys: Vec<&NodeKey> = planned.facts.iter().map(|fact| &fact.node_key).collect();
    assert_eq!(
        planned_keys,
        [&fresh.node_key],
        "only the fact within the period: {planned:?}"
    );
    assert_eq!(planned.version, Some(2), "the version is the profile's");
    assert_eq!(own.facts.len(), 2, "until retention removes it");
}

#[tokio::test]
async fn every_write_notes_its_tenant_for_retention() {
    let graph = Graph::new();

    graph.add_skill("Rust").await;
    // A tenant whose only data is in graph storage, as an agent's manage
    // call or an administrator's add leaves it, is still swept.
    assert_eq!(
        *graph.tenants.tenants.lock().unwrap(),
        [graph.key.tenant_id].into()
    );

    // A profile that existed before its tenant was noted, as one written
    // before this release, has its tenant noted on its next write.
    graph.tenants.tenants.lock().unwrap().clear();
    graph.add_skill("Go").await;
    assert_eq!(
        *graph.tenants.tenants.lock().unwrap(),
        [graph.key.tenant_id].into(),
        "a write to an existing profile notes its tenant too"
    );
}
