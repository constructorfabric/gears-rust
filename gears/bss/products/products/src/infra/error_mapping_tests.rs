//! Retained domain error mapping census.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use crate::domain::{error::DomainError, validation::ValidationReport};
use std::collections::HashSet;
use toolkit::api::canonical_prelude::CanonicalError;

fn code_of(err: &CanonicalError) -> Option<&str> {
    match err {
        CanonicalError::Aborted { ctx, .. } => Some(ctx.reason.as_str()),
        CanonicalError::PermissionDenied { ctx, .. } => Some(ctx.reason.as_str()),
        CanonicalError::FailedPrecondition { ctx, .. } => {
            ctx.violations.first().map(|v| v.type_.as_str())
        }
        _ => None,
    }
}

fn declared_status_and_code(err: &DomainError) -> (u16, Option<&'static str>) {
    match err {
        DomainError::StaleUnit { .. }
        | DomainError::Validation(_)
        | DomainError::UsageTypeUnresolved(_)
        | DomainError::UnrecognizedUnit(_)
        | DomainError::MeterDeclarationIncomplete(_) => (400, Some(err.code())),
        DomainError::Conflict { .. }
        | DomainError::StaleRevision { .. }
        | DomainError::IdempotencyConflict(_)
        | DomainError::IdempotencyKeyInFlight(_) => (409, Some(err.code())),
        DomainError::Forbidden { .. } | DomainError::UsageTypeForbidden(_) => {
            (403, Some(err.code()))
        }
        DomainError::NotFound { .. } => (404, None),
        DomainError::Approval(r) => match r.code {
            "SOD_VIOLATION" | "NOT_SUBMITTER" => (403, Some(r.code)),
            "NOTE_REQUIRED" | "NOTE_TOO_LONG" | "VALIDATION" | "GENERATION_MISMATCH" => {
                (400, Some(r.code))
            }
            "DB" | "STORE" => (500, None),
            _ => (409, Some(r.code)),
        },
        DomainError::AuditUnavailable(_)
        | DomainError::UsageTypeUnavailable(_)
        | DomainError::UsageUnavailable(_) => (503, None),
    }
}

fn one_of_every_variant() -> Vec<DomainError> {
    let mut report = ValidationReport::new();
    report.violate("VALIDATION", "field", "invalid");
    vec![
        DomainError::Validation(report),
        DomainError::Conflict {
            code: "SKU_TYPE_FROZEN",
            detail: "references".into(),
        },
        DomainError::Forbidden {
            code: "NOT_SUBMITTER",
            detail: "hidden".into(),
        },
        DomainError::NotFound {
            what: "sku",
            id: uuid::Uuid::new_v4(),
        },
        DomainError::Approval(crate::domain::error::ApprovalRefusal {
            code: "DUPLICATE_VOTE",
            detail: "vote".into(),
        }),
        DomainError::StaleUnit { generation: 2 },
        DomainError::StaleRevision {
            expected: 1,
            found: 2,
        },
        DomainError::IdempotencyConflict("detail".to_owned()),
        DomainError::IdempotencyKeyInFlight("detail".to_owned()),
        DomainError::AuditUnavailable("detail".to_owned()),
        DomainError::UsageTypeUnresolved("detail".to_owned()),
        DomainError::UsageTypeUnavailable("detail".to_owned()),
        DomainError::UsageTypeForbidden("detail".to_owned()),
        DomainError::UsageUnavailable("detail".to_owned()),
        DomainError::UnrecognizedUnit("detail".to_owned()),
        DomainError::MeterDeclarationIncomplete("detail".to_owned()),
    ]
}
const DOMAIN_ERROR_VARIANTS: usize = 16;

#[test]
fn every_domain_error_variant_lands_in_its_declared_category() {
    let roster = one_of_every_variant();

    assert_eq!(
        roster.len(),
        DOMAIN_ERROR_VARIANTS,
        "the roster must carry one value of every variant; a variant added to `DomainError` and \
         to `declared_status_and_code` but not to the roster is a variant the ladder is not \
         checked on"
    );
    let distinct: HashSet<_> = roster.iter().map(std::mem::discriminant).collect();
    assert_eq!(
        distinct.len(),
        DOMAIN_ERROR_VARIANTS,
        "and one value **each**: a duplicate would satisfy the count while leaving a variant out"
    );

    for err in roster {
        let (expected_status, expected_code) = declared_status_and_code(&err);
        let wire_code = err.code();
        let name = format!("{err:?}");
        let canonical = CanonicalError::from(err);

        assert_eq!(
            canonical.status_code(),
            expected_status,
            "the ladder must answer {expected_status} for {name}"
        );
        assert_eq!(
            code_of(&canonical),
            expected_code,
            "the ladder must carry {expected_code:?} for {name}"
        );
        if let Some(code) = expected_code {
            // The wire code and `DomainError::code()` are one string; the
            // assertion holds the pair together so a second literal cannot
            // drift in unnoticed.
            assert_eq!(
                code, wire_code,
                "the ladder's own code for {name} must be `DomainError::code()`'s, not a \
                 second literal"
            );
        }
    }
}

#[test]
fn approval_refusals_preserve_their_status_code_and_generation() {
    use crate::domain::error::ApprovalRefusal;
    for (code, status) in [
        ("SOD_VIOLATION", 403),
        ("NOT_SUBMITTER", 403),
        ("NOTE_REQUIRED", 400),
        ("NOTE_TOO_LONG", 400),
        ("VALIDATION", 400),
        ("GENERATION_MISMATCH", 400),
        ("DB", 500),
        ("STORE", 500),
        ("UNIT_ALREADY_DECIDED", 409),
        ("DUPLICATE_VOTE", 409),
        ("UNIT_CONTENDED", 409),
        ("ROW_LOCKED_PENDING", 409),
        ("APPLY_REFUSED", 409),
    ] {
        let err = CanonicalError::from(DomainError::Approval(ApprovalRefusal {
            code,
            detail: "generation 7".into(),
        }));
        assert_eq!(err.status_code(), status, "{code}");
        assert_eq!(code_of(&err), if status == 500 { None } else { Some(code) });
    }
    let err = CanonicalError::from(DomainError::StaleUnit { generation: 7 });
    assert!(
        matches!(err, CanonicalError::FailedPrecondition {ctx,..} if ctx.violations[0].description.contains('7'))
    );
}

#[test]
fn actual_approval_errors_keep_custom_codes_fields_and_details() {
    use bss_approval::ApprovalError as A;
    let invalid = CanonicalError::from(DomainError::from(A::InvalidSubmit {
        code: "USAGE_NEEDS_METER",
        field: "unit".into(),
        detail: "a meter is required".into(),
    }));
    assert_eq!(invalid.status_code(), 400);
    assert_eq!(code_of(&invalid), Some("USAGE_NEEDS_METER"));
    assert!(
        matches!(invalid,CanonicalError::FailedPrecondition {ctx,..} if ctx.violations[0].subject=="unit" && ctx.violations[0].description=="a meter is required")
    );
    let apply = CanonicalError::from(DomainError::from(A::ApplyRefused {
        code: "SKU_REFERENCED",
        detail: "one live reference".into(),
    }));
    assert_eq!(apply.status_code(), 409);
    assert_eq!(code_of(&apply), Some("SKU_REFERENCED"));
    for (error, status, code) in [
        (A::SodViolation, 403, Some("SOD_VIOLATION")),
        (A::NotSubmitter, 403, Some("NOT_SUBMITTER")),
        (A::AlreadyDecided, 409, Some("UNIT_ALREADY_DECIDED")),
        (A::DuplicateVote, 409, Some("DUPLICATE_VOTE")),
        (A::Contended, 409, Some("UNIT_CONTENDED")),
        (
            A::Locked {
                item_type: "sku".into(),
                item_id: uuid::Uuid::new_v4(),
            },
            409,
            Some("ROW_LOCKED_PENDING"),
        ),
        (A::NoteRequired, 400, Some("NOTE_REQUIRED")),
        (A::NoteTooLong, 400, Some("NOTE_TOO_LONG")),
        (A::Empty, 400, Some("VALIDATION")),
        (
            A::GenerationMismatch {
                seen: 1,
                current: 7,
            },
            400,
            Some("GENERATION_MISMATCH"),
        ),
        (A::Store("private detail".into()), 500, None),
        (
            A::Db(sea_orm::DbErr::Custom("private driver detail".into())),
            500,
            None,
        ),
    ] {
        let canonical = CanonicalError::from(DomainError::from(error));
        assert_eq!(canonical.status_code(), status);
        assert_eq!(code_of(&canonical), code);
        if code == Some("GENERATION_MISMATCH") {
            assert!(
                matches!(canonical,CanonicalError::FailedPrecondition {ctx,..} if ctx.violations[0].description.contains('7'))
            );
        }
    }
}

/// P-D-207: a publish report carrying the catalog's refusal of the caller answers 403
/// `USAGE_TYPE_FORBIDDEN`, as the door's own resolve does, never a 400 field fix.
#[test]
fn a_report_carrying_a_catalog_denial_is_403() {
    let mut report = ValidationReport::new();
    report.violate("USAGE_NEEDS_METER", "unit", "a usage SKU names its unit");
    report.violate("USAGE_TYPE_FORBIDDEN", "usage_type_ref", "refused");
    let canonical = CanonicalError::from(DomainError::Validation(report));
    assert_eq!(canonical.status_code(), 403);
    assert_eq!(code_of(&canonical), Some("USAGE_TYPE_FORBIDDEN"));
}

/// W1a left the engine's `UnitNotFound` on the `other` arm, a 409. It is the unit's 404, naming the
/// unit as the door's own pre-load does.
#[test]
fn an_engine_unit_not_found_is_the_units_404() {
    let unit_id = uuid::Uuid::new_v4();
    let canonical = CanonicalError::from(DomainError::from(
        bss_approval::ApprovalError::UnitNotFound { unit_id },
    ));
    assert_eq!(canonical.status_code(), 404);
    assert_eq!(
        canonical.resource_name(),
        Some(unit_id.to_string().as_str())
    );
    assert_eq!(
        canonical.resource_type(),
        Some(crate::authz::labels::APPROVAL_UNIT)
    );
}

/// RS-25: a refusal names the resource it refuses, one of the gear's registered authz labels,
/// never the unregistered `product.v1~` every refusal carried.
#[test]
fn a_refusal_names_the_resource_it_refuses() {
    use crate::authz::labels;
    use crate::domain::error::ApprovalRefusal;
    let id = uuid::Uuid::new_v4();
    for (err, expected) in [
        (DomainError::NotFound { what: "sku", id }, labels::SKU),
        (
            DomainError::NotFound {
                what: "category",
                id,
            },
            labels::CATEGORY,
        ),
        (
            DomainError::NotFound {
                what: "approval_unit",
                id,
            },
            labels::APPROVAL_UNIT,
        ),
        (
            DomainError::NotFound {
                what: "reference",
                id,
            },
            labels::SKU,
        ),
        (
            DomainError::Approval(ApprovalRefusal {
                code: "DUPLICATE_VOTE",
                detail: "vote".into(),
            }),
            labels::APPROVAL_UNIT,
        ),
        (
            DomainError::StaleUnit { generation: 2 },
            labels::APPROVAL_UNIT,
        ),
        (
            DomainError::Conflict {
                code: "SKU_CODE_TAKEN",
                detail: "taken".into(),
            },
            labels::SKU,
        ),
        (
            DomainError::Conflict {
                code: "DERIVED_CODE_TAKEN",
                detail: "taken".into(),
            },
            labels::DERIVED_USAGE_TYPE,
        ),
        (
            crate::domain::derived::declaration_invalid("too_few_inputs", "one input"),
            labels::DERIVED_USAGE_TYPE,
        ),
        // P-D-232: the binding's refusals refuse the SKU's write, at submit (a report) and at
        // apply (a conflict); only the derived doors' own codes name the derived usage type.
        (
            DomainError::Validation({
                let mut r = ValidationReport::new();
                r.violate("DERIVED_USAGE_TYPE_UNKNOWN", "usage_type_ref", "unknown");
                r
            }),
            labels::SKU,
        ),
        (
            DomainError::Validation({
                let mut r = ValidationReport::new();
                r.violate("DERIVED_UNIT_MISMATCH", "unit", "mismatch");
                r
            }),
            labels::SKU,
        ),
        (
            crate::domain::derived::metering_immutable("usage_type_ref"),
            labels::SKU,
        ),
        (
            DomainError::Conflict {
                code: "METERING_IMMUTABLE",
                detail: "pinned".into(),
            },
            labels::SKU,
        ),
    ] {
        let name = format!("{err:?}");
        let canonical = CanonicalError::from(err);
        assert_eq!(canonical.resource_type(), Some(expected), "{name}");
    }
}
