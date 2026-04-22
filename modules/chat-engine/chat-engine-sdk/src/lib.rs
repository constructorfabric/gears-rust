#![allow(clippy::module_name_repetitions)]
#![allow(clippy::struct_field_names)]
#![allow(clippy::struct_excessive_bools)]

pub mod error;
pub mod models;
pub mod plugin;

pub use error::PluginError;
pub use models::{
    Capability, CapabilityValue, HealthStatus, MemoryStrategy, Message, MessageRole,
    RetentionPolicy, Session, SessionType, StreamingChunkEvent, StreamingCompleteEvent,
    StreamingErrorEvent, StreamingEvent, StreamingStartEvent, VariantInfo,
};
pub use plugin::{
    ChatEngineBackendPlugin, MessagePluginCtx, PluginCallContext, SessionPluginCtx,
};
