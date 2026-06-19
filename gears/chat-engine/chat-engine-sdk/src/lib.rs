#![allow(clippy::module_name_repetitions)]
#![allow(clippy::struct_field_names)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::unnested_or_patterns)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::trivially_copy_pass_by_ref)]
#![allow(clippy::ref_option)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::missing_fields_in_debug)]

pub mod error;
pub mod models;
pub mod plugin;

pub use error::PluginError;
pub use models::{
    Capability, CapabilityValue, FileCitation, HealthStatus, LifecycleState, LinkCitation,
    LinkReference, MemoryStrategy, Message, MessagePart, MessagePartInput, MessagePartType,
    MessageRole, RetentionPolicy, Session, SessionType, StreamingChunkEvent, StreamingCompleteEvent,
    StreamingErrorEvent, StreamingEvent, StreamingStartEvent, TenantId, TextPositionAnchor, UserId,
    VariantInfo,
};
pub use plugin::{
    ChatEngineBackendPlugin, MessagePluginCtx, PluginCallContext, PluginStream,
    SessionPluginCtx, SessionPluginResponse, empty_stream, stream_from_events,
};
