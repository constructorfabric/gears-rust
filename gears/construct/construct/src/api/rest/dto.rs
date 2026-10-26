use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::planner::processing::{Changes, Managed};
use crate::domain::profile::Category;
use crate::domain::profile_read::{OriginKind, ProfileView, StoredFact};
use crate::domain::record_intake::IntakeOutcome;
use crate::domain::subject_settings::SubjectSettings;

/// One connector record, as JSON. Intake checks it against its GTS type, so
/// the body is taken as it is and not shaped by this DTO.
#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
#[schema(value_type = Object)]
pub struct RecordRequest(pub serde_json::Value);

/// The query of the intake route: the tenant the connector sends the record
/// for. The platform must authorize the connector for it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordQuery {
    pub tenant: Uuid,
}

/// The outcome of a record intake took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum OutcomeDto {
    /// Taken for processing.
    Received,
    /// The same identity was received before; nothing changes.
    Repeat,
}

/// What intake answers for a record it takes.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct RecordOutcomeDto {
    pub outcome: OutcomeDto,
}

impl From<IntakeOutcome> for RecordOutcomeDto {
    fn from(outcome: IntakeOutcome) -> Self {
        let outcome = match outcome {
            IntakeOutcome::Received => OutcomeDto::Received,
            IntakeOutcome::Repeat => OutcomeDto::Repeat,
        };
        Self { outcome }
    }
}

/// A subject's settings as the API returns them.
///
/// @cpt-dod:cpt-cf-construct-dod-subject-settings-routes:p1
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct SubjectSettingsDto {
    /// The subject the settings belong to: the caller.
    #[schema(value_type = String)]
    pub subject_id: Uuid,
    /// Whether personalization is on. A subject without stored settings has
    /// the tenant's default.
    pub personalization_enabled: bool,
    /// Whether an erasure of the subject is under way. While it is, nothing is
    /// personalized, even with personalization on.
    pub erasure_in_progress: bool,
}

impl SubjectSettingsDto {
    #[must_use]
    pub fn new(subject_id: Uuid, settings: SubjectSettings) -> Self {
        Self {
            subject_id,
            personalization_enabled: settings.personalization_enabled,
            erasure_in_progress: settings.erasure_in_progress,
        }
    }
}

/// What the subject sends to change its settings. Only personalization can be
/// changed; an unknown field, such as `erasure_in_progress`, is refused.
#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct PutSubjectSettingsRequest {
    /// Turn personalization on or off. Applies from the next request.
    pub personalization_enabled: bool,
}

/// The longest `message` a manage-facts call takes, in characters.
pub const MESSAGE_LIMIT: usize = 4_000;
/// The longest `conversation` a manage-facts call takes, in characters. It is sent again in every round of the agent
/// loop, so it is bounded by the token cap over ten rounds.
pub const CONVERSATION_LIMIT: usize = 16_000;

/// What the subject told an agent, for the gear to decide on and store.
#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct ManageFactsRequest {
    /// What the subject said, at most 4 000 characters.
    #[schema(max_length = 4000)]
    pub message: String,
    /// The agent's recent conversation, which shows what the message refers to, at most 16 000 characters. No fact is
    /// taken from it alone.
    #[serde(default)]
    #[schema(max_length = 16000)]
    pub conversation: String,
}

impl ManageFactsRequest {
    /// The first field longer than its limit, with the limit.
    #[must_use]
    pub fn field_past_its_limit(&self) -> Option<(&'static str, usize)> {
        [
            ("message", &self.message, MESSAGE_LIMIT),
            ("conversation", &self.conversation, CONVERSATION_LIMIT),
        ]
        .into_iter()
        .find(|(_, text, limit)| text.chars().count() > *limit)
        .map(|(field, _, limit)| (field, limit))
    }
}

/// Whether a manage-facts call stored anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum ManageOutcomeDto {
    /// The changes are stored.
    Stored,
    /// Nothing is stored; `reason` says why.
    NothingChanged,
}

/// What a manage-facts call did.
///
/// @cpt-dod:cpt-cf-construct-dod-planner-manage-route:p1
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct ManageFactsResponse {
    pub outcome: ManageOutcomeDto,
    /// Why nothing was stored: `no_change`, `timeout`, or a drop reason such as `personalization_off`. It never names
    /// a blocked value.
    pub reason: Option<String>,
    pub added: usize,
    pub replaced: usize,
    pub removed: usize,
}

impl From<Managed> for ManageFactsResponse {
    fn from(managed: Managed) -> Self {
        match managed {
            Managed::Stored(Changes {
                added,
                replaced,
                removed,
            }) => Self {
                outcome: ManageOutcomeDto::Stored,
                reason: None,
                added,
                replaced,
                removed,
            },
            Managed::NothingStored(nothing) => Self {
                outcome: ManageOutcomeDto::NothingChanged,
                reason: Some(nothing.name().to_owned()),
                added: 0,
                replaced: 0,
                removed: 0,
            },
        }
    }
}

/// The query of the profile read: the tenant, when the caller reads a subject
/// of a tenant other than its own login's. The platform must authorize the
/// caller for it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileQuery {
    #[serde(default)]
    pub tenant: Option<Uuid>,
}

/// Who a fact came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum OriginKindDto {
    Connector,
    Agent,
    Reviewer,
    Administrator,
}

impl From<OriginKind> for OriginKindDto {
    fn from(origin: OriginKind) -> Self {
        match origin {
            OriginKind::Connector => Self::Connector,
            OriginKind::Agent => Self::Agent,
            OriginKind::Reviewer => Self::Reviewer,
            OriginKind::Administrator => Self::Administrator,
        }
    }
}

/// Who a fact came from.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct OriginDto {
    pub kind: OriginKindDto,
}

/// A category of a person's profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum EntityKindDto {
    Identity,
    Role,
    Skill,
    Preference,
}

impl From<Category> for EntityKindDto {
    fn from(category: Category) -> Self {
        match category {
            Category::Identity => Self::Identity,
            Category::Role => Self::Role,
            Category::Skill => Self::Skill,
            Category::Preference => Self::Preference,
        }
    }
}

/// One fact: its id, its one property with its value, who it came from, when
/// it was stored, and when its source asserted it.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct ProfileFactDto {
    /// The fact's id, the graph node key.
    pub fact_id: String,
    /// The fact's property, by its plain key, with its value.
    #[schema(value_type = Object)]
    pub properties: serde_json::Map<String, serde_json::Value>,
    pub origin: OriginDto,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub stored_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub produced_at: Option<OffsetDateTime>,
}

/// The facts of one category.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct ProfileCategoryDto {
    pub entity_kind: EntityKindDto,
    pub facts: Vec<ProfileFactDto>,
}

/// A subject's profile by category. A category the caller may not read, or
/// without facts, is not listed.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-reader-route:p1
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct ProfileDto {
    #[schema(value_type = String)]
    pub subject_id: Uuid,
    pub categories: Vec<ProfileCategoryDto>,
}

impl From<StoredFact> for ProfileFactDto {
    fn from(fact: StoredFact) -> Self {
        let mut properties = serde_json::Map::new();
        properties.insert(fact.property, fact.value);
        Self {
            fact_id: fact.node_key.0,
            properties,
            origin: OriginDto {
                kind: fact.origin.into(),
            },
            stored_at: fact.stored_at,
            produced_at: fact.produced_at,
        }
    }
}

impl From<ProfileView> for ProfileDto {
    fn from(view: ProfileView) -> Self {
        Self {
            subject_id: view.subject_id,
            categories: view
                .categories
                .into_iter()
                .map(|group| ProfileCategoryDto {
                    entity_kind: group.category.into(),
                    facts: group.facts.into_iter().map(Into::into).collect(),
                })
                .collect(),
        }
    }
}
