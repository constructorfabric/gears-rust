//! `messages` repository.
//!
//! Owns inserts + active-path reads for the immutable message tree
//! (ADR-0001). The repository is consumed by
//! [`crate::domain::service::message_service::MessageService`] for the
//! `POST /messages/send` pipeline:
//!
//! - [`MessageRepo::insert_user_and_assistant_stub`] runs a SERIALIZABLE
//!   transaction that computes the next `variant_index` (via
//!   [`crate::infra::db::compute_next_variant_index`]) and inserts the
//!   user message + a matching assistant stub (`is_complete=false`) in
//!   the **same** transaction. The whole pair is retried up to
//!   `VARIANT_INDEX_MAX_RETRIES` times on
//!   `uq_messages_session_parent_variant` collisions so a concurrent
//!   sibling-insert is recovered transparently; exhaustion maps to HTTP
//!   409.
//! - [`MessageRepo::finalize_assistant`] atomically writes the assistant
//!   message's final state (`content`, `is_complete`, `metadata`) once the
//!   plugin stream resolves (success, error, or cancellation).
//! - [`MessageRepo::fetch_active_history`] returns the visible active-path
//!   history the plugin needs to answer the next message — filtering
//!   `is_hidden_from_backend=false AND is_active=true` and ordering by
//!   `created_at ASC`.
//!
//! The trait is object-safe so the service can hold `Arc<dyn MessageRepo>`
//! and unit tests can drop in an in-memory mock. The Sea-ORM impl
//! [`SeaMessageRepo`] threads the toolkit-db `DBProvider` through the same
//! handle the migration runner uses.
//
// @cpt-cf-chat-engine-message-repo:p5
// @cpt-cf-chat-engine-adr-message-tree-structure:p5

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chat_engine_sdk::models::{
    FileCitation, LinkCitation, LinkReference, MessagePart, MessagePartInput,
};
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, QueryOrder};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use toolkit_db::secure::{
    AccessScope, DBRunner, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
    TxConfig,
};
use uuid::Uuid;

use crate::domain::error::ChatEngineError;
use crate::domain::message::Message;
use crate::domain::ports::{
    FinalizeOutcome, InsertedPair, MessageRepo, NewUserMessage, PartCitations,
};
use crate::infra::db::conversions::part_type_to_entity;
use crate::infra::db::entity::message::{self as message_entity, Entity as MessageEntity};
use crate::infra::db::entity::message_part::{
    self as message_part_entity, Entity as MessagePartEntity, compute_next_part_number,
};
use crate::infra::db::entity::{
    file_citation as file_citation_entity, link_citation as link_citation_entity,
    link_reference as link_reference_entity,
};
use crate::infra::db::repo::ChatEngineDb;
use crate::infra::db::{
    VARIANT_INDEX_MAX_RETRIES, compute_next_variant_index, is_variant_unique_violation,
};

/// Sea-ORM-backed implementation of [`MessageRepo`].
///
/// Holds the toolkit-db `DBProvider` so every query runs against the same
/// connection the migration runner used. `messages` is marked
/// `#[secure(unrestricted)]`; the secure wrappers run with
/// `AccessScope::allow_all()` and exist to expose a `&impl DBRunner`
/// execution path.
pub struct SeaMessageRepo {
    db: Arc<ChatEngineDb>,
}

impl SeaMessageRepo {
    #[must_use]
    pub fn new(db: Arc<ChatEngineDb>) -> Self {
        Self { db }
    }

    /// Batch-load the ordered `message_parts` for the given messages and
    /// attach them to each `Message.parts`. One query for the whole batch
    /// (ordered by `message_id, number`); messages with no parts keep an
    /// empty list. `From<message_entity::Model>` always yields `parts = []`,
    /// so every read path that returns `Message`(s) funnels through here.
    async fn attach_parts(&self, mut msgs: Vec<Message>) -> Result<Vec<Message>, ChatEngineError> {
        if msgs.is_empty() {
            return Ok(msgs);
        }
        let ids: Vec<Uuid> = msgs.iter().map(|m| m.message_id).collect();
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let rows = MessagePartEntity::find()
            .order_by_asc(message_part_entity::Column::MessageId)
            .order_by_asc(message_part_entity::Column::Number)
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(message_part_entity::Column::MessageId.is_in(ids)))
            .all(&conn)
            .await?;

        let mut parts: Vec<MessagePart> = rows.into_iter().map(MessagePart::from).collect();
        self.attach_citations(&mut parts).await?;

        let mut by_msg: HashMap<Uuid, Vec<MessagePart>> = HashMap::new();
        for part in parts {
            by_msg.entry(part.message_id).or_default().push(part);
        }
        for m in &mut msgs {
            if let Some(parts) = by_msg.remove(&m.message_id) {
                m.parts = parts;
            }
        }
        Ok(msgs)
    }

    /// Batch-load citations/references for the given parts and attach them to
    /// each text part (FR-023). One query per child table for the whole batch,
    /// each ordered by `number`; payloads are deserialized from the stored
    /// JSON. Non-text parts simply have no rows.
    async fn attach_citations(&self, parts: &mut [MessagePart]) -> Result<(), ChatEngineError> {
        if parts.is_empty() {
            return Ok(());
        }
        let ids: Vec<Uuid> = parts.iter().map(|p| p.id).collect();
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();

        let file_rows = file_citation_entity::Entity::find()
            .order_by_asc(file_citation_entity::Column::Number)
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(file_citation_entity::Column::MessagePartId.is_in(ids.clone())),
            )
            .all(&conn)
            .await?;
        let link_rows = link_citation_entity::Entity::find()
            .order_by_asc(link_citation_entity::Column::Number)
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(link_citation_entity::Column::MessagePartId.is_in(ids.clone())),
            )
            .all(&conn)
            .await?;
        let ref_rows = link_reference_entity::Entity::find()
            .order_by_asc(link_reference_entity::Column::Number)
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(link_reference_entity::Column::MessagePartId.is_in(ids)))
            .all(&conn)
            .await?;

        let mut files: HashMap<Uuid, Vec<FileCitation>> = HashMap::new();
        for row in file_rows {
            if let Ok(c) = serde_json::from_value::<FileCitation>(row.content) {
                files.entry(row.message_part_id).or_default().push(c);
            }
        }
        let mut links: HashMap<Uuid, Vec<LinkCitation>> = HashMap::new();
        for row in link_rows {
            if let Ok(c) = serde_json::from_value::<LinkCitation>(row.content) {
                links.entry(row.message_part_id).or_default().push(c);
            }
        }
        let mut refs: HashMap<Uuid, Vec<LinkReference>> = HashMap::new();
        for row in ref_rows {
            if let Ok(r) = serde_json::from_value::<LinkReference>(row.content) {
                refs.entry(row.message_part_id).or_default().push(r);
            }
        }

        for p in parts.iter_mut() {
            if let Some(v) = files.remove(&p.id) {
                p.file_citations = v;
            }
            if let Some(v) = links.remove(&p.id) {
                p.link_citations = v;
            }
            if let Some(v) = refs.remove(&p.id) {
                p.references = v;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl MessageRepo for SeaMessageRepo {
    async fn insert_user_and_assistant_stub(
        &self,
        req: NewUserMessage,
    ) -> Result<InsertedPair, ChatEngineError> {
        // SERIALIZABLE-bound retry loop that wraps BOTH the
        // `MAX(variant_index)+1` SELECT and the matching INSERT in a
        // single transaction.
        let session_id = req.session_id;
        let parent = req.parent_message_id;
        let file_ids_json = req
            .file_ids
            .as_ref()
            .filter(|ids| !ids.is_empty())
            .and_then(|ids| serde_json::to_value(ids).ok());
        let parts = req.parts;
        let metadata = req.metadata;
        let tenant_id = req.tenant_id;
        let author_id = req.user_id;

        let mut last_err: Option<ChatEngineError> = None;
        for _attempt in 0..VARIANT_INDEX_MAX_RETRIES {
            let user_message_id = Uuid::new_v4();
            let assistant_message_id = Uuid::new_v4();
            let now = OffsetDateTime::now_utc();
            let parts_attempt = parts.clone();
            let metadata_attempt = metadata.clone();
            let file_ids_attempt = file_ids_json.clone();
            let user_tenant = tenant_id.clone();
            let author = author_id.clone();
            // Assistant stub inherits the owning tenant but has no author.
            let assistant_tenant = tenant_id.clone();

            let outcome: Result<i32, ChatEngineError> = self
                .db
                .transaction_with_config(TxConfig::serializable(), move |tx| {
                    Box::pin(async move {
                        let scope = AccessScope::allow_all();
                        let user_variant_index =
                            compute_next_variant_index(tx, session_id, parent).await?;

                        let user_active = message_entity::ActiveModel {
                            message_id: Set(user_message_id),
                            session_id: Set(session_id),
                            // Owning tenant (denormalized) + authoring user, both
                            // sourced from the JWT identity by the service layer.
                            tenant_id: Set(user_tenant),
                            user_id: Set(author),
                            parent_message_id: Set(parent),
                            role: Set(message_entity::MessageRole::User),
                            file_ids: Set(file_ids_attempt),
                            variant_index: Set(user_variant_index),
                            is_active: Set(true),
                            is_complete: Set(true),
                            is_hidden_from_user: Set(false),
                            is_hidden_from_backend: Set(false),
                            metadata: Set(metadata_attempt),
                            created_at: Set(now),
                        };
                        let assistant_active = message_entity::ActiveModel {
                            message_id: Set(assistant_message_id),
                            session_id: Set(session_id),
                            // Inherits the owning tenant; assistant messages have
                            // no human author, so `user_id` stays NULL.
                            tenant_id: Set(assistant_tenant),
                            user_id: Set(None),
                            parent_message_id: Set(Some(user_message_id)),
                            role: Set(message_entity::MessageRole::Assistant),
                            file_ids: Set(None),
                            variant_index: Set(0),
                            is_active: Set(true),
                            is_complete: Set(false),
                            is_hidden_from_user: Set(false),
                            is_hidden_from_backend: Set(false),
                            metadata: Set(None),
                            created_at: Set(now),
                        };
                        MessageEntity::insert(user_active)
                            .secure()
                            .scope_unchecked(&scope)?
                            .exec(tx)
                            .await?;
                        // Persist the user message body as ordered parts; the
                        // assistant stub starts part-less until finalize.
                        insert_message_parts(tx, &scope, user_message_id, &parts_attempt).await?;
                        MessageEntity::insert(assistant_active)
                            .secure()
                            .scope_unchecked(&scope)?
                            .exec(tx)
                            .await?;
                        Ok(user_variant_index)
                    })
                })
                .await;

            match outcome {
                Ok(user_variant_index) => {
                    return Ok(InsertedPair {
                        user_message_id,
                        assistant_message_id,
                        user_variant_index,
                    });
                }
                Err(e) => {
                    if let Some(db_err) = chat_engine_db_err(&e) {
                        if !is_variant_unique_violation(db_err) {
                            return Err(e);
                        }
                    } else {
                        return Err(e);
                    }
                    last_err = Some(e);
                }
            }
        }
        Err(retry_exhausted_conflict(last_err))
    }

    async fn finalize_assistant(
        &self,
        session_id: Uuid,
        assistant_message_id: Uuid,
        outcome: FinalizeOutcome,
    ) -> Result<(), ChatEngineError> {
        let (text, metadata, is_complete, citations, extra_parts) = match outcome {
            FinalizeOutcome::Complete {
                text,
                metadata,
                citations,
                extra_parts,
            } => (text, metadata, true, citations, extra_parts),
            FinalizeOutcome::Cancelled { text } => {
                let mut meta = serde_json::Map::new();
                meta.insert("cancelled".into(), JsonValue::Bool(true));
                meta.insert("partial".into(), JsonValue::Bool(true));
                (
                    text,
                    Some(JsonValue::Object(meta)),
                    false,
                    PartCitations::default(),
                    Vec::new(),
                )
            }
            FinalizeOutcome::Errored {
                text,
                error,
                finish_reason,
            } => {
                let mut meta = serde_json::Map::new();
                meta.insert(
                    "finish_reason".into(),
                    JsonValue::String(finish_reason.to_string()),
                );
                meta.insert("error".into(), JsonValue::String(error));
                meta.insert("partial".into(), JsonValue::Bool(true));
                (
                    text,
                    Some(JsonValue::Object(meta)),
                    false,
                    PartCitations::default(),
                    Vec::new(),
                )
            }
        };

        // The state flip and the text-part write are one transaction so a
        // reader never sees a completed assistant row without its body. The
        // `is_complete = false AND metadata IS NULL` predicate matches only
        // the still-pending stub — any prior finalize leaves the row outside
        // the match set, so a concurrent finalize cannot clobber the winner's
        // terminal state, and only the winning UPDATE appends the part (no
        // duplicate body on a double finalize).
        let rows_affected = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::allow_all();
                    let result = MessageEntity::update_many()
                        .secure()
                        .scope_with(&scope)
                        .filter(
                            Condition::all()
                                .add(message_entity::Column::MessageId.eq(assistant_message_id))
                                .add(message_entity::Column::SessionId.eq(session_id))
                                .add(message_entity::Column::IsComplete.eq(false))
                                .add(message_entity::Column::Metadata.is_null()),
                        )
                        .col_expr(message_entity::Column::IsComplete, Expr::value(is_complete))
                        .col_expr(message_entity::Column::Metadata, Expr::value(metadata))
                        .exec(tx)
                        .await?;

                    if result.rows_affected == 1 {
                        let part_id =
                            append_text_part(tx, &scope, assistant_message_id, &text).await?;
                        // Attach the plugin's citations/references to the text
                        // part in the same transaction (FR-023).
                        insert_part_citations(tx, &scope, part_id, &citations).await?;
                        // Append any parts streamed via `StreamingEvent::Part`
                        // (FR-024 Phase B), each with its own citations.
                        for part in &extra_parts {
                            let extra_id =
                                append_part(tx, &scope, assistant_message_id, part).await?;
                            let part_citations = PartCitations {
                                file_citations: part.file_citations.clone(),
                                link_citations: part.link_citations.clone(),
                                references: part.references.clone(),
                            };
                            if !part_citations.is_empty() {
                                insert_part_citations(tx, &scope, extra_id, &part_citations)
                                    .await?;
                            }
                        }
                    }
                    Ok::<u64, ChatEngineError>(result.rows_affected)
                })
            })
            .await?;

        if rows_affected == 0 {
            tracing::debug!(
                session_id = %session_id,
                assistant_message_id = %assistant_message_id,
                "finalize_assistant no-op: stub already terminated or not in session",
            );
        }
        Ok(())
    }

    async fn fetch_active_history(
        &self,
        session_id: Uuid,
        depth: Option<u32>,
    ) -> Result<Vec<Message>, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let mut query = MessageEntity::find()
            .order_by_asc(message_entity::Column::CreatedAt)
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(message_entity::Column::SessionId.eq(session_id))
                    .add(message_entity::Column::IsActive.eq(true))
                    .add(message_entity::Column::IsHiddenFromBackend.eq(false))
                    .add(message_entity::Column::IsComplete.eq(true)),
            );

        if let Some(d) = depth {
            query = query.limit(u64::from(d));
        }

        let rows = query.all(&conn).await?;
        self.attach_parts(rows.into_iter().map(Message::from).collect())
            .await
    }

    async fn find_message_in_session(
        &self,
        session_id: Uuid,
        message_id: Uuid,
    ) -> Result<Option<Message>, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let row = MessageEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(message_entity::Column::MessageId.eq(message_id))
                    .add(message_entity::Column::SessionId.eq(session_id)),
            )
            .one(&conn)
            .await?;
        match row {
            Some(row) => Ok(self.attach_parts(vec![Message::from(row)]).await?.pop()),
            None => Ok(None),
        }
    }

    async fn find_message_by_id(
        &self,
        message_id: Uuid,
    ) -> Result<Option<Message>, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let row = MessageEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(message_entity::Column::MessageId.eq(message_id)))
            .one(&conn)
            .await?;
        match row {
            Some(row) => Ok(self.attach_parts(vec![Message::from(row)]).await?.pop()),
            None => Ok(None),
        }
    }

    async fn list_active_path(&self, session_id: Uuid) -> Result<Vec<Message>, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        // Active-path traversal per the Phase 6 contract: `is_active=true`
        // siblings only, `created_at ASC`. We also require `is_complete=true`
        // so an in-flight assistant stub does not leak into history. Notably
        // we do NOT filter `is_hidden_from_backend` here — the memory
        // strategy may need hidden messages to satisfy the "keep last K"
        // rule.
        let rows = MessageEntity::find()
            .order_by_asc(message_entity::Column::CreatedAt)
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(message_entity::Column::SessionId.eq(session_id))
                    .add(message_entity::Column::IsActive.eq(true))
                    .add(message_entity::Column::IsComplete.eq(true)),
            )
            .all(&conn)
            .await?;
        self.attach_parts(rows.into_iter().map(Message::from).collect())
            .await
    }

    async fn list_non_root_messages_chrono(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<Message>, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let rows = MessageEntity::find()
            .order_by_asc(message_entity::Column::CreatedAt)
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(message_entity::Column::SessionId.eq(session_id))
                    .add(message_entity::Column::ParentMessageId.is_not_null()),
            )
            .all(&conn)
            .await?;
        self.attach_parts(rows.into_iter().map(Message::from).collect())
            .await
    }

    async fn list_non_root_messages_older_than(
        &self,
        session_id: Uuid,
        older_than: OffsetDateTime,
    ) -> Result<Vec<Message>, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let rows = MessageEntity::find()
            .order_by_asc(message_entity::Column::CreatedAt)
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(message_entity::Column::SessionId.eq(session_id))
                    .add(message_entity::Column::ParentMessageId.is_not_null())
                    .add(message_entity::Column::CreatedAt.lt(older_than)),
            )
            .all(&conn)
            .await?;
        self.attach_parts(rows.into_iter().map(Message::from).collect())
            .await
    }

    async fn count_non_root_messages(&self, session_id: Uuid) -> Result<u64, ChatEngineError> {
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let n = MessageEntity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(message_entity::Column::SessionId.eq(session_id))
                    .add(message_entity::Column::ParentMessageId.is_not_null()),
            )
            .count(&conn)
            .await?;
        Ok(n)
    }

    async fn list_oldest_non_root_message_ids(
        &self,
        session_id: Uuid,
        limit: u32,
    ) -> Result<Vec<Uuid>, ChatEngineError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        // The secure layer doesn't yet expose `select_only() +
        // into_tuple()`, so we fetch full rows and project in-process.
        // The cost is one extra column per row (the `content` JSONB),
        // bounded by `limit`. Retention runs in batches sized by
        // [`crate::config::ChatEngineConfig::retention_max_deletes_per_session`]
        // so the per-call cap is small (default 100).
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let rows = MessageEntity::find()
            .order_by_asc(message_entity::Column::CreatedAt)
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(message_entity::Column::SessionId.eq(session_id))
                    .add(message_entity::Column::ParentMessageId.is_not_null()),
            )
            .limit(u64::from(limit))
            .all(&conn)
            .await?;
        Ok(rows.into_iter().map(|m| m.message_id).collect())
    }

    async fn list_non_root_message_ids_older_than(
        &self,
        session_id: Uuid,
        older_than: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<Uuid>, ChatEngineError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.db.conn()?;
        let scope = AccessScope::allow_all();
        let rows = MessageEntity::find()
            .order_by_asc(message_entity::Column::CreatedAt)
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(message_entity::Column::SessionId.eq(session_id))
                    .add(message_entity::Column::ParentMessageId.is_not_null())
                    .add(message_entity::Column::CreatedAt.lt(older_than)),
            )
            .limit(u64::from(limit))
            .all(&conn)
            .await?;
        Ok(rows.into_iter().map(|m| m.message_id).collect())
    }

    async fn insert_summary_message(
        &self,
        session_id: Uuid,
        text: String,
        metadata: Option<JsonValue>,
        summarized_ids: Vec<Uuid>,
        tenant_id: Option<String>,
    ) -> Result<Uuid, ChatEngineError> {
        let summary_id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();

        let summarized = summarized_ids.clone();
        self.db
            .transaction_with_config(TxConfig::serializable(), move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::allow_all();
                    // The summary is a new root (parent_message_id = NULL).
                    // `uq_messages_session_root_variant` forbids two roots in
                    // the same session sharing a variant_index, so it must take
                    // the next free root index — hardcoding 0 collides with the
                    // session's first user message.
                    let variant_index = compute_next_variant_index(tx, session_id, None).await?;
                    let summary_active = message_entity::ActiveModel {
                        message_id: Set(summary_id),
                        session_id: Set(session_id),
                        // Inherits the owning tenant; system-generated, no author.
                        tenant_id: Set(tenant_id),
                        user_id: Set(None),
                        parent_message_id: Set(None),
                        role: Set(message_entity::MessageRole::System),
                        file_ids: Set(None),
                        variant_index: Set(variant_index),
                        is_active: Set(true),
                        is_complete: Set(true),
                        is_hidden_from_user: Set(true),
                        is_hidden_from_backend: Set(false),
                        metadata: Set(metadata),
                        created_at: Set(now),
                    };
                    MessageEntity::insert(summary_active)
                        .secure()
                        .scope_unchecked(&scope)?
                        .exec(tx)
                        .await?;
                    // The summary body is a single text part.
                    append_text_part(tx, &scope, summary_id, &text).await?;
                    if !summarized.is_empty() {
                        MessageEntity::update_many()
                            .secure()
                            .scope_with(&scope)
                            .filter(
                                Condition::all()
                                    .add(message_entity::Column::SessionId.eq(session_id))
                                    .add(
                                        message_entity::Column::MessageId.is_in(summarized.clone()),
                                    ),
                            )
                            .col_expr(
                                message_entity::Column::IsHiddenFromBackend,
                                Expr::value(true),
                            )
                            .exec(tx)
                            .await?;
                    }
                    Ok(summary_id)
                })
            })
            .await
    }

    async fn delete_message_subtree(
        &self,
        session_id: Uuid,
        root_id: Uuid,
    ) -> Result<u64, ChatEngineError> {
        // Idempotency: a missing root is a no-op (returns 0). The
        // SERIALIZABLE transaction wraps the walk so concurrent cleanup runs
        // cannot observe partial deletes — keeping it short (O(depth)
        // round-trips, not O(n)) limits serialization-failure / lock
        // contention on the retention hot path.
        self.db
            .transaction_with_config(TxConfig::serializable(), move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::allow_all();
                    // Collect the subtree breadth-first, one
                    // `is_in(frontier)` query per tree level instead of one
                    // `find` per node (mirrors
                    // `SeaVariantRepo::collect_descendants`). `levels[0]` is
                    // the root; deeper levels follow.
                    let mut levels: Vec<Vec<Uuid>> = Vec::new();
                    let mut frontier: Vec<Uuid> = vec![root_id];
                    loop {
                        let children: Vec<Uuid> = MessageEntity::find()
                            .secure()
                            .scope_with(&scope)
                            .filter(
                                Condition::all()
                                    .add(message_entity::Column::SessionId.eq(session_id))
                                    .add(
                                        message_entity::Column::ParentMessageId
                                            .is_in(frontier.clone()),
                                    ),
                            )
                            .all(tx)
                            .await?
                            .into_iter()
                            .map(|m| m.message_id)
                            .collect();
                        levels.push(frontier);
                        if children.is_empty() {
                            break;
                        }
                        frontier = children;
                    }
                    // Delete one whole level per round-trip, deepest first.
                    // A single `is_in(all_ids)` delete is unsafe here: the
                    // `messages.parent_message_id -> messages.message_id` FK
                    // is `on_delete = Restrict`, which is checked immediately
                    // (not deferred to statement end), so a batch that
                    // removed a parent before its child would fail. Sibling
                    // nodes within one level never reference each other, so
                    // each per-level batch is FK-safe once the level below it
                    // is already gone.
                    let mut removed: u64 = 0;
                    for level in levels.into_iter().rev() {
                        let res = MessageEntity::delete_many()
                            .secure()
                            .scope_with(&scope)
                            .filter(
                                Condition::all()
                                    .add(message_entity::Column::SessionId.eq(session_id))
                                    .add(message_entity::Column::MessageId.is_in(level)),
                            )
                            .exec(tx)
                            .await?;
                        removed += res.rows_affected;
                    }
                    Ok(removed)
                })
            })
            .await
    }

    async fn insert_assistant_variant_stub(
        &self,
        session_id: Uuid,
        parent_message_id: Uuid,
        tenant_id: Option<String>,
    ) -> Result<InsertedPair, ChatEngineError> {
        // Same race-free pattern as `insert_user_and_assistant_stub`:
        // the SELECT for `MAX(variant_index)+1` runs in the SAME
        // SERIALIZABLE transaction as the INSERT.
        let mut last_err: Option<ChatEngineError> = None;
        for _attempt in 0..VARIANT_INDEX_MAX_RETRIES {
            let new_message_id = Uuid::new_v4();
            let now = OffsetDateTime::now_utc();
            let variant_tenant = tenant_id.clone();

            let outcome: Result<i32, ChatEngineError> = self
                .db
                .transaction_with_config(TxConfig::serializable(), move |tx| {
                    Box::pin(async move {
                        let scope = AccessScope::allow_all();
                        let new_variant_index =
                            compute_next_variant_index(tx, session_id, Some(parent_message_id))
                                .await?;
                        let assistant_active = message_entity::ActiveModel {
                            message_id: Set(new_message_id),
                            session_id: Set(session_id),
                            // Inherits the owning tenant; recreated assistant
                            // variant has no human author.
                            tenant_id: Set(variant_tenant),
                            user_id: Set(None),
                            parent_message_id: Set(Some(parent_message_id)),
                            role: Set(message_entity::MessageRole::Assistant),
                            file_ids: Set(None),
                            variant_index: Set(new_variant_index),
                            is_active: Set(true),
                            is_complete: Set(false),
                            is_hidden_from_user: Set(false),
                            is_hidden_from_backend: Set(false),
                            metadata: Set(None),
                            created_at: Set(now),
                        };
                        MessageEntity::insert(assistant_active)
                            .secure()
                            .scope_unchecked(&scope)?
                            .exec(tx)
                            .await?;
                        Ok(new_variant_index)
                    })
                })
                .await;

            match outcome {
                Ok(new_variant_index) => {
                    return Ok(InsertedPair {
                        user_message_id: parent_message_id,
                        assistant_message_id: new_message_id,
                        user_variant_index: new_variant_index,
                    });
                }
                Err(e) => {
                    if let Some(db_err) = chat_engine_db_err(&e) {
                        if !is_variant_unique_violation(db_err) {
                            return Err(e);
                        }
                    } else {
                        return Err(e);
                    }
                    last_err = Some(e);
                }
            }
        }
        Err(retry_exhausted_conflict(last_err))
    }
}

/// Reach into a `ChatEngineError` to recover the underlying `sea_orm::DbErr`
/// for retry classification. Only the `Internal { source }` branch carries
/// one; everything else short-circuits the retry.
fn chat_engine_db_err(err: &ChatEngineError) -> Option<&sea_orm::DbErr> {
    let ChatEngineError::Internal { source, .. } = err else {
        return None;
    };
    let source = source.as_ref()?;
    source.downcast_ref::<sea_orm::DbErr>().or_else(|| {
        source
            .downcast_ref::<toolkit_db::DbError>()
            .and_then(|dbe| match dbe {
                toolkit_db::DbError::Sea(inner) => Some(inner),
                _ => None,
            })
    })
}

/// Map a retry-exhaustion outcome to a `Conflict` ChatEngineError that
/// downstream handlers will render as HTTP 409. The original cause is
/// preserved as the source for log triage.
fn retry_exhausted_conflict(last_err: Option<ChatEngineError>) -> ChatEngineError {
    let base = format!(
        "variant index allocation contended; exhausted {VARIANT_INDEX_MAX_RETRIES} retries"
    );
    ChatEngineError::conflict(match last_err {
        Some(e) => format!("{base}: {e}"),
        None => base,
    })
}

/// Build the `content` JSON of a `text` part with the body under `text`
/// (the SDK-canonical `{ "text": ... }` shape, see ADR-0006).
fn text_part_content(text: &str) -> JsonValue {
    serde_json::json!({ "text": text })
}

/// Insert `parts` for a freshly-created `message_id`, numbering them `0..n` in
/// list order, inside the caller's transaction. The message must have no
/// existing parts (create path); ordering is the list order.
pub(crate) async fn insert_message_parts<R>(
    runner: &R,
    scope: &AccessScope,
    message_id: Uuid,
    parts: &[MessagePartInput],
) -> Result<(), ChatEngineError>
where
    R: DBRunner,
{
    for (idx, part) in parts.iter().enumerate() {
        let am = message_part_entity::ActiveModel {
            id: Set(Uuid::new_v4()),
            message_id: Set(message_id),
            r#type: Set(part_type_to_entity(&part.part_type)),
            content: Set(part.content.clone()),
            number: Set(i32::try_from(idx).unwrap_or(i32::MAX)),
        };
        MessagePartEntity::insert(am)
            .secure()
            .scope_unchecked(scope)?
            .exec(runner)
            .await?;
    }
    Ok(())
}

/// Append a single `text` part to `message_id` (number = `MAX(number)+1`)
/// inside the caller's transaction and return its new id. Used by
/// finalize/summary which add the machine-generated text body after the
/// message row already exists.
async fn append_text_part<R>(
    runner: &R,
    scope: &AccessScope,
    message_id: Uuid,
    text: &str,
) -> Result<Uuid, ChatEngineError>
where
    R: DBRunner,
{
    let number = compute_next_part_number(runner, message_id).await?;
    let part_id = Uuid::new_v4();
    let am = message_part_entity::ActiveModel {
        id: Set(part_id),
        message_id: Set(message_id),
        r#type: Set(message_part_entity::MessagePartType::Text),
        content: Set(text_part_content(text)),
        number: Set(number),
    };
    MessagePartEntity::insert(am)
        .secure()
        .scope_unchecked(scope)?
        .exec(runner)
        .await?;
    Ok(part_id)
}

/// Append a single typed part (image/video/link/code/…) to `message_id`
/// (number = `MAX(number)+1`) inside the caller's transaction and return its new
/// id. Used by finalize to persist parts streamed via `StreamingEvent::Part`
/// (FR-024 Phase B). Citations are inserted separately by the caller.
async fn append_part<R>(
    runner: &R,
    scope: &AccessScope,
    message_id: Uuid,
    part: &MessagePartInput,
) -> Result<Uuid, ChatEngineError>
where
    R: DBRunner,
{
    let number = compute_next_part_number(runner, message_id).await?;
    let part_id = Uuid::new_v4();
    let am = message_part_entity::ActiveModel {
        id: Set(part_id),
        message_id: Set(message_id),
        r#type: Set(part_type_to_entity(&part.part_type)),
        content: Set(part.content.clone()),
        number: Set(number),
    };
    MessagePartEntity::insert(am)
        .secure()
        .scope_unchecked(scope)?
        .exec(runner)
        .await?;
    Ok(part_id)
}

/// Persist the plugin's citations/references for a finalized `text` part into
/// the three child tables, numbered `0..n` in list order, inside the caller's
/// transaction (FR-023). The payloads are stored verbatim as JSON.
async fn insert_part_citations<R>(
    runner: &R,
    scope: &AccessScope,
    part_id: Uuid,
    citations: &PartCitations,
) -> Result<(), ChatEngineError>
where
    R: DBRunner,
{
    for (idx, c) in citations.file_citations.iter().enumerate() {
        let am = file_citation_entity::ActiveModel {
            id: Set(Uuid::new_v4()),
            message_part_id: Set(part_id),
            content: Set(serde_json::to_value(c).unwrap_or(JsonValue::Null)),
            number: Set(i32::try_from(idx).unwrap_or(i32::MAX)),
        };
        file_citation_entity::Entity::insert(am)
            .secure()
            .scope_unchecked(scope)?
            .exec(runner)
            .await?;
    }
    for (idx, c) in citations.link_citations.iter().enumerate() {
        let am = link_citation_entity::ActiveModel {
            id: Set(Uuid::new_v4()),
            message_part_id: Set(part_id),
            content: Set(serde_json::to_value(c).unwrap_or(JsonValue::Null)),
            number: Set(i32::try_from(idx).unwrap_or(i32::MAX)),
        };
        link_citation_entity::Entity::insert(am)
            .secure()
            .scope_unchecked(scope)?
            .exec(runner)
            .await?;
    }
    for (idx, r) in citations.references.iter().enumerate() {
        let am = link_reference_entity::ActiveModel {
            id: Set(Uuid::new_v4()),
            message_part_id: Set(part_id),
            content: Set(serde_json::to_value(r).unwrap_or(JsonValue::Null)),
            number: Set(i32::try_from(idx).unwrap_or(i32::MAX)),
        };
        link_reference_entity::Entity::insert(am)
            .secure()
            .scope_unchecked(scope)?
            .exec(runner)
            .await?;
    }
    Ok(())
}
