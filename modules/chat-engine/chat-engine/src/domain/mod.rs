//! Domain primitives for the `chat_engine` crate.
//!
//! This module is the canonical home for every type the service layer
//! works with. It re-exports the SDK models verbatim (so plugin authors
//! and service code share one type) and adds service-local primitives
//! that don't belong in the SDK (e.g., `ChatEngineError`, `ShareToken`,
//! reserved-metadata helpers).
//!
//! Framework-neutrality: this module MUST NOT import `axum`, `tower`,
//! `http`, or any SeaORM connection / query DSL type. It MAY read SeaORM
//! `Model` and `ActiveModel` types from `infra::db::entity` to provide
//! conversion impls — see ADR-0001 and the Phase 1 contract for the DB
//! boundary contract.
//
// @cpt-cf-chat-engine-domain-root:p2

pub mod context;
pub mod error;
pub mod export;
pub mod llm_config;
pub mod memory_strategy;
pub mod message;
pub mod reaction;
pub mod retention;
pub mod search;
pub mod service;
pub mod session;

// ---- Re-exports of SDK types that have no per-type sub-module ----
pub use chat_engine_sdk::models::{Capability, CapabilityValue, HealthStatus, TenantId, UserId};

// ---- Service-local types ----
pub use error::{ChatEngineError, Result};
// `ShareToken` is the redaction-shell newtype defined in
// `domain::export`. The earlier Phase 2 struct in `domain::share` was a
// bearer secret that derived Serialize/Deserialize — meaning any future
// caller that serialised the struct (response body, structured log,
// `tracing` field) would leak the raw token, defeating the manual
// Debug redaction. That struct had zero production callers and has
// been deleted; this `pub use` now points at the safe newtype so the
// canonical `chat_engine::domain::ShareToken` path is no longer a
// loaded gun.
pub use export::{
    ExportFormat, ExportStorage, ExportedSession, MessageView, ShareToken, ShareTokenIssue,
    SharedSessionView, StorageError, StubExportStorage, generate_share_token,
};
pub use memory_strategy::{MemoryStrategy, default_memory_strategy};
pub use message::{
    Message, MessageRole, StreamingChunkEvent, StreamingCompleteEvent, StreamingErrorEvent,
    StreamingEvent, StreamingStartEvent, VariantInfo,
};
pub use reaction::{MessageReaction, MessageReactionEvent, ReactionType};
pub use retention::RetentionPolicy;
pub use search::{
    Cursor, MessageRef, SearchError, SearchPage, SearchQuery, SearchResult, SessionMeta,
    escape_like_pattern, extract_searchable_text, make_snippet, sanitize_for_tsquery,
    DEFAULT_CONTEXT_RADIUS, DEFAULT_PAGE_SIZE as SEARCH_DEFAULT_PAGE_SIZE,
    MAX_PAGE_SIZE as SEARCH_MAX_PAGE_SIZE, MAX_QUERY_LENGTH,
};
pub use session::{
    LifecycleState, METADATA_KEY_MEMORY_STRATEGY, METADATA_KEY_RETENTION_POLICY,
    METADATA_KEY_SHARE_EXPIRES_AT, RESERVED_METADATA_KEYS, Session, SessionType,
    ensure_can_transition, get_memory_strategy, get_retention_policy, get_share_expires_at,
    public_metadata, set_memory_strategy, set_retention_policy, set_share_expires_at,
};
