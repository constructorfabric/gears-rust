//! Recognition persistence errors retain business meaning through the posting boundary.
use crate::domain::{error::DomainError, model::RepoError};

/// Preserve exact numeric errors and corruption; refine only the recognition cap.
pub(super) fn map_recognition_repo_err(error: RepoError) -> DomainError {
    match error {
        RepoError::MoneyOutCapExceeded(detail) => DomainError::OverRecognition(detail),
        other => crate::infra::posting::error_transport::repo_to_domain(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recognition_errors_keep_categories_through_transport() {
        assert!(matches!(
            map_recognition_repo_err(RepoError::Conflict("stale".into())),
            DomainError::ConcurrentModification(_)
        ));
        assert!(matches!(
            map_recognition_repo_err(RepoError::MoneyOutCapExceeded("cap".into())),
            DomainError::OverRecognition(_)
        ));
        assert!(matches!(
            map_recognition_repo_err(RepoError::InvalidStoredMoney("database is locked".into())),
            DomainError::Internal(_)
        ));
        assert!(matches!(
            map_recognition_repo_err(RepoError::RecognitionPolicyConflict("phase".into())),
            DomainError::RecognitionPolicyConflict(_)
        ));
        assert!(matches!(
            map_recognition_repo_err(RepoError::Db("connection reset".into())),
            DomainError::Internal(_)
        ));
        assert!(matches!(
            map_recognition_repo_err(RepoError::Money(bss_ledger_sdk::MoneyError::ScaleMismatch)),
            DomainError::InconsistentScale(_)
        ));
    }
}
