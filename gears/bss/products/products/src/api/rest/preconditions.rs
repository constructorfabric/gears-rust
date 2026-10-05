//! The `If-Match` precondition every mutating door on this surface speaks.
//!
//! `docs/features/foundation.md`'s save-an-edit flow names the contract in two
//! sentences, and this module exists so every door that lands in Phase 4
//! reads them off one function rather than growing its own copy:
//!
//! - **"A `GET` on a head returns an `ETag` that a subsequent `PATCH` accepts
//!   as `If-Match`"** — [`etag`] renders it, [`if_match`] parses it back.
//! - **"A save without `If-Match` is refused `VALIDATION`; a save with a
//!   stale `If-Match` is refused `STALE_REVISION`"** — two distinct refusals,
//!   kept distinct below.
//!
//! # Two refusals, and only one of them is this module's to raise
//!
//! An **absent** header is refused here, by [`if_match`], as
//! [`DomainError::Validation`] — the wire code `VALIDATION`. The same section
//! of the design set is explicit that this is not a second bare-400 class:
//! *"the request parsed, so the bare 400 this gear reserves for a malformed
//! request does not apply"*. Products mints no such second class at all — an
//! absent header, an unparseable one, and the wildcard all ride `VALIDATION`,
//! distinguished only by the message a caller reads.
//!
//! A **stale** but well-formed `If-Match` is `STALE_REVISION`, and this module
//! never raises it: `docs/design/01-foundation.md` places the comparison at
//! "the `If-Match` verb and the publish pin" as part of the **precondition
//! check**, which reads the row under the write — the door and its repository
//! compare-and-swap, inside the transaction that would otherwise race it. A
//! second, weaker comparison here, ahead of that one, would buy nothing and
//! could disagree with it. What this module does is parse a header into the
//! [`InternalRevision`] that comparison is made against; it never performs the
//! comparison itself.
//!
//! # The wildcard is refused, and products does not follow every gear here
//!
//! `If-Match: *` means "if the resource exists at all" — overwrite whichever
//! revision is current — and [`InternalRevision::from_etag`] refuses it for
//! that reason, the same reading `gears/bss/pricing`'s `RowVersion::from_etag`
//! gives. **`gears/file-storage`'s write path
//! (`domain/service/write.rs`) takes the opposite position**: it accepts `*`
//! (`if m != "*" && Some(m) != current_etag ...`) and treats `If-Match` as
//! optional on several of its routes. That is a deliberate divergence, not an
//! oversight either side should be brought in line with: products has a
//! Foundation rule — `fr-concurrent-edit` in spirit, stated here as the save
//! flow's own Acceptance Criteria — that a mutation of a head asserts the
//! revision it was authored against, and a caller-chosen wildcard is exactly
//! the unconditional write that rule exists to make unreachable. A later
//! reader who knows the file-storage gear should not "fix" this refusal to
//! match it.
//!
//! # The entity tag is the domain's, not the transport's
//!
//! [`InternalRevision::to_etag`] and [`InternalRevision::from_etag`] live in
//! [`crate::domain::concurrency`] and are called from here rather than
//! re-implemented: the repositories and this layer have to agree, to the
//! byte, on what tag denotes what revision, and a second rendering here would
//! be free to drift from the one that parses tags back. What this module adds
//! is the **header** layer the domain must not know about — which header
//! carries the tag, and what an absent one means.
//!
//! # `If-Match` only, in this module
//!
//! List and reference reads answer `If-None-Match` with `304` (P-D-261). This
//! module still parses only `If-Match` for the mutating doors. A weak tag is
//! not a row version, and [`if_match`] refuses one.
//! @cpt-dod:cpt-cf-bss-products-dod-if-match-version:p1

use axum::http::HeaderMap;
use axum::http::header::IF_MATCH;
use toolkit::api::operation_builder::{ParamSpec, ResponseHeaderSpec, ResponseHeaderType};

use crate::domain::concurrency::{ContentTag, InternalRevision};
use crate::domain::error::DomainError;
use crate::domain::validation::ValidationReport;

/// The entity tag denoting `revision`, ready for an `ETag` response header.
///
/// Emitting it is not optional on any door that also accepts `If-Match`: a
/// caller who cannot obtain a tag cannot satisfy the precondition on the next
/// verb, which would make the precondition unsatisfiable rather than merely
/// undocumented.
#[must_use]
pub fn etag(revision: InternalRevision) -> String {
    revision.to_etag()
}

/// The internal revision an `If-Match` header on a mutating verb asserts.
///
/// # Errors
///
/// [`DomainError::Validation`] naming the `If-Match` subject when the header
/// is **absent** — a save that ran without it would be exactly the
/// unconditional write the save flow's Acceptance Criteria refuse; when the
/// value is not valid UTF-8; and, through [`InternalRevision::from_etag`],
/// when it is a weak validator, the wildcard `*`, a comma-separated list, or
/// not a single quoted decimal. Every one of these rides the same
/// `VALIDATION` code — see this module's own doc for why a second bare-400
/// class does not exist here.
pub fn if_match(headers: &HeaderMap) -> Result<InternalRevision, DomainError> {
    let Some(raw) = headers.get(IF_MATCH) else {
        return Err(refuse(
            "If-Match is required on this verb: a save asserts the internal revision it was \
             authored against, and an unconditional write would overwrite a concurrent editor's \
             work. Read the `ETag` off the `GET` for this head and send it back verbatim",
        ));
    };
    let raw = raw
        .to_str()
        .map_err(|_| refuse("If-Match: the header value is not valid UTF-8"))?;
    InternalRevision::from_etag(raw)
}

/// The content tag an `If-Match` header asserts, for a resource tagged by its
/// content rather than a revision (P-D-205: the approval policy).
///
/// # Errors
///
/// [`DomainError::Validation`] naming `If-Match` when the header is absent,
/// not UTF-8, or not one strong decimal tag ([`ContentTag::from_etag`]).
pub fn if_match_content(headers: &HeaderMap) -> Result<ContentTag, DomainError> {
    let Some(raw) = headers.get(IF_MATCH) else {
        return Err(refuse(
            "If-Match is required on this verb: a write asserts the content it was authored \
             against. Read the `ETag` off the `GET` and send it back verbatim",
        ));
    };
    let raw = raw
        .to_str()
        .map_err(|_| refuse("If-Match: the header value is not valid UTF-8"))?;
    ContentTag::from_etag(raw)
}

/// Build the [`DomainError::Validation`] an absent or unreadable `If-Match`
/// header is refused with.
///
/// A single site so every case [`if_match`] itself raises — as distinct from
/// the cases [`InternalRevision::from_etag`] raises for a header it can read
/// but not parse — carries the same subject and the same wire code.
fn refuse(detail: &str) -> DomainError {
    let mut report = ValidationReport::new();
    report.violate("VALIDATION", "If-Match", detail);
    DomainError::Validation(report)
}

#[cfg(test)]
#[path = "preconditions_tests.rs"]
mod preconditions_tests;

/// Required revision precondition in `OpenAPI`.
pub(crate) fn if_match_param() -> ParamSpec {
    // A scalar string header: `ParamSpec::header` defaults to single-valued
    // `string`, which every parameter this gear declares is.
    ParamSpec::header("If-Match")
        .required(true)
        .description("Send the ETag returned by the last read. Missing or malformed: 400; stale: 409 STALE_REVISION.")
}

/// The `ETag` response header in `OpenAPI`: the tag a following write sends back as `If-Match`.
pub(crate) fn etag_header() -> ResponseHeaderSpec {
    ResponseHeaderSpec::new(
        "ETag",
        "The version to send back as If-Match",
        ResponseHeaderType::String,
    )
}

/// `If-None-Match` on a list read (P-D-261). A match is 304; the header is optional.
#[must_use]
pub(crate) fn if_none_match_param() -> ParamSpec {
    ParamSpec::header("If-None-Match")
        .required(false)
        .description("A weak ETag from an earlier read of this answer, or *. A match is 304.")
}

/// The weak `ETag` of the JSON body (P-D-261). It is not the version a write sends as `If-Match`.
#[must_use]
pub(crate) fn weak_etag_header() -> ResponseHeaderSpec {
    ResponseHeaderSpec::new(
        "ETag",
        "Weak tag of this JSON body",
        ResponseHeaderType::String,
    )
}

/// `Cache-Control` for a list the browser must revalidate (P-D-261).
#[must_use]
pub(crate) fn revalidate_header() -> ResponseHeaderSpec {
    ResponseHeaderSpec::new(
        "Cache-Control",
        "private, no-cache",
        ResponseHeaderType::String,
    )
}
