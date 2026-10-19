//! The exception register (DESIGN §3.7 `bss_rating__exception`; feature 01 §3.3).
//!
//! A pipeline feature records a classified failure as an exception: one **open** row per
//! `(tenant_id, subject_kind, subject_ref, reason_code)`, whose `attempts` counts the failures. It
//! is resolved when its subject succeeds or an operator resolves it; a later failure of the same
//! subject and reason opens a new row. The sweeper re-enqueues retryable subjects every 15 minutes
//! while it holds the Cluster leadership `exception-sweep`.

mod sweeper;

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_db::Db;
use toolkit_db::secure::DbTx;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

pub use sweeper::{ExceptionSweeper, SWEEP_BATCH};

pub use crate::domain::ports::SweepReport;

use crate::domain::error::DomainError;
use crate::domain::ports::Clock;

/// The longest subject reference or reason code stored.
pub const MAX_REF_LEN: usize = 255;

/// The kind of object an exception is about.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SubjectKind {
    /// A captured usage record.
    UsageRecord,
    /// A child window.
    Child,
    /// A commercial fact.
    Fact,
    /// An inbox entry.
    Inbox,
    /// A source loss.
    SourceLoss,
}

impl SubjectKind {
    /// Every kind, in a stable order.
    pub const ALL: [Self; 5] = [
        Self::UsageRecord,
        Self::Child,
        Self::Fact,
        Self::Inbox,
        Self::SourceLoss,
    ];

    /// The stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UsageRecord => "usage_record",
            Self::Child => "child",
            Self::Fact => "fact",
            Self::Inbox => "inbox",
            Self::SourceLoss => "source_loss",
        }
    }

    /// The kind a stored name denotes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == name)
    }
}

impl fmt::Display for SubjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What an open exception is unique on. Built only through [`ExceptionKey::new`], so every key
/// holds its bounds.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExceptionKey {
    tenant_id: Uuid,
    subject_kind: SubjectKind,
    subject_ref: String,
    reason_code: String,
}

impl ExceptionKey {
    /// A validated key.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidArgument`] for an empty or oversized subject reference or reason code.
    pub fn new(
        tenant_id: Uuid,
        subject_kind: SubjectKind,
        subject_ref: impl Into<String>,
        reason_code: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let subject_ref = bounded("subject_ref", subject_ref.into())?;
        let reason_code = bounded("reason_code", reason_code.into())?;
        Ok(Self {
            tenant_id,
            subject_kind,
            subject_ref,
            reason_code,
        })
    }

    /// The tenant that owns the subject.
    #[must_use]
    pub const fn tenant_id(&self) -> Uuid {
        self.tenant_id
    }

    /// The subject's kind.
    #[must_use]
    pub const fn subject_kind(&self) -> SubjectKind {
        self.subject_kind
    }

    /// The subject's id, as text.
    #[must_use]
    pub fn subject_ref(&self) -> &str {
        &self.subject_ref
    }

    /// The failure's reason code (an open set, e.g. `awaiting_attribution`).
    #[must_use]
    pub fn reason_code(&self) -> &str {
        &self.reason_code
    }
}

fn bounded(field: &'static str, value: String) -> Result<String, DomainError> {
    if value.is_empty() || value.len() > MAX_REF_LEN || value.chars().any(char::is_control) {
        return Err(DomainError::InvalidArgument {
            field,
            detail: "1-255 bytes without control characters".to_owned(),
            reason: bss_rating_sdk::errors::reason::INVALID_REQUEST,
        });
    }
    Ok(value)
}

/// A stored exception.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExceptionRecord {
    /// Its id.
    pub exception_id: Uuid,
    /// What it is about.
    pub key: ExceptionKey,
    /// The last failure's description (no other tenant's data, DESIGN §4.7).
    pub detail: String,
    /// The first failure of this open row.
    pub first_seen_at: OffsetDateTime,
    /// The latest failure.
    pub last_attempt_at: OffsetDateTime,
    /// The failures counted on this open row.
    pub attempts: i32,
    /// When it was resolved; `None` while open.
    pub resolved_at: Option<OffsetDateTime>,
    /// How it was resolved.
    pub resolution: Option<String>,
}

/// Records and resolves exceptions inside the caller's transaction.
#[domain_model]
#[derive(Clone)]
pub struct ExceptionRegister {
    repo: Arc<dyn ExceptionRepo>,
    clock: Arc<dyn Clock>,
}

impl ExceptionRegister {
    /// A register over `repo` stamping instants with `clock`.
    #[must_use]
    pub fn new(repo: Arc<dyn ExceptionRepo>, clock: Arc<dyn Clock>) -> Self {
        Self { repo, clock }
    }

    /// Records a failure of `key`'s subject: opens the exception, or counts one more attempt on
    /// the open one.
    ///
    /// # Errors
    ///
    /// A storage failure, or the key's tenant outside `scope`.
    pub async fn record(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        key: &ExceptionKey,
        detail: &str,
    ) -> Result<ExceptionRecord, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-algo-foundation-exceptions:p1:inst-ex-upsert
        self.repo
            .upsert_open(tx, scope, key, detail, self.clock.now())
            .await
        // @cpt-end:cpt-cf-bss-rating-algo-foundation-exceptions:p1:inst-ex-upsert
    }

    /// Resolves the open exception of `key`, if any: `true` when one was resolved.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub async fn resolve(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        key: &ExceptionKey,
        resolution: &str,
    ) -> Result<bool, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-algo-foundation-exceptions:p1:inst-ex-resolve
        self.repo
            .resolve_open(tx, scope, key, resolution, self.clock.now())
            .await
        // @cpt-end:cpt-cf-bss-rating-algo-foundation-exceptions:p1:inst-ex-resolve
    }
}

/// Re-enqueues the subjects of one kind of retryable exception. The pipeline feature that owns
/// the subject implements it; the sweeper calls it for each open exception it handles.
#[async_trait]
pub trait ExceptionRetrier: Send + Sync {
    /// The subject kind it retries.
    fn subject_kind(&self) -> SubjectKind;

    /// Whether `reason_code` is one it retries (for example `awaiting_attribution`).
    fn retries(&self, reason_code: &str) -> bool;

    /// Re-enqueues the exception's subject under `scope` (narrowed to the exception's tenant). It
    /// must be idempotent: the sweeper may call it again for the same open exception.
    ///
    /// # Errors
    ///
    /// Whatever the owning feature refuses with; the sweeper logs it and moves on.
    async fn retry(
        &self,
        db: &Db,
        scope: &AccessScope,
        exception: &ExceptionRecord,
    ) -> Result<(), DomainError>;
}

/// `bss_rating__exception`: one open row per subject and reason.
#[async_trait]
pub trait ExceptionRepo: Send + Sync {
    /// Opens the exception of `key`, or counts one more attempt on the open one.
    async fn upsert_open(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        key: &ExceptionKey,
        detail: &str,
        now: OffsetDateTime,
    ) -> Result<ExceptionRecord, DomainError>;

    /// Resolves the open exception of `key`; `true` when one was open.
    async fn resolve_open(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        key: &ExceptionKey,
        resolution: &str,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError>;

    /// One keyset page of the open exceptions of `kinds` within `scope`: up to `limit` of them in
    /// `exception_id` order, after `after` when given.
    async fn list_open(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        kinds: &[SubjectKind],
        after: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<ExceptionRecord>, DomainError>;

    /// One keyset page of the tenants holding open exceptions of `kinds`: up to `limit` of them in
    /// `tenant_id` order, after `after` when given (the sweeper's tenant discovery: the ownership
    /// column alone, across tenants).
    async fn open_tenants(
        &self,
        tx: &DbTx<'_>,
        kinds: &[SubjectKind],
        after: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<Uuid>, DomainError>;
}

#[cfg(test)]
#[path = "exception_tests.rs"]
mod tests;
