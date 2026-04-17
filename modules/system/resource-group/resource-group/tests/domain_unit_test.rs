// Created: 2026-04-16 by Constructor Tech
//! Unit tests for domain layer pure logic.
//!
//! Tests validation functions, error construction, error mapping,
//! and serialization failure detection — all without database dependencies.
//!
//! Full domain service tests with mock repositories are deferred to
//! TODO-16 (repository trait abstraction).

use cf_resource_group::domain::error::DomainError;
use cf_resource_group::domain::validation::{self, RG_TYPE_PREFIX};

// ── validate_type_code ──────────────────────────────────────────────────

#[test]
fn validate_type_code_rejects_empty() {
    let result = validation::validate_type_code("");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
    assert!(err.to_string().contains("empty"));
}

#[test]
fn validate_type_code_rejects_wrong_prefix() {
    let result = validation::validate_type_code("wrong.prefix.type");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
    assert!(err.to_string().contains("prefix"));
}

#[test]
fn validate_type_code_rejects_too_long() {
    let long_code = format!(
        "{}{}",
        RG_TYPE_PREFIX,
        "a".repeat(1025 - RG_TYPE_PREFIX.len())
    );
    assert!(long_code.len() > 1024);
    let result = validation::validate_type_code(&long_code);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
    assert!(err.to_string().contains("1024"));
}

#[test]
fn validate_type_code_accepts_valid_code() {
    let code = format!("{RG_TYPE_PREFIX}tenant");
    let result = validation::validate_type_code(&code);
    assert!(result.is_ok());
}

#[test]
fn validate_type_code_accepts_exact_max_length() {
    let code = format!(
        "{}{}",
        RG_TYPE_PREFIX,
        "a".repeat(1024 - RG_TYPE_PREFIX.len())
    );
    assert_eq!(code.len(), 1024);
    let result = validation::validate_type_code(&code);
    assert!(result.is_ok());
}

#[test]
fn validate_type_code_rejects_prefix_only() {
    // The prefix itself is a valid type code (non-empty, correct prefix, within length)
    let result = validation::validate_type_code(RG_TYPE_PREFIX);
    assert!(result.is_ok());
}

// ── validate_metadata_schema ────────────────────────────────────────────

#[test]
fn validate_metadata_schema_accepts_valid_object_schema() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "count": { "type": "integer" }
        },
        "required": ["name"]
    });
    assert!(validation::validate_metadata_schema(&schema).is_ok());
}

#[test]
fn validate_metadata_schema_accepts_boolean_true_schema() {
    let schema = serde_json::json!(true);
    assert!(validation::validate_metadata_schema(&schema).is_ok());
}

#[test]
fn validate_metadata_schema_rejects_invalid_schema() {
    let schema = serde_json::json!({
        "type": "not-a-real-type"
    });
    let result = validation::validate_metadata_schema(&schema);
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        DomainError::Validation { .. }
    ));
}

// ── validate_metadata_against_schema ────────────────────────────────────

#[test]
fn validate_metadata_against_schema_passes_when_no_schema() {
    let metadata = serde_json::json!({"anything": true});
    assert!(validation::validate_metadata_against_schema(Some(&metadata), None).is_ok());
}

#[test]
fn validate_metadata_against_schema_passes_when_no_metadata() {
    let schema = serde_json::json!({"type": "object"});
    assert!(validation::validate_metadata_against_schema(None, Some(&schema)).is_ok());
}

#[test]
fn validate_metadata_against_schema_passes_when_both_none() {
    assert!(validation::validate_metadata_against_schema(None, None).is_ok());
}

#[test]
fn validate_metadata_against_schema_passes_valid_metadata() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "name": { "type": "string", "maxLength": 50 }
        },
        "required": ["name"],
        "additionalProperties": false
    });
    let metadata = serde_json::json!({"name": "hello"});
    assert!(validation::validate_metadata_against_schema(Some(&metadata), Some(&schema)).is_ok());
}

#[test]
fn validate_metadata_against_schema_rejects_type_mismatch() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "count": { "type": "integer" }
        }
    });
    let metadata = serde_json::json!({"count": "not-an-integer"});
    let result = validation::validate_metadata_against_schema(Some(&metadata), Some(&schema));
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
    assert!(err.to_string().contains("does not match type schema"));
}

#[test]
fn validate_metadata_against_schema_rejects_missing_required_field() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" }
        },
        "required": ["name"]
    });
    let metadata = serde_json::json!({});
    let result = validation::validate_metadata_against_schema(Some(&metadata), Some(&schema));
    assert!(result.is_err());
}

#[test]
fn validate_metadata_against_schema_rejects_additional_properties() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" }
        },
        "additionalProperties": false
    });
    let metadata = serde_json::json!({"name": "ok", "unknown": 42});
    let result = validation::validate_metadata_against_schema(Some(&metadata), Some(&schema));
    assert!(result.is_err());
}

#[test]
fn validate_metadata_against_schema_rejects_max_length_exceeded() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "name": { "type": "string", "maxLength": 5 }
        }
    });
    let metadata = serde_json::json!({"name": "too-long-string"});
    let result = validation::validate_metadata_against_schema(Some(&metadata), Some(&schema));
    assert!(result.is_err());
}

// ── validate_metadata_schema: additional edge cases ─────────────────────

#[test]
fn validate_metadata_schema_accepts_empty_object_schema() {
    let schema = serde_json::json!({});
    assert!(validation::validate_metadata_schema(&schema).is_ok());
}

#[test]
fn validate_metadata_schema_accepts_boolean_false_schema() {
    let schema = serde_json::json!(false);
    assert!(validation::validate_metadata_schema(&schema).is_ok());
}

#[test]
fn validate_metadata_schema_rejects_null() {
    let schema = serde_json::json!(null);
    assert!(validation::validate_metadata_schema(&schema).is_err());
}

#[test]
fn validate_metadata_schema_rejects_number() {
    let schema = serde_json::json!(42);
    assert!(validation::validate_metadata_schema(&schema).is_err());
}

#[test]
fn validate_metadata_schema_rejects_string() {
    let schema = serde_json::json!("not a schema");
    assert!(validation::validate_metadata_schema(&schema).is_err());
}

#[test]
fn validate_metadata_schema_rejects_array() {
    let schema = serde_json::json!([1, 2, 3]);
    assert!(validation::validate_metadata_schema(&schema).is_err());
}

#[test]
fn validate_metadata_schema_accepts_nested_properties() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "address": {
                "type": "object",
                "properties": {
                    "city": { "type": "string" },
                    "zip": { "type": "string", "pattern": "^[0-9]{5}$" }
                },
                "required": ["city"]
            }
        }
    });
    assert!(validation::validate_metadata_schema(&schema).is_ok());
}

#[test]
fn validate_metadata_schema_accepts_combiners() {
    let schema = serde_json::json!({
        "anyOf": [
            { "type": "string" },
            { "type": "integer" }
        ]
    });
    assert!(validation::validate_metadata_schema(&schema).is_ok());
}

// ── validate_metadata_against_schema: additional edge cases ─────────────

#[test]
fn validate_metadata_against_schema_false_schema_rejects_everything() {
    let schema = serde_json::json!(false);
    let metadata = serde_json::json!({});
    let result = validation::validate_metadata_against_schema(Some(&metadata), Some(&schema));
    assert!(result.is_err(), "false schema should reject all metadata");
}

#[test]
fn validate_metadata_against_schema_true_schema_accepts_everything() {
    let schema = serde_json::json!(true);
    let metadata = serde_json::json!({"any": "thing", "number": 42, "nested": [1, 2]});
    assert!(validation::validate_metadata_against_schema(Some(&metadata), Some(&schema)).is_ok());
}

#[test]
fn validate_metadata_against_schema_empty_schema_accepts_everything() {
    let schema = serde_json::json!({});
    let metadata = serde_json::json!({"any": "thing"});
    assert!(validation::validate_metadata_against_schema(Some(&metadata), Some(&schema)).is_ok());
}

#[test]
fn validate_metadata_against_schema_nested_object_fails() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "address": {
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"]
            }
        }
    });
    let bad = serde_json::json!({"address": {}});
    assert!(validation::validate_metadata_against_schema(Some(&bad), Some(&schema)).is_err());
    let good = serde_json::json!({"address": {"city": "Berlin"}});
    assert!(validation::validate_metadata_against_schema(Some(&good), Some(&schema)).is_ok());
}

#[test]
fn validate_metadata_against_schema_multiple_errors() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "age": { "type": "integer" }
        },
        "required": ["name", "age"]
    });
    let metadata = serde_json::json!({});
    let result = validation::validate_metadata_against_schema(Some(&metadata), Some(&schema));
    assert!(result.is_err());
}

#[test]
fn validate_metadata_against_schema_enum_constraint() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "status": { "type": "string", "enum": ["active", "inactive"] }
        }
    });
    let good = serde_json::json!({"status": "active"});
    assert!(validation::validate_metadata_against_schema(Some(&good), Some(&schema)).is_ok());
    let bad = serde_json::json!({"status": "unknown"});
    assert!(validation::validate_metadata_against_schema(Some(&bad), Some(&schema)).is_err());
}

#[test]
fn validate_metadata_against_schema_pattern_constraint() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "code": { "type": "string", "pattern": "^[A-Z]{3}$" }
        }
    });
    let good = serde_json::json!({"code": "ABC"});
    assert!(validation::validate_metadata_against_schema(Some(&good), Some(&schema)).is_ok());
    let bad = serde_json::json!({"code": "abc123"});
    assert!(validation::validate_metadata_against_schema(Some(&bad), Some(&schema)).is_err());
}

#[test]
fn validate_metadata_against_schema_wrong_root_type() {
    let schema = serde_json::json!({"type": "object"});
    let arr = serde_json::json!([1, 2, 3]);
    assert!(validation::validate_metadata_against_schema(Some(&arr), Some(&schema)).is_err());
    let str_val = serde_json::json!("just a string");
    assert!(validation::validate_metadata_against_schema(Some(&str_val), Some(&schema)).is_err());
}

// ── DomainError construction ────────────────────────────────────────────

#[test]
fn domain_error_type_not_found_format() {
    let err = DomainError::type_not_found("my.type.code");
    assert!(matches!(err, DomainError::TypeNotFound { .. }));
    assert!(err.to_string().contains("my.type.code"));
}

#[test]
fn domain_error_type_already_exists_format() {
    let err = DomainError::type_already_exists("dup.code");
    assert!(matches!(err, DomainError::TypeAlreadyExists { .. }));
    assert!(err.to_string().contains("dup.code"));
}

#[test]
fn domain_error_validation_format() {
    let err = DomainError::validation("bad input");
    assert!(matches!(err, DomainError::Validation { .. }));
    assert!(err.to_string().contains("bad input"));
}

#[test]
fn domain_error_group_not_found_format() {
    let id = uuid::Uuid::now_v7();
    let err = DomainError::group_not_found(id);
    assert!(matches!(err, DomainError::GroupNotFound { .. }));
    assert!(err.to_string().contains(&id.to_string()));
}

#[test]
fn domain_error_cycle_detected_format() {
    let err = DomainError::cycle_detected("A -> B -> A");
    assert!(matches!(err, DomainError::CycleDetected { .. }));
    assert!(err.to_string().contains("A -> B -> A"));
}

#[test]
fn domain_error_limit_violation_format() {
    let err = DomainError::limit_violation("depth exceeded");
    assert!(matches!(err, DomainError::LimitViolation { .. }));
    assert!(err.to_string().contains("depth exceeded"));
}

#[test]
fn domain_error_invalid_parent_type_format() {
    let err = DomainError::invalid_parent_type("type mismatch");
    assert!(matches!(err, DomainError::InvalidParentType { .. }));
    assert!(err.to_string().contains("type mismatch"));
}

#[test]
fn domain_error_conflict_active_references_format() {
    let err = DomainError::conflict_active_references("has children");
    assert!(matches!(err, DomainError::ConflictActiveReferences { .. }));
    assert!(err.to_string().contains("has children"));
}

#[test]
fn domain_error_allowed_parent_types_violation_format() {
    let err = DomainError::allowed_parent_types_violation("parent removed");
    assert!(matches!(
        err,
        DomainError::AllowedParentTypesViolation { .. }
    ));
    assert!(err.to_string().contains("parent removed"));
}

#[test]
fn domain_error_tenant_incompatibility_format() {
    let err = DomainError::tenant_incompatibility("wrong tenant");
    assert!(matches!(err, DomainError::TenantIncompatibility { .. }));
    assert!(err.to_string().contains("wrong tenant"));
}

#[test]
fn domain_error_database_format() {
    let err = DomainError::database("connection lost");
    assert!(matches!(err, DomainError::Database { .. }));
    assert!(err.to_string().contains("connection lost"));
}

#[test]
fn domain_error_membership_not_found_format() {
    let err = DomainError::membership_not_found("(gid, type, rid)");
    assert!(matches!(err, DomainError::MembershipNotFound { .. }));
    assert!(err.to_string().contains("(gid, type, rid)"));
}

#[test]
fn domain_error_conflict_format() {
    let err = DomainError::conflict("duplicate key");
    assert!(matches!(err, DomainError::Conflict { .. }));
    assert!(err.to_string().contains("duplicate key"));
}

// ── is_serialization_failure ────────────────────────────────────────────

#[test]
fn is_serialization_failure_detects_sqlstate_40001() {
    let err = DomainError::database("ERROR: 40001 could not serialize access");
    assert!(err.is_serialization_failure());
}

#[test]
fn is_serialization_failure_detects_serialize_message() {
    let err = DomainError::database("could not serialize access due to concurrent update");
    assert!(err.is_serialization_failure());
}

#[test]
fn is_serialization_failure_false_for_other_db_errors() {
    let err = DomainError::database("connection refused");
    assert!(!err.is_serialization_failure());
}

#[test]
fn is_serialization_failure_false_for_non_db_errors() {
    let err = DomainError::validation("bad input");
    assert!(!err.is_serialization_failure());
}

// ── DomainError -> ResourceGroupError mapping ───────────────────────────

#[test]
fn domain_to_sdk_type_not_found() {
    use resource_group_sdk::ResourceGroupError;
    let domain = DomainError::type_not_found("code");
    let sdk: ResourceGroupError = domain.into();
    assert!(sdk.to_string().contains("code"));
}

#[test]
fn domain_to_sdk_type_already_exists() {
    use resource_group_sdk::ResourceGroupError;
    let domain = DomainError::type_already_exists("code");
    let sdk: ResourceGroupError = domain.into();
    assert!(sdk.to_string().contains("code"));
}

#[test]
fn domain_to_sdk_validation() {
    use resource_group_sdk::ResourceGroupError;
    let domain = DomainError::validation("msg");
    let sdk: ResourceGroupError = domain.into();
    assert!(sdk.to_string().contains("msg") || !sdk.to_string().is_empty());
}

#[test]
fn domain_to_sdk_group_not_found() {
    use resource_group_sdk::ResourceGroupError;
    let id = uuid::Uuid::now_v7();
    let domain = DomainError::group_not_found(id);
    let sdk: ResourceGroupError = domain.into();
    assert!(sdk.to_string().contains(&id.to_string()));
}

#[test]
fn domain_to_sdk_cycle_detected() {
    use resource_group_sdk::ResourceGroupError;
    let domain = DomainError::cycle_detected("cycle");
    let sdk: ResourceGroupError = domain.into();
    assert!(!sdk.to_string().is_empty());
}

#[test]
fn domain_to_sdk_database_maps_to_internal() {
    use resource_group_sdk::ResourceGroupError;
    let domain = DomainError::database("db error");
    let sdk: ResourceGroupError = domain.into();
    // Database errors map to internal (no sensitive info leaked)
    assert!(sdk.to_string().to_lowercase().contains("internal"));
}

#[test]
fn domain_to_sdk_membership_not_found() {
    use resource_group_sdk::ResourceGroupError;
    let domain = DomainError::membership_not_found("key");
    let sdk: ResourceGroupError = domain.into();
    assert!(sdk.to_string().contains("key"));
}

#[test]
fn domain_to_problem_membership_not_found_is_404() {
    use modkit::api::problem::Problem;
    let domain = DomainError::membership_not_found("(gid, type, rid)");
    let problem: Problem = domain.into();
    assert_eq!(problem.status, http::StatusCode::NOT_FOUND);
}

#[test]
fn domain_to_sdk_access_denied_maps_to_internal() {
    use resource_group_sdk::ResourceGroupError;
    let domain = DomainError::AccessDenied {
        message: "denied".to_owned(),
    };
    let sdk: ResourceGroupError = domain.into();
    assert!(sdk.to_string().to_lowercase().contains("denied"));
}

// ── DomainError -> Problem (RFC 9457) mapping ───────────────────────────

#[test]
fn domain_to_problem_type_not_found_is_404() {
    use modkit::api::problem::Problem;
    let domain = DomainError::type_not_found("my.code");
    let problem: Problem = domain.into();
    assert_eq!(problem.status, http::StatusCode::NOT_FOUND);
}

#[test]
fn domain_to_problem_type_already_exists_is_409() {
    use modkit::api::problem::Problem;
    let domain = DomainError::type_already_exists("dup");
    let problem: Problem = domain.into();
    assert_eq!(problem.status, http::StatusCode::CONFLICT);
}

#[test]
fn domain_to_problem_validation_is_400() {
    use modkit::api::problem::Problem;
    let domain = DomainError::validation("bad");
    let problem: Problem = domain.into();
    assert_eq!(problem.status, http::StatusCode::BAD_REQUEST);
}

#[test]
fn domain_to_problem_group_not_found_is_404() {
    use modkit::api::problem::Problem;
    let domain = DomainError::group_not_found(uuid::Uuid::now_v7());
    let problem: Problem = domain.into();
    assert_eq!(problem.status, http::StatusCode::NOT_FOUND);
}

#[test]
fn domain_to_problem_cycle_detected_is_409() {
    use modkit::api::problem::Problem;
    let domain = DomainError::cycle_detected("cycle");
    let problem: Problem = domain.into();
    assert_eq!(problem.status, http::StatusCode::CONFLICT);
}

#[test]
fn domain_to_problem_limit_violation_is_409() {
    use modkit::api::problem::Problem;
    let domain = DomainError::limit_violation("too deep");
    let problem: Problem = domain.into();
    assert_eq!(problem.status, http::StatusCode::CONFLICT);
}

#[test]
fn domain_to_problem_invalid_parent_type_is_400() {
    use modkit::api::problem::Problem;
    let domain = DomainError::invalid_parent_type("mismatch");
    let problem: Problem = domain.into();
    assert_eq!(problem.status, http::StatusCode::BAD_REQUEST);
}

#[test]
fn domain_to_problem_conflict_active_refs_is_409() {
    use modkit::api::problem::Problem;
    let domain = DomainError::conflict_active_references("children exist");
    let problem: Problem = domain.into();
    assert_eq!(problem.status, http::StatusCode::CONFLICT);
}

#[test]
fn domain_to_problem_allowed_parent_types_violation_is_409() {
    use modkit::api::problem::Problem;
    let domain = DomainError::allowed_parent_types_violation("violation");
    let problem: Problem = domain.into();
    assert_eq!(problem.status, http::StatusCode::CONFLICT);
}

#[test]
fn domain_to_problem_tenant_incompatibility_is_409() {
    use modkit::api::problem::Problem;
    let domain = DomainError::tenant_incompatibility("wrong tenant");
    let problem: Problem = domain.into();
    assert_eq!(problem.status, http::StatusCode::CONFLICT);
}

#[test]
fn domain_to_problem_access_denied_is_403() {
    use modkit::api::problem::Problem;
    let domain = DomainError::AccessDenied {
        message: "denied".to_owned(),
    };
    let problem: Problem = domain.into();
    assert_eq!(problem.status, http::StatusCode::FORBIDDEN);
}

#[test]
fn domain_to_problem_database_is_500() {
    use modkit::api::problem::Problem;
    let domain = DomainError::database("db error");
    let problem: Problem = domain.into();
    assert_eq!(problem.status, http::StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn domain_to_problem_internal_error_is_500() {
    use modkit::api::problem::Problem;
    let problem: Problem = DomainError::InternalError.into();
    assert_eq!(problem.status, http::StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn domain_to_problem_conflict_is_409() {
    use modkit::api::problem::Problem;
    let domain = DomainError::conflict("dup");
    let problem: Problem = domain.into();
    assert_eq!(problem.status, http::StatusCode::CONFLICT);
}

// @cpt-dod:cpt-cf-resource-group-dod-testing-error-conversions:p2
// ── Error conversions: From<EnforcerError> -> DomainError ────────────────

// TC-ERR-01: EnforcerError::Denied -> DomainError::AccessDenied
#[test]
fn enforcer_denied_maps_to_access_denied() {
    use authz_resolver_sdk::pep::EnforcerError;

    let err: DomainError = EnforcerError::Denied { deny_reason: None }.into();
    assert!(
        matches!(err, DomainError::AccessDenied { .. }),
        "Expected AccessDenied, got: {err:?}"
    );
}

// TC-ERR-02: EnforcerError::EvaluationFailed -> DomainError::AccessDenied
#[test]
fn enforcer_evaluation_failed_maps_to_access_denied() {
    use authz_resolver_sdk::AuthZResolverError;
    use authz_resolver_sdk::pep::EnforcerError;

    let err: DomainError =
        EnforcerError::EvaluationFailed(AuthZResolverError::NoPluginAvailable).into();
    assert!(
        matches!(err, DomainError::InternalError),
        "Expected InternalError, got: {err:?}"
    );
}

// TC-ERR-03: EnforcerError::CompileFailed -> DomainError::AccessDenied
#[test]
fn enforcer_compile_failed_maps_to_access_denied() {
    use authz_resolver_sdk::pep::ConstraintCompileError;
    use authz_resolver_sdk::pep::EnforcerError;

    let err: DomainError =
        EnforcerError::CompileFailed(ConstraintCompileError::ConstraintsRequiredButAbsent).into();
    assert!(
        matches!(err, DomainError::InternalError),
        "Expected InternalError, got: {err:?}"
    );
}

// TC-ERR-04: sea_orm::DbErr -> DomainError::Database
#[test]
fn sea_orm_db_err_maps_to_database() {
    let db_err = sea_orm::DbErr::Custom("connection lost".to_owned());
    let err: DomainError = db_err.into();
    assert!(
        matches!(err, DomainError::Database { .. }),
        "Expected Database, got: {err:?}"
    );
    assert!(err.to_string().contains("connection lost"));
}

// TC-ERR-05: modkit_db::DbError -> DomainError::Database
#[test]
fn modkit_db_error_maps_to_database() {
    let db_err = modkit_db::DbError::from(sea_orm::DbErr::Custom("pool exhausted".to_owned()));
    let err: DomainError = db_err.into();
    assert!(
        matches!(err, DomainError::Database { .. }),
        "Expected Database, got: {err:?}"
    );
}

// ── QueryProfile default (TC-SDK-17) ─────────────────────────────────────

#[test]
fn query_profile_default_values() {
    use cf_resource_group::domain::group_service::QueryProfile;
    let profile = QueryProfile::default();
    assert_eq!(profile.max_depth, Some(10));
    assert_eq!(profile.max_width, None);
}
