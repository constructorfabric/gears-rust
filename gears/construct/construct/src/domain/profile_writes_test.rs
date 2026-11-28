//! The removal of the nodes of a kind the schema lost, over an in-memory profile.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use time::OffsetDateTime;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::error::DomainError;
use super::plan::{Change, NodeKey, Origin, Plan};
use super::planner::processing::{PlanWriter, Written};
use super::profile::Category;
use super::profile_writes::Orphan;
use super::subject_settings::SubjectKey;
use crate::test_support::{MemoryProfile, MemoryWriter, profile_writes, stored_fact};

fn key() -> SubjectKey {
    SubjectKey {
        tenant_id: Uuid::new_v4(),
        subject_id: Uuid::new_v4(),
    }
}

fn orphan() -> Orphan {
    Orphan {
        node_key: NodeKey::new_in(Uuid::new_v4()),
        stored_at: OffsetDateTime::now_utc(),
    }
}

fn identity() -> Uuid {
    Uuid::new_v4()
}

#[tokio::test]
async fn the_nodes_of_a_lost_kind_are_removed_in_one_plan_as_the_subjects_own_delete() {
    let key = key();
    let profile = MemoryProfile::with(vec![stored_fact(
        Category::PROJECT,
        "project",
        json!({ "name": "Rust" }),
        10,
    )]);
    let (first, second) = (orphan(), orphan());
    *profile.orphans.lock().unwrap() = vec![first.clone(), second.clone()];
    let writer = MemoryWriter::new(Arc::clone(&profile), 0);
    let remover = profile_writes(Arc::clone(&profile), Arc::clone(&writer), Some(identity()));
    let (construct, _) = remover.construct(key.tenant_id).await.unwrap();

    let freed = remover
        .forget_orphans(&construct, key, remover.deadline())
        .await
        .unwrap();

    assert_eq!(freed, 2);
    let [(_, plan)] = writer.plans.lock().unwrap().clone().try_into().unwrap();
    assert_eq!(
        plan.origin,
        Origin::Subject {
            subject: key.subject_id
        }
    );
    let removals: Vec<&NodeKey> = plan
        .steps
        .iter()
        .filter_map(|step| match &step.change {
            Change::RemoveEntity { entity } => Some(entity),
            _ => None,
        })
        .collect();
    assert_eq!(removals, [&first.node_key, &second.node_key]);
    assert!(profile.orphans.lock().unwrap().is_empty());
    assert_eq!(profile.live().len(), 1, "the live fact stays");
}

#[tokio::test]
async fn a_profile_without_such_nodes_is_not_written() {
    let key = key();
    let profile = MemoryProfile::with(Vec::new());
    let writer = MemoryWriter::new(Arc::clone(&profile), 0);
    let remover = profile_writes(Arc::clone(&profile), Arc::clone(&writer), Some(identity()));
    let (construct, _) = remover.construct(key.tenant_id).await.unwrap();

    let freed = remover
        .forget_orphans(&construct, key, remover.deadline())
        .await
        .unwrap();

    assert_eq!(freed, 0);
    assert!(writer.plans.lock().unwrap().is_empty());
}

/// A writer whose write fails with no answer, while the owner has added the lost kind back: the nodes it was to
/// remove read as live facts of that kind afterwards.
struct ReaddingWriter(Arc<MemoryProfile>);

#[async_trait]
impl PlanWriter for ReaddingWriter {
    async fn write_by(
        &self,
        _ctx: &SecurityContext,
        _plan: &Plan,
        _deadline: tokio::time::Instant,
    ) -> Result<Written, DomainError> {
        let readded: Vec<_> = self
            .0
            .orphans
            .lock()
            .unwrap()
            .drain(..)
            .map(|orphan| {
                let mut fact =
                    stored_fact(Category::PROJECT, "project", json!({ "name": "Rust" }), 10);
                fact.node_key = orphan.node_key;
                fact
            })
            .collect();
        self.0.facts.lock().unwrap().extend(readded);
        Err(DomainError::Unavailable("no answer".to_owned()))
    }
}

#[tokio::test]
async fn a_removal_whose_nodes_read_live_again_after_an_unknown_result_is_not_reported_done() {
    let key = key();
    let profile = MemoryProfile::with(Vec::new());
    *profile.orphans.lock().unwrap() = vec![orphan()];
    let remover = profile_writes(
        Arc::clone(&profile),
        Arc::new(ReaddingWriter(Arc::clone(&profile))),
        Some(identity()),
    );
    let (construct, _) = remover.construct(key.tenant_id).await.unwrap();

    let result = remover
        .forget_orphans(&construct, key, remover.deadline())
        .await;

    assert!(
        result.is_err(),
        "the nodes are live facts again, so the removal did not land: {result:?}"
    );
}
