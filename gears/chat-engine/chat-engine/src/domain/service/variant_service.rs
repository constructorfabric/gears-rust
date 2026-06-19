//! Message variant & branching service.
//!
//! Phase 6 layers variant operations on top of the Phase 5 streaming
//! pipeline:
//!
//! 1. **Recreate** (`POST /messages/{id}/recreate`) — pre-allocate a fresh
//!    assistant sibling under the target's `parent_message_id`, then
//!    delegate the NDJSON streaming to
//!    [`MessageService::dispatch_to_plugin`] with
//!    [`MessageEventKind::Recreate`] (so the SDK routes the call through
//!    `on_message_recreate`, ADR-0013). After the stream resolves, the
//!    active path is recomputed.
//! 2. **Branch** (`POST /messages/{id}/branch`) — INSERT a fresh user
//!    message under the branch-point parent + a fresh assistant stub,
//!    then dispatch with [`MessageEventKind::New`] over a history
//!    truncated to the branch-point ancestry (ADR-0014).
//! 3. **Navigate** (`GET /messages/{id}/variants`) — list sibling
//!    messages sharing `parent_message_id`, each annotated with a
//!    [`VariantInfo`] envelope.
//! 4. **Set active** (`PATCH /sessions/{id}/active-variant`) — activate
//!    a specific sibling, deactivate the rest of the siblings, and
//!    cascade the active flag along the ancestor chain and *off* the
//!    descendants of the previously-active sibling.
//! 5. **Switch type** (`PATCH /sessions/{id}/type`) — validate plugin
//!    capability superset (ADR-0015), invoke `on_session_updated`, and
//!    persist the new `session_type_id` + refreshed
//!    `enabled_capabilities`.
//!
//! Concurrency: the recreate / branch flows allocate `variant_index` via
//! the Phase-1 `compute_next_variant_index` helper which retries up to 3 times
//! on `uq_messages_session_parent_variant` collisions and surfaces an
//! exhausted retry as
//! [`ChatEngineError::Conflict`] mapping to HTTP 409.
//!
//! Lifecycle gating: mutations require `session.lifecycle_state == active`;
//! variant *navigation* additionally accepts `archived`.
//
// @cpt-cf-chat-engine-variant-service:p6
// @cpt-cf-chat-engine-adr-message-variants:p6
// @cpt-cf-chat-engine-adr-variant-indexing:p6
// @cpt-cf-chat-engine-adr-message-recreation:p6
// @cpt-cf-chat-engine-adr-branching-strategy:p6
// @cpt-cf-chat-engine-adr-session-switching:p6

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use serde_json::{Value as JsonValue, json};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;
use tracing::{debug, info, instrument, warn};
use uuid::Uuid;

use chat_engine_sdk::models::{
    Capability, CapabilityValue, LifecycleState, MessagePartInput, TenantId, UserId, VariantInfo,
};
use chat_engine_sdk::plugin::{PluginCallContext, SessionPluginCtx};

use crate::domain::error::{ChatEngineError, Result};
use crate::domain::message::{Message, MessageRole, StreamingEvent};
use crate::domain::service::message_service::{
    MessageEventKind, MessageService, SendMessageStream,
};
use crate::domain::service::plugin_service::PluginService;
use crate::domain::service::session_service::Identity;
use crate::domain::session::Session;
use crate::infra::db::entity::session_type as session_type_entity;
use crate::infra::db::repo::message_repo::MessageRepo;
use crate::infra::db::repo::session_repo::SessionRepo;
use crate::infra::db::repo::session_type_repo::SessionTypeRepo;

/// Default plugin-call deadline applied to `on_session_updated` during
/// session-type switching. Mirrors the lifecycle-hook budget in
/// `SessionService::DEFAULT_PLUGIN_CALL_TIMEOUT`.
pub const DEFAULT_SWITCH_TYPE_DEADLINE: Duration = Duration::from_secs(10);

/// Combined list-variants result returned by [`VariantService::list_variants`].
#[domain_model]
#[derive(Debug, Clone)]
pub struct VariantListing {
    /// Sibling entries ordered by `variant_index ASC`.
    pub variants: Vec<VariantEntry>,
    /// Index in `variants` of the currently-active sibling (if any). The
    /// REST handler exposes this as `current_index` to make the JSON
    /// payload easier to render.
    pub current_index: Option<u32>,
}

/// One sibling row in the [`VariantListing`].
#[domain_model]
#[derive(Debug, Clone)]
pub struct VariantEntry {
    pub message: Message,
    pub info: VariantInfo,
}

// ============================================================================
//  Repository surface — Phase 6
// ============================================================================

/// Sibling-and-active-path repository surface used by [`VariantService`].
///
/// Kept as a trait so unit tests can drop in an in-memory mock without
/// standing up Postgres. The Sea-ORM impl
/// (`crate::infra::db::repo::variant_repo::SeaVariantRepo`) is the only
/// place that talks to the database directly.
#[async_trait]
pub trait VariantRepo: Send + Sync {
    /// Return every sibling sharing `parent_message_id` (NULL allowed)
    /// inside `session_id`, ordered by `variant_index ASC`.
    async fn list_siblings(
        &self,
        session_id: Uuid,
        parent_message_id: Option<Uuid>,
    ) -> Result<Vec<Message>>;

    /// INSERT a user message as child of `parent_message_id` with
    /// `variant_index = MAX+1` (uses `compute_next_variant_index`) AND its
    /// assistant stub (`variant_index = 0`, `is_active=true`,
    /// `is_complete=false`) inside a single SERIALIZABLE transaction.
    ///
    /// Mirrors [`MessageRepo::insert_user_and_assistant_stub`] for the
    /// branch path: either both rows commit or neither does, so the
    /// streaming pipeline never observes a user turn with no assistant
    /// child.
    ///
    /// `tenant_id` (denormalized owning tenant) is stamped on both rows;
    /// `user_id` (the branching author, from the JWT identity) is stamped on
    /// the user message only — the assistant stub has no human author.
    ///
    /// Returns `(user_message_id, user_variant_index, assistant_message_id)`.
    async fn insert_user_and_assistant_stub_for_branch(
        &self,
        session_id: Uuid,
        parent_message_id: Uuid,
        parts: Vec<MessagePartInput>,
        file_ids: Option<Vec<Uuid>>,
        tenant_id: Option<String>,
        user_id: Option<String>,
    ) -> Result<(Uuid, i32, Uuid)>;

    /// Walk the ancestor chain of `message_id` up to the root, returning
    /// `[message_id, ..., root]`.
    async fn ancestor_chain(&self, session_id: Uuid, message_id: Uuid) -> Result<Vec<Uuid>>;

    /// Collect every descendant of `message_id` (excluding itself) by
    /// recursive walk.
    async fn collect_descendants(&self, session_id: Uuid, message_id: Uuid) -> Result<Vec<Uuid>>;

    /// Apply the active-path mutation per the Update-Active-Path
    /// algorithm — `activate_ids` go `is_active=true`,
    /// `deactivate_ids` go `is_active=false`. Single transaction.
    async fn apply_active_flips(
        &self,
        session_id: Uuid,
        activate_ids: Vec<Uuid>,
        deactivate_ids: Vec<Uuid>,
    ) -> Result<()>;

    /// Persist the session-type swap atomically — single UPDATE that
    /// writes `session_type_id`, `enabled_capabilities`, and
    /// `updated_at`.
    async fn update_session_type(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
        new_session_type_id: Uuid,
        new_capabilities: JsonValue,
    ) -> Result<crate::infra::db::entity::session::Model>;
}

// ============================================================================
//  Service
// ============================================================================

/// Public service.
#[domain_model]
#[derive(Clone)]
pub struct VariantService {
    sessions: Arc<dyn SessionRepo>,
    session_types: Arc<dyn SessionTypeRepo>,
    messages: Arc<dyn MessageRepo>,
    variants: Arc<dyn VariantRepo>,
    plugins: PluginService,
    message_service: Arc<MessageService>,
    plugin_timeout: Duration,
}

impl VariantService {
    #[must_use]
    pub fn new(
        sessions: Arc<dyn SessionRepo>,
        session_types: Arc<dyn SessionTypeRepo>,
        messages: Arc<dyn MessageRepo>,
        variants: Arc<dyn VariantRepo>,
        plugins: PluginService,
        message_service: Arc<MessageService>,
    ) -> Self {
        Self {
            sessions,
            session_types,
            messages,
            variants,
            plugins,
            message_service,
            plugin_timeout: DEFAULT_SWITCH_TYPE_DEADLINE,
        }
    }

    /// Override the plugin-call deadline used for `on_session_updated`
    /// during session-type switching.
    #[must_use]
    pub fn with_plugin_timeout(mut self, timeout: Duration) -> Self {
        self.plugin_timeout = timeout;
        self
    }

    // Variant-index allocation is intentionally NOT exposed as a standalone
    // method: an index must be allocated and INSERTed within the same
    // SERIALIZABLE transaction (see `MessageRepo::insert_assistant_variant_stub`
    // and `VariantRepo::insert_user_and_assistant_stub_for_branch`), which run
    // the Phase-1 `compute_next_variant_index` retry loop. A free-standing
    // allocator would hand back an index that is immediately stale.

    // ------------------------------------------------------------------
    //  Variant navigation
    // ------------------------------------------------------------------

    /// `GET /sessions/{session_id}/messages/{message_id}/variants`.
    ///
    /// Returns every sibling sharing the same `parent_message_id` as
    /// `message_id`, plus an active-pointer convenience field.
    ///
    /// Lifecycle: accepts `{active, archived}` (navigation MAY happen
    /// against an archived session — only mutations are gated to
    /// `active`).
    #[instrument(
        skip(self, identity),
        fields(
            session_id = %session_id,
            message_id = %message_id,
            operation = "navigate",
        ),
    )]
    pub async fn list_variants(
        &self,
        identity: &Identity,
        session_id: Uuid,
        message_id: Uuid,
    ) -> Result<VariantListing> {
        let started = OffsetDateTime::now_utc();

        let session = self.load_session(identity, session_id).await?;
        self.gate_lifecycle_navigation(&session)?;

        let target = self
            .messages
            .find_message_in_session(session_id, message_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("message", message_id))?;

        let siblings = self
            .variants
            .list_siblings(session_id, target.parent_message_id)
            .await?;
        let total = u32::try_from(siblings.len()).unwrap_or(u32::MAX);

        let mut variants = Vec::with_capacity(siblings.len());
        let mut current_index: Option<u32> = None;
        for (idx, m) in siblings.into_iter().enumerate() {
            let is_active = m.is_active;
            let info = VariantInfo {
                message_id: m.message_id,
                variant_index: m.variant_index,
                total_variants: total,
                is_active,
            };
            if is_active {
                current_index = Some(u32::try_from(idx).unwrap_or(0));
            }
            variants.push(VariantEntry { message: m, info });
        }

        log_op_finished(started, "navigate", session_id, message_id, None);
        Ok(VariantListing {
            variants,
            current_index,
        })
    }

    // ------------------------------------------------------------------
    //  Active-variant selection
    // ------------------------------------------------------------------

    /// `PATCH /sessions/{id}/active-variant` (canonical) +
    /// `PUT /sessions/{s}/messages/{m}/variants/active` (compat).
    ///
    /// Activates the chosen sibling and runs
    /// [`VariantService::update_active_path`].
    #[instrument(
        skip(self, identity),
        fields(
            session_id = %session_id,
            message_id = %message_id,
            operation = "set_active",
        ),
    )]
    pub async fn set_active_variant(
        &self,
        identity: &Identity,
        session_id: Uuid,
        message_id: Uuid,
    ) -> Result<VariantEntry> {
        let started = OffsetDateTime::now_utc();

        let session = self.load_session(identity, session_id).await?;
        self.gate_lifecycle_mutation(&session)?;

        let target = self
            .messages
            .find_message_in_session(session_id, message_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("message", message_id))?;

        self.update_active_path(session_id, target.message_id)
            .await?;

        // Re-load to capture the freshly-applied `is_active=true`.
        let refreshed = self
            .messages
            .find_message_in_session(session_id, message_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("message", message_id))?;
        let total = u32::try_from(
            self.variants
                .list_siblings(session_id, refreshed.parent_message_id)
                .await?
                .len(),
        )
        .unwrap_or(u32::MAX);

        let info = VariantInfo {
            message_id: refreshed.message_id,
            variant_index: refreshed.variant_index,
            total_variants: total,
            is_active: refreshed.is_active,
        };
        log_op_finished(
            started,
            "set_active",
            session_id,
            message_id,
            Some(refreshed.variant_index),
        );
        increment_variant_creation_total("set_active");
        Ok(VariantEntry {
            message: refreshed,
            info,
        })
    }

    /// Compute and apply the active-path mutation for a newly-active
    /// `message_id`.
    ///
    /// Algorithm (feature spec — Update Active Path):
    ///   1. Walk ancestors from `message_id` to root → `chain`.
    ///   2. Activate every id in `chain`.
    ///   3. For each ancestor, deactivate sibling variants at the same
    ///      parent that are NOT in `chain`.
    ///   4. Deactivate descendants of those off-path siblings.
    pub async fn update_active_path(&self, session_id: Uuid, message_id: Uuid) -> Result<()> {
        let chain = self.variants.ancestor_chain(session_id, message_id).await?;
        if chain.is_empty() {
            return Err(ChatEngineError::not_found("message", message_id));
        }

        // chain[0] == message_id (the target leaf); subsequent entries
        // are its parents up to the root.
        let chain_set: std::collections::HashSet<Uuid> = chain.iter().copied().collect();
        let mut deactivate: Vec<Uuid> = Vec::new();

        // For every ancestor (including the target leaf), find siblings
        // sharing the same parent and mark off-path ones for
        // deactivation. Then collect descendants of each off-path
        // sibling.
        for ancestor_id in &chain {
            let ancestor = self
                .messages
                .find_message_in_session(session_id, *ancestor_id)
                .await?
                .ok_or_else(|| ChatEngineError::not_found("message", *ancestor_id))?;
            let siblings = self
                .variants
                .list_siblings(session_id, ancestor.parent_message_id)
                .await?;
            for sibling in siblings {
                if !chain_set.contains(&sibling.message_id) {
                    deactivate.push(sibling.message_id);
                    let descendants = self
                        .variants
                        .collect_descendants(session_id, sibling.message_id)
                        .await?;
                    deactivate.extend(descendants);
                }
            }
        }

        // De-duplicate (descendants of one off-path sibling may overlap
        // with the descendants collected through another ancestor).
        deactivate.sort();
        deactivate.dedup();
        // Never deactivate an id that is also on the active chain — the
        // SQL writes deactivation last, so any overlap would silently
        // flip is_active=false on a node we just activated. In a strict
        // tree this should be a no-op; we enforce it explicitly to stay
        // safe against malformed subtrees.
        deactivate.retain(|id| !chain_set.contains(id));

        self.variants
            .apply_active_flips(session_id, chain, deactivate)
            .await
    }

    // ------------------------------------------------------------------
    //  Recreate
    // ------------------------------------------------------------------

    /// `POST /sessions/{session_id}/messages/{message_id}/recreate`.
    ///
    /// Pre-allocates a new assistant variant sibling, then delegates the
    /// NDJSON stream to [`MessageService::dispatch_to_plugin`] with
    /// [`MessageEventKind::Recreate`]. The active path is recomputed
    /// after the stream resolves; the variant_info is appended to the
    /// `StreamingCompleteEvent.metadata` envelope so clients can update
    /// their navigation UI without a follow-up `GET /variants`.
    #[instrument(
        skip(self, identity, cancel),
        fields(
            session_id = %session_id,
            message_id = %message_id,
            operation = "recreate",
        ),
    )]
    pub async fn recreate_variant(
        &self,
        identity: &Identity,
        session_id: Uuid,
        message_id: Uuid,
        capabilities: Option<Vec<CapabilityValue>>,
        cancel: CancellationToken,
    ) -> Result<SendMessageStream> {
        let started = OffsetDateTime::now_utc();

        let session = self.load_session(identity, session_id).await?;
        self.gate_lifecycle_mutation(&session)?;

        // Validate target message: must be an assistant message in the
        // session (ADR-0013).
        let target = self
            .messages
            .find_message_in_session(session_id, message_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("message", message_id))?;
        if !matches!(target.role, MessageRole::Assistant) {
            return Err(ChatEngineError::bad_request(
                "recreate only applies to assistant messages",
            ));
        }
        let parent_message_id = target.parent_message_id.ok_or_else(|| {
            ChatEngineError::bad_request(
                "target assistant message has no parent \u{2014} cannot recreate",
            )
        })?;

        // Resolve session-type → plugin binding now so we can fail fast
        // before the SERIALIZABLE INSERT lands.
        let session_type_id = session.session_type_id.ok_or_else(|| {
            ChatEngineError::bad_request(
                "session has no session_type bound; recreate cannot be routed",
            )
        })?;
        let session_type = self
            .session_types
            .find_by_id(session_type_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("session_type", session_type_id))?;
        let plugin_instance_id = session_type.plugin_instance_id.clone().ok_or_else(|| {
            ChatEngineError::bad_request(
                "session_type has no plugin_instance_id; recreate cannot be routed",
            )
        })?;

        // Pre-persist the new assistant variant stub. The SERIALIZABLE
        // retry loop happens inside `prepare_recreate_stub` via the
        // Phase 1 `assign_variant_index` helper.
        let inserted = self
            .message_service
            .prepare_recreate_stub(
                session_id,
                parent_message_id,
                Some(identity.tenant_id.clone()),
            )
            .await
            .map_err(map_unique_violation_to_conflict)?;
        let new_message_id = inserted.assistant_message_id;
        let new_variant_index = inserted.user_variant_index;

        // Build history visible to the plugin from the explicit
        // ancestor chain of `parent_message_id` (the user message that
        // triggered the recreate). This sidesteps the global
        // `is_active` filter: at this point BOTH the old assistant
        // sibling (target) and the new stub are `is_active = true` —
        // the active-path swap doesn't happen until after the stream
        // closes — so a `fetch_active_history` call would see two
        // candidate descendants of the parent and the resulting path
        // would be ambiguous. Walking the parent's ancestry, the way
        // `build_branched_history` does for branch, gives a stable
        // history independent of the not-yet-resolved active-path
        // state. The helper already drops hidden / incomplete rows.
        let history = self
            .build_branched_history(session_id, parent_message_id)
            .await?;

        // Total siblings BEFORE the active-path swap (used to build the
        // VariantInfo appended to the StreamingCompleteEvent metadata).
        let siblings_now = self
            .variants
            .list_siblings(session_id, Some(parent_message_id))
            .await?;
        let total_variants = u32::try_from(siblings_now.len()).unwrap_or(u32::MAX);
        let new_variant_info = VariantInfo {
            message_id: new_message_id,
            variant_index: u32::try_from(new_variant_index).unwrap_or(0),
            total_variants,
            is_active: true,
        };

        let stream = self
            .message_service
            .dispatch_to_plugin(
                identity,
                session_id,
                session_type_id,
                plugin_instance_id,
                new_message_id,
                history,
                capabilities,
                MessageEventKind::Recreate,
                cancel,
            )
            .await?;

        // Wrap the upstream stream so we can:
        //   (a) append `variant_info` to the StreamingCompleteEvent
        //       metadata (per feature-spec "recreate-stream-complete"),
        //   (b) schedule the active-path update *after* the stream
        //       closes (so finalize_assistant has had a chance to land
        //       successful Complete metadata before we flip is_active).
        let variants_repo = Arc::clone(&self.variants);
        let messages_repo = Arc::clone(&self.messages);
        let wrapped = wrap_stream_with_finalizer(
            stream,
            new_variant_info,
            session_id,
            new_message_id,
            move || {
                let variants = Arc::clone(&variants_repo);
                let messages = Arc::clone(&messages_repo);
                async move {
                    update_active_path_with_repos(variants, messages, session_id, new_message_id)
                        .await
                }
            },
        );

        log_op_finished(
            started,
            "recreate",
            session_id,
            new_message_id,
            Some(u32::try_from(new_variant_index).unwrap_or(0)),
        );
        increment_variant_creation_total("recreate");
        Ok(wrapped)
    }

    // ------------------------------------------------------------------
    //  Branch
    // ------------------------------------------------------------------

    /// `POST /sessions/{session_id}/messages/{message_id}/branch`.
    ///
    /// Creates a new user message as a child of the branch-point parent,
    /// pre-allocates an assistant stub under that user message, and
    /// delegates the NDJSON stream to
    /// [`MessageService::dispatch_to_plugin`] with
    /// [`MessageEventKind::New`].
    #[instrument(
        skip(self, identity, parts, cancel),
        fields(
            session_id = %session_id,
            branch_point_message_id = %branch_point_message_id,
            operation = "branch",
        ),
    )]
    pub async fn branch_message(
        &self,
        identity: &Identity,
        session_id: Uuid,
        branch_point_message_id: Uuid,
        parts: Vec<MessagePartInput>,
        file_ids: Option<Vec<Uuid>>,
        capabilities: Option<Vec<CapabilityValue>>,
        cancel: CancellationToken,
    ) -> Result<SendMessageStream> {
        let started = OffsetDateTime::now_utc();

        let session = self.load_session(identity, session_id).await?;
        self.gate_lifecycle_mutation(&session)?;

        let _branch_point = self
            .messages
            .find_message_in_session(session_id, branch_point_message_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("message", branch_point_message_id))?;

        // Resolve session-type → plugin binding.
        let session_type_id = session.session_type_id.ok_or_else(|| {
            ChatEngineError::bad_request(
                "session has no session_type bound; branch cannot be routed",
            )
        })?;
        let session_type = self
            .session_types
            .find_by_id(session_type_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("session_type", session_type_id))?;
        let plugin_instance_id = session_type.plugin_instance_id.clone().ok_or_else(|| {
            ChatEngineError::bad_request(
                "session_type has no plugin_instance_id; branch cannot be routed",
            )
        })?;

        // INSERT user message under the branch point (variant_index =
        // MAX+1) + an assistant stub under that user message in a
        // single SERIALIZABLE transaction — mirrors the send_message
        // path so we never leave a user turn without its assistant
        // child if the second insert fails.
        let (user_message_id, _user_variant_index, assistant_message_id) = self
            .variants
            .insert_user_and_assistant_stub_for_branch(
                session_id,
                branch_point_message_id,
                parts,
                file_ids,
                Some(identity.tenant_id.clone()),
                Some(identity.user_id.clone()),
            )
            .await
            .map_err(map_unique_violation_to_conflict)?;

        // Build a truncated history up to (and including) the branch
        // point. The plugin must see the conversation as if the previous
        // path never existed past `branch_point_message_id`.
        let history = self
            .build_branched_history(session_id, branch_point_message_id)
            .await?;

        let stream = self
            .message_service
            .dispatch_to_plugin(
                identity,
                session_id,
                session_type_id,
                plugin_instance_id,
                assistant_message_id,
                history,
                capabilities,
                MessageEventKind::New,
                cancel,
            )
            .await?;

        // Re-compute the active path after the stream closes so the new
        // branch becomes the canonical path and the old siblings'
        // descendants are detached.
        let variants_repo = Arc::clone(&self.variants);
        let messages_repo = Arc::clone(&self.messages);
        let wrapped = wrap_stream_simple(stream, move || {
            let variants = Arc::clone(&variants_repo);
            let messages = Arc::clone(&messages_repo);
            async move {
                update_active_path_with_repos(variants, messages, session_id, assistant_message_id)
                    .await
            }
        });

        log_op_finished(started, "branch", session_id, user_message_id, None);
        increment_variant_creation_total("branch");
        Ok(wrapped)
    }

    /// Walk the ancestor chain from `branch_point_message_id` to the
    /// root, returning the messages oldest-first. Each ancestor is
    /// loaded individually so the result reflects the latest stored
    /// content even if the active flag was just flipped.
    async fn build_branched_history(
        &self,
        session_id: Uuid,
        branch_point_message_id: Uuid,
    ) -> Result<Vec<Message>> {
        let chain = self
            .variants
            .ancestor_chain(session_id, branch_point_message_id)
            .await?;
        let mut out: Vec<Message> = Vec::with_capacity(chain.len());
        for id in chain.iter().rev() {
            if let Some(m) = self
                .messages
                .find_message_in_session(session_id, *id)
                .await?
            {
                // Skip hidden-from-backend / incomplete entries the way
                // `fetch_active_history` does, so plugins see a clean
                // call shape.
                if m.is_hidden_from_backend || !m.is_complete {
                    continue;
                }
                out.push(m);
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------------
    //  Session-type switch
    // ------------------------------------------------------------------

    /// Validate compatibility for a session-type switch.
    ///
    /// The new type's `available_capabilities` MUST be a superset of
    /// the session's current `enabled_capabilities` (ADR-0015). The
    /// session_type table in Phase 1 does not yet carry an
    /// `available_capabilities` column; this method calls the plugin's
    /// `on_session_updated` hook to learn the current declared
    /// capabilities and uses that as the canonical superset reference.
    pub async fn validate_session_type_switch(
        &self,
        identity: &Identity,
        session_id: Uuid,
        target_session_type_id: Uuid,
    ) -> Result<(session_type_entity::Model, String, Vec<Capability>)> {
        let session = self.load_session(identity, session_id).await?;
        self.gate_lifecycle_mutation(&session)?;

        let target = self
            .session_types
            .find_by_id(target_session_type_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("session_type", target_session_type_id))?;
        let plugin_instance_id = target.plugin_instance_id.clone().ok_or_else(|| {
            ChatEngineError::bad_request("target session_type has no plugin_instance_id")
        })?;

        // Resolve plugin and refresh capabilities. A missing plugin is
        // a 502 per the feature spec (see Error Semantics §). A plugin
        // error is also a 502 (the standard PluginError → ChatEngineError
        // conversion).
        let plugin = self
            .plugins
            .resolve(&plugin_instance_id)
            .map_err(|err| match err {
                ChatEngineError::NotFound { .. } => ChatEngineError::BackendUnavailable {
                    reason: format!("target plugin '{plugin_instance_id}' is not registered"),
                    retry_after: None,
                    source: None,
                },
                other => other,
            })?;
        let plugin_config = self
            .plugins
            .load_config(&plugin_instance_id, target_session_type_id)
            .await?;

        let cancel = CancellationToken::new();
        let call_ctx = PluginCallContext {
            request_id: Uuid::new_v4(),
            tenant_id: TenantId::new(identity.tenant_id.as_str()),
            user_id: UserId::new(identity.user_id.as_str()),
            plugin_instance_id: plugin_instance_id.clone(),
            session_type_id: target_session_type_id,
            plugin_config,
            enabled_capabilities: None,
            deadline: Some(Instant::now() + self.plugin_timeout),
            cancel: cancel.clone(),
        };
        let session_ctx = SessionPluginCtx {
            session_type_id: target_session_type_id,
            session_id: Some(session_id),
            call_ctx,
        };

        // Capability superset check: every currently-enabled capability
        // name must be in the freshly-declared list.
        let response =
            tokio::time::timeout(self.plugin_timeout, plugin.on_session_updated(session_ctx))
                .await
                .map_err(|_| {
                    cancel.cancel();
                    ChatEngineError::BackendUnavailable {
                        reason: "plugin on_session_updated deadline elapsed".into(),
                        retry_after: None,
                        source: None,
                    }
                })?
                .map_err(ChatEngineError::from)?;
        // This is a validation-only call to learn the target type's declared
        // capabilities for the superset check; any returned metadata is not
        // persisted on the type-switch path (capability updates go through
        // `SessionService::update_capabilities`, which does merge it).
        let available = response.capabilities;

        let current_names: Vec<String> = enabled_capability_names(&session);
        let available_names: std::collections::HashSet<&str> =
            available.iter().map(|c| c.name.as_str()).collect();
        for name in &current_names {
            if !available_names.contains(name.as_str()) {
                return Err(ChatEngineError::conflict(format!(
                    "new session type's available_capabilities is not a superset of \
                     enabled_capabilities (missing '{name}')",
                )));
            }
        }

        Ok((target, plugin_instance_id, available))
    }

    /// `PATCH /sessions/{session_id}/type` (canonical) /
    /// `PATCH /sessions/{session_id}/session-type` (compat).
    #[instrument(
        skip(self, identity),
        fields(
            session_id = %session_id,
            target_session_type_id = %target_session_type_id,
            operation = "switch_type",
        ),
    )]
    pub async fn switch_session_type(
        &self,
        identity: &Identity,
        session_id: Uuid,
        target_session_type_id: Uuid,
    ) -> Result<Session> {
        let started = OffsetDateTime::now_utc();

        let (_target_type, _plugin_instance_id, capabilities) = self
            .validate_session_type_switch(identity, session_id, target_session_type_id)
            .await?;

        let caps_json = serde_json::to_value(&capabilities).unwrap_or(JsonValue::Array(Vec::new()));

        let updated = self
            .variants
            .update_session_type(
                &identity.tenant_id,
                &identity.user_id,
                session_id,
                target_session_type_id,
                caps_json,
            )
            .await?;

        log_op_finished(started, "switch_type", session_id, session_id, None);
        increment_variant_creation_total("switch_type");

        Ok(updated.into())
    }

    // ------------------------------------------------------------------
    //  Internal helpers
    // ------------------------------------------------------------------

    async fn load_session(&self, identity: &Identity, session_id: Uuid) -> Result<Session> {
        let row = self
            .sessions
            .find_by_id(&identity.tenant_id, &identity.user_id, session_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("session", session_id))?;
        Ok(row.into())
    }

    fn gate_lifecycle_mutation(&self, session: &Session) -> Result<()> {
        if matches!(session.lifecycle_state, LifecycleState::Active) {
            Ok(())
        } else {
            Err(ChatEngineError::conflict(format!(
                "session is {} and does not accept variant mutations",
                session.lifecycle_state,
            )))
        }
    }

    fn gate_lifecycle_navigation(&self, session: &Session) -> Result<()> {
        if matches!(
            session.lifecycle_state,
            LifecycleState::Active | LifecycleState::Archived
        ) {
            Ok(())
        } else {
            Err(ChatEngineError::conflict(format!(
                "session is {} and does not accept variant navigation",
                session.lifecycle_state,
            )))
        }
    }
}

// ============================================================================
//  Helper functions
// ============================================================================

/// Map UNIQUE-constraint exhaustion errors from the variant_index
/// retry loop to `ChatEngineError::Conflict` (HTTP 409). The Phase 1
/// helper returns `DbErr::Custom("assign_variant_index exhausted …")`
/// or the underlying `DbErr` on the final retry — both fold to
/// `Internal` through the standard conversion. We intercept and
/// reclassify here so the REST layer sees a consistent 409.
fn map_unique_violation_to_conflict(err: ChatEngineError) -> ChatEngineError {
    if let ChatEngineError::Internal { reason, source } = &err {
        let lower = reason.to_lowercase();
        if lower.contains("exhausted")
            || lower.contains("uq_messages_session_parent_variant")
            || lower.contains("unique constraint")
        {
            return ChatEngineError::Conflict {
                reason: format!("concurrent variant creation: {reason}"),
            };
        }
        // Drop the source intentionally; the conflict reason is enough.
        let _ = source;
    }
    err
}

/// Standalone counterpart of [`VariantService::update_active_path`] —
/// used by the stream-finalizer closures so we can drive the active
/// path mutation without holding a `Clone` of the entire service.
async fn update_active_path_with_repos(
    variants: Arc<dyn VariantRepo>,
    messages: Arc<dyn MessageRepo>,
    session_id: Uuid,
    message_id: Uuid,
) -> Result<()> {
    let chain = variants.ancestor_chain(session_id, message_id).await?;
    if chain.is_empty() {
        return Err(ChatEngineError::not_found("message", message_id));
    }
    let chain_set: std::collections::HashSet<Uuid> = chain.iter().copied().collect();
    let mut deactivate: Vec<Uuid> = Vec::new();

    for ancestor_id in &chain {
        let ancestor = messages
            .find_message_in_session(session_id, *ancestor_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("message", *ancestor_id))?;
        let siblings = variants
            .list_siblings(session_id, ancestor.parent_message_id)
            .await?;
        for sibling in siblings {
            if !chain_set.contains(&sibling.message_id) {
                deactivate.push(sibling.message_id);
                let descendants = variants
                    .collect_descendants(session_id, sibling.message_id)
                    .await?;
                deactivate.extend(descendants);
            }
        }
    }
    deactivate.sort();
    deactivate.dedup();
    // Never deactivate an id that is also on the active chain — see
    // the comment in `VariantService::update_active_path`.
    deactivate.retain(|id| !chain_set.contains(id));
    variants
        .apply_active_flips(session_id, chain, deactivate)
        .await
}

/// Names of capabilities currently enabled on a session, decoded from
/// the JSONB column. Returns an empty vector if the column is absent
/// or the shape is unexpected (mirrors the read in
/// `MessageService::validate_request`).
fn enabled_capability_names(session: &Session) -> Vec<String> {
    let Some(JsonValue::Array(arr)) = session.enabled_capabilities.as_ref() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|entry| match entry {
            JsonValue::Object(map) => map.get("name").and_then(|n| n.as_str()).map(str::to_owned),
            _ => None,
        })
        .collect()
}

/// Wrap an upstream `SendMessageStream` so:
///   - `StreamingCompleteEvent`s are rewritten to carry `variant_info`
///     in their metadata envelope;
///   - once the upstream stream closes, the supplied `finalizer` async
///     task runs (used to apply the active-path mutation after the
///     persist hook).
fn wrap_stream_with_finalizer<F, Fut>(
    upstream: SendMessageStream,
    variant_info: VariantInfo,
    session_id: Uuid,
    new_message_id: Uuid,
    finalizer: F,
) -> SendMessageStream
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let info_json = serde_json::to_value(&variant_info).unwrap_or(JsonValue::Null);
    let mapped = upstream.map(move |evt| augment_complete_event(evt, &info_json));
    // Append a sentinel that triggers the finalizer when the upstream
    // is exhausted, then yields nothing further. `stream::once` calls
    // its future exactly once so the FnOnce can be consumed directly.
    let sentinel = stream::once(async move {
        if let Err(err) = finalizer().await {
            warn!(
                session_id = %session_id,
                message_id = %new_message_id,
                error = %err,
                "active-path update after stream end failed (variant retained, but is_active state may be stale)"
            );
        }
        None::<StreamingEvent>
    })
    .filter_map(|v: Option<StreamingEvent>| async move { v });
    mapped.chain(sentinel).boxed()
}

/// Like [`wrap_stream_with_finalizer`] but without the
/// `variant_info` rewrite — used by `branch_message` where the new
/// branch's variant payload is sent as a normal `Complete` event from
/// the plugin.
fn wrap_stream_simple<F, Fut>(upstream: SendMessageStream, finalizer: F) -> SendMessageStream
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let sentinel = stream::once(async move {
        if let Err(err) = finalizer().await {
            warn!(error = %err, "post-stream active-path update failed");
        }
        None::<StreamingEvent>
    })
    .filter_map(|v: Option<StreamingEvent>| async move { v });
    upstream.chain(sentinel).boxed()
}

/// Rewrite a [`StreamingEvent`] to include the recreate-variant
/// metadata block expected by the feature spec.
fn augment_complete_event(evt: StreamingEvent, variant_info: &JsonValue) -> StreamingEvent {
    match evt {
        StreamingEvent::Complete(mut c) => {
            // Merge {"variant_info": …} into the existing metadata
            // object. If the metadata is None or not an object, replace
            // it with a fresh object carrying just the variant_info.
            let merged = match c.metadata.take() {
                Some(JsonValue::Object(mut map)) => {
                    map.insert("variant_info".to_string(), variant_info.clone());
                    Some(JsonValue::Object(map))
                }
                Some(other) => {
                    // Preserve the prior value under `inner` to avoid
                    // silently dropping it.
                    Some(json!({ "inner": other, "variant_info": variant_info }))
                }
                None => Some(json!({ "variant_info": variant_info })),
            };
            c.metadata = merged;
            StreamingEvent::Complete(c)
        }
        other => other,
    }
}

fn log_op_finished(
    started: OffsetDateTime,
    operation: &'static str,
    session_id: Uuid,
    message_id: Uuid,
    variant_index: Option<u32>,
) {
    let now = OffsetDateTime::now_utc();
    let duration_ms = (now - started).whole_milliseconds().max(0);
    if let Some(idx) = variant_index {
        info!(
            session_id = %session_id,
            message_id = %message_id,
            variant_index = idx,
            operation,
            duration_ms,
            "variant operation completed"
        );
    } else {
        info!(
            session_id = %session_id,
            message_id = %message_id,
            operation,
            duration_ms,
            "variant operation completed"
        );
    }
}

/// Increment the `variant_creation_total` counter tagged by operation
/// type. The crate does not currently carry a metrics facade — this
/// is a documented hook for the future. See the contract doc for the
/// canonical metric name and labels.
fn increment_variant_creation_total(operation: &'static str) {
    // FIXME(phase-6): wire variant_creation_total counter once the
    // crate-wide metrics facade lands (tracked by Phase 15 module
    // wiring).
    debug!(
        operation,
        "variant_creation_total += 1 (no metrics facade yet)"
    );
}

// The SeaORM-backed `VariantRepo` impl lives at
// `crate::infra::db::repo::variant_repo::SeaVariantRepo` — it carries a
// `DatabaseConnection` and so belongs in the infra layer per the
// `#[domain_model]` boundary.

// ============================================================================
//  Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ------------------------------------------------------------------
    //  Mock VariantRepo that simulates UNIQUE violations.
    // ------------------------------------------------------------------

    /// Repository double that emulates the variant_index allocator's
    /// race-and-retry behaviour. Configurable: each call to
    /// `insert_user_and_assistant_stub_for_branch` fails up to
    /// `fail_until_attempt` times before succeeding (mimics the
    /// SERIALIZABLE retry loop's UNIQUE constraint contention).
    struct RetryingMockRepo {
        /// Counts attempts per `(session_id, parent_message_id)` key.
        attempts: Mutex<std::collections::HashMap<(Uuid, Uuid), usize>>,
        /// Force every call to error out (simulates exhausted retries).
        always_fail: bool,
        /// On the Nth attempt (0-based) succeed; earlier attempts
        /// surface a UNIQUE-violation-like error.
        succeed_on_attempt: usize,
        /// Number of times the combined insert has been called across
        /// all keys.
        total_calls: AtomicUsize,
    }

    impl RetryingMockRepo {
        fn new(succeed_on_attempt: usize) -> Arc<Self> {
            Arc::new(Self {
                attempts: Mutex::new(std::collections::HashMap::new()),
                always_fail: false,
                succeed_on_attempt,
                total_calls: AtomicUsize::new(0),
            })
        }

        fn always_failing() -> Arc<Self> {
            Arc::new(Self {
                attempts: Mutex::new(std::collections::HashMap::new()),
                always_fail: true,
                succeed_on_attempt: usize::MAX,
                total_calls: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl VariantRepo for RetryingMockRepo {
        async fn list_siblings(
            &self,
            _session_id: Uuid,
            _parent_message_id: Option<Uuid>,
        ) -> Result<Vec<Message>> {
            Ok(Vec::new())
        }

        async fn insert_user_and_assistant_stub_for_branch(
            &self,
            session_id: Uuid,
            parent_message_id: Uuid,
            _parts: Vec<MessagePartInput>,
            _file_ids: Option<Vec<Uuid>>,
            _tenant_id: Option<String>,
            _user_id: Option<String>,
        ) -> Result<(Uuid, i32, Uuid)> {
            self.total_calls.fetch_add(1, Ordering::SeqCst);
            let mut attempts = self.attempts.lock();
            let key = (session_id, parent_message_id);
            let n = attempts.entry(key).or_insert(0);
            let attempt_idx = *n;
            *n += 1;
            drop(attempts);

            if self.always_fail {
                return Err(ChatEngineError::internal(
                    "assign_variant_index exhausted 3 retries (uq_messages_session_parent_variant)",
                ));
            }
            if attempt_idx < self.succeed_on_attempt {
                return Err(ChatEngineError::internal(
                    "UNIQUE constraint violated: uq_messages_session_parent_variant",
                ));
            }
            Ok((
                Uuid::new_v4(),
                i32::try_from(attempt_idx).unwrap_or(0),
                Uuid::new_v4(),
            ))
        }

        async fn ancestor_chain(&self, _session_id: Uuid, message_id: Uuid) -> Result<Vec<Uuid>> {
            Ok(vec![message_id])
        }

        async fn collect_descendants(
            &self,
            _session_id: Uuid,
            _message_id: Uuid,
        ) -> Result<Vec<Uuid>> {
            Ok(Vec::new())
        }

        async fn apply_active_flips(
            &self,
            _session_id: Uuid,
            _activate_ids: Vec<Uuid>,
            _deactivate_ids: Vec<Uuid>,
        ) -> Result<()> {
            Ok(())
        }

        async fn update_session_type(
            &self,
            _tenant_id: &str,
            _user_id: &str,
            _session_id: Uuid,
            _new_session_type_id: Uuid,
            _new_capabilities: JsonValue,
        ) -> Result<crate::infra::db::entity::session::Model> {
            Err(ChatEngineError::internal("not implemented for this mock"))
        }
    }

    // ------------------------------------------------------------------
    //  Race-with-retry: a helper that emulates the SERIALIZABLE retry
    //  loop semantics directly against the mock repo.
    // ------------------------------------------------------------------

    /// Drive `insert_user_and_assistant_stub_for_branch` with up to
    /// `max_attempts` retries on a UNIQUE-violation-like `Internal`
    /// error, then promote a final exhaustion into a `Conflict`.
    /// Mirrors the production retry semantics enforced by
    /// `assign_variant_index` + the `map_unique_violation_to_conflict`
    /// wrapper.
    async fn insert_with_retry(
        repo: Arc<dyn VariantRepo>,
        session_id: Uuid,
        parent_message_id: Uuid,
        max_attempts: usize,
    ) -> Result<(Uuid, i32, Uuid)> {
        let mut last_err: Option<ChatEngineError> = None;
        for _ in 0..max_attempts {
            match repo
                .insert_user_and_assistant_stub_for_branch(
                    session_id,
                    parent_message_id,
                    vec![MessagePartInput {
                        part_type: chat_engine_sdk::models::MessagePartType::Text,
                        content: json!({"text": "x"}),
                        file_citations: vec![],
                        link_citations: vec![],
                        references: vec![],
                    }],
                    None,
                    None,
                    None,
                )
                .await
            {
                Ok(v) => return Ok(v),
                Err(err) => {
                    let is_retryable = matches!(
                        &err,
                        ChatEngineError::Internal { reason, .. }
                            if reason.to_lowercase().contains("unique")
                                || reason.to_lowercase().contains("exhausted")
                    );
                    if !is_retryable {
                        return Err(err);
                    }
                    last_err = Some(err);
                }
            }
        }
        Err(map_unique_violation_to_conflict(last_err.unwrap_or_else(
            || ChatEngineError::internal("exhausted retries with no recorded error"),
        )))
    }

    #[tokio::test]
    async fn assign_variant_index_race_retries_then_succeeds() {
        // Succeed on the 3rd attempt (0-indexed: succeed_on_attempt=2).
        // Capped at the same 3 attempts the production helper uses.
        let repo: Arc<dyn VariantRepo> = RetryingMockRepo::new(2);
        let session_id = Uuid::new_v4();
        let parent = Uuid::new_v4();
        let (_user_id, variant_index, _assistant_id) =
            insert_with_retry(Arc::clone(&repo), session_id, parent, 3)
                .await
                .expect("should succeed within 3 retries");
        assert_eq!(
            variant_index, 2,
            "should reflect the attempt that succeeded"
        );
    }

    #[tokio::test]
    async fn assign_variant_index_exhausted_retries_returns_conflict() {
        let repo: Arc<dyn VariantRepo> = RetryingMockRepo::always_failing();
        let session_id = Uuid::new_v4();
        let parent = Uuid::new_v4();
        let err = insert_with_retry(Arc::clone(&repo), session_id, parent, 3)
            .await
            .expect_err("must exhaust retries");
        assert!(
            matches!(err, ChatEngineError::Conflict { .. }),
            "expected Conflict, got {err:?}"
        );
    }

    #[test]
    fn augment_complete_event_merges_variant_info_into_existing_metadata() {
        use crate::domain::message::StreamingCompleteEvent;

        let info = json!({
            "message_id": "00000000-0000-0000-0000-000000000001",
            "variant_index": 2,
            "total_variants": 3,
            "is_active": true,
        });
        let evt = StreamingEvent::Complete(StreamingCompleteEvent {
            message_id: Uuid::nil(),
            metadata: Some(json!({"model": "gpt-test"})),
            file_citations: vec![],
            link_citations: vec![],
            references: vec![],
        });
        let out = augment_complete_event(evt, &info);
        match out {
            StreamingEvent::Complete(c) => {
                let meta = c.metadata.expect("metadata must be present");
                assert_eq!(meta["model"], "gpt-test");
                assert_eq!(meta["variant_info"]["variant_index"], 2);
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn augment_complete_event_creates_metadata_when_absent() {
        use crate::domain::message::StreamingCompleteEvent;

        let info = json!({"variant_index": 0});
        let evt = StreamingEvent::Complete(StreamingCompleteEvent {
            message_id: Uuid::nil(),
            metadata: None,
            file_citations: vec![],
            link_citations: vec![],
            references: vec![],
        });
        let out = augment_complete_event(evt, &info);
        match out {
            StreamingEvent::Complete(c) => {
                let meta = c.metadata.expect("metadata must be created");
                assert_eq!(meta["variant_info"]["variant_index"], 0);
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn map_unique_violation_to_conflict_only_converts_known_messages() {
        let benign = ChatEngineError::internal("totally unrelated db hiccup");
        match map_unique_violation_to_conflict(benign) {
            ChatEngineError::Internal { .. } => {}
            other => panic!("benign internal must stay Internal, got {other:?}"),
        }
        let exhausted = ChatEngineError::internal(
            "assign_variant_index exhausted 3 retries (uq_messages_session_parent_variant)",
        );
        match map_unique_violation_to_conflict(exhausted) {
            ChatEngineError::Conflict { .. } => {}
            other => panic!("exhausted internal must map to Conflict, got {other:?}"),
        }
    }

    #[test]
    fn enabled_capability_names_handles_missing_and_malformed_inputs() {
        use chat_engine_sdk::models::{TenantId, UserId};
        let mut s = Session {
            session_id: Uuid::nil(),
            tenant_id: TenantId::new("t"),
            user_id: UserId::new("u"),
            client_id: None,
            session_type_id: None,
            enabled_capabilities: None,
            metadata: None,
            lifecycle_state: LifecycleState::Active,
            share_token: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        };
        assert!(enabled_capability_names(&s).is_empty());

        s.enabled_capabilities =
            Some(json!([{"name": "model", "value": "x"}, {"name": "stream", "value": true}]));
        let names = enabled_capability_names(&s);
        assert_eq!(names, vec!["model".to_string(), "stream".to_string()]);

        s.enabled_capabilities = Some(json!("not an array"));
        assert!(enabled_capability_names(&s).is_empty());
    }
}
