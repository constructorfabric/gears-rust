use std::sync::Arc;

use authz_resolver_sdk::pep::ResourceType;
use authz_resolver_sdk::{AuthZResolverClient, PolicyEnforcer};
use modkit_db::DBProvider;
use modkit_macros::domain_model;

use crate::config::{EstimationBudgets, MiniChatConfig, QuotaConfig, StreamingConfig};
use crate::domain::repos::{
    AttachmentOutboxEnqueuer, AttachmentRepository, ChatRepository, MessageRepository,
    ModelResolver, OutboxEnqueuer, PolicySnapshotProvider, QuotaUsageRepository,
    ReactionRepository, ThreadSummaryRepository, TurnRepository, UserLimitsProvider,
    VectorStoreRepository,
};
use crate::domain::service::quota_settler::QuotaSettler;
use crate::infra::llm::provider_resolver::ProviderResolver;

mod attachment_service;
mod chat_service;
pub(crate) mod credit_arithmetic;
pub(crate) mod finalization_service;
mod message_service;
mod model_service;
mod quota_service;
pub(crate) mod quota_settler;
mod reaction_service;
pub(crate) mod replay;
pub(crate) mod retrieval_scope;
mod stream_service;
#[cfg(test)]
pub(crate) mod test_helpers;
pub(crate) mod token_estimator;
mod turn_service;

pub(crate) use attachment_service::AttachmentService;
pub(crate) use chat_service::ChatService;
pub(crate) use finalization_service::FinalizationService;
pub(crate) use message_service::MessageService;
pub(crate) use model_service::ModelService;
pub(crate) use quota_service::QuotaService;
pub(crate) use reaction_service::ReactionService;
pub(crate) use stream_service::{StreamAttachmentContext, StreamError, StreamService};
pub(crate) use turn_service::{MutationError, MutationResult, TurnService};

pub(crate) type DbProvider = DBProvider<modkit_db::DbError>;

/// Authorization resource type for mini-chat.
///
/// All sub-resources (message, turn, attachment, reaction) inherit
/// authorization from the chat level — there is a single GTS resource type.
/// TODO: discuss with the team about resource type GTS identifier.
#[allow(dead_code)]
pub(crate) mod resources {
    use super::ResourceType;
    use modkit_security::pep_properties;

    pub const CHAT: ResourceType = ResourceType {
        name: "gts.cf.core.ai_chat.chat.v1~cf.core.mini_chat.chat.v1",
        supported_properties: &[
            pep_properties::OWNER_TENANT_ID,
            pep_properties::OWNER_ID,
            pep_properties::RESOURCE_ID,
        ],
    };

    pub const MODEL: ResourceType = ResourceType {
        name: "gts.cf.core.ai_chat.model.v1~cf.core.mini_chat.model.v1",
        supported_properties: &[pep_properties::OWNER_TENANT_ID],
    };
}

#[allow(dead_code)]
pub(crate) mod actions {
    pub const CREATE: &str = "create";
    pub const READ: &str = "read";
    pub const LIST: &str = "list";
    pub const UPDATE: &str = "update";
    pub const DELETE: &str = "delete";
    pub const LIST_MESSAGES: &str = "list_messages";
    pub const SEND_MESSAGE: &str = "send_message";
    pub const READ_TURN: &str = "read_turn";
    pub const RETRY_TURN: &str = "retry_turn";
    pub const EDIT_TURN: &str = "edit_turn";
    pub const DELETE_TURN: &str = "delete_turn";
    pub const UPLOAD_ATTACHMENT: &str = "upload_attachment";
    pub const READ_ATTACHMENT: &str = "read_attachment";
    pub const SET_REACTION: &str = "set_reaction";
    pub const DELETE_REACTION: &str = "delete_reaction";
}

/// All repository instances passed to `AppServices::new` as a single bundle.
#[domain_model]
pub(crate) struct Repositories<
    TR: TurnRepository,
    MR: MessageRepository,
    QR: QuotaUsageRepository,
    RR: ReactionRepository,
    CR: ChatRepository,
    A: AttachmentRepository + 'static,
    V: VectorStoreRepository + 'static,
> {
    pub(crate) chat: Arc<CR>,
    pub(crate) attachment: Arc<A>,
    pub(crate) message: Arc<MR>,
    pub(crate) quota: Arc<QR>,
    pub(crate) turn: Arc<TR>,
    pub(crate) reaction: Arc<RR>,
    pub(crate) thread_summary: Arc<dyn ThreadSummaryRepository>,
    pub(crate) vector_store: Arc<V>,
}

/// DI container — aggregates all domain services.
///
/// Created once during `Module::init` and shared with handlers via `Arc`.
/// Services acquire database connections via `DbProvider` and pass them
/// to stateless repositories per the canonical modkit pattern.
#[domain_model]
#[allow(dead_code)]
pub(crate) struct AppServices<
    TR: TurnRepository + 'static,
    MR: MessageRepository + 'static,
    QR: QuotaUsageRepository + 'static,
    RR: ReactionRepository + 'static,
    CR: ChatRepository + 'static,
    A: AttachmentRepository + 'static,
    V: VectorStoreRepository + 'static,
> {
    pub(crate) chats: ChatService<CR>,
    pub(crate) messages: MessageService<MR, CR>,
    pub(crate) stream: StreamService<TR, MR, QR, CR>,
    pub(crate) turns: TurnService<TR, MR, CR>,
    pub(crate) reactions: ReactionService<RR, MR, CR>,
    pub(crate) attachments: AttachmentService<A, V>,
    pub(crate) models: ModelService,
    pub(crate) quota: Arc<QuotaService<QR>>,
    pub(crate) finalization: Arc<FinalizationService<TR, MR>>,
    pub(crate) db: Arc<DbProvider>,
    pub(crate) message_repo: Arc<MR>,
    pub(crate) turn_repo: Arc<TR>,
    pub(crate) enforcer: PolicyEnforcer,
}

impl<
    TR: TurnRepository + 'static,
    MR: MessageRepository + 'static,
    QR: QuotaUsageRepository + 'static,
    RR: ReactionRepository + 'static,
    CR: ChatRepository + 'static,
    A: AttachmentRepository + 'static,
    V: VectorStoreRepository + 'static,
> AppServices<TR, MR, QR, RR, CR, A, V>
{
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        repos: &Repositories<TR, MR, QR, RR, CR, A, V>,
        db: Arc<DbProvider>,
        authz: Arc<dyn AuthZResolverClient>,
        model_resolver: &Arc<dyn ModelResolver>,
        provider_resolver: Arc<ProviderResolver>,
        streaming_config: StreamingConfig,
        policy_provider: Arc<dyn PolicySnapshotProvider>,
        limits_provider: Arc<dyn UserLimitsProvider>,
        estimation_budgets: EstimationBudgets,
        quota_config: QuotaConfig,
        outbox_enqueuer: Arc<dyn OutboxEnqueuer>,
        attachment_outbox: Arc<dyn AttachmentOutboxEnqueuer>,
        config: Arc<MiniChatConfig>,
    ) -> Self {
        let enforcer = PolicyEnforcer::new(authz);

        // Shared QuotaService used by both StreamService (preflight) and
        // FinalizationService (settlement via QuotaSettler trait).
        let quota_svc = Arc::new(QuotaService::new(
            Arc::clone(&db),
            Arc::clone(&repos.quota),
            policy_provider,
            limits_provider,
            estimation_budgets,
            quota_config,
        ));

        let finalization = Arc::new(FinalizationService::new(
            Arc::clone(&db),
            Arc::clone(&repos.turn),
            Arc::clone(&repos.message),
            Arc::clone(&quota_svc) as Arc<dyn QuotaSettler>,
            outbox_enqueuer,
        ));

        let turns = TurnService::new(
            Arc::clone(&db),
            Arc::clone(&repos.turn),
            Arc::clone(&repos.message),
            Arc::clone(&repos.chat),
            enforcer.clone(),
        );

        Self {
            chats: ChatService::new(
                Arc::clone(&db),
                Arc::clone(&repos.chat),
                Arc::clone(&repos.thread_summary),
                enforcer.clone(),
                Arc::clone(model_resolver),
            ),
            messages: MessageService::new(
                Arc::clone(&db),
                Arc::clone(&repos.message),
                Arc::clone(&repos.chat),
                enforcer.clone(),
            ),
            stream: StreamService::new(
                Arc::clone(&db),
                Arc::clone(&repos.turn),
                Arc::clone(&repos.message),
                Arc::clone(&repos.chat),
                enforcer.clone(),
                provider_resolver,
                streaming_config,
                Arc::clone(&finalization),
                Arc::clone(&quota_svc),
            ),
            turns,
            reactions: ReactionService::new(
                Arc::clone(&db),
                Arc::clone(&repos.reaction),
                Arc::clone(&repos.message),
                Arc::clone(&repos.chat),
                enforcer.clone(),
            ),
            attachments: AttachmentService::new(
                Arc::clone(&db),
                Arc::clone(&repos.attachment),
                Arc::clone(&repos.vector_store),
                enforcer.clone(),
                config,
                attachment_outbox,
            ),
            models: ModelService::new(
                Arc::clone(&db),
                enforcer.clone(),
                Arc::clone(model_resolver),
            ),
            quota: Arc::clone(&quota_svc),
            finalization,
            db,
            message_repo: Arc::clone(&repos.message),
            turn_repo: Arc::clone(&repos.turn),
            enforcer,
        }
    }
}
