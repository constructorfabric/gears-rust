//! Domain services.
//!
//! Each module exposes a service struct that orchestrates repositories,
//! plugins, and SDK primitives. Services depend on traits (object-safe
//! `Arc<dyn …>`) so unit tests can inject mocks; concrete wiring happens in
//! `module.rs` (Phase 15).
//
// @cpt-cf-chat-engine-domain-service-root:p3

pub mod export_service;
pub mod intelligence_service;
pub mod message_service;
pub mod plugin_service;
pub mod reaction_service;
pub mod search_service;
pub mod session_service;
pub mod variant_service;
pub mod webhook;

pub use export_service::{ExportService, ShareUrlBuilder, is_share_token_expired};
pub use intelligence_service::{
    DEFAULT_SUMMARY_BUFFER_SIZE, DEFAULT_SUMMARY_DEADLINE, IntelligenceService,
    RetentionCleanupReport, SessionCleanupOutcome, SummaryStream,
    resolve_effective_policy, retention_policy_label, validate_retention_policy,
};
pub use message_service::{
    DEFAULT_PLUGIN_DEADLINE, DEFAULT_STREAMING_BUFFER_SIZE, MessageEventKind, MessageService,
    SendMessageRequest, SendMessageStream,
};
pub use plugin_service::PluginService;
pub use reaction_service::{
    CAPABILITY_FEEDBACK, ReactionMutation, ReactionService, ReactionsListing,
    SetReactionResponse,
};
pub use search_service::{
    BackendHit, InMemorySearchBackend, ParsedQuery, SearchBackend, SearchScope,
    SearchScopeFilter, SearchService, parse_search_query,
};
pub use session_service::{
    CreateSessionRequest, DEFAULT_PLUGIN_CALL_TIMEOUT, Identity, PaginatedSessions,
    RegisterSessionTypeRequest, SessionDeleteOutcome, SessionService, redact_session,
    reject_reserved_metadata,
};
pub use variant_service::{
    DEFAULT_SWITCH_TYPE_DEADLINE, VariantEntry, VariantListing, VariantRepo, VariantService,
};
pub use webhook::{NoopWebhookEmitter, WebhookEmitter, WebhookEvent};
