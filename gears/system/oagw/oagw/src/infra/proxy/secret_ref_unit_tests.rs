//! Unit tests of the per-request secret reference steps (ADR-0022).

use std::collections::HashMap;

use http::{HeaderMap, HeaderName, HeaderValue};
use oagw_sdk::{SECRET_REF_HEADER, field, reason};
use uuid::Uuid;

use super::*;
use crate::domain::error::DomainError;
use crate::domain::gts_helpers::{APIKEY_AUTH_PLUGIN_ID, NOOP_AUTH_PLUGIN_ID};
use crate::domain::model::{AuthConfig, SharingMode};
use crate::domain::plugin::credentials::CredentialMode;

fn headers(values: &[&[u8]]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for v in values {
        h.append(SECRET_REF_HEADER, HeaderValue::from_bytes(v).unwrap());
    }
    h
}

fn reason_of(e: &DomainError) -> String {
    match e {
        DomainError::Validation { field, reason, .. } => {
            assert_eq!(*field, SECRET_REF_HEADER);
            (*reason).to_owned()
        }
        DomainError::Forbidden { reason, .. } => reason.clone(),
        other => panic!("unexpected {other:?}"),
    }
}

/// SREF-REQ-003, SREF-REQ-004: the header parser accepts exactly one
/// reference, with or without `cred://`, under any name case.
#[test]
fn requested_ref_parses_one_reference() {
    assert!(requested_ref(&HeaderMap::new(), "").unwrap().is_none());
    for ok in [&b"cred://k-1"[..], b"k-1", b"  cred://k-1  "] {
        let got = requested_ref(&headers(&[ok]), "").unwrap().unwrap();
        assert_eq!(got.as_ref(), "k-1");
    }
    for bad in [
        &[&b""[..]][..],
        &[b" "],
        &[b"cred://"],
        &[b"a,b"],
        &[b"a b"],
        &[b"a:b"],
        &[b"cred://a/b"],
        &[b"\xff"],
        &[b"a", b"a"],
        &[b"a", b"b"],
    ] {
        let e = requested_ref(&headers(bad), "").unwrap_err();
        assert_eq!(reason_of(&e), field::INVALID_SECRET_REF, "{bad:?}");
    }
}

/// SREF-GUARD-001, SREF-GUARD-002, SREF-GUARD-003: the owner must be the auth
/// owner or below it on the caller's chain.
#[test]
fn owner_within_chain() {
    let (root, partner, tenant, other) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let chain = [tenant, partner, root];
    assert!(owner_within(partner, partner, &chain));
    assert!(owner_within(tenant, partner, &chain));
    assert!(!owner_within(root, partner, &chain));
    assert!(owner_within(tenant, tenant, &chain));
    assert!(!owner_within(partner, tenant, &chain));
    assert!(!owner_within(other, partner, &chain));
    assert!(!owner_within(tenant, other, &chain));
}

fn apikey(entries: &[(&str, &str)]) -> AuthConfig {
    AuthConfig {
        plugin_type: APIKEY_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Private,
        config: Some(
            entries
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect::<HashMap<_, _>>(),
        ),
    }
}

fn key(s: &str) -> credstore_sdk::SecretRef {
    credstore_sdk::SecretRef::new(s).unwrap()
}

/// SREF-REQ-001, SREF-CFG-006: the mode table, and other plugins read as
/// `config`.
#[test]
fn select_follows_the_mode_table() {
    let config = apikey(&[("header", "authorization"), ("secret_ref", "c")]);
    let with_default = apikey(&[
        ("header", "authorization"),
        ("credential_mode", "request_ref"),
        ("secret_ref", "d"),
    ]);
    let no_default = apikey(&[
        ("header", "authorization"),
        ("credential_mode", "request_ref"),
    ]);
    let noop = AuthConfig {
        plugin_type: NOOP_AUTH_PLUGIN_ID.into(),
        ..with_default.clone()
    };
    let run = |auth: Option<&AuthConfig>, requested: Option<&str>| {
        select(CredentialMode::of(auth), auth, requested.map(key), "")
    };

    for auth in [Some(&config), Some(&noop), None] {
        assert_eq!(
            reason_of(&run(auth, Some("r")).err().unwrap()),
            field::SECRET_REF_NOT_ALLOWED
        );
        assert!(run(auth, None).unwrap().is_none());
    }
    let plan = run(Some(&with_default), Some("r")).unwrap().unwrap();
    assert!(matches!(plan.source, SecretSource::Requested(ref r) if r.as_ref() == "r"));
    let plan = run(Some(&with_default), None).unwrap().unwrap();
    assert!(matches!(plan.source, SecretSource::Default(ref r) if r.as_ref() == "d"));
    let plan = run(Some(&no_default), Some("r")).unwrap().unwrap();
    assert!(matches!(plan.source, SecretSource::Requested(_)));
    assert_eq!(
        reason_of(&run(Some(&no_default), None).err().unwrap()),
        field::SECRET_REF_REQUIRED
    );
}

fn injected(value: &str) -> InjectedSecret {
    InjectedSecret {
        header: HeaderName::from_static("authorization"),
        value: HeaderValue::from_str(value).unwrap(),
    }
}

/// SREF-HDR-001..004: the auth header is stripped before header rules and
/// transforms, must be unset after them, and the key goes in last.
#[test]
fn auth_header_held_back_until_final_setup() {
    let secret = injected("Bearer sk");

    // A client value, repeated, is stripped.
    let mut h = HeaderMap::new();
    h.append("authorization", HeaderValue::from_static("Bearer client"));
    h.append("authorization", HeaderValue::from_static("Basic c"));
    secret.strip(&mut h);
    assert!(h.get("authorization").is_none());
    let (upstream, route) = (Uuid::nil(), Uuid::nil());
    assert!(check_auth_header_unset(&h, &secret, upstream, route).is_ok());

    // A value set after the strip, by a rule or transform, is refused as a
    // gateway misconfiguration (internal), not a caller-side policy denial.
    let mut set = h.clone();
    set.insert("authorization", HeaderValue::from_static("Bearer other"));
    assert!(matches!(
        check_auth_header_unset(&set, &secret, upstream, route).unwrap_err(),
        DomainError::Internal { .. }
    ));

    // The late injection sets exactly the key.
    secret.apply(&mut h);
    let values: Vec<_> = h.get_all("authorization").iter().collect();
    assert_eq!(values, [&HeaderValue::from_static("Bearer sk")]);
}

/// SREF-SDK-007: the proxy reads [`oagw_sdk::SECRET_REF_HEADER`], and its
/// errors carry the SDK's field and reason constants.
#[test]
fn errors_use_sdk_constants() {
    assert_eq!(SECRET_REF_HEADER, "x-oagw-secret-ref");
    let mut h = HeaderMap::new();
    h.insert(SECRET_REF_HEADER, HeaderValue::from_static("k"));
    assert!(requested_ref(&h, "").unwrap().is_some());

    let canonical = |e: DomainError| {
        oagw_sdk::ServiceGatewayError::from(toolkit_canonical_errors::CanonicalError::from(e))
    };
    let e = requested_ref(&headers(&[b"a,b"]), "").unwrap_err();
    assert!(matches!(
        e,
        DomainError::Validation { field: f, reason: r, .. }
            if f == SECRET_REF_HEADER && r == field::INVALID_SECRET_REF
    ));
    assert!(matches!(
        canonical(e),
        oagw_sdk::ServiceGatewayError::InvalidSecretRef {
            code: oagw_sdk::field::SecretRefCode::Invalid,
            ..
        }
    ));
    for (e, wire) in [
        (
            not_accessible(),
            reason::permission::SECRET_REF_NOT_ACCESSIBLE,
        ),
        (
            policy_denied("x"),
            reason::permission::SECRET_REF_POLICY_DENIED,
        ),
    ] {
        assert_eq!(reason_of(&e), wire);
        assert!(matches!(
            canonical(e),
            oagw_sdk::ServiceGatewayError::PermissionDenied { ref reason, .. }
                if reason.as_wire() == wire
        ));
    }
    assert_eq!(CredentialMode::RequestRef.as_str(), "request_ref");
    assert_eq!(CredentialMode::Config.as_str(), "config");
}
