#![doc = include_str!("../README.md")]

pub mod activate;
pub mod cleanup;
pub mod cli;
pub mod copy;
mod db;
pub mod error;
pub mod fence;
pub mod legacy;
pub mod results;

pub use activate::{ActivateReport, activate};
pub use cleanup::{CleanupReport, cleanup};
pub use cli::{run_cli, run_cli_from};
pub use copy::{CopyReport, RowRef, TypeDivergence, copy};
pub use error::{LegacyStoreError, MigrationError};
pub use legacy::LegacyValueStore;
pub use results::{Entry, Outcome};

/// Whether a stage changes anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Read-only: report what [`Mode::Apply`] would do. Writes nothing to the
    /// new store, the database, the legacy store or the results file.
    DryRun,
    /// Perform the stage.
    Apply,
}

/// Fixed identity of the migration tool, used as the subject and tenant of
/// the [`toolkit_security::SecurityContext`] handed to the new store. Plugins
/// use the context for correlation only.
const TOOL_ID: uuid::Uuid = uuid::Uuid::from_u128(0x6d69_6772_6174_696f_6e2d_746f_6f6c_0001);

pub(crate) fn tool_context() -> Result<toolkit_security::SecurityContext, MigrationError> {
    toolkit_security::SecurityContext::builder()
        .subject_id(TOOL_ID)
        .subject_tenant_id(TOOL_ID)
        .subject_type("service")
        .build()
        .map_err(|e| {
            MigrationError::Target(credstore_sdk::CredStoreError::internal(format!(
                "cannot build the migration security context: {e}"
            )))
        })
}
