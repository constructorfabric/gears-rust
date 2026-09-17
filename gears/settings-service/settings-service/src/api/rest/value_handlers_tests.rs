// Created: 2026-09-17 by Constructor Tech
//! What the write handlers read off a request before any service runs, and
//! the one response they build themselves: the RFC 9470 challenge.

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{STEP_UP_HEADER, actor, if_match, parse_key, parse_tenant, respond, step_up_challenge};
use crate::domain::error::DomainError;
use crate::field;

const KEY: &str = "gts.cf.core.settings.setting_type.v1~acme.billing.network.proxy.v1~";

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(
            header::HeaderName::from_bytes(name.as_bytes()).expect("a header name"),
            HeaderValue::from_str(value).expect("a header value"),
        );
    }
    map
}

fn context() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(1))
        .subject_tenant_id(Uuid::nil())
        .build()
        .expect("context")
}

fn context_with_bearer(token: &str) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(1))
        .subject_tenant_id(Uuid::nil())
        .bearer_token(token.to_owned())
        .build()
        .expect("context")
}

#[test]
fn a_tag_is_taken_as_the_caller_wrote_it_quotes_and_spacing_aside() {
    // An `ETag` travels quoted and a client may echo it with either spelling.
    // The tag itself is opaque and compared verbatim, so the two spellings
    // have to arrive as one string or half the conditional writes fail.
    for raw in ["\"v2\"", "v2", "  \"v2\"  ", " v2 "] {
        assert_eq!(
            if_match(&headers(&[("if-match", raw)])),
            Some("v2"),
            "{raw}"
        );
    }
    assert_eq!(if_match(&HeaderMap::new()), None, "absent is not empty");
    assert_eq!(
        if_match(&headers(&[("if-match", "absent")])),
        Some("absent"),
        "the literal a first write sends is a tag like any other"
    );
}

#[test]
fn the_step_up_header_is_preferred_and_the_session_token_is_the_fallback() {
    // The rule a browser depends on: after a fresh re-authentication the page
    // sends the new assertion in the header, and the service must weigh that
    // one. With no header, a session that itself re-authenticated a moment
    // ago carries its own fresh `auth_time`, so the bearer is the assertion.
    let with_both = actor(
        &context_with_bearer("session-token"),
        &headers(&[(STEP_UP_HEADER, "fresh-token")]),
    );
    assert_eq!(with_both.step_up_token.as_deref(), Some("fresh-token"));

    let header_only = actor(&context(), &headers(&[(STEP_UP_HEADER, "fresh-token")]));
    assert_eq!(header_only.step_up_token.as_deref(), Some("fresh-token"));

    let bearer_only = actor(&context_with_bearer("session-token"), &HeaderMap::new());
    assert_eq!(bearer_only.step_up_token.as_deref(), Some("session-token"));

    let neither = actor(&context(), &HeaderMap::new());
    assert_eq!(neither.step_up_token, None);
}

#[test]
fn every_write_carries_a_request_id_the_trace_header_supplies_or_one_it_mints() {
    let traced = actor(
        &context(),
        &headers(&[("x-request-id", "11111111-1111-1111-1111-111111111111")]),
    );
    assert_eq!(traced.request_id, "11111111-1111-1111-1111-111111111111");

    // Nothing to inherit: the audit record still needs an id, so one is made.
    let minted = actor(&context(), &HeaderMap::new());
    assert!(
        Uuid::parse_str(&minted.request_id).is_ok(),
        "{}",
        minted.request_id
    );
    assert_ne!(
        minted.request_id,
        actor(&context(), &HeaderMap::new()).request_id,
        "two requests are not one"
    );
}

#[test]
fn a_malformed_key_or_tenant_is_refused_by_the_field_the_caller_can_fix() {
    let key = parse_key("not a key").expect_err("refused");
    match key {
        DomainError::Validation { field, code, .. } => {
            assert_eq!(field, "key");
            assert_eq!(code, field::VALIDATION);
        }
        other => panic!("expected a validation refusal, got {other:?}"),
    }
    assert!(parse_key(KEY).is_ok());

    let tenant = parse_tenant(Some("nope")).expect_err("refused");
    match tenant {
        DomainError::Validation {
            field,
            code,
            message,
        } => {
            assert_eq!(field, "tenant");
            assert_eq!(code, field::TENANT_PARAM);
            assert!(message.contains("nope"), "{message}");
        }
        other => panic!("expected a validation refusal, got {other:?}"),
    }
}

#[test]
fn an_absent_or_empty_tenant_means_the_callers_own_not_a_refusal() {
    // `?tenant=` is what a form submits when the field was left alone; it has
    // to read as "my own scope", the same as omitting the parameter.
    assert_eq!(parse_tenant(None).expect("accepted"), None);
    assert_eq!(parse_tenant(Some("")).expect("accepted"), None);

    let id = Uuid::from_u128(9);
    assert_eq!(
        parse_tenant(Some(&id.to_string())).expect("accepted"),
        Some(id)
    );
}

#[test]
fn the_challenge_names_the_scheme_the_reason_and_the_window() {
    let response = step_up_challenge(DomainError::StepUpRequired {
        reason: "stale",
        max_age_seconds: 300,
        acr_values: Vec::new(),
    });
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let challenge = response
        .headers()
        .get(header::WWW_AUTHENTICATE)
        .expect("the challenge")
        .to_str()
        .expect("ascii");
    assert!(challenge.starts_with("Bearer "), "{challenge}");
    assert!(
        challenge.contains("error=\"insufficient_user_authentication\""),
        "{challenge}"
    );
    assert!(challenge.contains("(stale)"), "{challenge}");
    assert!(challenge.contains("max_age=300"), "{challenge}");
    assert!(
        !challenge.contains("acr_values"),
        "nothing is required, so nothing is asked for: {challenge}"
    );
}

#[test]
fn a_required_assurance_is_asked_for_in_the_challenge_space_separated() {
    let response = step_up_challenge(DomainError::StepUpRequired {
        reason: "assurance",
        max_age_seconds: 60,
        acr_values: vec!["mfa".to_owned(), "hwk".to_owned()],
    });

    let challenge = response
        .headers()
        .get(header::WWW_AUTHENTICATE)
        .expect("the challenge")
        .to_str()
        .expect("ascii");
    assert!(challenge.contains("acr_values=\"mfa hwk\""), "{challenge}");
}

#[test]
fn a_refusal_that_is_not_a_step_up_still_answers_without_a_window_it_cannot_know() {
    // Reachable only by misuse, but it must not panic or claim a freshness
    // window no deployment configured.
    let response = step_up_challenge(DomainError::Conflict {
        detail: "not a step-up".to_owned(),
    });
    let challenge = response
        .headers()
        .get(header::WWW_AUTHENTICATE)
        .expect("the challenge")
        .to_str()
        .expect("ascii");
    assert!(challenge.contains("(unknown)"), "{challenge}");
    assert!(challenge.contains("max_age=0"), "{challenge}");
}

#[test]
fn only_a_step_up_refusal_becomes_a_response_every_other_stays_an_error() {
    // The distinction the write path rests on: a step-up refusal is answered
    // with its challenge, and anything else travels as the canonical error so
    // the error layer renders it once, in one shape.
    let ok = respond(Ok::<_, DomainError>("body")).expect("a response");
    assert_eq!(ok.status(), StatusCode::OK);

    let challenged = respond(Err::<&str, _>(DomainError::StepUpRequired {
        reason: "missing",
        max_age_seconds: 300,
        acr_values: Vec::new(),
    }))
    .expect("a response, not an error");
    assert_eq!(challenged.status(), StatusCode::UNAUTHORIZED);
    assert!(
        challenged.headers().contains_key(header::WWW_AUTHENTICATE),
        "the challenge survives the mapping"
    );

    let err = respond(Err::<&str, _>(DomainError::Retired {
        key: "k".to_owned(),
    }))
    .expect_err("an error");
    assert_eq!(err.into_response().status(), StatusCode::GONE);
}
