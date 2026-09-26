use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::task::Family;
use crate::domain::error::DomainError;
use crate::domain::repo::{SyncWatermarkRecord, SyncWatermarkRepository};

pub const SWEEP_OVERLAP: Duration = Duration::minutes(5);

#[must_use]
fn stop_threshold(stored: Option<&SyncWatermarkRecord>, force: bool) -> Option<DateTime<Utc>> {
    if force {
        return None;
    }
    stored
        .and_then(|w| w.last_seen_updated_at.as_deref())
        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
        .map(|at| at.with_timezone(&Utc) - SWEEP_OVERLAP)
}

#[must_use]
pub fn is_stale(updated_at: Option<&str>, threshold: Option<DateTime<Utc>>) -> bool {
    let (Some(updated_at), Some(threshold)) = (updated_at, threshold) else {
        return false;
    };
    DateTime::parse_from_rfc3339(updated_at).is_ok_and(|at| at.with_timezone(&Utc) < threshold)
}

/// The later of the watermark already stored and the candidate this sweep
/// staged.
///
/// A sweep starts its high-water mark at its own lower bound, the stored
/// watermark less [`SWEEP_OVERLAP`], so one that saw nothing new stages that
/// bound rather than the watermark it started from. Promoting it as it comes
/// would walk the watermark five minutes back on every idle sweep, widening
/// the window each time.
///
/// A stamp that will not parse counts as older than one that will, so an
/// unreadable stored value is replaced by a readable candidate. When neither
/// parses the stored value stays: the next sweep is the one that can fix it,
/// and writing an equally unreadable candidate over it would only move the
/// problem. What the sweep does in the meantime is unaffected, because an
/// unreadable watermark is treated as no watermark and the walk covers
/// everything.
fn later_watermark(stored: Option<&str>, candidate: String) -> String {
    let instant = |raw: &str| {
        DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|at| at.with_timezone(&Utc))
    };
    let Some(stored) = stored else {
        return candidate;
    };
    match (instant(stored), instant(&candidate)) {
        (Some(stored_at), Some(candidate_at)) if candidate_at > stored_at => candidate,
        (None, Some(_)) => candidate,
        _ => stored.to_owned(),
    }
}

#[must_use]
pub fn high_water(seen: &[&str], threshold: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    seen.iter()
        .filter_map(|at| DateTime::parse_from_rfc3339(at).ok())
        .map(|at| at.with_timezone(&Utc))
        .max()
        .max(threshold)
}

/// What a sweep needs before it walks: the instant below which an entity is
/// too old to be worth looking at, and the validator page one carried last
/// time so an unchanged listing can stop before page two.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepStart {
    pub updated_after: Option<DateTime<Utc>>,
    pub page1_etag: Option<String>,
}

pub struct SweepWatermark {
    watermark_store: Arc<dyn SyncWatermarkRepository>,
}

impl SweepWatermark {
    #[must_use]
    pub fn new(watermark_store: Arc<dyn SyncWatermarkRepository>) -> Self {
        Self { watermark_store }
    }

    /// # Errors
    /// `Database`/`Internal` when the watermark row cannot be read.
    pub async fn start_sweep(
        &self,
        scope: &AccessScope,
        repo_id: i64,
        family: Family,
        force: bool,
    ) -> Result<SweepStart, DomainError> {
        let stored = self
            .watermark_store
            .find(scope, repo_id, family.as_str())
            .await?;
        Ok(SweepStart {
            updated_after: stop_threshold(stored.as_ref(), force),
            page1_etag: if force {
                None
            } else {
                stored.and_then(|w| w.page1_etag)
            },
        })
    }

    /// # Errors
    /// `Database`/`Internal` when the watermark row cannot be read or written.
    pub async fn stage(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        repo_id: i64,
        family: Family,
        candidate: Option<DateTime<Utc>>,
    ) -> Result<(), DomainError> {
        let stored = self
            .watermark_store
            .find(scope, repo_id, family.as_str())
            .await?;
        let candidate = candidate.map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        self.watermark_store
            .upsert(
                scope,
                tenant_id,
                SyncWatermarkRecord {
                    repo_id,
                    family: family.as_str().to_owned(),
                    last_seen_updated_at: stored
                        .as_ref()
                        .and_then(|w| w.last_seen_updated_at.clone()),
                    page1_etag: stored.and_then(|w| w.page1_etag),
                    sweep_in_progress: true,
                    candidate_high_water: candidate,
                },
            )
            .await?;
        Ok(())
    }

    /// Move the staged candidate into `last_seen_updated_at` and record page
    /// one's validator. Both happen only here, at the family-complete point
    /// (ALGORITHMS §6.4): a run that stopped before its refinements finished
    /// leaves neither behind, so the next run walks the listing in full and
    /// the gate re-seeds whatever was left `pending`.
    ///
    /// A family with nothing in it stages no candidate; the sweep still
    /// finished, so the row is closed out with the watermark it already had.
    ///
    /// # Errors
    /// `Database`/`Internal` when the watermark row cannot be read or written.
    pub async fn promote(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        repo_id: i64,
        family: Family,
        page1_etag: Option<String>,
    ) -> Result<(), DomainError> {
        let Some(stored) = self
            .watermark_store
            .find(scope, repo_id, family.as_str())
            .await?
        else {
            return Ok(());
        };
        let last_seen_updated_at = match stored.candidate_high_water.clone() {
            Some(candidate) => Some(later_watermark(
                stored.last_seen_updated_at.as_deref(),
                candidate,
            )),
            None => stored.last_seen_updated_at.clone(),
        };
        self.watermark_store
            .upsert(
                scope,
                tenant_id,
                SyncWatermarkRecord {
                    last_seen_updated_at,
                    page1_etag: page1_etag.or_else(|| stored.page1_etag.clone()),
                    sweep_in_progress: false,
                    candidate_high_water: None,
                    ..stored
                },
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "sweep_watermark_tests.rs"]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a panic in these tests is the failure report"
)]
mod sweep_watermark_tests;
