//! Each case names the defect it closes rather than the function it calls.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use axum::http::{HeaderMap, HeaderValue};

use super::{etag, if_match};
use crate::domain::concurrency::InternalRevision;
use crate::domain::error::DomainError;

fn headers_with(value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "if-match",
        HeaderValue::from_str(value).expect("header value"),
    );
    headers
}

/// Every refusal in this module rides one variant; a case that got a
/// different one would be minting the second bare-400 class the design set
/// says does not exist here.
fn violation_detail(err: &DomainError) -> String {
    match err {
        DomainError::Validation(report) => report
            .violations()
            .first()
            .expect("a raised Validation always carries at least one violation")
            .detail
            .clone(),
        other => panic!("expected DomainError::Validation, got {other:?}"),
    }
}

#[test]
fn a_tag_round_trips_through_the_header_it_is_carried_in() {
    // The whole point of emitting an `ETag`: what a `GET` hands back has to be
    // submittable verbatim on the next `PATCH`. A rendering and a parse that
    // disagreed would make the precondition unsatisfiable for every caller.
    let revision = InternalRevision::new(3);
    let headers = headers_with(&etag(revision));

    assert_eq!(if_match(&headers).expect("the tag parses"), revision);
}

#[test]
fn an_absent_if_match_is_refused_validation_and_names_the_header() {
    // The Acceptance Criteria: "A save without `If-Match` is refused
    // `VALIDATION`" — and, distinctly, *not* `STALE_REVISION`. A caller with
    // no header has read nothing stale; they have sent an incomplete request.
    let err = if_match(&HeaderMap::new()).expect_err("an absent precondition is refused");

    assert!(matches!(err, DomainError::Validation(_)), "{err:?}");
    assert!(violation_detail(&err).contains("If-Match"), "{err:?}");
}

#[test]
fn the_wildcard_is_refused_because_it_is_an_unconditional_write() {
    // `*` means "if the resource exists at all", i.e. overwrite whichever
    // revision is current. This is the point where products and
    // `gears/file-storage`'s write path deliberately part ways: that gear's
    // `if m != "*" && ...` accepts the wildcard, and this module's whole
    // reason for refusing it is that products does not.
    let err = if_match(&headers_with("*")).expect_err("the wildcard is refused");

    assert!(matches!(err, DomainError::Validation(_)), "{err:?}");
    assert!(violation_detail(&err).contains("wildcard"), "{err:?}");
}

#[test]
fn a_weak_validator_is_refused() {
    // RFC 9110 §13.1.1 forbids a weak validator on `If-Match`: a weak
    // comparison cannot decide whether a write is safe, which is the one
    // question this header exists to answer.
    let err = if_match(&headers_with("W/\"3\"")).expect_err("a weak validator is refused");

    assert!(matches!(err, DomainError::Validation(_)), "{err:?}");
}

#[test]
fn garbage_in_the_precondition_is_refused_rather_than_coerced() {
    // Every one of these is a header that parsed but named no revision: a
    // bare integer, an empty tag, a negative sign, a non-digit body, a list,
    // and the empty string. None of them is a stale revision — there is no
    // revision to compare — so all of them stay `VALIDATION`, never
    // `STALE_REVISION`.
    for raw in ["3", "\"\"", "\"-1\"", "\"three\"", "\"1\", \"2\"", ""] {
        let err = if_match(&headers_with(raw))
            .expect_err("only one strong quoted decimal names a revision");
        assert!(
            matches!(err, DomainError::Validation(_)),
            "{raw:?} -> {err:?}"
        );
    }
}

#[test]
fn a_header_that_is_not_utf8_is_refused_rather_than_panicking() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "if-match",
        HeaderValue::from_bytes(&[0xff, 0xfe]).expect("a non-UTF-8 header value"),
    );

    let err = if_match(&headers).expect_err("a header this gear cannot read back is refused");
    assert!(matches!(err, DomainError::Validation(_)), "{err:?}");
}

#[test]
fn internal_revision_from_etag_round_trips_independent_of_the_header_layer() {
    // The domain type's own contract, exercised without a `HeaderMap`: the
    // module doc's claim that the repositories and this layer "agree, to the
    // byte" on what tag denotes what revision would be untestable if the only
    // path to it went through a header.
    let revision = InternalRevision::new(41);

    assert_eq!(
        InternalRevision::from_etag(&revision.to_etag()).expect("the tag parses"),
        revision
    );
}
