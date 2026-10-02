//! Stage 2: point the migrated rows at their new versions.
//!
//! Runs after `m0002`, before the new gear version starts.

use std::path::Path;

use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, TransactionTrait};
use uuid::Uuid;

use crate::Mode;
use crate::db::{self, Schema};
use crate::error::MigrationError;
use crate::results::{self, Entry, Outcome};

const STATUS_ACTIVE: i16 = 2;
const STATUS_DECLARED: i16 = 4;
const FALLBACK_NONE: i16 = 2;

/// Result of [`activate`].
///
/// In [`Mode::DryRun`] the counts are what an apply run would do.
#[derive(Debug, Clone, Default)]
pub struct ActivateReport {
    /// Rows set to `active` with their `value_version`.
    pub promoted: usize,
    /// Rows left `declared` and marked `fallback = 2` (`none`), because their
    /// value was missing or failed verification.
    pub suppressed: usize,
    /// Entries whose row was already in the target state (a re-run).
    pub already_done: usize,
    /// Entries whose row no longer exists (deleted in the meantime).
    pub unknown_ids: Vec<Uuid>,
    /// Rows in a state the stage does not touch (for example `active` with a
    /// different version, or `active` although the value was lost). Left as
    /// they are; the operator must look at them.
    pub unexpected: Vec<Uuid>,
    /// Results-file entries of rows that were `provisioning`/`deprovisioning`:
    /// `m0002` deleted those rows, nothing to do.
    pub ignored_unfinished: usize,
}

enum Plan {
    Promote(String),
    Suppress,
}

struct State {
    status: i16,
    value_version: Option<String>,
    fallback: i16,
}

async fn state_of(
    db: &DatabaseConnection,
    backend: DatabaseBackend,
    id: Uuid,
) -> Result<Option<State>, MigrationError> {
    let sql = format!(
        "SELECT status, value_version, fallback FROM credstore_secrets WHERE id = {}",
        db::ph(backend, 1)
    );
    let Some(row) = db
        .query_one_raw(db::stmt(backend, &sql, vec![id.into()]))
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(State {
        status: row.try_get("", "status")?,
        value_version: row.try_get("", "value_version")?,
        fallback: row.try_get("", "fallback")?,
    }))
}

enum Verdict {
    Do(Plan),
    Done,
    Unknown,
    Unexpected,
}

fn judge(entry: &Entry, state: Option<&State>) -> Verdict {
    let Some(state) = state else {
        return Verdict::Unknown;
    };
    if entry.outcome.is_copied() {
        let Some(version) = entry.value_version.as_deref() else {
            return Verdict::Unexpected;
        };
        return match (state.status, state.value_version.as_deref()) {
            (STATUS_DECLARED, None) => Verdict::Do(Plan::Promote(version.to_owned())),
            (STATUS_ACTIVE, Some(v)) if v == version => Verdict::Done,
            _ => Verdict::Unexpected,
        };
    }
    match state.status {
        STATUS_DECLARED if state.fallback == FALLBACK_NONE => Verdict::Done,
        STATUS_DECLARED => Verdict::Do(Plan::Suppress),
        _ => Verdict::Unexpected,
    }
}

async fn apply_plan(
    txn: &impl ConnectionTrait,
    backend: DatabaseBackend,
    id: Uuid,
    plan: &Plan,
) -> Result<bool, MigrationError> {
    let result = match plan {
        Plan::Promote(version) => {
            let sql = format!(
                "UPDATE credstore_secrets SET status = 2, value_version = {}, \
                 version = version + 1 WHERE id = {} AND status = 4 AND value_version IS NULL",
                db::ph(backend, 1),
                db::ph(backend, 2)
            );
            txn.execute_raw(db::stmt(
                backend,
                &sql,
                vec![version.clone().into(), id.into()],
            ))
            .await?
        }
        Plan::Suppress => {
            let sql = format!(
                "UPDATE credstore_secrets SET fallback = 2 WHERE id = {} AND status = 4",
                db::ph(backend, 1)
            );
            txn.execute_raw(db::stmt(backend, &sql, vec![id.into()]))
                .await?
        }
    };
    Ok(result.rows_affected() == 1)
}

/// Activates the rows of a finished `copy`, using only the results file.
///
/// For each copied entry: `status = 2, value_version = <from the file>,
/// version = version + 1` where the row is still `declared` without a
/// version. For each `missing` / `fp_mismatch` / `unknown_fence_key` entry:
/// `fallback = 2` where the row is `declared`, so the reference does not
/// silently fall through to an ancestor's value. Rows are classified first
/// (including ids that no longer exist), then changed in one transaction;
/// re-running is safe.
///
/// # Errors
///
/// [`MigrationError::WrongSchema`] when `m0002` has not been applied (or did
/// not complete); [`MigrationError::ResultsFileMissing`] when there is no
/// results file; database and I/O failures.
pub async fn activate(
    db: &DatabaseConnection,
    results_path: &Path,
    mode: Mode,
) -> Result<ActivateReport, MigrationError> {
    let backend = db::backend(db)?;
    if db::detect_schema(db, backend).await? != Schema::ValueVersions {
        return Err(MigrationError::WrongSchema(
            "activate runs after m0002: value_version is missing or value_fp still exists"
                .to_owned(),
        ));
    }
    let entries = results::read(results_path)?
        .ok_or_else(|| MigrationError::ResultsFileMissing(results_path.to_path_buf()))?;

    let mut report = ActivateReport::default();
    let mut work = Vec::new();
    for entry in &entries {
        if entry.outcome == Outcome::Unfinished {
            report.ignored_unfinished += 1;
            continue;
        }
        let state = state_of(db, backend, entry.id).await?;
        match judge(entry, state.as_ref()) {
            Verdict::Do(plan) => work.push((entry.id, plan)),
            Verdict::Done => report.already_done += 1,
            Verdict::Unknown => report.unknown_ids.push(entry.id),
            Verdict::Unexpected => report.unexpected.push(entry.id),
        }
    }
    run_plan(db, backend, &work, mode, &mut report).await?;
    Ok(report)
}

async fn run_plan(
    db: &DatabaseConnection,
    backend: DatabaseBackend,
    work: &[(Uuid, Plan)],
    mode: Mode,
    report: &mut ActivateReport,
) -> Result<(), MigrationError> {
    let txn = db.begin().await?;
    for (id, plan) in work {
        let changed = match mode {
            Mode::Apply => apply_plan(&txn, backend, *id, plan).await?,
            Mode::DryRun => true,
        };
        if !changed {
            report.unexpected.push(*id);
            continue;
        }
        match plan {
            Plan::Promote(_) => report.promoted += 1,
            Plan::Suppress => report.suppressed += 1,
        }
        tracing::info!(
            id = %id,
            action = match plan {
                Plan::Promote(_) => "promoted",
                Plan::Suppress => "suppressed",
            },
            dry_run = mode == Mode::DryRun,
            "credstore value migration: row activated"
        );
    }
    txn.commit().await?;
    Ok(())
}
