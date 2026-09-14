//! Each case names the defect it closes rather than the function it calls.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use axum::http::{HeaderMap, HeaderValue};
use serde::Serialize;
use std::collections::BTreeMap;

use super::{
    IDEMPOTENCY_KEY, etag, idempotency_key, if_match, if_match_policy, if_match_revision,
    if_none_match, not_modified, parse_body, policy_etag, request_digest, revision_etag,
};
use crate::domain::concurrency::{PolicyTag, RowVersion};
use crate::domain::error::DomainError;

fn headers_with(name: &'static str, value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(name, HeaderValue::from_str(value).expect("header value"));
    headers
}

fn refusal(err: &DomainError) -> &str {
    match err {
        DomainError::InvalidRequest(detail) => detail,
        other => panic!("expected a malformed-request refusal, got {other:?}"),
    }
}

#[test]
fn a_tag_round_trips_through_the_header_it_is_carried_in() {
    // The whole point of emitting an `ETag`: what a caller reads back has to be
    // submittable verbatim on the next mutating verb. A rendering and a parse
    // that disagreed would make every precondition unsatisfiable.
    let version = RowVersion::new(7);
    let headers = headers_with("if-match", &etag(version));

    assert_eq!(if_match(&headers).expect("the tag parses"), version);
}

#[test]
fn an_absent_if_match_is_a_malformed_request_and_mints_no_code() {
    // D-141: "an absent precondition is a malformed request under the Foundation
    // validation envelope, so no new code is minted." The refusal has to be
    // InvalidRequest, and it has to name the header so the caller knows what to
    // add.
    let err = if_match(&HeaderMap::new()).expect_err("an absent precondition is refused");

    assert!(refusal(&err).contains("If-Match"), "{err:?}");
}

#[test]
fn the_wildcard_is_refused_because_it_is_an_unconditional_write() {
    // `*` means "if the resource exists at all", i.e. overwrite whichever
    // version is current — exactly what D-141 and D-145 exist to prevent.
    let err = if_match(&headers_with("if-match", "*")).expect_err("the wildcard is refused");

    assert!(refusal(&err).contains("wildcard"), "{err:?}");
}

#[test]
fn a_weak_validator_and_a_list_are_both_refused() {
    let weak = if_match(&headers_with("if-match", "W/\"7\"")).expect_err("weak is refused");
    let list = if_match(&headers_with("if-match", "\"7\", \"8\"")).expect_err("a list is refused");

    assert!(matches!(weak, DomainError::InvalidRequest(_)), "{weak:?}");
    assert!(matches!(list, DomainError::InvalidRequest(_)), "{list:?}");
}

#[test]
fn garbage_in_the_precondition_is_refused_rather_than_coerced() {
    for raw in ["7", "\"\"", "\"-1\"", "\"seven\"", ""] {
        let err = if_match(&headers_with("if-match", raw))
            .expect_err("only one strong quoted decimal is a tag");
        assert!(
            matches!(err, DomainError::InvalidRequest(_)),
            "{raw:?} -> {err:?}"
        );
    }
}

#[test]
fn an_absent_idempotency_key_is_refused_on_a_guarded_create() {
    // The decision made once here rather than per route: S2/S3 §5 name the cell
    // *client idempotency key* and §4.2 makes the gate the first step, so an
    // unguarded create is not an option the surface offers.
    let err = idempotency_key(&HeaderMap::new()).expect_err("the guarded create requires a key");

    assert!(refusal(&err).contains("Idempotency-Key"), "{err:?}");
}

#[test]
fn a_key_is_bounded_and_printable() {
    let long = "k".repeat(256);
    for raw in ["", long.as_str(), "with\tcontrol"] {
        let err = idempotency_key(&headers_with(IDEMPOTENCY_KEY, raw))
            .expect_err("the key is stored and echoed, so its shape is bounded");
        assert!(
            matches!(err, DomainError::InvalidRequest(_)),
            "{raw:?} -> {err:?}"
        );
    }
    // A byte a header may carry (RFC 9110 obs-text) and UTF-8 may not: the
    // refusal comes from the decode rather than from the charset check, and
    // both arms have to answer, or a caller meets a panic instead of a 400.
    let mut binary = HeaderMap::new();
    binary.insert(
        IDEMPOTENCY_KEY,
        HeaderValue::from_bytes(&[0xffu8]).expect("obs-text is a legal header byte"),
    );
    assert!(
        matches!(
            idempotency_key(&binary).expect_err("a non-UTF-8 key is refused"),
            DomainError::InvalidRequest(_)
        ),
        "a header this gear cannot read back is a malformed request, not a panic"
    );
    assert_eq!(
        idempotency_key(&headers_with(
            IDEMPOTENCY_KEY,
            "3f2504e0-4f89-11d3-9a0c-0305e82c3301"
        ))
        .expect("an ordinary client key is accepted"),
        "3f2504e0-4f89-11d3-9a0c-0305e82c3301"
    );
}

// `Deserialize` and `Debug` as well as `Serialize`: the digest cases need the
// first, and `parse_body`'s three cases need the other two. One fixture for both
// halves rather than two, so a request type that round-trips here round-trips in
// the shape a route actually holds it.
#[derive(Debug, Serialize, serde::Deserialize)]
struct Sample {
    name: String,
    tags: BTreeMap<String, String>,
}

#[test]
fn two_requests_that_differ_only_in_member_order_digest_identically() {
    // The reason the digest is over the PARSED request rather than the raw body:
    // a retry through a different client serializes the same intent in a
    // different order, and hashing bytes would answer it
    // IDEMPOTENCY_PAYLOAD_MISMATCH — the refusal spent on a caller who did
    // nothing wrong.
    let first = Sample {
        name: "starter".to_owned(),
        tags: BTreeMap::from([
            ("b".to_owned(), "2".to_owned()),
            ("a".to_owned(), "1".to_owned()),
        ]),
    };
    let second = Sample {
        name: "starter".to_owned(),
        tags: BTreeMap::from([
            ("a".to_owned(), "1".to_owned()),
            ("b".to_owned(), "2".to_owned()),
        ]),
    };

    assert_eq!(
        request_digest(&first).expect("digest"),
        request_digest(&second).expect("digest"),
        "a BTreeMap serializes in key order, so insertion order cannot move the digest"
    );
}

#[test]
fn two_requests_that_differ_in_a_value_digest_differently() {
    let first = Sample {
        name: "starter".to_owned(),
        tags: BTreeMap::new(),
    };
    let second = Sample {
        name: "pro".to_owned(),
        tags: BTreeMap::new(),
    };

    assert_ne!(
        request_digest(&first).expect("digest"),
        request_digest(&second).expect("digest"),
        "a different intent under one key must be refused, so it must digest differently"
    );
}

// ---------------------------------------------------------------------------
// The five public functions no case reached, and the two the conditional read
// added. Named here because the inversion mattered: `if_match_revision` exists
// for the argument D-145 spends two paragraphs on — a bare version is ambiguous
// on a plan route, since a freshly opened successor starts at `RowVersion::new(0)`,
// which is exactly the value the plan's first draft carries — and that argument's
// implementation was reachable only through a suite needing a router, a PDP double
// and an in-memory SQLite. `parse_body` is the function the `Json`-extractor
// finding is about and had no test either.
// ---------------------------------------------------------------------------

#[test]
fn a_revision_tag_round_trips_and_carries_both_numbers() {
    // The property `etag`/`if_match` has on the row plane, on the plane where one
    // number is not enough. Both members are asserted against **different** values,
    // so a reader that returned the same number twice, or swapped the pair, fails.
    let headers = headers_with("if-match", &revision_etag(3, RowVersion::new(7)));

    let tag = if_match_revision(&headers).expect("the tag parses");
    assert_eq!(tag.revision, 3);
    assert_eq!(tag.version, RowVersion::new(7));
}

#[test]
fn a_bare_version_is_not_a_revision_tag() {
    // The ambiguity D-145 names: `"0"` is a legal row tag and must not be readable
    // as revision 0 at version 0, or a caller who read a first draft could satisfy
    // the precondition of a freshly opened successor.
    let headers = headers_with("if-match", &etag(RowVersion::new(0)));

    let err = if_match_revision(&headers).expect_err("a bare version names no revision");
    assert!(
        refusal(&err).contains("revision"),
        "the refusal says what shape it wanted: {}",
        refusal(&err)
    );
}

#[test]
fn an_absent_if_match_on_a_revision_route_names_both_numbers_it_wants() {
    let err =
        if_match_revision(&HeaderMap::new()).expect_err("an unconditional plan write is refused");
    let detail = refusal(&err);
    assert!(detail.contains("If-Match"), "{detail}");
    assert!(
        detail.contains("revision") && detail.contains("version"),
        "a caller told only that a header is missing cannot build one: {detail}"
    );
}

#[test]
fn a_policy_tag_round_trips_through_its_header() {
    // The third tag plane, and the one whose `GET` is the only source of a tag —
    // so a rendering and a parse that disagreed would make the `PUT` unsatisfiable
    // for every caller including the bootstrap.
    let tag = PolicyTag::of(Some(4), None);
    let headers = headers_with("if-match", &policy_etag(&tag));

    assert_eq!(if_match_policy(&headers).expect("the tag parses"), tag);
}

#[test]
fn the_unset_policy_tag_is_not_the_tag_of_version_zero() {
    // Absence framed distinctly from any number, asserted at the header layer
    // rather than only in the domain: a tenant's first version is `0`, so a digest
    // that collided the two would tell a bootstrapping caller the world had not
    // moved when it had.
    assert_ne!(
        policy_etag(&PolicyTag::of(None, None)),
        policy_etag(&PolicyTag::of(Some(0), None))
    );
}

#[test]
fn a_policy_if_match_refuses_the_wildcard_it_shares_the_envelope_with() {
    // The shared envelope doing its job on the third plane: `strong_tag_body` is
    // one function, so this reader cannot be laxer than `if_match` on the wildcard
    // — which on this verb would author a threshold set over whichever policy
    // happens to be current.
    let err = if_match_policy(&headers_with("if-match", "*"))
        .expect_err("a wildcard is an unconditional write");
    assert!(!refusal(&err).is_empty());
}

#[test]
fn an_empty_body_is_refused_with_the_remedy_in_it() {
    // The case a `Json` extractor answers 400 for with no document — and the reason
    // the remedy is spelled: `{}` is a legal request on the routes whose members are
    // all optional, so "send a body" alone would not tell a caller what to send.
    let err = parse_body::<Sample>(b"").expect_err("an empty body is not a request");
    assert!(refusal(&err).contains("{}"), "{}", refusal(&err));
}

#[test]
fn a_body_that_does_not_fit_the_type_is_a_four_hundred_and_not_a_422() {
    // The whole reason this function exists rather than the extractor: axum answers
    // a well-formed JSON body that does not fit the target type with a **422**, and
    // §3.3 says no path in this gear can produce one. The variant is the assertion —
    // `InvalidRequest` is the only one that renders 400 here — and `refusal` panics
    // on any other.
    let err = parse_body::<Sample>(br#"{"name": 7}"#).expect_err("a number is not a name");
    assert!(refusal(&err).contains("not readable"), "{}", refusal(&err));
}

#[test]
fn a_well_formed_body_parses_to_the_value_it_names() {
    // The positive control the two refusals above need, or a `parse_body` that
    // refused everything would pass both.
    let parsed: Sample = parse_body(br#"{"name": "starter", "tags": {}}"#).expect("it parses");
    assert_eq!(parsed.name, "starter");
}

#[test]
fn a_conditional_read_matches_weakly_and_through_a_list() {
    // The three shapes RFC 9110 section 13.1.2 requires of `If-None-Match` and which
    // `if_match` deliberately refuses on its own verbs. Refusing them here would make
    // a conforming client's request fail rather than merely miss the cache.
    let tag = "\"3-7\"";
    assert!(if_none_match(&headers_with("if-none-match", tag), tag));
    assert!(if_none_match(&headers_with("if-none-match", "*"), tag));
    assert!(if_none_match(
        &headers_with("if-none-match", "W/\"3-7\""),
        tag
    ));
    assert!(if_none_match(
        &headers_with("if-none-match", "\"1-1\", \"3-7\""),
        tag
    ));
}

#[test]
fn a_conditional_read_that_does_not_match_serves_the_representation() {
    // Both fail-directions, and they are the half that says the comparison happened:
    // a reader that answered `true` for everything would pass the case above.
    let tag = "\"3-7\"";
    assert!(!if_none_match(
        &headers_with("if-none-match", "\"3-8\""),
        tag
    ));
    assert!(
        !if_none_match(&HeaderMap::new(), tag),
        "an absent header is not a match: the caller asked for the body"
    );
    // An unreadable value is not a match either — the RFC's own fallback, and the
    // only fail-direction that cannot withhold data from a caller.
    let mut broken = HeaderMap::new();
    broken.insert(
        "if-none-match",
        HeaderValue::from_bytes(&[0xff, 0xfe]).expect("a non-UTF-8 header value"),
    );
    assert!(!if_none_match(&broken, tag));
}

#[test]
fn a_not_modified_carries_the_validator_it_matched() {
    // RFC 9110 section 15.4.5: without the tag a caller who is told nothing changed
    // has to re-read unconditionally next time, which is the round trip the whole
    // conditional read saves.
    let response = not_modified("\"3-7\"");
    assert_eq!(response.status(), axum::http::StatusCode::NOT_MODIFIED);
    assert_eq!(
        response.headers().get("etag").and_then(|v| v.to_str().ok()),
        Some("\"3-7\"")
    );
}
