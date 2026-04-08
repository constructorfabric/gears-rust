use modkit_db::secure::Scopable;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "chat_turns")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
#[allow(clippy::struct_field_names)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    pub request_id: Uuid,
    pub requester_type: String,
    pub requester_user_id: Option<Uuid>,
    pub state: TurnState,
    pub provider_name: Option<String>,
    pub provider_response_id: Option<String>,
    pub assistant_message_id: Option<Uuid>,
    pub error_code: Option<String>,
    #[sea_orm(column_type = "Text")]
    pub error_detail: Option<String>,
    pub reserve_tokens: Option<i64>,
    pub max_output_tokens_applied: Option<i32>,
    pub reserved_credits_micro: Option<i64>,
    pub policy_version_applied: Option<i64>,
    pub effective_model: Option<String>,
    pub minimal_generation_floor_applied: Option<i32>,
    pub deleted_at: Option<OffsetDateTime>,
    pub replaced_by_request_id: Option<Uuid>,
    pub started_at: OffsetDateTime,
    pub completed_at: Option<OffsetDateTime>,
    pub updated_at: OffsetDateTime,
}

/// Turn lifecycle states. Only `Running` is non-terminal.
/// Allowed transitions: Running → Completed | Failed | Cancelled.
#[derive(Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(16))")]
pub enum TurnState {
    #[sea_orm(string_value = "running")]
    Running,
    #[sea_orm(string_value = "completed")]
    Completed,
    #[sea_orm(string_value = "failed")]
    Failed,
    #[sea_orm(string_value = "cancelled")]
    Cancelled,
}

impl TurnState {
    /// Returns `true` if the state is terminal (no further transitions).
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Running)
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
