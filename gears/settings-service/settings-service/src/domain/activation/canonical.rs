// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-canonical:p1
//! The expected value's encoding: RFC 8785 canonical JSON, and the salted hash
//! of a `secret`-trait value.
//!
//! The encoder lives in the SDK (`settings_service_sdk::canonical`): the one
//! implementation both sides of a report run — this service when a change
//! commits, the consumer's SDK for its `applied_value` — so a report is compared
//! with the expectation byte for byte. This module is the service's name for it.
//! The conformance corpus is the SDK's, re-run here against the value a change
//! actually stores (`canonical_tests.rs`).

pub use settings_service_sdk::canonical::{
    EncodingError as CanonicalError, ExpectedValue, SECRET_HASH_DOMAIN, canonical_json,
    plaintext_text, secret_hash,
};

#[cfg(test)]
#[path = "canonical_tests.rs"]
mod canonical_tests;
