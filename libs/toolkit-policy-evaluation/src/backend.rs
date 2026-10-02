//! Backend-neutral evaluation contract: input ([`EvaluationContext`]), bound
//! ([`CostBound`]), compiled artefact ([`CompiledDocument`]) and backend
//! ([`EvaluationBackend`]).

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

/// Key under which the facility injects the caller-supplied evaluation
/// timestamp, as an RFC 3339 string in UTC, into the evaluation input.
pub const EVALUATED_AT_KEY: &str = "evaluated_at";

/// Key under which the facility injects the caller-supplied evaluation
/// timestamp, as signed nanoseconds since the Unix epoch, into the
/// evaluation input.
pub const EVALUATED_AT_NS_KEY: &str = "evaluated_at_ns";

/// Wall-clock bound for evaluating one document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CostBound {
    /// Wall-clock time the evaluation may take before it is abandoned.
    pub limit: Duration,
}

impl CostBound {
    /// Creates a bound of `limit`.
    #[must_use]
    pub const fn new(limit: Duration) -> Self {
        Self { limit }
    }
}

/// Why an [`EvaluationContext`] could not be constructed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContextError {
    /// The request data is not a JSON object, so the timestamp cannot be
    /// injected next to it.
    #[error("evaluation input must be a JSON object")]
    NotAnObject,
    /// The request data already carries a key the facility reserves for the
    /// injected timestamp; accepting it would let request data impersonate
    /// the caller-supplied clock.
    #[error("evaluation input must not carry the reserved key `{key}`")]
    ReservedKey {
        /// The reserved key found in the request data.
        key: &'static str,
    },
    /// The timestamp cannot be represented as RFC 3339 or as `i64`
    /// nanoseconds since the Unix epoch.
    #[error("evaluation timestamp is outside the representable range")]
    TimestampOutOfRange,
}

/// The complete, immutable input of one evaluation: caller-built request data
/// plus a caller-supplied timestamp, injected under [`EVALUATED_AT_KEY`]
/// (RFC 3339, UTC) and [`EVALUATED_AT_NS_KEY`] (`i64` Unix nanoseconds).
/// Nothing else is reachable from an evaluation.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluationContext {
    document: serde_json::Value,
    evaluated_at: OffsetDateTime,
}

impl EvaluationContext {
    /// Builds the evaluation input from caller-constructed request data and a
    /// caller-supplied timestamp.
    ///
    /// The timestamp is normalised to UTC so that the injected values do not
    /// depend on the offset the caller happened to use.
    ///
    /// # Errors
    ///
    /// - [`ContextError::NotAnObject`] if `input` is not a JSON object;
    /// - [`ContextError::ReservedKey`] if `input` already carries
    ///   [`EVALUATED_AT_KEY`] or [`EVALUATED_AT_NS_KEY`];
    /// - [`ContextError::TimestampOutOfRange`] if `evaluated_at` cannot be
    ///   represented in either injected form.
    pub fn new(
        input: serde_json::Value,
        evaluated_at: OffsetDateTime,
    ) -> Result<Self, ContextError> {
        let serde_json::Value::Object(mut map) = input else {
            return Err(ContextError::NotAnObject);
        };
        for key in [EVALUATED_AT_KEY, EVALUATED_AT_NS_KEY] {
            if map.contains_key(key) {
                return Err(ContextError::ReservedKey { key });
            }
        }

        let evaluated_at = evaluated_at.to_offset(UtcOffset::UTC);
        let rfc3339 = evaluated_at
            .format(&Rfc3339)
            .map_err(|_| ContextError::TimestampOutOfRange)?;
        let nanos = i64::try_from(evaluated_at.unix_timestamp_nanos())
            .map_err(|_| ContextError::TimestampOutOfRange)?;

        map.insert(
            EVALUATED_AT_KEY.to_owned(),
            serde_json::Value::String(rfc3339),
        );
        map.insert(
            EVALUATED_AT_NS_KEY.to_owned(),
            serde_json::Value::from(nanos),
        );

        Ok(Self {
            document: serde_json::Value::Object(map),
            evaluated_at,
        })
    }

    /// The exact input document a backend evaluates against, including the
    /// injected timestamp keys.
    #[must_use]
    pub const fn document(&self) -> &serde_json::Value {
        &self.document
    }

    /// The caller-supplied evaluation timestamp, normalised to UTC.
    #[must_use]
    pub const fn evaluated_at(&self) -> OffsetDateTime {
        self.evaluated_at
    }
}

/// Why an evaluation produced no value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvaluationError {
    /// The document ran past the caller's [`CostBound`] and was abandoned.
    /// No partial result is returned.
    #[error("evaluation exceeded its cost bound of {limit:?}")]
    BoundExceeded {
        /// The limit that was exceeded.
        limit: Duration,
    },
    /// The backend failed for any other reason (runtime error in the
    /// document, a strict builtin error, an input the backend cannot
    /// represent, or a result it cannot convert).
    #[error("evaluation failed: {message}")]
    Failed {
        /// Backend-reported detail, for records and diagnostics.
        message: String,
    },
}

/// Why a document could not be validated or compiled.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompileError {
    /// The document is not well-formed as the backend judges it. Carries the
    /// backend's position when it reports one.
    #[error("syntax error: {message}")]
    Syntax {
        /// Backend-reported detail.
        message: String,
        /// 1-based line of the error, when known.
        line: Option<u32>,
        /// 1-based column of the error, when known.
        column: Option<u32>,
    },
    /// The document is well-formed but cannot be used as requested, for
    /// example because it does not define the required entrypoint rule.
    #[error("unsupported document: {message}")]
    Unsupported {
        /// What is missing or not supported.
        message: String,
    },
}

/// A document compiled by an [`EvaluationBackend`], ready for repeated
/// evaluation.
///
/// Immutable after compilation; may be evaluated concurrently.
pub trait CompiledDocument: Send + Sync + fmt::Debug {
    /// Builtins the document references, computed over the parsed form
    /// (not by searching the source text). This is the input of the
    /// determinism denylist screen ([`crate::screen_denylist`]).
    fn referenced_builtins(&self) -> &BTreeSet<String>;

    /// Static references into the evaluation input, as dotted paths relative
    /// to the input root (`input.properties.size` is reported as
    /// `"properties.size"`). Best-effort: dynamic indexing ends a path at its
    /// last static segment, and a bare reference to the whole input is not
    /// reported.
    fn referenced_input_paths(&self) -> &BTreeSet<String>;

    /// Evaluates the document's entrypoint against `ctx` under `bound`.
    ///
    /// Returns the entrypoint's value converted to JSON; an entrypoint that
    /// is undefined for this input yields [`serde_json::Value::Null`].
    ///
    /// # Errors
    ///
    /// - [`EvaluationError::BoundExceeded`] if `bound` expired, including
    ///   when the evaluation completes only after `bound.limit` elapsed — a
    ///   value is never returned past the limit;
    /// - [`EvaluationError::Failed`] for any other backend failure, including
    ///   a result the backend cannot represent faithfully as JSON and a panic
    ///   inside the backend.
    fn evaluate(
        &self,
        ctx: &EvaluationContext,
        bound: CostBound,
    ) -> Result<serde_json::Value, EvaluationError>;
}

/// A policy-language backend: syntax validation separate from evaluation,
/// and compilation into an evaluable document.
pub trait EvaluationBackend: Send + Sync {
    /// Checks that `source` is well-formed. Parses only; never evaluates
    /// anything, so its cost does not depend on what the document computes.
    ///
    /// Synchronous CPU work: async callers should use `spawn_blocking`.
    ///
    /// # Errors
    ///
    /// [`CompileError::Syntax`] with the backend's position when available.
    fn validate_syntax(&self, source: &str) -> Result<(), CompileError>;

    /// Compiles `source` (identified as `document_name` in diagnostics) into
    /// an evaluable document whose value is the rule named `entrypoint`.
    /// Compilation evaluates nothing.
    ///
    /// Synchronous CPU work, like [`Self::validate_syntax`].
    ///
    /// # Errors
    ///
    /// - [`CompileError::Syntax`] if the document is malformed;
    /// - [`CompileError::Unsupported`] if it does not define `entrypoint`.
    fn compile(
        &self,
        document_name: &str,
        source: &str,
        entrypoint: &str,
    ) -> Result<Arc<dyn CompiledDocument>, CompileError>;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{ContextError, EVALUATED_AT_KEY, EVALUATED_AT_NS_KEY, EvaluationContext};
    use serde_json::json;
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;

    fn at(rfc3339: &str) -> OffsetDateTime {
        OffsetDateTime::parse(rfc3339, &Rfc3339).unwrap()
    }

    #[test]
    fn context_injects_timestamp_in_utc() {
        let ctx = EvaluationContext::new(
            json!({"action": "create"}),
            at("2026-09-23T14:00:00.5+02:00"),
        )
        .unwrap();
        assert_eq!(
            ctx.document(),
            &json!({
                "action": "create",
                "evaluated_at": "2026-09-23T12:00:00.5Z",
                "evaluated_at_ns": 1_790_164_800_500_000_000_i64,
            })
        );
        assert_eq!(ctx.evaluated_at(), at("2026-09-23T12:00:00.5Z"));
    }

    #[test]
    fn context_rejects_non_object_input() {
        for input in [json!(null), json!([1]), json!("x"), json!(1)] {
            assert_eq!(
                EvaluationContext::new(input, at("2026-01-01T00:00:00Z")),
                Err(ContextError::NotAnObject)
            );
        }
    }

    #[test]
    fn context_rejects_reserved_keys() {
        for key in [EVALUATED_AT_KEY, EVALUATED_AT_NS_KEY] {
            let mut map = serde_json::Map::new();
            map.insert(key.to_owned(), json!(0));
            assert_eq!(
                EvaluationContext::new(serde_json::Value::Object(map), at("2026-01-01T00:00:00Z")),
                Err(ContextError::ReservedKey { key })
            );
        }
    }

    #[test]
    fn context_rejects_unrepresentable_timestamp() {
        assert_eq!(
            EvaluationContext::new(json!({}), at("2300-01-01T00:00:00Z")),
            Err(ContextError::TimestampOutOfRange)
        );
    }
}
