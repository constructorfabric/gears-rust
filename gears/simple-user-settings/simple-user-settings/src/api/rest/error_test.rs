#[cfg(test)]
mod tests {
    use crate::domain::error::DomainError;
    use toolkit_canonical_errors::{CanonicalError, Problem};
    use toolkit_gts::gts_id;

    const SETTINGS_RESOURCE_TYPE: &str = gts_id!("cf.simple_user_settings.settings.user.v1~");

    /// Build the wire `Problem` the canonical error middleware would emit
    /// for a given `DomainError`. Tests run without the middleware in
    /// scope, so `instance` / `trace_id` stay `None` here — that injection
    /// is exercised by integration tests that drive the full router.
    fn wire(err: DomainError) -> Problem {
        Problem::from(CanonicalError::from(err))
    }

    #[test]
    fn test_not_found_error_to_problem() {
        let problem = wire(DomainError::NotFound);

        assert_eq!(problem.status, 404);
        assert!(problem.instance.is_none());
        assert!(problem.detail.contains("Settings not found"));
        assert_eq!(
            problem
                .context
                .get("resource_type")
                .and_then(|v| v.as_str()),
            Some(SETTINGS_RESOURCE_TYPE),
        );
    }

    #[test]
    fn test_validation_error_to_problem() {
        let problem = wire(DomainError::Validation {
            field: "theme".to_owned(),
            message: "exceeds max length".to_owned(),
        });

        // InvalidArgument is 400 in canonical (decision C).
        assert_eq!(problem.status, 400);
        assert!(problem.instance.is_none());

        // Caller-supplied field + message live in context.field_violations[0].
        let violation = problem
            .context
            .get("field_violations")
            .and_then(|v| v.get(0))
            .expect("expected at least one field violation");
        assert_eq!(
            violation.get("field").and_then(|v| v.as_str()),
            Some("theme")
        );
        assert_eq!(
            violation.get("description").and_then(|v| v.as_str()),
            Some("exceeds max length"),
        );
        assert_eq!(
            violation.get("reason").and_then(|v| v.as_str()),
            Some("VALIDATION_ERROR"),
        );
    }

    #[test]
    fn test_database_arm_maps_to_500() {
        let problem = wire(DomainError::Database(toolkit_db::DbError::InvalidConfig(
            "connection failed".to_owned(),
        )));

        assert_eq!(problem.status, 500);
        assert!(problem.instance.is_none());
    }

    #[test]
    fn test_forbidden_arm_masks_as_not_found() {
        // Pin the disclosure-prevention contract: `Forbidden` must surface
        // as 404 with no leak of the original forbidden message, otherwise
        // the response would tell the caller that the resource exists.
        let raw = "user 42 lacks scope settings:write";
        let problem = wire(DomainError::Forbidden(raw.to_owned()));

        assert_eq!(problem.status, 404);
        assert!(!problem.detail.contains("scope settings:write"));
        assert!(!problem.detail.contains("user 42"));
        assert_eq!(
            problem
                .context
                .get("resource_type")
                .and_then(|v| v.as_str()),
            Some(SETTINGS_RESOURCE_TYPE),
        );
    }

    #[test]
    fn test_internal_arm_masks_raw_message() {
        // The canonical internal mapping replaces caller-supplied diagnostic
        // text with an opaque public detail; assert the raw msg never reaches
        // the wire.
        let problem = wire(DomainError::Internal("db pool exhausted".to_owned()));

        assert_eq!(problem.status, 500);
        assert!(!problem.detail.contains("db pool exhausted"));
    }
}
