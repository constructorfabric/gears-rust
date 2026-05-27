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
pub mod share;

// ---- Re-exports of SDK types that have no per-type sub-module ----
pub use chat_engine_sdk::models::{Capability, CapabilityValue, HealthStatus, TenantId, UserId};

// ---- Service-local types ----
pub use error::{ChatEngineError, Result};
pub use export::{
    ExportFormat, ExportStorage, ExportedSession, MessageView, ShareTokenIssue, SharedSessionView,
    StorageError, StubExportStorage, generate_share_token,
};
// `domain::export::ShareToken` (redaction wrapper, Phase 10) is intentionally
// NOT re-exported at the crate root — the pre-existing
// `domain::share::ShareToken` (full token record, Phase 2) keeps the canonical
// `chat_engine::domain::ShareToken` path. Phase 10 callers reach the wrapper
// via the fully-qualified `domain::export::ShareToken` import.
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
pub use share::ShareToken;
