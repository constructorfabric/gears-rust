use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::planner::processing::{Changes, Managed};
use crate::domain::profile::Category;
use crate::domain::profile_read::{OriginKind, ProfileView, StoredFact};
use crate::domain::record_intake::IntakeOutcome;
use crate::domain::review_queue::{
    MAX_TEXT_CHARS, Resolution, ReviewOutcome, ReviewRequest, ReviewState,
};
use crate::domain::subject_control::{SubjectExport, SubjectFact, SubjectFactsPage};
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
    /// The fact's property, by its plain key, such as `skills`. A category
    /// has several properties, so this names which one the fact holds.
    pub property: String,
    /// The property's value: a string, or an object for a property with
    /// fields, such as `{"name": "Rust"}` for a skill.
    pub value: serde_json::Value,
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
        Self {
            fact_id: fact.node_key.0,
            property: fact.property,
            value: fact.value,
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

/// One of the subject's facts: the profile read's fact with its category,
/// and its latest review request, or none.
///
/// @cpt-dod:cpt-cf-construct-dod-subject-control-view:p1
/// @cpt-dod:cpt-cf-construct-dod-review-queue-subject-control:p1
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct SubjectFactDto {
    pub entity_kind: EntityKindDto,
    #[serde(flatten)]
    pub fact: ProfileFactDto,
    pub review_request: Option<FactReviewDto>,
}

impl From<SubjectFact> for SubjectFactDto {
    fn from(item: SubjectFact) -> Self {
        Self {
            entity_kind: item.fact.category.into(),
            fact: item.fact.into(),
            review_request: item.review_request.map(Into::into),
        }
    }
}

/// One fact of the export, which carries no review request: the export lists
/// the requests apart.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct ExportedFactDto {
    pub entity_kind: EntityKindDto,
    #[serde(flatten)]
    pub fact: ProfileFactDto,
}

impl From<StoredFact> for ExportedFactDto {
    fn from(fact: StoredFact) -> Self {
        Self {
            entity_kind: fact.category.into(),
            fact: fact.into(),
        }
    }
}

/// Whether a review request still waits for a reviewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum ReviewStateDto {
    Open,
    Closed,
}

impl From<ReviewState> for ReviewStateDto {
    fn from(state: ReviewState) -> Self {
        match state {
            ReviewState::Open => Self::Open,
            ReviewState::Closed => Self::Closed,
        }
    }
}

/// How a review request was, or is to be, closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(request, response)]
pub enum ReviewOutcomeDto {
    Corrected,
    Deleted,
    Rejected,
}

impl From<ReviewOutcome> for ReviewOutcomeDto {
    fn from(outcome: ReviewOutcome) -> Self {
        match outcome {
            ReviewOutcome::Corrected => Self::Corrected,
            ReviewOutcome::Deleted => Self::Deleted,
            ReviewOutcome::Rejected => Self::Rejected,
        }
    }
}

/// A fact's latest review request, as the subject's view shows it.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct FactReviewDto {
    #[schema(value_type = String)]
    pub id: Uuid,
    pub state: ReviewStateDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ReviewOutcomeDto>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}

impl From<ReviewRequest> for FactReviewDto {
    fn from(request: ReviewRequest) -> Self {
        Self {
            id: request.id,
            state: request.state.into(),
            outcome: request.outcome.map(Into::into),
            created_at: request.created_at,
        }
    }
}

/// One review request. `value` is the flagged value, given only while the
/// request is open.
///
/// @cpt-dod:cpt-cf-construct-dod-review-queue-subject:p1
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct ReviewRequestDto {
    #[schema(value_type = String)]
    pub id: Uuid,
    #[schema(value_type = String)]
    pub subject_id: Uuid,
    pub fact_id: String,
    pub entity_kind: EntityKindDto,
    pub property: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub state: ReviewStateDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ReviewOutcomeDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub resolved_at: Option<OffsetDateTime>,
    /// The id of the fact a correction stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corrected_fact_id: Option<String>,
}

impl From<ReviewRequest> for ReviewRequestDto {
    fn from(request: ReviewRequest) -> Self {
        Self {
            id: request.id,
            subject_id: request.subject_id,
            fact_id: request.fact_id.0,
            entity_kind: request.category.into(),
            property: request.property,
            value: request.value,
            comment: request.comment,
            state: request.state.into(),
            outcome: request.outcome.map(Into::into),
            reason: request.reason,
            created_at: request.created_at,
            resolved_at: request.resolved_at,
            corrected_fact_id: request.corrected_fact_id.map(|key| key.0),
        }
    }
}

/// A page of review requests.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct ReviewRequestsPageDto {
    pub items: Vec<ReviewRequestDto>,
    pub page_info: toolkit_odata::PageInfo,
}

impl From<toolkit_odata::Page<ReviewRequest>> for ReviewRequestsPageDto {
    fn from(page: toolkit_odata::Page<ReviewRequest>) -> Self {
        Self {
            items: page.items.into_iter().map(Into::into).collect(),
            page_info: page.page_info,
        }
    }
}

/// What the subject sends to flag one of its facts as incorrect.
#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct FlagFactRequest {
    /// The fact's id, as the facts list gives it.
    pub fact_id: String,
    /// What is wrong with it, at most 1 000 characters.
    #[serde(default)]
    #[schema(max_length = 1000)]
    pub comment: Option<String>,
}

/// A reviewer's decision on an open review request. `reason` is required to
/// reject; `properties` is required to correct, and only there: the fact's
/// own property with its new value.
#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct ResolutionRequest {
    pub outcome: ReviewOutcomeDto,
    /// Why, at most 1 000 characters.
    #[serde(default)]
    #[schema(max_length = 1000)]
    pub reason: Option<String>,
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub properties: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Why a resolution's body does not fit its outcome: the field and the rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyMistake {
    pub field: &'static str,
    pub rule: &'static str,
}

impl TryFrom<ResolutionRequest> for Resolution {
    type Error = BodyMistake;

    fn try_from(body: ResolutionRequest) -> Result<Self, BodyMistake> {
        let mistake = |field, rule| Err(BodyMistake { field, rule });
        let reason = body.reason.filter(|reason| !reason.trim().is_empty());
        if reason
            .as_ref()
            .is_some_and(|reason| reason.chars().count() > MAX_TEXT_CHARS)
        {
            return mistake("reason", "at most 1000 characters");
        }
        match (body.outcome, body.properties) {
            (ReviewOutcomeDto::Rejected, None) => match reason {
                Some(reason) => Ok(Self::Rejected { reason }),
                None => mistake("reason", "a rejection needs a reason"),
            },
            (ReviewOutcomeDto::Deleted, None) => Ok(Self::Deleted { reason }),
            (ReviewOutcomeDto::Corrected, Some(properties)) => {
                let mut properties = properties.into_iter();
                match (properties.next(), properties.next()) {
                    (Some((property, value)), None) => Ok(Self::Corrected {
                        property,
                        value,
                        reason,
                    }),
                    _ => mistake("properties", "a correction sets exactly one property"),
                }
            }
            (ReviewOutcomeDto::Corrected, None) => {
                mistake("properties", "a correction needs the new value")
            }
            (ReviewOutcomeDto::Rejected | ReviewOutcomeDto::Deleted, Some(_)) => {
                mistake("properties", "only a correction takes properties")
            }
        }
    }
}

/// A page of the subject's facts, and the version of the profile it was
/// cut from: a client polls the version to see whether anything changed.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct SubjectFactsPageDto {
    pub items: Vec<SubjectFactDto>,
    pub page_info: toolkit_odata::PageInfo,
    /// The profile's version; none for a subject without a profile.
    pub version: Option<u64>,
}

impl From<SubjectFactsPage> for SubjectFactsPageDto {
    fn from(page: SubjectFactsPage) -> Self {
        Self {
            items: page.page.items.into_iter().map(Into::into).collect(),
            page_info: page.page.page_info,
            version: page.version,
        }
    }
}

/// All of a subject's data, as the export file holds it.
///
/// @cpt-dod:cpt-cf-construct-dod-subject-control-export:p1
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct SubjectExportDto {
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    #[schema(value_type = String)]
    pub subject_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub exported_at: OffsetDateTime,
    pub settings: SubjectSettingsDto,
    pub facts: Vec<ExportedFactDto>,
    pub review_requests: Vec<ReviewRequestDto>,
}

impl SubjectExportDto {
    #[must_use]
    pub fn new(export: SubjectExport, exported_at: OffsetDateTime) -> Self {
        Self {
            tenant_id: export.tenant_id,
            subject_id: export.subject_id,
            exported_at,
            settings: SubjectSettingsDto::new(export.subject_id, export.settings),
            facts: export.facts.into_iter().map(Into::into).collect(),
            review_requests: export.review_requests.into_iter().map(Into::into).collect(),
        }
    }
}
