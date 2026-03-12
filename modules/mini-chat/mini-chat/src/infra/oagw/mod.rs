pub mod files_client;
pub mod vector_store_client;

use crate::domain::error::DomainError;

/// Reject path segments that could cause path traversal or injection.
pub(crate) fn validate_path_segment(value: &str, name: &str) -> Result<(), DomainError> {
    if value.is_empty()
        || value.contains('/')
        || value.contains('\\')
        || value.contains("..")
        || value.contains('%')
        || value.contains('?')
        || value.contains('#')
    {
        return Err(DomainError::Internal {
            message: format!("invalid {name}: {value:?}"),
        });
    }
    Ok(())
}
