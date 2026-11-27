//! Batch evaluation: the `AuthZEN` Access Evaluations API (§7).
//!
//! A batch carries many independent evaluations in one call. Top-level
//! `subject`, `action`, `resource` and `context` are defaults for items that
//! omit the same key; a key present in an item replaces the default as a whole.
//! [`EvaluationBatchRequest::resolve`] expands every item into a complete
//! [`EvaluationRequest`] before any plugin sees it, so result `i` of a batch is
//! the result of `evaluate` on resolved item `i`.
//!
//! Every entry of an [`EvaluationBatchResponse`] is an actual PDP decision. A
//! technical failure fails the whole batch, and a denial carries its own
//! `deny_reason.error_code`. Consumers convert a response with
//! [`EvaluationBatchResponse::into_results`], the fail-closed conversion that
//! checks the shape the semantic allows before any result is trusted.
//!
//! See `docs/arch/authorization/DESIGN.md` (Batch Evaluation) and ADR 0006.

use std::future::Future;
use std::num::NonZeroUsize;

use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_security::PlatformSecurityContext;

use crate::api::AuthZResolverApi;
use crate::error::AuthZResolverError;
use crate::models::{
    Action, EvaluationRequest, EvaluationRequestContext, EvaluationResponse, Resource, Subject,
};

#[resource_error(toolkit_canonical_errors::gts_id!("cf.core.authz_resolver.evaluation_batch.v1~"))]
struct EvaluationBatchProblem;

/// How many items of a batch the PDP evaluates (`AuthZEN` §7.1.2.1).
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EvaluationsSemantic {
    /// Evaluate every item; one result per item (default).
    #[default]
    ExecuteAll,
    /// Evaluate items in order and stop after the first denied item. The
    /// response is the evaluated prefix; later items have no result.
    DenyOnFirstDeny,
    // `permit_on_first_permit` is deferred (see DESIGN.md, Evaluation semantics):
    // a permit can carry constraints that exclude the target.
}

impl EvaluationsSemantic {
    /// Whether evaluation stops after `response` under this semantic.
    #[must_use]
    pub fn stops_after(self, response: &EvaluationResponse) -> bool {
        match self {
            Self::ExecuteAll => false,
            Self::DenyOnFirstDeny => !response.decision,
        }
    }
}

/// Batch-wide options of an [`EvaluationBatchRequest`].
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    utoipa::ToSchema,
)]
#[non_exhaustive]
pub struct EvaluationBatchOptions {
    /// Evaluation semantic; `execute_all` when absent.
    #[serde(default)]
    pub evaluations_semantic: EvaluationsSemantic,
}

impl EvaluationBatchOptions {
    /// Select the evaluation semantic.
    #[must_use]
    pub fn with_semantic(mut self, semantic: EvaluationsSemantic) -> Self {
        self.evaluations_semantic = semantic;
        self
    }

    /// Whether these are the default options (omitted from the wire).
    #[must_use]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde `skip_serializing_if` passes a reference"
    )]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// One evaluation of a batch; each absent key is taken from the request's default.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema, utoipa::ToSchema)]
pub struct EvaluationBatchItem {
    /// The subject; replaces the default subject as a whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Subject>,
    /// The action; replaces the default action as a whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
    /// The resource; replaces the default resource as a whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<Resource>,
    /// The context; replaces the default context as a whole (no field merging).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<EvaluationRequestContext>,
}

/// Batch evaluation request (`AuthZEN` Access Evaluations API).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, utoipa::ToSchema)]
pub struct EvaluationBatchRequest {
    /// Default subject for items that omit `subject`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Subject>,
    /// Default action for items that omit `action`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
    /// Default resource for items that omit `resource`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<Resource>,
    /// Default context for items that omit `context`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<EvaluationRequestContext>,
    /// The evaluations, at least one; an empty list is rejected with `400`.
    /// Decoding stops past [`MAX_BATCH_ITEMS`], so an oversized array never
    /// allocates in full.
    #[schema(min_items = 1, max_items = 1000)]
    #[schemars(length(min = 1, max = 1000))]
    #[serde(deserialize_with = "deserialize_bounded_items")]
    pub evaluations: Vec<EvaluationBatchItem>,
    /// Batch options; omitted from the wire when they equal the default.
    #[serde(default, skip_serializing_if = "EvaluationBatchOptions::is_default")]
    pub options: EvaluationBatchOptions,
}

/// Hard upper bound on the items of one batch, enforced while decoding. A
/// resolver's configured item limit may be lower, never higher.
pub const MAX_BATCH_ITEMS: usize = 1000;

fn deserialize_bounded_items<'de, D>(deserializer: D) -> Result<Vec<EvaluationBatchItem>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Bounded;

    impl<'de> serde::de::Visitor<'de> for Bounded {
        type Value = Vec<EvaluationBatchItem>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "an array of at most {MAX_BATCH_ITEMS} evaluations")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(MAX_BATCH_ITEMS));
            while let Some(item) = seq.next_element()? {
                if items.len() == MAX_BATCH_ITEMS {
                    return Err(serde::de::Error::invalid_length(MAX_BATCH_ITEMS + 1, &self));
                }
                items.push(item);
            }
            Ok(items)
        }
    }

    deserializer.deserialize_seq(Bounded)
}

impl EvaluationBatchRequest {
    /// Applies the default values and checks the request. Returns one complete
    /// [`EvaluationRequest`] per item, in order, and the evaluation semantic.
    ///
    /// `max_items` bounds the number of items, counted before defaults are
    /// applied; `None` applies no bound beyond [`MAX_BATCH_ITEMS`] at decoding.
    ///
    /// # Errors
    ///
    /// [`BatchRequestError`] (`400` once converted into a [`CanonicalError`])
    /// when the batch is empty, holds more than `max_items` items, or an item
    /// lacks `subject`, `action`, `resource` or `context` and no default
    /// supplies it. No item is evaluated then.
    pub fn resolve(
        self,
        max_items: Option<usize>,
    ) -> Result<(Vec<EvaluationRequest>, EvaluationsSemantic), BatchRequestError> {
        let Self {
            subject,
            action,
            resource,
            context,
            evaluations,
            options,
        } = self;

        if evaluations.is_empty() {
            return Err(BatchRequestError::Empty);
        }
        if let Some(max) = max_items
            && evaluations.len() > max
        {
            return Err(BatchRequestError::TooManyItems {
                max,
                got: evaluations.len(),
            });
        }

        let requests = evaluations
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                Ok(EvaluationRequest {
                    subject: pick(item.subject, subject.as_ref(), index, "subject")?,
                    action: pick(item.action, action.as_ref(), index, "action")?,
                    resource: pick(item.resource, resource.as_ref(), index, "resource")?,
                    context: pick(item.context, context.as_ref(), index, "context")?,
                })
            })
            .collect::<Result<Vec<_>, BatchRequestError>>()?;

        Ok((requests, options.evaluations_semantic))
    }
}

fn pick<T: Clone>(
    item: Option<T>,
    default: Option<&T>,
    index: usize,
    key: &'static str,
) -> Result<T, BatchRequestError> {
    match item {
        Some(value) => Ok(value),
        None => default
            .cloned()
            .ok_or(BatchRequestError::MissingKey { index, key }),
    }
}

/// Why a batch request is invalid. Converts into the `InvalidArgument`
/// (`400`) returned to the caller, with one field violation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BatchRequestError {
    /// The batch has no items.
    #[error("a batch must contain at least one evaluation")]
    Empty,
    /// The batch has more items than the item limit.
    #[error("a batch may contain at most {max} evaluations, got {got}")]
    TooManyItems { max: usize, got: usize },
    /// An item lacks a key and the request has no default for it.
    #[error("evaluation {index} has no `{key}` and the request has no default `{key}`")]
    MissingKey { index: usize, key: &'static str },
}

impl From<BatchRequestError> for CanonicalError {
    fn from(e: BatchRequestError) -> Self {
        let (field, description, reason) = match e {
            BatchRequestError::Empty => (
                "evaluations".to_owned(),
                "a batch must contain at least one evaluation".to_owned(),
                "EMPTY_BATCH",
            ),
            BatchRequestError::TooManyItems { max, got } => (
                "evaluations".to_owned(),
                format!("a batch may contain at most {max} evaluations, got {got}"),
                "BATCH_TOO_LARGE",
            ),
            BatchRequestError::MissingKey { index, key } => (
                format!("evaluations[{index}].{key}"),
                format!("evaluation {index} has no `{key}` and the request has no default `{key}`"),
                "MISSING_EVALUATION_FIELD",
            ),
        };
        EvaluationBatchProblem::invalid_argument()
            .with_field_violation(field, description, reason)
            .create()
    }
}

/// Batch evaluation response: one entry per evaluated item, in request order.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, utoipa::ToSchema)]
pub struct EvaluationBatchResponse {
    /// PDP decisions for the evaluated prefix of the request items.
    pub evaluations: Vec<EvaluationResponse>,
}

impl EvaluationBatchResponse {
    /// Fail-closed conversion used by every SDK consumer of batch results.
    ///
    /// Returns the decisions for the evaluated prefix of the `expected` request
    /// items, in request order: result `i` answers item `i`. `execute_all`
    /// returns all `expected`. `deny_on_first_deny` may return fewer, ending
    /// in a denial; items from `len()` on were not evaluated and have no
    /// decision, which is never an allow.
    ///
    /// # Errors
    ///
    /// `Internal` when the results do not have the shape `semantic` allows for
    /// `expected` items, or a denial lacks `deny_reason.error_code`.
    pub fn into_results(
        self,
        expected: usize,
        semantic: EvaluationsSemantic,
    ) -> Result<Vec<EvaluationResponse>, CanonicalError> {
        check_batch_results(&self.evaluations, expected, semantic).map_err(|e| {
            CanonicalError::internal(format!("malformed batch evaluation response: {e}")).create()
        })?;
        Ok(self.evaluations)
    }
}

/// Why a list of batch results does not match its request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BatchResultsError {
    /// `execute_all` must return exactly one result per item.
    #[error("execute_all expects {expected} results, got {actual}")]
    CountMismatch { expected: usize, actual: usize },
    /// `deny_on_first_deny` returned no result or more results than items.
    #[error("deny_on_first_deny expects 1..={items} results, got {results}")]
    ResultCountOutOfRange { items: usize, results: usize },
    /// `deny_on_first_deny` continued after a denial.
    #[error("result {index} is a denial but evaluation continued after it")]
    ContinuedAfterDeny { index: usize },
    /// `deny_on_first_deny` stopped early without a denial.
    #[error("evaluation stopped after result {index}, which is not a denial")]
    StoppedWithoutDeny { index: usize },
    /// A denial without `deny_reason.error_code`.
    #[error("result {index} is a denial without deny_reason.error_code")]
    DenialWithoutErrorCode { index: usize },
}

/// Checks that `results` have the shape `semantic` allows for `expected`
/// request items and that every denial carries `deny_reason.error_code`.
///
/// Shared by the resolver (before it answers) and by
/// [`EvaluationBatchResponse::into_results`] (before a consumer trusts the
/// answer).
///
/// # Errors
///
/// The first violation found.
pub fn check_batch_results(
    results: &[EvaluationResponse],
    expected: usize,
    semantic: EvaluationsSemantic,
) -> Result<(), BatchResultsError> {
    let actual = results.len();
    match semantic {
        EvaluationsSemantic::ExecuteAll => {
            if actual != expected {
                return Err(BatchResultsError::CountMismatch { expected, actual });
            }
        }
        EvaluationsSemantic::DenyOnFirstDeny => {
            if actual > expected || (actual == 0 && expected > 0) {
                return Err(BatchResultsError::ResultCountOutOfRange {
                    items: expected,
                    results: actual,
                });
            }
            let last = actual.saturating_sub(1);
            if let Some(index) = results[..last].iter().position(|r| !r.decision) {
                return Err(BatchResultsError::ContinuedAfterDeny { index });
            }
            if actual < expected && results.get(last).is_some_and(|r| r.decision) {
                return Err(BatchResultsError::StoppedWithoutDeny { index: last });
            }
        }
    }
    if let Some(index) = results.iter().position(|r| {
        !r.decision
            && r.context
                .deny_reason
                .as_ref()
                .is_none_or(|reason| reason.error_code.is_empty())
    }) {
        return Err(BatchResultsError::DenialWithoutErrorCode { index });
    }
    Ok(())
}

/// Sequential batch evaluation over [`AuthZResolverApi::evaluate`].
///
/// Resolves the defaults (with no item limit), evaluates each item in order,
/// stops as `semantic` requires, and fails the whole call on the first error.
/// Meant for test doubles and other adapters that deliberately evaluate a
/// batch one item at a time; it is never applied implicitly.
///
/// # Errors
///
/// The resolution error, or the first evaluation error.
pub async fn evaluate_batch_sequentially<A>(
    api: &A,
    ctx: PlatformSecurityContext,
    req: EvaluationBatchRequest,
) -> Result<EvaluationBatchResponse, CanonicalError>
where
    A: AuthZResolverApi + ?Sized,
{
    let (requests, semantic) = req.resolve(None)?;
    let mut evaluations = Vec::with_capacity(requests.len());
    for request in requests {
        let response = api.evaluate(ctx.clone(), request).await?;
        let stop = semantic.stops_after(&response);
        evaluations.push(response);
        if stop {
            break;
        }
    }
    Ok(EvaluationBatchResponse { evaluations })
}

/// Bounded, order-preserving fan-out of a plugin batch over its per-item
/// `evaluate`.
///
/// Runs at most `max_concurrency` evaluations at once and yields results in
/// request order. Each evaluation waits for a permit of `in_flight`, the
/// process-wide semaphore shared by all batches, and holds it only until it
/// finishes, so a freed permit goes to the next waiting item of any batch.
/// For a short-circuit semantic it stops after the stopping item; dropping the
/// stream cancels later evaluations still in flight or waiting for a permit.
/// An error for any item fails the whole batch. Each result equals an
/// independent `evaluate` of the same request, so batch and single evaluation
/// agree by construction. Local plugins call it explicitly from
/// [`AuthZResolverPluginClient::evaluate_batch`](crate::AuthZResolverPluginClient::evaluate_batch).
///
/// # Errors
///
/// The first item error, in request order. `Internal` if `in_flight` is
/// closed.
pub async fn evaluate_batch_concurrently<F, Fut>(
    requests: Vec<EvaluationRequest>,
    semantic: EvaluationsSemantic,
    max_concurrency: NonZeroUsize,
    in_flight: &Semaphore,
    mut evaluate: F,
) -> Result<Vec<EvaluationResponse>, AuthZResolverError>
where
    F: FnMut(EvaluationRequest) -> Fut,
    Fut: Future<Output = Result<EvaluationResponse, AuthZResolverError>>,
{
    let mut pending = stream::iter(requests)
        .map(|request| {
            // Futures are lazy: nothing of `evaluate` runs before the permit.
            let evaluation = evaluate(request);
            async move {
                let _permit = in_flight.acquire().await.map_err(|_| {
                    AuthZResolverError::Internal("batch in-flight semaphore closed".to_owned())
                })?;
                evaluation.await
            }
        })
        .buffered(max_concurrency.get());
    let mut evaluations = Vec::new();
    while let Some(result) = pending.next().await {
        let response = result.map_err(|e| with_item_index(e, evaluations.len()))?;
        let stop = semantic.stops_after(&response);
        evaluations.push(response);
        if stop {
            break;
        }
    }
    Ok(evaluations)
}

/// Names the failing item in a batch error, keeping its variant (and so its
/// `503`/`500` classification). The resolver logs it once, at its boundary.
fn with_item_index(error: AuthZResolverError, index: usize) -> AuthZResolverError {
    match error {
        AuthZResolverError::ServiceUnavailable(msg) => {
            AuthZResolverError::ServiceUnavailable(format!("batch item {index}: {msg}"))
        }
        AuthZResolverError::Internal(msg) => {
            AuthZResolverError::Internal(format!("batch item {index}: {msg}"))
        }
        AuthZResolverError::NoPluginAvailable => AuthZResolverError::NoPluginAvailable,
    }
}

mod _api_dto_markers {
    use super::{EvaluationBatchRequest, EvaluationBatchResponse};
    use toolkit::api::api_dto::{RequestApiDto, ResponseApiDto};

    impl RequestApiDto for EvaluationBatchRequest {}
    impl ResponseApiDto for EvaluationBatchResponse {}
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "batch_tests.rs"]
mod batch_tests;
