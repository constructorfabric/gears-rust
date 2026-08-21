//! Admission failures shared by unit evaluation and worker orchestration.

use std::borrow::Cow;

use serde_json::json;
use toolkit_db::DbError;
use toolkit_db::secure::ScopeError;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::gts_store::StoreBuildError;

/// An infrastructure failure. Retryable by construction: nothing here is a
/// statement about the candidate.
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("operation {operation_id} does not exist")]
    OperationNotFound { operation_id: Uuid },
    #[error("operation item {item_id} carries no request payload")]
    MissingPayload { item_id: i64 },
    /// Not a fault, and never reaches a caller: the worker catches it and reports
    /// the outcome the other pass recorded. It exists as an error because rolling
    /// the commit transaction back is the only way to *not* write an entity behind
    /// an item that is already terminal.
    #[error("operation item {item_id} was terminalized by another pass")]
    ItemAlreadyTerminal { item_id: i64 },
    #[error("building the transient store failed: {0}")]
    StoreBuild(#[source] StoreBuildError),
    #[error("the blocking evaluation task failed: {0}")]
    EvaluationTask(#[source] tokio::task::JoinError),
    /// An Instance's conforming Type Schema has no committed current revision.
    ///
    /// **Retryable, not terminal**: the value is not wrong, its type has not landed
    /// yet. A terminal failure would make the outcome depend on the order two
    /// unrelated submissions reached the worker; a redelivery re-reads and succeeds.
    /// Until T21 there is no outbox, so it surfaces inline as a `500` like every
    /// other `WorkerError`.
    #[error("instance '{gts_id}' conforms to '{type_id}', which has no current revision")]
    ConformingTypeAbsent { gts_id: String, type_id: String },
    #[error("storage failure during admission: {0}")]
    Storage(#[from] ScopeError),
    #[error("database failure during admission: {0}")]
    Db(#[from] DbError),
}

/// A candidate-level failure: final, recorded, and never retried.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemFailure {
    /// A stable machine reason, so T16 can count failures by kind and a client can
    /// branch on them without parsing prose.
    ///
    /// `Cow`, not `&'static str`, for one case: a failure read back out of a stored
    /// `error_payload` carries a reason that was a literal in some *earlier* process.
    /// Owned-or-borrowed keeps [`Self::from_payload`] able to return the real reason
    /// instead of a placeholder; every constructor at a failure site still passes a
    /// `&'static str`.
    pub reason: Cow<'static, str>,
    pub message: String,
}

impl ItemFailure {
    #[must_use]
    pub fn new(reason: &'static str, message: String) -> Self {
        Self {
            reason: Cow::Borrowed(reason),
            message,
        }
    }

    /// The stored `error_payload`: structured, so the reason survives the round
    /// trip as a field rather than as a substring.
    #[must_use]
    pub fn to_payload(&self) -> String {
        json!({ "reason": self.reason, "message": self.message }).to_string()
    }

    /// The inverse of [`Self::to_payload`], for an outcome read back off the row.
    ///
    /// Without it a redelivery and a first pass report *different shapes of the same
    /// fact* — `{reason, message}` versus `reason: "recorded"` with the JSON stuffed
    /// into `message`. Invisible on the wire today, since REST reads `error_payload`
    /// from the row, but T16 counts refusals by `reason` and a metric reading
    /// `recorded` for every redelivered item counts nothing.
    ///
    /// A payload that does not parse is kept verbatim under a reason that says so,
    /// rather than being dropped or panicked on: a corrupt row should be visible.
    #[must_use]
    pub fn from_payload(payload: &str) -> Self {
        match serde_json::from_str::<serde_json::Value>(payload) {
            Ok(value) => {
                let reason = value.get("reason").and_then(serde_json::Value::as_str);
                let message = value.get("message").and_then(serde_json::Value::as_str);
                match (reason, message) {
                    (Some(reason), Some(message)) => Self {
                        reason: Cow::Owned(reason.to_owned()),
                        message: message.to_owned(),
                    },
                    _ => Self::new("unrecognized_payload", payload.to_owned()),
                }
            }
            Err(_) => Self::new("unparsable_payload", payload.to_owned()),
        }
    }
}
