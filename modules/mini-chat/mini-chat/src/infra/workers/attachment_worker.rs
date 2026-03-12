use std::sync::Arc;

use bytes::Bytes;
use mini_chat_sdk::models::{AttachmentKind, AttachmentStatus};
use modkit_macros::domain_model;
use modkit_security::{AccessScope, SecurityContext};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::config::MiniChatConfig;
use crate::domain::error::DomainError;
use crate::domain::repos::{AttachmentRepository, VectorStoreRepository};
use crate::domain::service::DbProvider;
use crate::infra::image::thumbnail::{self, ThumbnailConfig};
use crate::infra::llm::provider_resolver::ProviderResolver;
use crate::infra::llm::request::{
    Feature, LlmMessage, LlmRequestBuilder, RequestMetadata, RequestType,
};
use crate::infra::oagw::files_client::OagwFilesClient;
use crate::infra::oagw::vector_store_client::OagwVectorStoreClient;

/// Attachment processing worker.
///
/// Holds all dependencies needed for background attachment processing
/// (provider upload, thumbnails, vector store, doc summary).
/// Called by the outbox handler via `process_from_outbox()`.
#[domain_model]
pub struct AttachmentWorker<A: AttachmentRepository + 'static, V: VectorStoreRepository + 'static> {
    db: Arc<DbProvider>,
    oagw_files: Arc<OagwFilesClient>,
    oagw_vector_stores: Arc<OagwVectorStoreClient>,
    attachment_repo: Arc<A>,
    vector_store_repo: Arc<V>,
    provider_resolver: Arc<ProviderResolver>,
    config: Arc<MiniChatConfig>,
}

impl<A: AttachmentRepository + 'static, V: VectorStoreRepository + 'static> AttachmentWorker<A, V> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Arc<DbProvider>,
        oagw_files: Arc<OagwFilesClient>,
        oagw_vector_stores: Arc<OagwVectorStoreClient>,
        attachment_repo: Arc<A>,
        vector_store_repo: Arc<V>,
        provider_resolver: Arc<ProviderResolver>,
        config: Arc<MiniChatConfig>,
    ) -> Self {
        Self {
            db,
            oagw_files,
            oagw_vector_stores,
            attachment_repo,
            vector_store_repo,
            provider_resolver,
            config,
        }
    }

    /// Process an attachment from an outbox event.
    ///
    /// Loads the `upload_blob` from the DB (crash-safe: bytes survive pod restart),
    /// constructs the necessary context, and delegates to the existing `process_upload`.
    /// After successful provider upload, clears `upload_blob` to reclaim DB space.
    pub async fn process_from_outbox(
        &self,
        event: &crate::domain::repos::attachment_outbox::AttachmentProcessingEvent,
    ) -> Result<(), DomainError> {
        // Build a system security context for background processing
        let ctx = SecurityContext::builder()
            .subject_id(event.uploaded_by_user_id)
            .subject_tenant_id(event.tenant_id)
            .build()
            .map_err(|e| DomainError::internal(format!("build security context: {e}")))?;
        let scope = AccessScope::for_tenant(event.tenant_id);

        let conn = self.db.conn()?;

        // Load the upload_blob from the attachment row
        let blob = self
            .attachment_repo
            .load_upload_blob(&conn, &scope, event.attachment_id)
            .await?;

        let Some(blob) = blob else {
            // upload_blob is NULL — already processed (idempotent skip)
            info!(
                attachment_id = %event.attachment_id,
                "upload_blob is NULL, skipping (already processed)"
            );
            return Ok(());
        };

        let kind = match event.attachment_kind.as_str() {
            "image" => AttachmentKind::Image,
            _ => AttachmentKind::Document,
        };

        self.process_upload(
            ctx,
            scope.clone(),
            event.attachment_id,
            event.tenant_id,
            event.chat_id,
            time::OffsetDateTime::now_utc(), // approximation; exact created_at not critical for vector store metadata
            event.filename.clone(),
            event.content_type.clone(),
            kind,
            Bytes::from(blob),
        )
        .await?;

        // Clear upload_blob after successful processing to reclaim DB space.
        self.attachment_repo
            .clear_upload_blob(&conn, &scope, event.attachment_id)
            .await?;

        info!(attachment_id = %event.attachment_id, "upload_blob cleared after processing");
        Ok(())
    }

    /// Background processing for a single attachment upload.
    #[allow(clippy::too_many_arguments)]
    async fn process_upload(
        &self,
        ctx: SecurityContext,
        scope: AccessScope,
        attachment_id: Uuid,
        tenant_id: Uuid,
        chat_id: Uuid,
        created_at: time::OffsetDateTime,
        filename: String,
        content_type: String,
        kind: AttachmentKind,
        data: Bytes,
    ) -> Result<(), DomainError> {
        // 1. Upload file to provider via OAGW
        let provider_file_id = match self
            .oagw_files
            .upload_file(ctx.clone(), &filename, &content_type, data.clone())
            .await
        {
            Ok(id) => id,
            Err(e) => {
                error!(attachment_id = %attachment_id, error = %e, "Provider upload failed");
                let conn = self.db.conn()?;
                self.attachment_repo
                    .update_status(
                        &conn,
                        &scope,
                        attachment_id,
                        AttachmentStatus::Failed,
                        None,
                        Some("provider_upload_failed".to_owned()),
                    )
                    .await?;
                return Err(e);
            }
        };

        // 2. Dispatch by kind
        match kind {
            AttachmentKind::Image => {
                self.process_image(
                    attachment_id,
                    &content_type,
                    &data,
                    provider_file_id,
                    &scope,
                )
                .await?;
            }
            AttachmentKind::Document => {
                self.process_document(
                    ctx,
                    scope,
                    attachment_id,
                    tenant_id,
                    chat_id,
                    created_at,
                    filename,
                    content_type,
                    data,
                    provider_file_id,
                )
                .await?;
            }
        }

        info!(attachment_id = %attachment_id, kind = ?kind, "Attachment processing completed");
        Ok(())
    }

    /// Process an image attachment: generate thumbnail, then mark as ready.
    async fn process_image(
        &self,
        attachment_id: Uuid,
        content_type: &str,
        data: &[u8],
        provider_file_id: String,
        scope: &AccessScope,
    ) -> Result<(), DomainError> {
        self.try_generate_and_save_thumbnail(attachment_id, content_type, data, scope)
            .await;

        // Mark as ready
        let conn = self.db.conn()?;
        self.attachment_repo
            .update_status(
                &conn,
                scope,
                attachment_id,
                AttachmentStatus::Ready,
                Some(provider_file_id),
                None,
            )
            .await?;
        Ok(())
    }

    /// Generate and persist a thumbnail (fail-tolerant — errors are only logged).
    #[allow(clippy::cognitive_complexity)]
    async fn try_generate_and_save_thumbnail(
        &self,
        attachment_id: Uuid,
        content_type: &str,
        data: &[u8],
        scope: &AccessScope,
    ) {
        let thumb_config = ThumbnailConfig {
            width: self.config.attachments.thumbnail_width,
            height: self.config.attachments.thumbnail_height,
            max_bytes: self.config.attachments.thumbnail_max_bytes,
            max_pixels: self.config.attachments.thumbnail_max_pixels,
            max_decode_bytes: self.config.attachments.thumbnail_max_decode_bytes,
        };

        let result = match tokio::task::spawn_blocking({
            let data = data.to_vec();
            let ct = content_type.to_owned();
            move || thumbnail::generate_thumbnail(&data, &ct, &thumb_config)
        })
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => {
                warn!(attachment_id = %attachment_id, error = %e, "Thumbnail generation failed (continuing)");
                return;
            }
            Err(e) => {
                warn!(attachment_id = %attachment_id, error = %e, "Thumbnail task panicked (continuing)");
                return;
            }
        };

        let Ok(conn) = self.db.conn() else {
            warn!(attachment_id = %attachment_id, "Failed to get DB connection for thumbnail save");
            return;
        };
        #[allow(clippy::cast_possible_wrap)] // thumbnail dims are bounded by config
        let (tw, th) = (result.width as i32, result.height as i32);
        if let Err(e) = self
            .attachment_repo
            .update_thumbnail(&conn, scope, attachment_id, result.data, tw, th)
            .await
        {
            warn!(attachment_id = %attachment_id, error = %e, "Failed to save thumbnail");
        }
    }

    /// Process a document attachment: add to vector store, update status, generate summary.
    #[allow(clippy::too_many_arguments)]
    async fn process_document(
        &self,
        ctx: SecurityContext,
        scope: AccessScope,
        attachment_id: Uuid,
        tenant_id: Uuid,
        chat_id: Uuid,
        created_at: time::OffsetDateTime,
        filename: String,
        content_type: String,
        data: Bytes,
        provider_file_id: String,
    ) -> Result<(), DomainError> {
        // 1. Get-or-create vector store for chat
        let vector_store_ok = self
            .add_file_to_vector_store(
                &ctx,
                &scope,
                attachment_id,
                tenant_id,
                chat_id,
                created_at,
                &provider_file_id,
            )
            .await;

        // 2. Transition status based on vector store outcome
        let conn = self.db.conn()?;
        if vector_store_ok {
            self.attachment_repo
                .update_status(
                    &conn,
                    &scope,
                    attachment_id,
                    AttachmentStatus::Ready,
                    Some(provider_file_id.clone()),
                    None,
                )
                .await?;
        } else {
            self.attachment_repo
                .update_status(
                    &conn,
                    &scope,
                    attachment_id,
                    AttachmentStatus::Failed,
                    Some(provider_file_id.clone()),
                    Some("vector_store_add_failed".to_owned()),
                )
                .await?;
            // Still return Ok — the background task itself didn't panic,
            // and the attachment record captures the failure.
            return Ok(());
        }

        // 3. Generate doc summary (best-effort — failure doesn't affect attachment status).
        if let Err(e) = self
            .generate_doc_summary(
                ctx,
                scope,
                attachment_id,
                chat_id,
                &filename,
                &content_type,
                &data,
            )
            .await
        {
            warn!(
                attachment_id = %attachment_id,
                error = %e,
                "Doc summary generation failed (attachment stays ready)"
            );
        }

        Ok(())
    }

    /// Try to add a file to the chat's vector store. Returns `true` on success.
    #[allow(clippy::too_many_arguments, clippy::cognitive_complexity)]
    async fn add_file_to_vector_store(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        attachment_id: Uuid,
        tenant_id: Uuid,
        chat_id: Uuid,
        created_at: time::OffsetDateTime,
        provider_file_id: &str,
    ) -> bool {
        let store = match get_or_create_vector_store(
            ctx,
            scope,
            tenant_id,
            chat_id,
            &self.config.oagw_alias,
            &self.db,
            &self.oagw_vector_stores,
            &self.vector_store_repo,
        )
        .await
        {
            Ok(s) => s,
            Err(e) => {
                error!(chat_id = %chat_id, error = %e, "Failed to get-or-create vector store");
                return false;
            }
        };

        let Some(vs_id) = &store.vector_store_id else {
            // vector_store_id is None — provider store not created
            return false;
        };

        let uploaded_at = created_at
            .format(&time::format_description::well_known::Iso8601::DEFAULT)
            .unwrap_or_default();

        if let Err(e) = self
            .oagw_vector_stores
            .add_file_to_vector_store(
                ctx.clone(),
                vs_id,
                provider_file_id,
                &attachment_id.to_string(),
                &uploaded_at,
            )
            .await
        {
            error!(attachment_id = %attachment_id, error = %e, "Failed to add file to vector store");
            return false;
        }

        // Increment file_count (best-effort)
        if let Ok(conn) = self.db.conn()
            && let Err(e) = self
                .vector_store_repo
                .increment_file_count(&conn, scope, store.id)
                .await
        {
            warn!(error = %e, "Failed to increment vector store file count");
        }
        true
    }

    /// Generate a document summary via LLM (best-effort).
    ///
    /// Only text-decodable formats are summarized in P1. Binary formats (PDF, DOCX, etc.)
    /// are skipped — they are still indexed in the vector store for RAG/file-search.
    #[allow(clippy::too_many_arguments, clippy::cognitive_complexity)]
    async fn generate_doc_summary(
        &self,
        ctx: SecurityContext,
        scope: AccessScope,
        attachment_id: Uuid,
        chat_id: Uuid,
        filename: &str,
        content_type: &str,
        data: &[u8],
    ) -> Result<(), DomainError> {
        // 1. Only summarize text-decodable formats in P1
        if !TEXT_DECODABLE_TYPES.contains(&content_type) {
            info!(
                attachment_id = %attachment_id,
                content_type = %content_type,
                "Skipping doc summary for binary format"
            );
            return Ok(());
        }

        // 2. Decode bytes to UTF-8
        let text = String::from_utf8_lossy(data);
        if text.trim().is_empty() {
            info!(attachment_id = %attachment_id, "Skipping doc summary for empty document");
            return Ok(());
        }

        // 3. Truncate and call LLM
        let truncated =
            truncate_text_for_summary(&text, self.config.attachments.doc_summary_max_input_chars);
        let result = call_summary_llm(
            ctx,
            chat_id,
            filename,
            content_type,
            &truncated,
            &self.provider_resolver,
            &self.config,
        )
        .await?;

        let summary = result.content.trim().to_owned();
        if summary.is_empty() {
            warn!(attachment_id = %attachment_id, "LLM returned empty doc summary");
            return Ok(());
        }

        // 4. Persist summary
        let model = &self.config.attachments.doc_summary_model;
        let conn = self.db.conn()?;
        self.attachment_repo
            .update_doc_summary(&conn, &scope, attachment_id, summary, model.clone())
            .await?;

        info!(
            attachment_id = %attachment_id,
            model = %model,
            input_tokens = result.usage.input_tokens,
            output_tokens = result.usage.output_tokens,
            "Doc summary generated"
        );

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helper functions (not worker methods — shared logic)
// ---------------------------------------------------------------------------

/// Content types for which we can extract text directly from raw bytes.
const TEXT_DECODABLE_TYPES: &[&str] = &[
    "text/plain",
    "text/markdown",
    "text/csv",
    "application/json",
];

/// Truncate text to a maximum number of characters, appending a notice if truncated.
fn truncate_text_for_summary(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count > max_chars {
        let boundary = text
            .char_indices()
            .nth(max_chars)
            .map_or(text.len(), |(i, _)| i);
        format!(
            "{}\n\n[Document truncated \u{2014} showing first {} of {} characters]",
            &text[..boundary],
            max_chars,
            char_count
        )
    } else {
        text.to_owned()
    }
}

/// Build and execute a doc-summary LLM request, returning the response.
async fn call_summary_llm(
    ctx: SecurityContext,
    chat_id: Uuid,
    filename: &str,
    content_type: &str,
    truncated_text: &str,
    provider_resolver: &ProviderResolver,
    config: &MiniChatConfig,
) -> Result<crate::infra::llm::ResponseResult, DomainError> {
    let model = &config.attachments.doc_summary_model;
    let provider_id = &config.attachments.doc_summary_provider_id;
    let resolved = provider_resolver
        .resolve(provider_id)
        .map_err(|e| DomainError::Internal {
            message: format!("doc summary provider resolution failed: {e}"),
        })?;

    let user_content =
        format!("Filename: {filename}\nContent-Type: {content_type}\n\n{truncated_text}");
    let request = LlmRequestBuilder::new(model)
        .system_instructions(&config.attachments.doc_summary_prompt)
        .message(LlmMessage::user(user_content))
        .metadata(RequestMetadata {
            tenant_id: ctx.subject_tenant_id().to_string(),
            user_id: "system".to_owned(),
            chat_id: chat_id.to_string(),
            request_type: RequestType::DocSummary,
            feature: Feature::None,
        })
        .build_non_streaming();

    resolved
        .adapter
        .complete(ctx, request, resolved.upstream_alias)
        .await
        .map_err(|e| DomainError::Internal {
            message: format!("doc summary LLM call failed: {e}"),
        })
}

/// Get-or-create vector store: insert-first protocol (DESIGN §Creation protocol).
///
/// **Deviation from PLAN §Step-6 "Cleanup on provider failure"**: the PLAN specifies a
/// transaction-based rollback on provider creation failure. This implementation instead
/// uses `delete_if_null` (DELETE WHERE `vector_store_id` IS NULL) to clean up the orphaned
/// row, which avoids holding a long-running transaction across the OAGW network call
/// while preserving the same correctness guarantee (only NULL rows are removed).
#[allow(clippy::too_many_arguments)]
async fn get_or_create_vector_store<V: VectorStoreRepository>(
    ctx: &SecurityContext,
    scope: &AccessScope,
    tenant_id: Uuid,
    chat_id: Uuid,
    provider: &str,
    db: &Arc<DbProvider>,
    oagw_vector_stores: &OagwVectorStoreClient,
    vector_store_repo: &Arc<V>,
) -> Result<mini_chat_sdk::models::ChatVectorStore, DomainError> {
    let conn = db.conn()?;

    // Try to insert first (winner path)
    match vector_store_repo
        .insert_if_absent(&conn, scope, tenant_id, chat_id, provider)
        .await
    {
        Ok(store) => {
            // Winner: we inserted the row. Now create the provider vector store.
            create_provider_vector_store(
                ctx,
                scope,
                tenant_id,
                chat_id,
                store,
                db,
                oagw_vector_stores,
                vector_store_repo,
            )
            .await
        }
        Err(DomainError::AlreadyExists { .. }) => {
            // Loser path: row already exists (unique constraint violation).
            wait_for_vector_store_ready(scope, tenant_id, chat_id, db, vector_store_repo).await
        }
        Err(e) => Err(e),
    }
}

/// Winner path: create the provider-side vector store and update the DB row.
#[allow(clippy::too_many_arguments)]
async fn create_provider_vector_store<V: VectorStoreRepository>(
    ctx: &SecurityContext,
    scope: &AccessScope,
    tenant_id: Uuid,
    chat_id: Uuid,
    store: mini_chat_sdk::models::ChatVectorStore,
    db: &Arc<DbProvider>,
    oagw_vector_stores: &OagwVectorStoreClient,
    vector_store_repo: &Arc<V>,
) -> Result<mini_chat_sdk::models::ChatVectorStore, DomainError> {
    let vs_name = format!("mini-chat-{chat_id}");
    let provider_vs_id = match oagw_vector_stores
        .create_vector_store(ctx.clone(), &vs_name)
        .await
    {
        Ok(id) => id,
        Err(e) => {
            // Provider creation failed — clean up the orphaned NULL row
            cleanup_null_vector_store_row(scope, store.id, db, vector_store_repo).await;
            error!(error = %e, "Failed to create provider vector store");
            return Err(e);
        }
    };

    // Conditional UPDATE — only set if still NULL (race guard)
    let conn = db.conn()?;
    let updated = vector_store_repo
        .set_vector_store_id(&conn, scope, store.id, &provider_vs_id)
        .await?;

    if updated {
        return Ok(mini_chat_sdk::models::ChatVectorStore {
            vector_store_id: Some(provider_vs_id),
            ..store
        });
    }

    // Another request set it first — resolve race
    resolve_vector_store_race(
        ctx,
        scope,
        tenant_id,
        chat_id,
        &provider_vs_id,
        db,
        oagw_vector_stores,
        vector_store_repo,
    )
    .await
}

/// Best-effort cleanup of a DB row with NULL `vector_store_id`.
async fn cleanup_null_vector_store_row<V: VectorStoreRepository>(
    scope: &AccessScope,
    store_id: Uuid,
    db: &Arc<DbProvider>,
    vector_store_repo: &Arc<V>,
) {
    let Ok(conn) = db.conn() else { return };
    if let Err(e) = vector_store_repo
        .delete_if_null(&conn, scope, store_id)
        .await
    {
        warn!(error = %e, "Failed to clean up NULL vector store row");
    }
}

/// Handle the race where another request already set `vector_store_id`:
/// delete our orphan store and re-read the existing mapping.
#[allow(clippy::too_many_arguments)]
async fn resolve_vector_store_race<V: VectorStoreRepository>(
    ctx: &SecurityContext,
    scope: &AccessScope,
    tenant_id: Uuid,
    chat_id: Uuid,
    orphan_vs_id: &str,
    db: &Arc<DbProvider>,
    oagw_vector_stores: &OagwVectorStoreClient,
    vector_store_repo: &Arc<V>,
) -> Result<mini_chat_sdk::models::ChatVectorStore, DomainError> {
    warn!("Vector store race: another request set vector_store_id first, deleting orphan");
    if let Err(e) = oagw_vector_stores
        .delete_vector_store(ctx.clone(), orphan_vs_id)
        .await
    {
        warn!(
            vector_store_id = %orphan_vs_id,
            error = %e,
            "Failed to delete orphan vector store during race resolution"
        );
    }
    // Re-read existing mapping
    let conn = db.conn()?;
    vector_store_repo
        .find_by_chat(&conn, scope, tenant_id, chat_id)
        .await?
        .ok_or_else(|| DomainError::Internal {
            message: "vector store row disappeared after race".to_owned(),
        })
}

/// Loser path: wait for the winner to finish creating the provider vector store.
async fn wait_for_vector_store_ready<V: VectorStoreRepository>(
    scope: &AccessScope,
    tenant_id: Uuid,
    chat_id: Uuid,
    db: &Arc<DbProvider>,
    vector_store_repo: &Arc<V>,
) -> Result<mini_chat_sdk::models::ChatVectorStore, DomainError> {
    let mut retries: u64 = 0;
    loop {
        let conn = db.conn()?;
        let existing = vector_store_repo
            .find_by_chat(&conn, scope, tenant_id, chat_id)
            .await?;
        match existing {
            Some(store) if store.vector_store_id.is_some() => return Ok(store),
            Some(_) => {
                // Winner's creation is in progress — retry with backoff
                retries += 1;
                if retries > 10 {
                    return Err(DomainError::Internal {
                        message: "vector store creation timed out (loser backoff exceeded)"
                            .to_owned(),
                    });
                }
                tokio::time::sleep(std::time::Duration::from_millis(100 * retries)).await;
            }
            None => {
                return Err(DomainError::Internal {
                    message: "vector store row not found after constraint violation".to_owned(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use async_trait::async_trait;
    use oagw_sdk::body::Body;
    use oagw_sdk::error::ServiceGatewayError;
    use oagw_sdk::{
        CreateRouteRequest, CreateUpstreamRequest, ListQuery, Route, ServiceGatewayClientV1,
        UpdateRouteRequest, UpdateUpstreamRequest, Upstream,
    };

    use crate::domain::service::test_helpers::{inmem_db, mock_db_provider};

    // -- truncate_text_for_summary -----------------------------------------

    #[test]
    fn truncate_under_limit_returns_unchanged() {
        assert_eq!(truncate_text_for_summary("hello", 100), "hello");
    }

    #[test]
    fn truncate_at_exact_limit_returns_unchanged() {
        assert_eq!(truncate_text_for_summary("12345", 5), "12345");
    }

    #[test]
    fn truncate_over_limit_appends_notice() {
        let result = truncate_text_for_summary("abcdefghij", 5);
        assert!(result.starts_with("abcde"));
        assert!(result.contains("[Document truncated"));
        assert!(result.contains("5 of 10"));
    }

    #[test]
    fn truncate_multibyte_at_char_boundary() {
        // 6 multibyte chars + space + 3 more = 10 chars total
        let text = "\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442} \u{43c}\u{438}\u{440}";
        assert_eq!(text.chars().count(), 10);
        let result = truncate_text_for_summary(text, 6);
        // Must truncate at char boundary, not byte boundary.
        assert!(result.starts_with("\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442}"));
        assert!(!result.starts_with("\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442} "));
        assert!(result.contains("6 of 10"));
    }

    // -- worker lifecycle --------------------------------------------------

    /// Stub gateway that panics on any call — only for tests that never
    /// process commands (e.g. immediate channel close).
    struct NopGateway;

    #[async_trait]
    impl ServiceGatewayClientV1 for NopGateway {
        async fn proxy_request(
            &self,
            _: SecurityContext,
            _: http::Request<Body>,
        ) -> Result<http::Response<Body>, ServiceGatewayError> {
            unimplemented!()
        }
        async fn create_upstream(
            &self,
            _: SecurityContext,
            _: CreateUpstreamRequest,
        ) -> Result<Upstream, ServiceGatewayError> {
            unimplemented!()
        }
        async fn get_upstream(
            &self,
            _: SecurityContext,
            _: Uuid,
        ) -> Result<Upstream, ServiceGatewayError> {
            unimplemented!()
        }
        async fn list_upstreams(
            &self,
            _: SecurityContext,
            _: &ListQuery,
        ) -> Result<Vec<Upstream>, ServiceGatewayError> {
            unimplemented!()
        }
        async fn update_upstream(
            &self,
            _: SecurityContext,
            _: Uuid,
            _: UpdateUpstreamRequest,
        ) -> Result<Upstream, ServiceGatewayError> {
            unimplemented!()
        }
        async fn delete_upstream(
            &self,
            _: SecurityContext,
            _: Uuid,
        ) -> Result<(), ServiceGatewayError> {
            unimplemented!()
        }
        async fn create_route(
            &self,
            _: SecurityContext,
            _: CreateRouteRequest,
        ) -> Result<Route, ServiceGatewayError> {
            unimplemented!()
        }
        async fn get_route(
            &self,
            _: SecurityContext,
            _: Uuid,
        ) -> Result<Route, ServiceGatewayError> {
            unimplemented!()
        }
        async fn list_routes(
            &self,
            _: SecurityContext,
            _: Uuid,
            _: &ListQuery,
        ) -> Result<Vec<Route>, ServiceGatewayError> {
            unimplemented!()
        }
        async fn update_route(
            &self,
            _: SecurityContext,
            _: Uuid,
            _: UpdateRouteRequest,
        ) -> Result<Route, ServiceGatewayError> {
            unimplemented!()
        }
        async fn delete_route(
            &self,
            _: SecurityContext,
            _: Uuid,
        ) -> Result<(), ServiceGatewayError> {
            unimplemented!()
        }
        async fn resolve_upstream(
            &self,
            _: SecurityContext,
            _: &str,
        ) -> Result<Upstream, ServiceGatewayError> {
            unimplemented!()
        }
        async fn resolve_route(
            &self,
            _: SecurityContext,
            _: Uuid,
            _: &str,
            _: &str,
        ) -> Result<Route, ServiceGatewayError> {
            unimplemented!()
        }
    }

    fn build_test_worker(
        db: &Arc<crate::domain::service::DbProvider>,
    ) -> Arc<
        AttachmentWorker<
            crate::infra::db::repo::attachment_repo::AttachmentRepository,
            crate::infra::db::repo::vector_store_repo::VectorStoreRepository,
        >,
    > {
        let gw: Arc<dyn ServiceGatewayClientV1> = Arc::new(NopGateway);
        let config = Arc::new(crate::config::MiniChatConfig::default());
        Arc::new(AttachmentWorker::new(
            Arc::clone(db),
            Arc::new(OagwFilesClient::new(Arc::clone(&gw), "openai".to_owned())),
            Arc::new(OagwVectorStoreClient::new(
                Arc::clone(&gw),
                "openai".to_owned(),
            )),
            Arc::new(crate::infra::db::repo::attachment_repo::AttachmentRepository),
            Arc::new(crate::infra::db::repo::vector_store_repo::VectorStoreRepository),
            Arc::new(ProviderResolver::new(&gw, config.providers.clone())),
            config,
        ))
    }
}
