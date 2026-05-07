//! Self-healing reconciliation primitive.
//!
//! Per ADR-0004, the FileStorage row may legitimately diverge from the
//! backend after a presigned-first overwrite where `change_status` never
//! arrived. `sync_etag_from_backend` is the repair primitive used by:
//!
//! - `read_file` after the backend GET when `derived != row.etag`.
//! - `create_presigned_url` with `refresh_etag = true` (HEAD on backend
//!   first).
//! - `change_status` late-arrival idempotent path.
//!
//! The repair UPDATE runs in **system context** — no `SecurityContext`
//! consulted — and does **not** bump `meta_revision`.

use modkit_db::secure::DBRunner;
use uuid::Uuid;

use super::error::DomainError;
use super::etag::compose;
use super::repo::{FilesRepo, MutationOutcome};
use file_storage_sdk::Etag;

/// Outcome of a self-healing repair attempt.
#[derive(Debug, Clone)]
pub enum SelfHealOutcome {
    /// The row was already at `derived`; no UPDATE necessary.
    AlreadyConsistent,
    /// We won the repair UPDATE — caller may proceed with `derived`.
    Repaired { derived: Etag },
    /// Another writer beat us to the repair (or the row moved); caller
    /// should re-fetch the row to see the latest etag.
    Raced,
}

/// Run the repair primitive. Computes `derived = compose(content_hash,
/// meta_revision)`. If it equals `current_etag`, returns
/// `AlreadyConsistent`. Otherwise runs the system-context etag-conditional
/// UPDATE; returns `Repaired` on 1 row affected and `Raced` on 0.
pub async fn sync_etag_from_backend<R: FilesRepo, C: DBRunner>(
    repo: &R,
    runner: &C,
    file_id: Uuid,
    current_etag: &Etag,
    content_hash_from_backend: &str,
    meta_revision: i64,
) -> Result<SelfHealOutcome, DomainError> {
    let derived = compose(content_hash_from_backend, meta_revision);
    if derived == *current_etag {
        return Ok(SelfHealOutcome::AlreadyConsistent);
    }

    let outcome = repo
        .repair_etag_system_context(runner, file_id, current_etag, &derived)
        .await?;

    match outcome {
        MutationOutcome::Applied => Ok(SelfHealOutcome::Repaired { derived }),
        MutationOutcome::NoMatch => Ok(SelfHealOutcome::Raced),
    }
}
