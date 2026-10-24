//! The operator audit helper (DESIGN §3.7 `bss_rating__operator_audit`, §4.8): every action marked
//! "audited" writes one insert-only row — actor, subject and request id — in the same transaction
//! as its effect. It is Rating's audit sink until a platform audit service exists.

use std::sync::Arc;

use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_db::secure::DbTx;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::digest::Sha256Hex;
use crate::domain::error::DomainError;
use crate::domain::ports::Clock;

/// The most characters an operator's stated reason may have.
pub const MAX_REASON_LEN: usize = 1000;

/// An operator's stated reason for an audited action: 1-1000 characters once trimmed, with no
/// control character, so every audit row holds a reason a reader can trust. Built only through
/// [`OperatorReason::parse`], whatever entry point the action came through.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorReason(String);

impl OperatorReason {
    /// The trimmed reason.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidArgument`] on `reason` for a blank or overlong reason, or one holding
    /// a control character (a newline, a NUL, an escape).
    pub fn parse(reason: &str) -> Result<Self, DomainError> {
        let trimmed = reason.trim();
        if trimmed.is_empty()
            || trimmed.chars().count() > MAX_REASON_LEN
            || trimmed.chars().any(char::is_control)
        {
            return Err(DomainError::InvalidArgument {
                field: "reason",
                detail: "a reason is 1-1000 characters without control characters".to_owned(),
                reason: bss_rating_sdk::errors::reason::INVALID_REQUEST,
            });
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// The reason text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One audited action.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// The tenant the action's target belongs to.
    pub tenant_id: Uuid,
    /// The subject that performed it.
    pub actor: Uuid,
    /// The operation id (`bss_rating.inbox.release`, …).
    pub operation_id: &'static str,
    /// The kind of the target (`inbox_entry`, `exception`, …).
    pub target_kind: &'static str,
    /// The target's id.
    pub target_id: String,
    /// The digest of the request.
    pub request_digest: Sha256Hex,
    /// The operator's stated reason.
    pub reason: OperatorReason,
    /// The second operator of a two-person action.
    pub approved_by: Option<Uuid>,
    /// The caller's request id, when the request carried one.
    pub request_id: Option<String>,
}

/// Writes audit rows inside the audited action's transaction.
#[domain_model]
#[derive(Clone)]
pub struct OperatorAudit {
    repo: Arc<dyn AuditRepo>,
    clock: Arc<dyn Clock>,
}

impl OperatorAudit {
    /// An audit helper writing through `repo` and stamping instants with `clock`.
    #[must_use]
    pub fn new(repo: Arc<dyn AuditRepo>, clock: Arc<dyn Clock>) -> Self {
        Self { repo, clock }
    }

    /// Records `entry`; returns the audit id.
    ///
    /// # Errors
    ///
    /// A storage failure, or the entry's tenant outside `scope`. Either rolls the action back:
    /// an audited action never commits without its audit row.
    pub async fn record(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entry: &AuditEntry,
    ) -> Result<Uuid, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-algo-foundation-authorize:p1:inst-az-audit
        let audit_id = Uuid::now_v7();
        self.repo
            .insert(tx, scope, audit_id, entry, self.clock.now())
            .await?;
        Ok(audit_id)
        // @cpt-end:cpt-cf-bss-rating-algo-foundation-authorize:p1:inst-az-audit
    }
}

/// `bss_rating__operator_audit`: insert-only.
#[async_trait]
pub trait AuditRepo: Send + Sync {
    /// Inserts one audit row.
    async fn insert(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        audit_id: Uuid,
        entry: &AuditEntry,
        at: OffsetDateTime,
    ) -> Result<(), DomainError>;
}
