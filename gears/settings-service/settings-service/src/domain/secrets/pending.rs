// Created: 2026-09-15 by Virtuozzo International GmbH
// @cpt-dod:cpt-cf-settings-service-dod-secret-values-stage:p1
//! A secret staged ahead of the step-up redirect.
//!
//! The console applies its changes in one batch after step-up, which is a full
//! redirect to the identity provider, and a secret's plaintext cannot sit in
//! the browser across that redirect. So the gear takes the plaintext first —
//! into the Credential Store, exactly as a set would — and hands back a
//! `pending_id` it minted and owns. The token is what survives the redirect:
//! the batch names it in place of the value, and the write path adopts the
//! entry already stored rather than storing a second time.
//!
//! A pending row is single-use, bound to the setting, the tenant and the
//! subject that staged it, and short-lived: an unclaimed one is swept together
//! with its entry once `expires_at` has passed.

use serde_json::Value;
use time::{Duration, OffsetDateTime};
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::field;

/// How long a staged secret waits to be claimed: long enough for the redirect,
/// the re-authentication and the batch that follows, short enough that an
/// abandoned entry does not linger. Fixed by the design, not by the deployment.
pub const PENDING_SECRET_TTL: Duration = Duration::minutes(10);

/// The one member a batch change carries in place of a secret's value.
pub const PENDING_ID_FIELD: &str = "pending_id";

/// A staged secret as the table holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSecret {
    /// The token the caller holds.
    pub id: Uuid,
    /// The declaration the value is for.
    pub declaration_id: Uuid,
    /// The scope the value is for, as a tenant id.
    pub tenant_id: Uuid,
    /// The subject that staged it, the only one that may claim it.
    pub subject_id: String,
    /// The store reference the batch adopts.
    pub secret_ref: String,
    /// When it was staged.
    pub created_at: OffsetDateTime,
    /// When it stops being claimable and becomes the sweep's.
    pub expires_at: OffsetDateTime,
}

/// What a stage records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSecretDraft {
    /// The declaration the value is for.
    pub declaration_id: Uuid,
    /// The scope the value is for.
    pub tenant_id: Uuid,
    /// Who staged it.
    pub subject_id: String,
    /// The reference of the entry the stage created.
    pub secret_ref: String,
    /// When the row expires.
    pub expires_at: OffsetDateTime,
}

/// The repository port over `pending_secrets`.
#[async_trait::async_trait]
pub trait PendingSecretRepository: Send + Sync {
    /// Record a stage, minting its id.
    ///
    /// # Errors
    /// [`DomainError::Internal`] when the insert fails.
    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        draft: PendingSecretDraft,
    ) -> Result<PendingSecret, DomainError>;

    /// One row by its id.
    ///
    /// # Errors
    /// [`DomainError::Internal`] when the read fails.
    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<PendingSecret>, DomainError>;

    /// Remove one row; `false` when it was already gone.
    ///
    /// # Errors
    /// [`DomainError::Internal`] when the delete fails.
    async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;

    /// Consume one row as a claim: gone only if it is still unexpired at
    /// `now`, in the one statement — the expiry is re-asserted at the moment
    /// of use, not only at the check before it. `false` when the row is
    /// absent or past its expiry, and nothing was removed.
    ///
    /// # Errors
    /// [`DomainError::Internal`] when the delete fails.
    async fn claim<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError>;

    /// Up to `limit` rows whose `expires_at` lies behind `now`, oldest first.
    ///
    /// # Errors
    /// [`DomainError::Internal`] when the read fails.
    async fn list_expired<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        now: OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<PendingSecret>, DomainError>;
}

/// The `pending_id` a batch change names in place of a value: an object with
/// that one member and a UUID in it. Any other shape is a value.
#[must_use]
pub fn pending_id_of(value: &Value) -> Option<Uuid> {
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }
    object
        .get(PENDING_ID_FIELD)?
        .as_str()
        .and_then(|raw| Uuid::parse_str(raw).ok())
}

/// What a batch change must match to claim a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claim<'a> {
    /// The declaration the change writes.
    pub declaration_id: Uuid,
    /// The scope the change writes.
    pub tenant_id: Uuid,
    /// The batch's actor.
    pub subject_id: &'a str,
    /// The instant of the claim.
    pub now: OffsetDateTime,
}

/// The one refusal every mismatch gets. Which condition failed is not told: a
/// caller holding someone else's token learns nothing from the answer.
#[must_use]
pub fn invalid_pending() -> DomainError {
    DomainError::Validation {
        field: format!("value.{PENDING_ID_FIELD}"),
        code: field::PENDING_SECRET_INVALID,
        message: "the pending secret is unknown, expired, or was staged for another setting, \
                  tenant or subject"
            .to_owned(),
    }
}

/// Whether `claim` may take `row`: same setting, same tenant, same subject,
/// and not yet expired.
///
/// # Errors
/// [`invalid_pending`] on any mismatch.
pub fn check_claim(row: &PendingSecret, claim: &Claim<'_>) -> Result<(), DomainError> {
    // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-7
    let same_pair = row.declaration_id == claim.declaration_id
        && row.tenant_id == claim.tenant_id
        && row.subject_id == claim.subject_id;
    let unexpired = row.expires_at > claim.now;
    if same_pair && unexpired {
        Ok(())
    } else {
        Err(invalid_pending())
    }
    // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-7
}

#[cfg(test)]
#[path = "pending_tests.rs"]
mod pending_tests;
