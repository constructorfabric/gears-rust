//! Exercise the managed deduplication builder's rejections.

use super::*;
use crate::error::EventBrokerError;

#[test]
fn a_managed_builder_without_a_key_is_rejected() {
    let result = DbDeduplication::managed(ProducerMode::Chained).build();

    match result {
        Err(EventBrokerError::InvalidProducerOptions { detail }) => {
            assert_eq!(detail, "managed producer registration key is required");
        }
        other => panic!("expected InvalidProducerOptions, got {other:?}"),
    }
}

#[test]
fn a_managed_builder_with_a_blank_key_is_rejected() {
    let result = DbDeduplication::managed(ProducerMode::Chained)
        .key("  ")
        .build();

    match result {
        Err(EventBrokerError::InvalidProducerOptions { detail }) => {
            assert_eq!(
                detail,
                "managed producer registration key must not be empty"
            );
        }
        other => panic!("expected InvalidProducerOptions, got {other:?}"),
    }
}

#[test]
fn a_managed_builder_with_a_key_over_255_bytes_is_rejected() {
    let result = DbDeduplication::managed(ProducerMode::Chained)
        .key("k".repeat(256))
        .build();

    match result {
        Err(EventBrokerError::InvalidProducerOptions { detail }) => {
            assert_eq!(
                detail,
                "managed producer registration key must be at most 255 bytes"
            );
        }
        other => panic!("expected InvalidProducerOptions, got {other:?}"),
    }
}

#[test]
fn an_overlong_key_with_a_trailing_space_is_refused_for_whitespace() {
    let result = DbDeduplication::managed(ProducerMode::Chained)
        .key(format!("{} ", "k".repeat(255)))
        .build();

    match result {
        Err(EventBrokerError::InvalidProducerOptions { detail }) => {
            assert_eq!(
                detail,
                "managed producer registration key must not have leading or trailing whitespace"
            );
        }
        other => panic!("expected InvalidProducerOptions, got {other:?}"),
    }
}

#[test]
fn a_managed_builder_in_stateless_mode_is_rejected() {
    let result = DbDeduplication::managed(ProducerMode::Stateless)
        .key("orders")
        .build();

    match result {
        Err(EventBrokerError::InvalidProducerOptions { detail }) => {
            assert_eq!(
                detail,
                "managed producer registration requires monotonic or chained mode"
            );
        }
        other => panic!("expected InvalidProducerOptions, got {other:?}"),
    }
}

#[test]
fn a_managed_builder_with_a_255_byte_key_builds() {
    let key = "k".repeat(255);

    let deduplication = DbDeduplication::managed(ProducerMode::Monotonic)
        .key(key.clone())
        .build()
        .expect("a 255-byte key is within the limit");

    assert_eq!(
        deduplication,
        DbDeduplication::Managed(ManagedDeduplication {
            mode: ProducerMode::Monotonic,
            key,
        })
    );
}

#[test]
fn a_registration_key_with_edge_whitespace_is_refused() {
    for key in ["orders ", " orders", "orders\t"] {
        let result = DbDeduplication::managed(ProducerMode::Chained)
            .key(key)
            .build();

        match result {
            Err(EventBrokerError::InvalidProducerOptions { detail }) => {
                assert_eq!(
                    detail,
                    "managed producer registration key must not have leading or trailing whitespace",
                    "key {key:?}"
                );
            }
            other => panic!("expected InvalidProducerOptions for key {key:?}, got {other:?}"),
        }
    }
}

#[test]
fn a_registration_key_with_inner_whitespace_builds() {
    let deduplication = DbDeduplication::managed(ProducerMode::Chained)
        .key("orders created")
        .build()
        .expect("inner whitespace is part of the key");

    assert_eq!(
        deduplication,
        DbDeduplication::Managed(ManagedDeduplication {
            mode: ProducerMode::Chained,
            key: "orders created".to_owned(),
        })
    );
}
