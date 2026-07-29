#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Regression tests for typed derived schemas under abstract envelope bases.
//!
//! An `x-gts-abstract` envelope base (the account-management
//! `tenant_metadata.v1~` pattern: no payload properties of its own, traits in
//! `x-gts-traits-schema`, payload shape owned by derived schemas) must allow
//! derived schemas to declare typed payload properties when the envelope
//! explicitly opens its payload with `additionalProperties: true`. Without
//! the explicit opt-in, the OP#12 chain-narrowing check treats the envelope
//! as a closed empty object and rejects every typed derived schema, which
//! made typed tenant metadata unregistrable end-to-end.

mod common;

use common::create_service;
use serde_json::json;

#[test]
fn typed_derived_schema_registers_under_open_abstract_envelope() {
    let service = create_service();

    // Mirrors gts.cf.core.am.tenant_metadata.v1~ with the payload explicitly
    // opened (additionalProperties: true).
    let envelope = json!({
        "$id": "gts://gts.x.test.am.meta_envelope.v1~",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "description": "Abstract metadata envelope; derived schemas own the payload shape.",
        "type": "object",
        "additionalProperties": true,
        "x-gts-abstract": true,
        "x-gts-traits-schema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "inheritance_policy": {
                    "type": "string",
                    "enum": ["override_only", "inherit"],
                    "default": "override_only"
                }
            }
        }
    });

    // A typed derived schema — the exact shape adopters need for validated
    // per-tenant settings payloads (AM PRD §5.7).
    let derived = json!({
        "$id": "gts://gts.x.test.am.meta_envelope.v1~x.test.am.settings.v1~",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "description": "Typed settings payload derived from the envelope.",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "automation_level": {
                "type": "string",
                "enum": ["manual", "recommendations", "autonomous"]
            },
            "approved_worker_categories": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "x-gts-traits": { "inheritance_policy": "override_only" }
    });

    let results = service.register(vec![envelope, derived]);
    for result in &results {
        assert!(
            matches!(result, types_registry_sdk::RegisterResult::Ok { .. }),
            "registration must succeed for an open envelope chain, got: {result:?}",
        );
    }

    service
        .switch_to_ready()
        .expect("typed derived schema under an open abstract envelope must survive ready-mode validation");
}
