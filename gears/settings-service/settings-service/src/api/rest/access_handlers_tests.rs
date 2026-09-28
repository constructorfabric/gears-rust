// Created: 2026-09-17 by Virtuozzo International GmbH
//! What the restriction handlers read off a request: the target tenant this
//! surface insists on, and the tag a mutation must present.

use axum::http::{HeaderMap, HeaderValue, header};
use uuid::Uuid;

use super::{if_match, parse_key, parse_target};
use crate::domain::error::DomainError;
use crate::field;

fn headers(name: header::HeaderName, value: &str) -> HeaderMap {
    let mut map = HeaderMap::new();
    map.insert(name, HeaderValue::from_str(value).expect("a header value"));
    map
}

#[test]
fn the_target_tenant_is_required_here_unlike_everywhere_else() {
    // On the value surface an absent `tenant` means "my own scope". A
    // restriction is always *about* another tenant, so defaulting it would
    // silently restrict the caller instead of the tenant they meant.
    for absent in [None, Some("")] {
        match parse_target(absent) {
            Err(DomainError::Validation {
                field,
                code,
                message,
            }) => {
                assert_eq!(field, "tenant");
                assert_eq!(code, field::TENANT_PARAM);
                assert!(message.contains("required"), "{message}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    let id = Uuid::from_u128(5);
    assert_eq!(parse_target(Some(&id.to_string())).expect("accepted"), id);
}

#[test]
fn a_tenant_that_is_not_an_id_is_refused_with_what_was_sent() {
    match parse_target(Some("tenant-7")) {
        Err(DomainError::Validation { field, message, .. }) => {
            assert_eq!(field, "tenant");
            assert!(message.contains("tenant-7"), "{message}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_malformed_key_is_refused_on_the_key_field() {
    match parse_key("not a key") {
        Err(DomainError::Validation { field, code, .. }) => {
            assert_eq!(field, "key");
            assert_eq!(code, field::VALIDATION);
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(
        parse_key("gts.cf.core.settings.setting_type.v1~acme.settings.network.proxy.v1~").is_ok()
    );
}

#[test]
fn the_tag_arrives_as_one_string_however_the_client_quoted_it() {
    for raw in ["\"absent\"", "absent", "  \"absent\"  "] {
        assert_eq!(
            if_match(&headers(header::IF_MATCH, raw)),
            Some("absent"),
            "{raw}"
        );
    }
    assert_eq!(if_match(&HeaderMap::new()), None);
}
