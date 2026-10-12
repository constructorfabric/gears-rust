//! D-431: the one correlation id a request carries, established at the gear's
//! authoring edge.
//!
//! Every audit row a request writes carries a correlation id (D-433). D-431
//! names who produces it:
//!
//! 1. It is the request-scoped correlation this edge establishes: **minted here
//!    for every request**, so the field is always satisfiable and never NULL on a
//!    request path.
//! 2. **Every** audit row a single operator call writes carries **one** value.
//!    That is the clause with teeth: the audit rows one call writes share
//!    nothing else that says they were one act.
//! 3. It is **not** the `Idempotency-Key`. That one is client-minted, per
//!    operation, and the subject of a *different* comparison (D-396): a retried
//!    call carries the same idempotency key on purpose, and conflating the two
//!    would make the retry correlate to the original and an unretried sibling
//!    correlate to nothing. It is not derived from the payload either.
//!
//! # It is **minted unconditionally**, and the platform convention it declines
//! to consume is named here rather than denied
//!
//! **What the platform has.**
//! `toolkit::api::extract_trace_id` reads the live `OTel` span context, falling
//! back to the W3C `traceparent`: either way a real 32-hex trace-id, or absent
//! when neither is in scope. This gear already mounts that middleware around the
//! whole merged router (`crate::module`). So there *is* an inbound convention, it
//! is a standard rather than a local invention, and this gear is already inside
//! it. A W3C trace-id is 32 hex characters — 128 bits — and `Uuid::parse_str`
//! accepts the simple form, so the value would fit.
//!
//! **Why this edge still mints.** Three reasons, none of which is "there is
//! nothing to read":
//!
//! 1. **A trace-id is not an operator call.** Clause (2) binds the value to *one
//!    operator call*; a W3C trace-id identifies a whole distributed trace, which a
//!    batch caller may hold across hundreds of calls. Consuming it makes "these
//!    records were one act" a property of the **caller's** instrumentation rather
//!    than of this gear.
//! 2. **Partial adoption is a join that is right sometimes.** The platform value
//!    exists only when the caller sent a `traceparent` or a span is in scope;
//!    otherwise it is absent and this edge must mint regardless. Consuming it
//!    would give some calls a trace-derived `correlation_id` and others a minted
//!    one, with nothing on the record saying which.
//! 3. **A trace-id is 128 bits but not a UUID.** [`establish`] mints v7
//!    deliberately (see its own note), and the column is read as time-ordered.
//!    A parsed trace-id lands with an arbitrary version and variant, so consuming
//!    it would put two kinds of value in one column with nothing distinguishing
//!    them.
//!
//! What would let the edge consume it cleanly is `toolkit` exposing its
//! already-extracted trace id as a request extension, so one place decides the
//! span/`traceparent` precedence. [`establish`] is the single site that changes.
//!
//! # Why a layer, and why extraction **fails** rather than mints
//!
//! A handler that minted its own would satisfy "not NULL" and break clause (2)
//! the moment a call writes two records. So the value is established **once**,
//! before any handler runs; a handler takes it with [`require_correlation`] and
//! passes the raw `Uuid` to every writer (`support::audit(…, correlation, …)`).
//! A rereserve op mints its own when it is created — the ticker or a Tx C starts
//! it, even a Tx C a door drives inside a request — and the rows it writes carry
//! that id (D-431).
//!
//! [`require_correlation`] therefore refuses to mint. A route reachable without
//! this layer is a **wiring defect** — the authoring router applies
//! `axum::middleware::from_fn(correlation::establish)` in its own `router()`, so
//! the edge travels with the routes rather than with whoever remembered to
//! compose them, and the crate's own route suites drive the real routers — and a
//! mint there would hide the defect behind a per-record value, which is the
//! failure the layer exists to prevent. It answers 500, loudly.
//!
//! The read-only `read_contract` router does not apply it: nothing behind it
//! writes an audit record, so there is no field for a correlation to satisfy. If
//! it ever grows a writer, that writer has no correlation to pass until the
//! router mounts the layer, and [`require_correlation`] is what tells it so.

use axum::extract::{Extension, Request};
use axum::middleware::Next;
use axum::response::Response;
use toolkit::api::canonical_prelude::CanonicalError;
use uuid::Uuid;

use crate::infra::error_mapping::DomainError;

/// The request-scoped correlation, as the request extensions carry it.
///
/// A newtype rather than a bare `Uuid` because the extensions are keyed by type:
/// a bare one would collide with any other `Uuid` a layer inserts, and the
/// collision would be silent and would swap two unrelated identities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CorrelationId(Uuid);

impl CorrelationId {
    /// The value, for the writers that store it.
    #[must_use]
    pub const fn get(self) -> Uuid {
        self.0
    }
}

/// Establish the request's correlation id before any handler runs.
///
/// Idempotent with respect to itself: a request that already carries one keeps
/// it, so composing this layer twice cannot give one request two identities.
///
/// This is the single site that grows the propagation half. The platform **does**
/// have an inbound convention this could read — W3C `traceparent`, already
/// extracted by `toolkit`'s canonical-error middleware — and the module doc gives
/// the three reasons this edge declines it and mints instead. It is a decision,
/// not an absence.
pub async fn establish(mut request: Request, next: Next) -> Response {
    if request.extensions().get::<CorrelationId>().is_none() {
        // v7 rather than v4: the id is stored beside a `written_at` on an
        // append-only store, and a time-ordered value keeps an index on it
        // useful to a later audit read.
        request
            .extensions_mut()
            .insert(CorrelationId(Uuid::now_v7()));
    }
    next.run(request).await
}

/// The correlation this request was given, or an internal fault.
///
/// **It does not mint**, and the module doc argues why: an absent extension means
/// the route was mounted without [`establish`], which is a wiring defect in this
/// crate rather than anything a caller did, and a mint here would answer 200
/// while quietly reintroducing the per-record value D-431 forbids.
///
/// # Errors
/// [`CanonicalError`] (500) when no correlation was established.
pub fn require_correlation(
    extension: Option<Extension<CorrelationId>>,
) -> Result<Uuid, CanonicalError> {
    extension.map_or_else(
        || {
            Err(CanonicalError::from(DomainError::Internal(
                "bss-pricing: this route was reached without the correlation layer, so the \
                 records it writes would carry no correlation id (D-431)"
                    .to_owned(),
            )))
        },
        |Extension(correlation)| Ok(correlation.get()),
    )
}

#[cfg(test)]
#[path = "correlation_tests.rs"]
mod correlation_tests;
