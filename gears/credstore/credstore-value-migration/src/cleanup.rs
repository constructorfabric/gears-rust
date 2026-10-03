//! Stage 3: retire the superseded entries of the old store.
//!
//! Runs only after the migrated values have been tested. Needs no database:
//! the results file holds every legacy address.

use std::collections::HashSet;
use std::path::Path;

use uuid::Uuid;

use crate::Mode;
use crate::copy::RowRef;
use crate::error::MigrationError;
use crate::fence::FENCE_KEY_REF;
use crate::legacy::LegacyValueStore;
use crate::results::{self, Entry, Outcome};

/// Result of [`cleanup`].
///
/// In [`Mode::DryRun`] the counts are what an apply run would do.
#[derive(Debug, Clone, Default)]
pub struct CleanupReport {
    /// Legacy entries deleted (copied rows and unfinished rows).
    pub deleted: usize,
    /// Entries kept as evidence (`fp_mismatch`, `unknown_fence_key`).
    pub kept_as_evidence: Vec<RowRef>,
    /// `missing` entries: nothing was ever there to delete.
    pub nothing_to_delete: usize,
    /// Whether the old fence key entry was deleted (only with the explicit
    /// flag, and last).
    pub fence_key_deleted: bool,
}

fn candidates(entries: &[Entry]) -> (Vec<&Entry>, CleanupReport) {
    let mut report = CleanupReport::default();
    let mut delete = Vec::new();
    for entry in entries {
        match entry.outcome {
            Outcome::Copied | Outcome::Unverified | Outcome::Unfinished => delete.push(entry),
            Outcome::Missing => report.nothing_to_delete += 1,
            Outcome::FpMismatch | Outcome::UnknownFenceKey => {
                report.kept_as_evidence.push(RowRef {
                    id: entry.id,
                    tenant_id: entry.tenant_id,
                    reference: entry.reference.clone(),
                });
            }
        }
    }
    (delete, report)
}

/// Refuses when a legacy reference has the shape of a new key.
fn check_no_new_keys(delete: &[&Entry], entries: &[Entry]) -> Result<(), MigrationError> {
    let record_ids: HashSet<Uuid> = entries.iter().map(|e| e.id).collect();
    for entry in delete {
        let shaped = Uuid::parse_str(&entry.reference)
            .is_ok_and(|candidate| record_ids.contains(&candidate));
        if shaped {
            return Err(MigrationError::NewKeyShaped {
                reference: entry.reference.clone(),
                tenant_id: entry.tenant_id,
            });
        }
    }
    Ok(())
}

/// Deletes from the LEGACY store, strictly by the old addresses in the
/// results file (never by enumerating the store).
///
/// Copied entries and `provisioning`/`deprovisioning` rows are deleted;
/// `fp_mismatch` and `unknown_fence_key` entries are kept as evidence. The
/// fence key goes last, and only when `include_fence_key` is set.
///
/// Before anything is deleted the stage aborts if any candidate legacy
/// reference parses as a UUID equal to a record id of the results file (a new
/// key shape: the old and new stores may share a mount).
///
/// Deleting an absent entry is success, so a re-run is safe.
///
/// # Errors
///
/// [`MigrationError::ResultsFileMissing`], [`MigrationError::NewKeyShaped`],
/// and legacy-store failures.
pub async fn cleanup(
    legacy: &dyn LegacyValueStore,
    results_path: &Path,
    mode: Mode,
    include_fence_key: bool,
) -> Result<CleanupReport, MigrationError> {
    let entries = results::read(results_path)?
        .ok_or_else(|| MigrationError::ResultsFileMissing(results_path.to_path_buf()))?;
    let (delete, mut report) = candidates(&entries);
    check_no_new_keys(&delete, &entries)?;

    for entry in &delete {
        if mode == Mode::Apply {
            legacy
                .delete(entry.tenant_id, &entry.reference, entry.owner_id)
                .await?;
        }
        report.deleted += 1;
        tracing::info!(
            id = %entry.id,
            tenant = %entry.tenant_id,
            reference = %entry.reference,
            outcome = entry.outcome.as_str(),
            dry_run = mode == Mode::DryRun,
            "credstore value migration: legacy entry deleted"
        );
    }
    if include_fence_key {
        delete_fence_key(legacy, mode, &mut report).await?;
    }
    Ok(report)
}

async fn delete_fence_key(
    legacy: &dyn LegacyValueStore,
    mode: Mode,
    report: &mut CleanupReport,
) -> Result<(), MigrationError> {
    if !report.kept_as_evidence.is_empty() {
        tracing::warn!(
            kept = report.kept_as_evidence.len(),
            "deleting the fence key: the kept entries can no longer be verified"
        );
    }
    if mode == Mode::Apply {
        legacy.delete(Uuid::nil(), FENCE_KEY_REF, None).await?;
    }
    report.fence_key_deleted = true;
    tracing::info!(
        reference = FENCE_KEY_REF,
        dry_run = mode == Mode::DryRun,
        "credstore value migration: legacy fence key deleted"
    );
    Ok(())
}
