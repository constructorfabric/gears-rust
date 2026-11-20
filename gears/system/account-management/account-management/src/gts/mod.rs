//! Account-management's GTS declarations and configured schema templates.
//!
//! Permissions use the crate-local inventory. The root tenant-type template
//! only generates a schema; the startup manifest publishes its configured ID
//! and traits instead of adding an unconditional inventory declaration.

mod permissions;
mod rg_types;
pub mod root_type;
