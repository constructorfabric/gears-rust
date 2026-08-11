//! Infrastructure layer: plugin resolution, the local client, the concrete
//! `ClientHub`-backed RMS adapter registry, and boundary mappings to
//! outward-facing representations.

pub mod local_client;
pub mod plugin_select;
pub mod rms_registry;
pub mod sdk_error_mapping;
