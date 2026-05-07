#[cfg(test)]
mod tests {
    use super::super::error::{ProblemWrapper, file_storage_error_to_problem};
    use crate::domain::error::DomainError;
    use axum::http::StatusCode;
    use file_storage_sdk::FileStorageError;
    use modkit::api::problem::Problem;

    #[test]
    fn not_found_maps_to_404_with_code_not_found() {
        let p = file_storage_error_to_problem(&FileStorageError::NotFound, "/file/x");
        assert_eq!(p.status, StatusCode::NOT_FOUND);
        assert_eq!(p.code, "not_found");
        assert_eq!(p.instance, "/file/x");
        assert!(!p.detail.is_empty());
    }

    #[test]
    fn access_denied_maps_to_403() {
        let p = file_storage_error_to_problem(&FileStorageError::AccessDenied, "/x");
        assert_eq!(p.status, StatusCode::FORBIDDEN);
        assert_eq!(p.code, "access_denied");
    }

    #[test]
    fn bad_request_maps_to_400() {
        let p = file_storage_error_to_problem(
            &FileStorageError::BadRequest("missing field".to_owned()),
            "/x",
        );
        assert_eq!(p.status, StatusCode::BAD_REQUEST);
        assert_eq!(p.code, "bad_request");
        assert!(p.detail.contains("missing field"));
    }

    #[test]
    fn etag_mismatch_maps_to_412() {
        let p = file_storage_error_to_problem(&FileStorageError::EtagMismatch, "/x");
        assert_eq!(p.status, StatusCode::PRECONDITION_FAILED);
        assert_eq!(p.code, "etag_mismatch");
    }

    #[test]
    fn invalid_status_transition_maps_to_409() {
        let p = file_storage_error_to_problem(
            &FileStorageError::InvalidStatusTransition("bad".to_owned()),
            "/x",
        );
        assert_eq!(p.status, StatusCode::CONFLICT);
        assert_eq!(p.code, "invalid_status_transition");
    }

    #[test]
    fn capability_unavailable_maps_to_409() {
        let p = file_storage_error_to_problem(
            &FileStorageError::CapabilityUnavailable("nope".to_owned()),
            "/x",
        );
        assert_eq!(p.status, StatusCode::CONFLICT);
        assert_eq!(p.code, "capability_unavailable");
    }

    #[test]
    fn payload_too_large_maps_to_413() {
        let p = file_storage_error_to_problem(
            &FileStorageError::PayloadTooLarge { max_bytes: 1024 },
            "/x",
        );
        assert_eq!(p.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(p.code, "payload_too_large");
        assert!(p.detail.contains("1024"));
    }

    #[test]
    fn upload_expired_maps_to_410() {
        let p = file_storage_error_to_problem(&FileStorageError::UploadExpired, "/x");
        assert_eq!(p.status, StatusCode::GONE);
        assert_eq!(p.code, "upload_expired");
    }

    #[test]
    fn backend_failure_maps_to_502() {
        let p = file_storage_error_to_problem(
            &FileStorageError::BackendFailure("oops".to_owned()),
            "/x",
        );
        assert_eq!(p.status, StatusCode::BAD_GATEWAY);
        assert_eq!(p.code, "backend_failure");
    }

    #[test]
    fn internal_maps_to_500() {
        let p = file_storage_error_to_problem(&FileStorageError::Internal, "/x");
        assert_eq!(p.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(p.code, "internal");
    }

    #[test]
    fn instance_is_propagated_into_problem() {
        let p =
            file_storage_error_to_problem(&FileStorageError::NotFound, "/file-storage/v1/files/abc");
        assert_eq!(p.instance, "/file-storage/v1/files/abc");
    }

    // ── ProblemWrapper: From<FileStorageError> and From<DomainError> ────────

    #[test]
    fn problem_wrapper_from_file_storage_error() {
        let w: ProblemWrapper = FileStorageError::NotFound.into();
        let p: Problem = w.into();
        assert_eq!(p.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn problem_wrapper_from_domain_error() {
        let w: ProblemWrapper = DomainError::NotFound.into();
        let p: Problem = w.into();
        assert_eq!(p.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn problem_wrapper_passes_through_etag_mismatch() {
        let w: ProblemWrapper = DomainError::EtagMismatch.into();
        let p: Problem = w.into();
        assert_eq!(p.status, StatusCode::PRECONDITION_FAILED);
    }
}
