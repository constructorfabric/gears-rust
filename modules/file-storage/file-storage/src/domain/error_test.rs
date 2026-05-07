#[cfg(test)]
mod tests {
    use super::super::error::DomainError;
    use file_storage_sdk::FileStorageError;
    use modkit_db::DbError;

    #[test]
    fn helper_constructors_build_correct_variants() {
        assert!(matches!(
            DomainError::bad_request("x"),
            DomainError::BadRequest(s) if s == "x"
        ));
        assert!(matches!(
            DomainError::internal("x"),
            DomainError::Internal(s) if s == "x"
        ));
        assert!(matches!(
            DomainError::capability("x"),
            DomainError::CapabilityUnavailable(s) if s == "x"
        ));
        assert!(matches!(
            DomainError::backend("x"),
            DomainError::BackendFailure(s) if s == "x"
        ));
    }

    #[test]
    fn db_error_to_domain_error_maps_to_database() {
        // DbError::InvalidConfig is the simplest constructor.
        let db = DbError::InvalidConfig("connection failed".to_owned());
        let de: DomainError = db.into();
        assert!(
            matches!(&de, DomainError::Database(s) if s.contains("connection failed")),
            "DbError must map to Database variant, got: {de:?}"
        );
    }

    #[test]
    fn domain_error_display_messages_are_distinct() {
        // Smoke check that thiserror Display impls produce non-empty,
        // distinct strings — this guards against accidental empty templates.
        let cases = vec![
            DomainError::NotFound,
            DomainError::AccessDenied("denied".to_owned()),
            DomainError::BadRequest("bad".to_owned()),
            DomainError::EtagMismatch,
            DomainError::InvalidStatusTransition("nope".to_owned()),
            DomainError::CapabilityUnavailable("c".to_owned()),
            DomainError::PayloadTooLarge { max_bytes: 100 },
            DomainError::UploadExpired,
            DomainError::BackendFailure("b".to_owned()),
            DomainError::Internal("i".to_owned()),
            DomainError::Database("d".to_owned()),
        ];
        for e in cases {
            let s = e.to_string();
            assert!(!s.is_empty(), "Display must not be empty for {e:?}");
        }
    }

    // ── DomainError → FileStorageError mapping ──────────────────────────────

    #[test]
    fn map_not_found() {
        let err: FileStorageError = DomainError::NotFound.into();
        assert!(matches!(err, FileStorageError::NotFound), "got: {err:?}");
    }

    #[test]
    fn map_access_denied() {
        let err: FileStorageError = DomainError::AccessDenied("nope".to_owned()).into();
        assert!(matches!(err, FileStorageError::AccessDenied), "got: {err:?}");
    }

    #[test]
    fn map_bad_request_preserves_message() {
        let err: FileStorageError = DomainError::BadRequest("bad input".to_owned()).into();
        assert!(
            matches!(&err, FileStorageError::BadRequest(s) if s == "bad input"),
            "got: {err:?}"
        );
    }

    #[test]
    fn map_etag_mismatch() {
        let err: FileStorageError = DomainError::EtagMismatch.into();
        assert!(matches!(err, FileStorageError::EtagMismatch));
    }

    #[test]
    fn map_invalid_status_transition_preserves_message() {
        let err: FileStorageError =
            DomainError::InvalidStatusTransition("nope".to_owned()).into();
        assert!(
            matches!(&err, FileStorageError::InvalidStatusTransition(s) if s == "nope"),
            "got: {err:?}"
        );
    }

    #[test]
    fn map_capability_unavailable_preserves_message() {
        let err: FileStorageError =
            DomainError::CapabilityUnavailable("missing".to_owned()).into();
        assert!(
            matches!(&err, FileStorageError::CapabilityUnavailable(s) if s == "missing"),
            "got: {err:?}"
        );
    }

    #[test]
    fn map_payload_too_large_preserves_max_bytes() {
        let err: FileStorageError = DomainError::PayloadTooLarge { max_bytes: 1024 }.into();
        assert!(
            matches!(err, FileStorageError::PayloadTooLarge { max_bytes: 1024 }),
            "got: {err:?}"
        );
    }

    #[test]
    fn map_upload_expired() {
        let err: FileStorageError = DomainError::UploadExpired.into();
        assert!(matches!(err, FileStorageError::UploadExpired));
    }

    #[test]
    fn map_backend_failure_preserves_message() {
        let err: FileStorageError = DomainError::BackendFailure("oops".to_owned()).into();
        assert!(
            matches!(&err, FileStorageError::BackendFailure(s) if s == "oops"),
            "got: {err:?}"
        );
    }

    #[test]
    fn map_internal_collapses_to_internal() {
        let err: FileStorageError = DomainError::Internal("boom".to_owned()).into();
        assert!(matches!(err, FileStorageError::Internal), "got: {err:?}");
    }

    #[test]
    fn map_database_collapses_to_internal() {
        // Database errors must not leak as a separate variant; they must
        // collapse to Internal for the public SDK surface.
        let err: FileStorageError = DomainError::Database("db down".to_owned()).into();
        assert!(matches!(err, FileStorageError::Internal), "got: {err:?}");
    }

    // ── EnforcerError → DomainError mapping ─────────────────────────────────

    #[test]
    fn enforcer_error_denied_maps_to_access_denied() {
        let err: DomainError =
            authz_resolver_sdk::EnforcerError::Denied { deny_reason: None }.into();
        assert!(
            matches!(&err, DomainError::AccessDenied(s) if !s.is_empty()),
            "got: {err:?}"
        );
    }

    #[test]
    fn enforcer_error_compile_failed_maps_to_access_denied() {
        // Constraint compilation failures must map to AccessDenied (fail-closed),
        // not to Internal — otherwise an authz-config bug surfaces as 500
        // instead of 403.
        let err: DomainError = authz_resolver_sdk::EnforcerError::CompileFailed(
            authz_resolver_sdk::pep::compiler::ConstraintCompileError::ConstraintsRequiredButAbsent,
        )
        .into();
        assert!(
            matches!(&err, DomainError::AccessDenied(s) if s.contains("constraint")),
            "got: {err:?}"
        );
    }

    #[test]
    fn enforcer_error_evaluation_failed_maps_to_internal() {
        // PDP RPC failures are infrastructure faults, not authz denials.
        let err: DomainError = authz_resolver_sdk::EnforcerError::EvaluationFailed(
            authz_resolver_sdk::AuthZResolverError::NoPluginAvailable,
        )
        .into();
        assert!(
            matches!(&err, DomainError::Internal(s) if s.contains("evaluation")),
            "got: {err:?}"
        );
    }

    // ── ScopeError → DomainError mapping ────────────────────────────────────

    #[test]
    fn scope_error_denied_maps_to_access_denied() {
        let err: DomainError =
            modkit_db::secure::ScopeError::Denied("not yours").into();
        assert!(
            matches!(&err, DomainError::AccessDenied(s) if s.contains("not yours")),
            "got: {err:?}"
        );
    }

    #[test]
    fn scope_error_invalid_maps_to_internal_with_prefix() {
        let err: DomainError =
            modkit_db::secure::ScopeError::Invalid("missing tenant").into();
        assert!(
            matches!(&err, DomainError::Internal(s)
                if s.contains("scope invalid") && s.contains("missing tenant")),
            "got: {err:?}"
        );
    }

    #[test]
    fn scope_error_db_maps_to_internal_with_prefix() {
        let err: DomainError =
            modkit_db::secure::ScopeError::Db(sea_orm::DbErr::Custom("conn lost".to_owned()))
                .into();
        assert!(
            matches!(&err, DomainError::Internal(s)
                if s.contains("database error") && s.contains("conn lost")),
            "got: {err:?}"
        );
    }

    #[test]
    fn scope_error_tenant_not_in_scope_maps_to_access_denied() {
        let tenant_id = uuid::Uuid::nil();
        let err: DomainError =
            modkit_db::secure::ScopeError::TenantNotInScope { tenant_id }.into();
        assert!(
            matches!(&err, DomainError::AccessDenied(s)
                if s.contains("not in scope") && s.contains(&tenant_id.to_string())),
            "got: {err:?}"
        );
    }
}
