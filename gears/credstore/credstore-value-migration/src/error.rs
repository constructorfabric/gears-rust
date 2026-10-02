//! Error types of the migration stages.

use std::path::PathBuf;

use credstore_sdk::CredStoreError;
use uuid::Uuid;

/// Failure reported by a [`LegacyValueStore`](crate::LegacyValueStore)
/// implementation. The message must never contain a secret value.
#[derive(Debug, thiserror::Error)]
#[error("legacy store: {0}")]
pub struct LegacyStoreError(pub String);

impl LegacyStoreError {
    /// Wraps a description of the failure.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// A stage aborted. Nothing is silently skipped: every variant stops the
/// stage, and a re-run resumes from the results file.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    /// A database statement failed.
    #[error("database error: {0}")]
    Db(#[from] sea_orm::DbErr),
    /// The results file could not be read or written.
    #[error("results file {path}: {source}")]
    Io {
        /// Path of the results file.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },
    /// The results file is not a valid results file.
    #[error("results file {path}: {reason}")]
    ResultsFile {
        /// Path of the results file.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// The results file is required but does not exist.
    #[error("results file {0} does not exist")]
    ResultsFileMissing(PathBuf),
    /// The database is not in the schema state the stage requires.
    #[error("wrong schema state: {0}")]
    WrongSchema(String),
    /// The database backend is neither `PostgreSQL` nor `SQLite`.
    #[error("unsupported database backend (PostgreSQL and SQLite only)")]
    UnsupportedBackend,
    /// The legacy store failed.
    #[error(transparent)]
    Legacy(#[from] LegacyStoreError),
    /// The new store failed.
    #[error("new store: {0}")]
    Target(#[from] CredStoreError),
    /// Rows carry fingerprints but the legacy store holds no fence key, so
    /// none of them can be verified.
    #[error(
        "the legacy store has no fence key, but {rows} row(s) carry a value fingerprint: \
         nothing can be verified"
    )]
    FenceKeyAbsent {
        /// Rows with a non-NULL `value_fp`.
        rows: usize,
    },
    /// A row has a status the shipped schema does not produce.
    #[error("row {id} has unexpected status {status}")]
    UnexpectedStatus {
        /// Row id.
        id: Uuid,
        /// The status value found.
        status: i16,
    },
    /// A stored column holds a value outside its documented encoding.
    #[error("row {id}: {reason}")]
    BadRow {
        /// Row id.
        id: Uuid,
        /// What is wrong.
        reason: String,
    },
    /// The new store returned other bytes than were written, or no bytes at
    /// all: a store failure, not a property of the data.
    #[error("read-back from the new store failed for record {id}: {reason}")]
    ReadBack {
        /// Record id.
        id: Uuid,
        /// What happened.
        reason: String,
    },
    /// A legacy reference has the shape of a new store key (a UUID equal to a
    /// record id of the results file); deleting it could remove a new value.
    #[error(
        "legacy reference {reference} (tenant {tenant_id}) is a record id of the results file: \
         refusing to delete, the old and new stores may share a mount"
    )]
    NewKeyShaped {
        /// The offending reference.
        reference: String,
        /// Its tenant.
        tenant_id: Uuid,
    },
}

impl From<sea_orm::TryGetError> for MigrationError {
    fn from(e: sea_orm::TryGetError) -> Self {
        Self::Db(sea_orm::DbErr::from(e))
    }
}
