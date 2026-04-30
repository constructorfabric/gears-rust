//! Migration-time trace/instance helper for the canonical `Problem`.
//!
//! Mirrors [`crate::api::trace_layer::WithTraceContext`] /
//! [`crate::api::trace_layer::WithRequestContext`] but operates on
//! [`modkit_canonical_errors::Problem`] instead of the legacy
//! [`crate::api::problem::Problem`].
//!
//! **Scheduled for deletion** once the canonical error middleware
//! (`cpt-cf-errors-component-error-middleware`, see
//! `docs/arch/errors/DESIGN.md` §3.2) lands and starts injecting `trace_id`
//! / `instance` from request context. At that point every call site of
//! [`CanonicalProblemMigrationExt::with_temporary_request_context`]
//! disappears together with this trait.

use modkit_canonical_errors::Problem;

/// Extension trait that fills `trace_id` and `instance` on a canonical
/// [`Problem`] using the temporary span-id fallback documented in
/// `docs/arch/errors/DESIGN.md` §3.7.
///
/// Per-module `From<DomainError> for Problem` impls call this at the end of
/// the conversion so every wire response carries the same shape until the
/// canonical error middleware takes over.
pub trait CanonicalProblemMigrationExt: Sized {
    /// Set `instance` to the supplied path and `trace_id` to a span-id
    /// fallback derived from `tracing::Span::current()`.
    ///
    /// Pass `"/"` when no request URI is plumbed through to the call site
    /// (the common case for `From<DomainError> for Problem`).
    #[must_use]
    fn with_temporary_request_context(self, instance: impl Into<String>) -> Self;
}

impl CanonicalProblemMigrationExt for Problem {
    fn with_temporary_request_context(self, instance: impl Into<String>) -> Self {
        let mut problem = self.with_instance(instance);
        // TODO(cpt-cf-errors-component-error-middleware): replace with
        // header-aware extraction (`crate::api::error_layer::extract_trace_id`)
        // performed in the canonical error middleware.
        if let Some(id) = tracing::Span::current().id() {
            problem = problem.with_trace_id(id.into_u64().to_string());
        }
        problem
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use modkit_canonical_errors::CanonicalError;

    #[test]
    fn sets_instance() {
        let problem: Problem = CanonicalError::internal("boom").create().into();
        let problem = problem.with_temporary_request_context("/api/v1/widgets/42");
        assert_eq!(problem.instance.as_deref(), Some("/api/v1/widgets/42"));
    }

    #[test]
    fn sets_trace_id_when_in_span() {
        // A registered subscriber is required for `Span::current().id()` to
        // return Some — without it the span-id fallback is silently a no-op.
        use tracing_subscriber::fmt;
        let subscriber = fmt().with_test_writer().finish();
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("trace_id_test");
            let _enter = span.enter();
            let problem: Problem = CanonicalError::internal("boom").create().into();
            let problem = problem.with_temporary_request_context("/");
            assert!(
                problem.trace_id.is_some(),
                "expected span-id fallback to populate trace_id"
            );
        });
    }

    #[test]
    fn no_trace_id_outside_any_span() {
        let problem: Problem = CanonicalError::internal("boom").create().into();
        let problem = problem.with_temporary_request_context("/");
        assert!(problem.trace_id.is_none());
    }
}
