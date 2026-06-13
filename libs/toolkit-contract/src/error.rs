/// Errors that can occur during contract support operations.
#[derive(Debug, thiserror::Error)]
pub enum ContractError {
    /// Transport-level error during generated remote dispatch.
    #[error("transport error: {0}")]
    Transport(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// IR validation error.
    #[error("validation error: {0}")]
    Validation(String),
}
