use std::collections::HashMap;
use std::sync::Arc;

use aws_lc_rs::digest::{self, SHA256};
use chrono::{DateTime, Duration, Utc};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::task::Entity;
use crate::domain::error::DomainError;
use crate::domain::repo::{EntityFingerprintRecord, EntityFingerprintRepository};

pub const REFINEMENT_PENDING: &str = "pending";
pub const REFINEMENT_COMPLETE: &str = "complete";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateReason {
    New,
    FingerprintChanged,
    ChildCountsChanged,
    Incomplete,
    TtlExpired,
    Forced,
}

impl GateReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::FingerprintChanged => "fingerprint_changed",
            Self::ChildCountsChanged => "child_counts_changed",
            Self::Incomplete => "incomplete",
            Self::TtlExpired => "ttl_expired",
            Self::Forced => "forced",
        }
    }
}

#[derive(Debug, Clone)]
pub struct GateInputs {
    pub fingerprint: String,
    pub child_counts_hash: Option<String>,
    pub updated_at: Option<String>,
    pub node_id: Option<String>,
    pub terminal: bool,
}

#[must_use]
pub fn fingerprint(mut fields: Vec<(&str, String)>) -> String {
    fields.sort_unstable_by_key(|(key, _)| *key);
    let canonical = fields
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(";");
    hash(&canonical)
}

#[must_use]
pub fn child_counts_hash(counts: &[(&str, Option<i64>)]) -> Option<String> {
    let mut present: Vec<(&str, i64)> = counts
        .iter()
        .filter_map(|(name, count)| count.map(|c| (*name, c)))
        .collect();
    if present.is_empty() {
        return None;
    }
    present.sort_unstable_by_key(|(name, _)| *name);
    let canonical = present
        .iter()
        .map(|(name, count)| format!("{name}={count}"))
        .collect::<Vec<_>>()
        .join(";");
    Some(hash(&canonical))
}

fn hash(canonical: &str) -> String {
    hex::encode(digest::digest(&SHA256, canonical.as_bytes()).as_ref())
}

#[must_use]
pub fn family_ttl(entity: Entity, terminal: bool) -> Option<Duration> {
    match entity {
        Entity::Commit => (!terminal).then(|| Duration::hours(1)),
        Entity::PullRequest => Some(if terminal {
            Duration::days(7)
        } else {
            Duration::hours(2)
        }),
        Entity::Issue => Some(if terminal {
            Duration::days(7)
        } else {
            Duration::hours(4)
        }),
        Entity::WorkflowRun => Some(Duration::days(1)),
    }
}

pub struct ChangeGate {
    fingerprints: Arc<dyn EntityFingerprintRepository>,
}

impl ChangeGate {
    #[must_use]
    pub fn new(fingerprints: Arc<dyn EntityFingerprintRepository>) -> Self {
        Self { fingerprints }
    }

    /// The gate for a whole listing page: one read of the page's stored
    /// fingerprints and one write back, instead of a round trip per entity.
    /// The answer keeps the order of `items`.
    ///
    /// # Errors
    /// `Database`/`Internal` when the fingerprint rows cannot be read or
    /// written.
    #[allow(
        clippy::too_many_arguments,
        reason = "the gate needs the whole request to answer: who is asking, which \
                  repository and family, the page itself, the clock and the force \
                  flag. Bundling them into a struct would move the same list one \
                  call up"
    )]
    pub async fn evaluate_page(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        repo_id: i64,
        entity: Entity,
        items: &[(&str, &GateInputs)],
        now: DateTime<Utc>,
        force: bool,
    ) -> Result<Vec<Option<GateReason>>, DomainError> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let entity_ids: Vec<String> = items.iter().map(|(id, _)| (*id).to_owned()).collect();
        let mut stored: HashMap<String, EntityFingerprintRecord> = self
            .fingerprints
            .find_many(scope, repo_id, entity.as_str(), &entity_ids)
            .await?
            .into_iter()
            .map(|record| (record.entity_id.clone(), record))
            .collect();

        let mut reasons = Vec::with_capacity(items.len());
        let mut records = Vec::with_capacity(items.len());
        for (entity_id, inputs) in items {
            let stored = stored.remove(*entity_id);
            let reason = evaluate_refinement_gate(stored.as_ref(), inputs, entity, now, force);
            records.push(gated_record(
                repo_id,
                entity,
                entity_id,
                inputs,
                stored,
                reason.is_some(),
            ));
            reasons.push(reason);
        }
        self.fingerprints
            .upsert_many(scope, tenant_id, records)
            .await?;
        Ok(reasons)
    }

    /// # Errors
    /// `Database`/`Internal` when the fingerprint row cannot be read or
    /// written.
    pub async fn mark_refined(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        repo_id: i64,
        entity: Entity,
        entity_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        let Some(stored) = self
            .fingerprints
            .find(scope, repo_id, entity.as_str(), entity_id)
            .await?
        else {
            return Ok(());
        };
        self.fingerprints
            .upsert(
                scope,
                tenant_id,
                EntityFingerprintRecord {
                    last_refined_at: Some(now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                    refinement_status: REFINEMENT_COMPLETE.to_owned(),
                    ..stored
                },
            )
            .await?;
        Ok(())
    }
}

fn gated_record(
    repo_id: i64,
    entity: Entity,
    entity_id: &str,
    inputs: &GateInputs,
    stored: Option<EntityFingerprintRecord>,
    refine: bool,
) -> EntityFingerprintRecord {
    let refinement_status = if refine {
        REFINEMENT_PENDING.to_owned()
    } else {
        stored.as_ref().map_or_else(
            || REFINEMENT_PENDING.to_owned(),
            |s| s.refinement_status.clone(),
        )
    };
    let child_counts_hash = inputs
        .child_counts_hash
        .clone()
        .or_else(|| stored.as_ref().and_then(|s| s.child_counts_hash.clone()));
    EntityFingerprintRecord {
        repo_id,
        family: entity.as_str().to_owned(),
        entity_id: entity_id.to_owned(),
        fingerprint: inputs.fingerprint.clone(),
        updated_at: inputs.updated_at.clone(),
        node_id: inputs.node_id.clone(),
        child_counts_hash,
        last_refined_at: stored.and_then(|s| s.last_refined_at),
        refinement_status,
    }
}

fn evaluate_refinement_gate(
    stored: Option<&EntityFingerprintRecord>,
    inputs: &GateInputs,
    entity: Entity,
    now: DateTime<Utc>,
    force: bool,
) -> Option<GateReason> {
    if force {
        return Some(GateReason::Forced);
    }
    let Some(stored) = stored else {
        return Some(GateReason::New);
    };
    if stored.fingerprint != inputs.fingerprint {
        return Some(GateReason::FingerprintChanged);
    }
    if inputs.child_counts_hash.is_some() && stored.child_counts_hash != inputs.child_counts_hash {
        return Some(GateReason::ChildCountsChanged);
    }
    if stored.refinement_status != REFINEMENT_COMPLETE {
        return Some(GateReason::Incomplete);
    }
    let ttl = family_ttl(entity, inputs.terminal)?;
    let fresh = stored
        .last_refined_at
        .as_deref()
        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
        .is_some_and(|last| now - last.with_timezone(&Utc) <= ttl);
    (!fresh).then_some(GateReason::TtlExpired)
}

#[cfg(test)]
#[path = "change_gate_tests.rs"]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a panic in these tests is the failure report"
)]
mod change_gate_tests;
