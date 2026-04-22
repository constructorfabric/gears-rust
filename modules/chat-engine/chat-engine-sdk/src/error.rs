use thiserror::Error;

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("transient error: {0}")]
    Transient(String),
    #[error("permanent error: {0}")]
    Permanent(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("internal error: {0}")]
    Internal(String),
}
