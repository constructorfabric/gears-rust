//! Pure schedule-change vocabulary (design §3.6 / §4.6, Group H): the
//! modification **action** (`cancel` / `replace`) and the upstream
//! modification-accounting **treatment** gate. No DB / txn / async I/O — the
//! infra `RecognitionChangeService` applies the durable transition; this module
//! only parses the wire literals and decides whether the treatment lets the
//! change proceed.
//!
//! **Treatment gate (the §3.6 invariant).** A schedule modification is applied to
//! the ledger ONLY when upstream has decided it is `prospective` or a
//! `separate_contract` (both ⇒ apply prospectively / mint a new version). A
//! `catch_up` modification — or any unknown/unmarked treatment — is NEVER silently
//! treated as prospective: it is surfaced as
//! [`DomainError::ModificationTreatmentReview`] with NO state change, because the
//! ledger does not own the catch-up (cumulative true-up) decision (durable
//! exception-queue handling is Slice 7; v1 surfaces the rejection). The gate runs
//! FIRST, before any schedule read or mutation.

use toolkit_macros::domain_model;

use crate::domain::error::DomainError;

/// The change action a [`crate::api::rest`] request names: cancel the schedule
/// outright, or replace it with a new prospective version. Parsed from the wire
/// `action` literal at the service boundary.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeAction {
    /// Mark the ACTIVE schedule `CANCELLED`; the unreleased deferred remainder
    /// stays as `CONTRACT_LIABILITY` (no auto-reversal in v1).
    Cancel,
    /// Mark the ACTIVE schedule `REPLACED` and mint a new ACTIVE version that
    /// re-plans the remaining deferred over the supplied segments (prospective).
    Replace,
}

/// The `cancel` wire literal.
const ACTION_CANCEL: &str = "cancel";
/// The `replace` wire literal.
const ACTION_REPLACE: &str = "replace";

impl ChangeAction {
    /// Parse a wire `action` literal (`"cancel"` | `"replace"`), case-sensitive.
    ///
    /// # Errors
    /// [`DomainError::InvalidRequest`] for any other literal.
    pub fn parse(literal: &str) -> Result<Self, DomainError> {
        match literal {
            ACTION_CANCEL => Ok(Self::Cancel),
            ACTION_REPLACE => Ok(Self::Replace),
            other => Err(DomainError::InvalidRequest(format!(
                "unknown schedule-change action {other:?} (expected \"cancel\" or \"replace\")"
            ))),
        }
    }
}

/// The upstream modification-accounting treatment that lets a change PROCEED
/// (design §3.6). Only these two apply directly; every other value
/// (`catch_up` / unknown) is a review, not a treatment.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProceedTreatment {
    /// Apply the modification prospectively (the remaining deferred is re-planned
    /// forward; already-recognized revenue is not unwound).
    Prospective,
    /// Account the modification as a separate contract (treated, for the ledger's
    /// purposes, the same as prospective: the old schedule terminates / is
    /// superseded and a fresh schedule carries the remaining obligation).
    SeparateContract,
}

/// The `prospective` treatment literal.
const TREATMENT_PROSPECTIVE: &str = "prospective";
/// The `separate_contract` treatment literal.
const TREATMENT_SEPARATE_CONTRACT: &str = "separate_contract";

/// Gate the upstream `treatment` literal (design §3.6 — runs FIRST, before any
/// schedule state is read or mutated). `"prospective"` / `"separate_contract"`
/// ⇒ proceed; `"catch_up"` or any unknown/unmarked value ⇒ surface for review
/// ([`DomainError::ModificationTreatmentReview`]) with NO state change. The ledger
/// never silently treats a modification as prospective.
///
/// # Errors
/// [`DomainError::ModificationTreatmentReview`] when `treatment` is not one of the
/// two proceed treatments (incl. `"catch_up"` and any unknown literal).
pub fn gate_treatment(treatment: &str) -> Result<ProceedTreatment, DomainError> {
    match treatment {
        TREATMENT_PROSPECTIVE => Ok(ProceedTreatment::Prospective),
        TREATMENT_SEPARATE_CONTRACT => Ok(ProceedTreatment::SeparateContract),
        other => Err(DomainError::ModificationTreatmentReview(format!(
            "treatment {other:?} is not auto-prospective (expected \"prospective\" or \
             \"separate_contract\"); a catch-up / unknown modification needs upstream review"
        ))),
    }
}

#[cfg(test)]
#[path = "change_tests.rs"]
mod change_tests;
