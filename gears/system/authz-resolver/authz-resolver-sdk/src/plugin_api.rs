//! Plugin API trait for `AuthZ` resolver implementations.

use std::num::NonZeroUsize;

use async_trait::async_trait;
use tokio::sync::Semaphore;

use crate::batch::EvaluationsSemantic;
use crate::error::AuthZResolverError;
use crate::models::{EvaluationRequest, EvaluationResponse};

/// Plugin API trait for `AuthZ` resolver implementations.
///
/// Each plugin registers this trait with a scoped `ClientHub` entry
/// using its GTS instance ID as the scope.
#[async_trait]
pub trait AuthZResolverPluginClient: Send + Sync {
    /// Evaluate an authorization request.
    ///
    /// # Errors
    ///
    /// - `NoPluginAvailable` if no plugin can handle the request
    /// - `ServiceUnavailable` if the plugin is not available yet
    /// - `Internal` for unexpected errors
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError>;

    /// Evaluate a batch of complete requests (defaults already resolved).
    ///
    /// Returns the decisions for the evaluated prefix of `requests`, in request
    /// order: all of them for [`EvaluationsSemantic::ExecuteAll`]; up to and
    /// including the first denial for [`EvaluationsSemantic::DenyOnFirstDeny`].
    /// Every result must equal what [`evaluate`](Self::evaluate) returns for
    /// the same request, and every denial must carry `deny_reason.error_code`.
    ///
    /// Cancellation: the resolver drops this future when the batch deadline
    /// expires or the caller disconnects, and the fan-out helper drops
    /// in-flight per-item `evaluate` futures after a short-circuit or an item
    /// error. Either may happen at any `.await`, so implementations must be
    /// read-only or otherwise safe to cancel (work shared across items
    /// included).
    ///
    /// Required, with no default body: a plugin backed by a remote PDP must not
    /// silently turn one batch into one outbound request per item. Local
    /// plugins call [`evaluate_batch_concurrently`](crate::batch::evaluate_batch_concurrently)
    /// explicitly.
    ///
    /// Load bounds: at most `max_concurrency` items of this batch may be in
    /// flight at once, and each item in flight holds one permit of `in_flight`,
    /// the resolver's process-wide semaphore shared by all batches, until it
    /// finishes. The fan-out helper does both. A plugin that evaluates items
    /// another way (for example one request to its PDP's batch API) holds a
    /// permit per item for the work it has in flight.
    ///
    /// # Errors
    ///
    /// An error for any item is an error for the whole batch (same variants as
    /// [`evaluate`](Self::evaluate)).
    async fn evaluate_batch(
        &self,
        requests: Vec<EvaluationRequest>,
        semantic: EvaluationsSemantic,
        max_concurrency: NonZeroUsize,
        in_flight: &Semaphore,
    ) -> Result<Vec<EvaluationResponse>, AuthZResolverError>;
}
