//! D-178: the one correlation id a request carries, established at the gear's
//! HTTP edge.
//!
//! `inst-au-complete` lists the correlation id among the five fields an audit
//! record must carry, at `p1`, and §3.7's `pricing_outbox` bullet requires one on
//! every event. D-178 answers the question neither of them did — *who produces
//! it* — in three clauses, and this module is clauses (1) and (2):
//!
//! 1. It is the request-scoped correlation this edge establishes: **the value the
//!    platform propagates inbound when there is one, minted here when there is
//!    not**, so the field is always satisfiable and never NULL.
//! 2. **Every** audit record and every `pricing_outbox` row a single operator call
//!    produces carries **one** value. That is the clause with teeth: D-135
//!    segments the audit chain per aggregate, so the two records one `PATCH`
//!    writes when it opens a successor revision sit at two positions on the chain
//!    — and once Slice 12's bulk plane lands, on two segments — with this as the
//!    only thing that says they were one act.
//! 3. It is **not** the `Idempotency-Key`. That one is client-minted, per
//!    operation, and the subject of a *different* comparison (D-142/D-174): a
//!    retried call carries the same idempotency key on purpose, and conflating
//!    the two would make the retry correlate to the original and an unretried
//!    sibling correlate to nothing. It is not derived from the payload either.
//!
//! # It is **minted unconditionally**, and the platform convention it declines
//! to consume is named here rather than denied
//!
//! D-178 deliberately leaves the **spelling** of the propagated value's transport
//! to the platform — *"every gear's trail wants the same header, so naming one
//! here would be this gear legislating a platform contract"*. This section used
//! to conclude *"there is none this gear can consume"* on four bullets, and **two
//! of them were wrong**. The corrected finding is that a consumable convention
//! **exists**, that its primary spelling **does** fit the column, and that this
//! module still declines it — for reasons that are about the value's meaning and
//! not about its absence.
//!
//! **What the platform actually has.**
//! `toolkit::api::canonical_error_layer::extract_trace_id` reads
//! **W3C `traceparent` → `x-trace-id` → `x-request-id`** (then a span-id
//! fallback), and this gear already mounts that middleware around the whole
//! merged router (`crate::module`). So there *is* an inbound convention, it is a
//! standard rather than a local invention, and this gear is already inside it —
//! `toolkit` is not without inbound request-id middleware.
//!
//! Two of the old bullets survive as context and neither is load-bearing any
//! more. `telemetry.http.inject_request_id_header: "x-request-id"` is declared in
//! `toolkit::telemetry::config::HttpOpts`, set in four deployment configs and
//! read by no Rust code; and `toolkit-http`'s retry layer reads `x-request-id`
//! → `x-correlation-id` only to label its own logs on requests this gear
//! *sends*. Neither is the convention above, and the convention above does not
//! need them.
//!
//! **And the primary spelling fits.** A W3C trace-id is 32 hex characters — 128
//! bits, exactly a `uuid` column's width — and `Uuid::parse_str` accepts the
//! simple form and renders it back byte-identically (verified in-crate:
//! `ffffffffffffffffffffffffffffffff` round-trips). The old bullet "the shape
//! does not fit" is true only of the two free-text *fallbacks*, and it is
//! narrowed to them.
//!
//! **Why this edge still mints.** Three reasons, none of which is "there is
//! nothing to read":
//!
//! 1. **A trace-id is not an operator call.** D-178 clause (2) binds the value to
//!    *one operator call*; a W3C trace-id identifies a whole distributed trace,
//!    which a batch caller may hold across hundreds of calls. Consuming it makes
//!    "these records were one act" a property of the **caller's** instrumentation
//!    rather than of this gear, and there is no header that says which the caller
//!    meant. That is a widening of the join the field exists for, and it is a
//!    decision the design set has not taken.
//! 2. **Partial adoption is a join that is right sometimes.** Only one of the
//!    three spellings fits the column, so a caller sending `x-request-id` and no
//!    `traceparent` would get a `problem.trace_id` on an error response and a
//!    *different*, minted `correlation_id` on the trail — with nothing on either
//!    saying they disagree. Consuming all three needs the free-text problem D-178
//!    clause (a) already rejected.
//! 3. **A trace-id is 128 bits but not a UUID.** [`establish`] mints v7
//!    deliberately (see its own note), and the column is read as time-ordered.
//!    A parsed trace-id lands with an arbitrary version and variant — `Max` /
//!    `Future` for the all-`f` sample above — so consuming it puts two kinds of
//!    value in one column with nothing distinguishing them. Whether D-178's
//!    correlation admits a non-UUID 128-bit value is a question for the design
//!    set, and a code group answering it by writing such values is the shape
//!    "divergence is the product" forbids.
//!
//! **Reported, not closed.** The propagation half of clause (1) is reachable and
//! is deliberately not taken; the docs wave that lands this phase's debt owes
//! D-178 an answer on (1) and (3). What would close it cleanly on the platform
//! side is `toolkit` exposing its already-extracted trace id as a request
//! extension, so one place decides the three-spelling precedence and this gear
//! reads a value rather than re-parsing a header into a third copy of
//! `parse_w3c_trace_id`. [`establish`] is still the single site that changes.
//!
//! # Why a layer, and why extraction **fails** rather than mints
//!
//! A handler that minted its own would satisfy "not NULL" and break clause (2)
//! the moment a call writes two records — which the `PATCH` successor arm already
//! does, and which is exactly how the publish route was left: `Uuid::now_v7()` at
//! three call sites of one handler. So the value is established **once**, before
//! any handler runs, and reaches the writers through the [`AuditStamp`] that
//! [`crate::api::rest::auth_context::audit_stamp`] builds.
//!
//! [`AuditStamp`]: crate::domain::audit::AuditStamp
//!
//! [`require_correlation`] therefore refuses to mint. A route reachable without
//! this layer is a **wiring defect** — every router that mounts a mutating
//! surface applies `axum::middleware::from_fn(correlation::establish)` in its own
//! `router()`, so the edge travels with the routes rather than with whoever
//! remembered to compose them, and the crate's own route suites drive the real
//! routers — and a mint there would hide the defect behind a per-record value,
//! which is the failure the layer exists to prevent. It answers 500, loudly.
//!
//! The read-only `frontier` router does not apply it: nothing behind it writes an
//! audit record or an outbox row, so there is no field for a correlation to
//! satisfy. If it ever grows a writer, that writer cannot build an [`AuditStamp`]
//! without one and this function is what tells it so.

use axum::extract::{Extension, Request};
use axum::middleware::Next;
use axum::response::Response;
use toolkit::api::canonical_prelude::CanonicalError;
use uuid::Uuid;

use crate::domain::error::DomainError;

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
        // v7 rather than v4: the id is stored beside a `recorded_at` on a
        // ≥ 7-year append-only store, and a time-ordered value keeps an index on
        // it useful to the Auditor read surface Slice 12 owes.
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
/// while quietly reintroducing the per-record value D-178 clause (2) forbids.
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
                 records it writes would carry no correlation id (D-178)"
                    .to_owned(),
            )))
        },
        |Extension(correlation)| Ok(correlation.get()),
    )
}

#[cfg(test)]
#[path = "correlation_tests.rs"]
mod correlation_tests;
