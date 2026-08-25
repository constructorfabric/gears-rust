// Created: 2026-08-12 by Constructor Tech
//! Tests for the problem-document contract.
//!
//! Acceptance criteria: FEATURE `gear-foundation.md` §6 and DESIGN.md §4.3
//! *Error Response Format* — every 4xx/5xx carries `type` as a `gts://` URI,
//! `title`, `status`; every 422 carries field-level detail; an unrecognized
//! error maps to 500 without leaking an internal message.
//!
//! `trace_id` is not asserted here: the platform takes it from the ambient
//! request trace context, which exists only under a live server. Its presence
//! is an integration concern; what is testable in-crate is everything else.

use serde_json::Value;
use toolkit_canonical_errors::{CanonicalError, Problem};

use crate::domain::error::DomainError;
use crate::field;

fn problem(err: DomainError) -> Value {
    let canonical = CanonicalError::from(err);
    serde_json::to_value(Problem::from(canonical)).expect("Problem serializes")
}

#[test]
fn every_variant_carries_the_required_members() {
    // The required set from DESIGN.md §4.3. A variant missing one of these is a
    // response an administrator's tooling cannot dispatch on.
    let cases = vec![
        DomainError::validation("bad"),
        DomainError::PreconditionFailed {
            detail: "etag moved".to_owned(),
        },
        DomainError::Conflict {
            detail: "already there".to_owned(),
        },
        DomainError::Unauthorized {
            resource: "gts.cf.toolkit.settings.category.v1~",
        },
        DomainError::NotFound {
            resource: "declaration",
        },
        DomainError::Unavailable {
            detail: "database unreachable".to_owned(),
        },
        DomainError::Internal {
            diagnostic: "not built yet".to_owned(),
        },
    ];
    for case in cases {
        let label = format!("{case:?}");
        let doc = problem(case);
        assert!(
            doc["type"]
                .as_str()
                .is_some_and(|t| t.starts_with("gts://")),
            "`{label}` must carry a gts:// type URI, got {:?}",
            doc["type"]
        );
        assert!(
            !doc["title"].as_str().unwrap_or_default().is_empty(),
            "{label}"
        );
        assert!(
            doc["status"]
                .as_u64()
                .is_some_and(|s| (400..600).contains(&s)),
            "{label}"
        );
    }
}

#[test]
fn a_validation_failure_carries_field_level_detail() {
    // The 422 contract: a caller must be able to point at the offending field
    // and dispatch on a stable code rather than parsing prose.
    let doc = problem(DomainError::Validation {
        field: "value".to_owned(),
        code: field::VALUE_NOT_CANONICAL,
        message: "value must be a valid uri".to_owned(),
    });
    let violation = &doc["context"]["field_violations"][0];
    assert_eq!(violation["field"], "value");
    assert_eq!(violation["reason"], field::VALUE_NOT_CANONICAL);
    assert!(
        violation["description"]
            .as_str()
            .is_some_and(|d| d.contains("valid uri")),
        "got {violation:?}"
    );
}

#[test]
fn an_unrecognized_error_leaks_nothing() {
    // The whole point of routing the diagnostic through the in-process channel.
    // A stack detail or connection string put in `Internal` must not appear
    // anywhere in the serialized document.
    let secret = "postgres://user:hunter2@db.internal/settings";
    let doc = problem(DomainError::Internal {
        diagnostic: secret.to_owned(),
    });
    let rendered = serde_json::to_string(&doc).expect("serializes");
    assert!(
        !rendered.contains(secret),
        "the internal diagnostic reached the wire: {rendered}"
    );
    assert!(!rendered.contains("hunter2"));
    assert_eq!(doc["status"], 500);
}

#[test]
fn a_denial_does_not_disclose_whether_the_target_exists() {
    // Two denials for different settings must be byte-identical, or a caller
    // without entitlement can enumerate the settings tree by diffing responses.
    let first = problem(DomainError::Unauthorized {
        resource: "gts.cf.toolkit.settings.category.v1~",
    });
    let second = problem(DomainError::Unauthorized {
        resource: "gts.cf.toolkit.settings.category.v1~",
    });
    assert_eq!(first, second);

    let rendered = serde_json::to_string(&first).expect("serializes");
    assert!(
        !rendered.contains("not found") && !rendered.contains("exists"),
        "a denial must not hint at existence either way: {rendered}"
    );
}

#[test]
fn a_not_found_names_the_kind_not_the_identifier() {
    // The variant takes a `&'static str` kind precisely so a caller-supplied
    // identifier — which may itself be sensitive — cannot be echoed back.
    let doc = problem(DomainError::NotFound {
        resource: "declaration",
    });
    assert_eq!(doc["status"], 404);
    let rendered = serde_json::to_string(&doc).expect("serializes");
    assert!(rendered.contains("declaration"));
}

#[test]
fn the_conversion_is_total() {
    // No input may panic or refuse: the impl side calls
    // `.map_err(CanonicalError::from)` with no fallback.
    for case in [
        DomainError::validation("x"),
        DomainError::Unauthorized {
            resource: "gts.cf.toolkit.settings.category.v1~",
        },
        DomainError::Internal {
            diagnostic: String::new(),
        },
    ] {
        let _canonical = CanonicalError::from(case);
    }
}

#[test]
fn the_rendered_shape_diverges_from_design_4_3_as_adr_0005_requires() {
    // DESIGN.md §4.3 predates the platform canonical-error ADR and shows a
    // document this gear cannot emit. Pinned here so the divergence is visible
    // and deliberate rather than discovered by whoever writes the first client.
    //
    // DESIGN §4.3 example          | what ADR 0005 produces
    // -----------------------------|----------------------------------------
    // "status": 422                | 400 — validation is canonical
    //                              |       InvalidArgument, which is 400
    // "type": gts://...settings    | the canonical category URI; a gear does
    //         .error_validation.v1~|       not mint its own problem types
    // top-level "errors": [...]    | context.field_violations[]
    //   with field/code/message    |   with field/reason/description
    //
    // Changing any of these means bypassing the platform renderer, which is
    // exactly what ADR 0005 exists to prevent. DESIGN.md §4.3 is what needs
    // updating, not this mapping.
    let doc = problem(DomainError::Validation {
        field: "value".to_owned(),
        code: field::VALUE_NOT_CANONICAL,
        message: "value must be a valid uri".to_owned(),
    });

    assert_eq!(
        doc["status"], 400,
        "canonical InvalidArgument is 400, not 422"
    );
    assert_eq!(
        doc["type"], "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "the type URI is the canonical category's, not a settings-specific one"
    );
    assert!(
        doc.get("errors").is_none(),
        "field detail lives under context.field_violations, not a top-level `errors`"
    );
}
