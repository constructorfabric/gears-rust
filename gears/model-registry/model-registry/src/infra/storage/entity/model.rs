//! `model_registry__models` table (DESIGN §3.7).
//!
//! JSON columns are the source of truth; the `cap_*`, `api_*`, and `ctx_*`
//! columns are promoted copies for filtering and ordering, rewritten on every
//! write.

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[allow(clippy::struct_excessive_bools, clippy::struct_field_names)]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "model_registry__models")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "id",
    no_owner,
    no_type,
    pep_prop(provider_id = "provider_id"),
    pep_prop(canonical_id = "canonical_id"),
    pep_prop(gts_type = "gts_type")
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub provider_id: Uuid,
    pub name: String,
    pub provider_model_id: String,
    pub canonical_id: String,
    pub gts_type: String,
    pub sharing_mode: String,
    pub approval_state: String,
    pub approval_decided_by: Uuid,
    pub approval_decided_at: DateTime<Utc>,
    pub base_url: Option<String>,
    pub secret_ref: Option<String>,
    pub display_name: String,
    pub description: Option<String>,
    pub family: Option<String>,
    pub vendor: Option<String>,
    pub managed: bool,
    pub architecture: Option<String>,
    pub size_bytes: Option<i64>,
    pub format: Option<String>,
    pub region: Option<String>,
    pub hosted_by: Option<String>,
    pub last_release_at: Option<DateTime<Utc>>,
    pub reasoning_level: Option<String>,
    pub model_version: Option<String>,
    pub sort_order: i32,
    pub icon: Option<String>,
    pub multiplier_display: Option<String>,
    #[sea_orm(column_type = "JsonBinary")]
    pub cost: serde_json::Value,
    #[sea_orm(column_type = "JsonBinary")]
    pub performance: serde_json::Value,
    #[sea_orm(column_type = "JsonBinary")]
    pub additional_info: serde_json::Value,
    #[sea_orm(column_type = "JsonBinary")]
    pub supported_api: serde_json::Value,
    pub api_completion: bool,
    pub api_embedding: bool,
    pub api_batch: bool,
    #[sea_orm(column_type = "JsonBinary")]
    pub capabilities: serde_json::Value,
    #[sea_orm(column_type = "JsonBinary")]
    pub disabled_capabilities: serde_json::Value,
    pub cap_vision: bool,
    pub cap_reasoning_effort: bool,
    pub cap_tools: bool,
    pub cap_json_schema: bool,
    pub cap_streaming: bool,
    pub cap_file_input: bool,
    pub cap_image_generation: bool,
    pub cap_audio_input: bool,
    pub cap_audio_output: bool,
    pub cap_code_interpreter: bool,
    pub cap_web_search: bool,
    pub ctx_max_input_tokens: i64,
    pub ctx_max_output_tokens: Option<i64>,
    pub ctx_output_vector_size: Option<i64>,
    #[sea_orm(column_type = "JsonBinary")]
    pub default_parameters: serde_json::Value,
    pub allow_parameter_override: bool,
    #[sea_orm(column_type = "JsonBinary")]
    pub allow_extra_params: serde_json::Value,
    #[sea_orm(column_type = "JsonBinary")]
    pub provider_settings: serde_json::Value,
    /// Row version: 1 on create, incremented by every update.
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub created_by: Uuid,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Uuid,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::provider::Entity",
        from = "Column::ProviderId",
        to = "super::provider::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Provider,
}

impl Related<super::provider::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Provider.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
