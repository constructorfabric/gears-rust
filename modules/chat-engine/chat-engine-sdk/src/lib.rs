#![allow(clippy::module_name_repetitions)]
#![allow(clippy::struct_field_names)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::unnested_or_patterns)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::trivially_copy_pass_by_ref)]
#![allow(clippy::ref_option)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]

pub mod error;
pub mod models;
pub mod plugin;

pub use error::PluginError;
pub use models::{
    Capability, CapabilityValue, HealthStatus, LifecycleState, MemoryStrategy, Message,
    MessageRole, RetentionPolicy, Session, SessionType, StreamingChunkEvent,
    StreamingCompleteEvent, StreamingErrorEvent, StreamingEvent, StreamingStartEvent, TenantId,
    UserId, VariantInfo,
};
pub use plugin::{
    ChatEngineBackendPlugin, MessagePluginCtx, PluginCallContext, PluginStream,
    SessionPluginCtx, empty_stream, stream_from_events,
};
