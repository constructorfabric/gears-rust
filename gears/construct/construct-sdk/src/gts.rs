//! The GTS type ids of Construct: the connector record types it takes at intake, and the ids of its four person
//! types, whose schemas are in `person_types`.
//!
//! Each type is a Rust struct whose GTS type schema `#[gts_type_schema]`
//! generates and submits to the link-time schema inventory, which the types
//! registry drains when it starts: the connectors are not Rust and cannot
//! register their types themselves. This module is the source of truth of
//! the record types.
//!
//! One rule of the types is not in their schemas: a derived type's schema
//! can only refine `payload`, so it cannot make the top-level `subject_id`
//! required. Each type states it as the GTS trait `subject_required`
//! ([`RecordTraits`]) instead, which a derived type inherits and intake
//! enforces. It is on unless a type turns it off, so a type that says nothing
//! requires a subject.
//!
//! Other rules a derived schema cannot carry, because it refines only
//! `payload` and cannot tie one payload member to another, are in
//! [`check_type_rules`], which intake runs after the schema.
//!
//! The payload structs define the schemas; reading records into them has a
//! limit. `gts-macros` 0.12 (de)serializes a derived struct with its own code,
//! which ignores `#[serde(default)]` and `skip_serializing_if`: it reads every
//! payload member as required and writes an absent optional one as `null`,
//! which the schema refuses. A record whose payload leaves an optional member
//! out is valid but cannot be read into its payload struct. Intake checks the
//! JSON against the schemas and never reads the payload structs.
//!
//! @cpt-dod:cpt-cf-construct-dod-record-intake-types:p1

use gts::{GtsInstanceId, GtsSchema, GtsTypeId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use toolkit_gts::{GtsTraitsSchema, gts_id, gts_type_schema};
use uuid::Uuid;

/// A person's identity: names, external ids, public profiles, location, background.
pub const IDENTITY_TYPE: &str =
    gts_id!("cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.identity.v1~");
/// A person's roles: current role, education, programme, affiliations and work history.
pub const ROLE_TYPE: &str =
    gts_id!("cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.role.v1~");
/// A person's skills: skills, languages, research areas, publications, awards and research metrics.
pub const SKILL_TYPE: &str =
    gts_id!("cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.skill.v1~");
/// How a person wants to be served: language, format, tone, accessibility, constraints and goals.
pub const PREFERENCE_TYPE: &str =
    gts_id!("cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.preference.v1~");

/// The abstract base every connector record type derives from. It fixes the
/// envelope: `type`, `provenance`, `version`, `observed_at`, the optional
/// `subject_id`, and the `payload` a derived type refines.
pub const RECORD_BASE_TYPE: &str = <RecordV1<()> as GtsSchema>::TYPE_ID;

/// A raw chat message from the chat export.
pub const CHAT_MESSAGE_TYPE: &str = ChatMessageV1::TYPE_ID;

/// A fact candidate from an enrichment connector.
pub const ENRICHMENT_FACT_CANDIDATE_TYPE: &str = FactCandidateV1::TYPE_ID;

/// A course catalog entry from the mastery connector.
pub const MASTERY_COURSE_CATALOG_TYPE: &str = CourseCatalogV1::TYPE_ID;

/// A student's mastery from the mastery connector.
pub const MASTERY_STUDENT_MASTERY_TYPE: &str = StudentMasteryV1::TYPE_ID;

/// Every record type this crate registers: the base and its derived types.
pub const RECORD_TYPES: [&str; 5] = [
    RECORD_BASE_TYPE,
    CHAT_MESSAGE_TYPE,
    ENRICHMENT_FACT_CANDIDATE_TYPE,
    MASTERY_COURSE_CATALOG_TYPE,
    MASTERY_STUDENT_MASTERY_TYPE,
];

/// The name of the [`RecordTraits::subject_required`] trait in a type's
/// `x-gts-traits`.
pub const SUBJECT_REQUIRED_TRAIT: &str = "subject_required";

/// The schema of the record base type: the envelope every record must pass.
#[must_use]
pub fn record_base_schema() -> serde_json::Value {
    RecordV1::<()>::gts_schema_with_refs()
}

/// The schema of the record type `type_id` as this crate registers it, or
/// `None` for a type it does not define.
#[must_use]
pub fn record_type_schema(type_id: &str) -> Option<serde_json::Value> {
    Some(match type_id {
        RECORD_BASE_TYPE => record_base_schema(),
        CHAT_MESSAGE_TYPE => ChatMessageV1::gts_schema_with_refs(),
        ENRICHMENT_FACT_CANDIDATE_TYPE => FactCandidateV1::gts_schema_with_refs(),
        MASTERY_COURSE_CATALOG_TYPE => CourseCatalogV1::gts_schema_with_refs(),
        MASTERY_STUDENT_MASTERY_TYPE => StudentMasteryV1::gts_schema_with_refs(),
        _ => return None,
    })
}

// The traits of a record type, emitted as the base's `x-gts-traits-schema`.
// A plain comment: a doc comment would become the trait schema's
// description.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, GtsTraitsSchema,
)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RecordTraits {
    /// Whether a record of this type is about one person and so must set
    /// `subject_id`. On unless the type turns it off.
    #[serde(default = "subject_required_default")]
    pub subject_required: bool,
}

fn subject_required_default() -> bool {
    true
}

// The traits of a type that declares none: a subject is required.
impl Default for RecordTraits {
    fn default() -> Self {
        Self {
            subject_required: subject_required_default(),
        }
    }
}

/// The traits of a record type that is about one person.
fn about_a_person() -> serde_json::Value {
    serde_json::json!({ SUBJECT_REQUIRED_TRAIT: true })
}

/// The record envelope. `P` is the payload a derived type defines.
#[derive(Debug, Clone, PartialEq)]
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    gts_abstract = true,
    type_id = gts_id!("cf.connectors.core.record.v1~"),
    description = "Abstract base of one observed record inside a connector push. Dedupe and re-push idempotency are keyed on (tenant, connector, provenance, version) - tenant and connector identity from the push envelope, provenance and version from the record. The connector identity is one opaque value derived from the envelope's vendor and package, never the connector's build version. Provenance is the source-formed locator of the observed thing, version identifies its content at observation - re-pushing the same (provenance, version) is a no-op. Producer obligation: version MUST be derived from the observed content (each derived type documents its derivation): a producer obligation, not verifiable by the results API (the derivation input exists only at the source). Post-observation changes at the source are not propagated (forward-only delivery). The top level is frozen (additionalProperties: false): a derived content type refines only the payload member. A record about a person sets subject_id; a type states whether its records are about a person in the trait subject_required, on unless the type turns it off, and the results API enforces it. A record kept in a GTS store carries its GTS instance id in id; a record pushed to the results API carries none.",
    properties = "store_id,gts_type,provenance,version,subject_id,observed_at,payload",
    traits_schema = inline(RecordTraits),
    // The default value for the chain: a type that declares no traits
    // inherits it, and the gts store rejects a traits schema with no values.
    traits = about_a_person()
)]
pub struct RecordV1<P: GtsSchema> {
    // Named `store_id` here and `id` on the wire: the generator takes a base
    // with either a `GtsInstanceId` named `id` or a type member, and a record
    // needs `type`. A plain comment, so it stays out of the schema.
    /// The GTS instance id of a record kept in a GTS store: the type chain
    /// plus the instance's own final segment, without the trailing tilde. A
    /// record pushed to intake carries none.
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        with = "String",
        extend(
            "pattern" = r"^gts\.[a-z_][a-z0-9_]*(\.[a-z_][a-z0-9_]*){3}\.v[0-9]+(\.[0-9]+)?~([a-z_][a-z0-9_]*(\.[a-z_][a-z0-9_]*){3}\.v[0-9]+(\.[0-9]+)?~)*[a-z_][a-z0-9_]*(\.[a-z_][a-z0-9_]*){3}\.v[0-9]+(\.[0-9]+)?$"
        )
    )]
    pub store_id: Option<GtsInstanceId>,
    /// The derived content type this instance conforms to; it must derive
    /// from this base.
    #[serde(rename = "type")]
    #[schemars(extend(
        "x-gts-type-ref" = gts_id!("cf.connectors.core.record.v1~"),
        "maxLength" = 1024,
        "pattern" = r"^gts\.[a-z_][a-z0-9_]*(\.[a-z_][a-z0-9_]*){3}\.v[0-9]+(\.[0-9]+)?~([a-z_][a-z0-9_]*(\.[a-z_][a-z0-9_]*){3}\.v[0-9]+(\.[0-9]+)?~)+$"
    ))]
    pub gts_type: GtsTypeId,
    /// Opaque, source-formed locator of the observed thing, stable across
    /// re-pushes. E.g. for chat: source + thread id + message id.
    #[schemars(length(min = 1, max = 1024))]
    pub provenance: String,
    /// Identifies the record's content at observation, derived from that
    /// content per the producer obligation.
    #[schemars(length(min = 1, max = 256))]
    pub version: String,
    /// The person the record is about. Absent only for source-scoped records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        with = "Uuid",
        regex(
            pattern = r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"
        )
    )]
    pub subject_id: Option<Uuid>,
    /// When the connector formed the record. For a forwarding connector, when
    /// it observed the record at the source. For a computing connector, when
    /// it finished computing the record. Each derived type states in its own
    /// description which sense it uses.
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(
        with = "String",
        extend("format" = "date-time"),
        regex(pattern = r"^\d{4}-\d{2}-\d{2}[Tt]\d{2}:\d{2}:\d{2}(\.\d+)?([Zz]|[+-]\d{2}:\d{2})$")
    )]
    pub observed_at: OffsetDateTime,
    /// The observed content; derived types refine this member.
    pub payload: P,
}

/// Where a chat message was exported from. The values are fixed for
/// `chat.message.v1~`: a registered type version does not change, so a new
/// source needs a new version of the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum ChatSource {
    ChatEngine,
    AiSearch,
}

/// Who wrote a chat message, normalized to two values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum ChatRole {
    User,
    Assistant,
}

/// The payload of a chat message record.
#[derive(Debug, Clone, PartialEq, Eq)]
#[gts_type_schema(
    dir_path = "schemas",
    base = RecordV1,
    type_id = gts_id!("cf.connectors.core.record.v1~cf.construct.chat.message.v1~"),
    description = "One raw chat message from the normalized fetch-history export (chat-engine or ai-search). Models the export contract, not either source's storage: ids are opaque strings and no source-specific concept surfaces here (filtering on session origin/type stays inside each source service). Only fields immutable once the message completes are carried - the record's version is the message's created_at at the source and is not repeated in the payload; the record's observed_at is when the connector read the message from the source's export, the forwarding sense of the base. The role set is normalized to two values, matching the OpenAI chat-completion convention; the export maps source roles as: user/human -> user; assistant/ai/llm/agent -> assistant; system/developer messages are not exported. Record provenance = source + thread id + message id. The record's subject_id is always set.",
    properties = "source,message_id,thread_id,role,text",
    traits = about_a_person()
)]
pub struct ChatMessageV1 {
    /// The service the message was exported from.
    pub source: ChatSource,
    /// Opaque source message id.
    #[schemars(length(min = 1))]
    pub message_id: String,
    /// Opaque source thread/session id.
    #[schemars(length(min = 1))]
    pub thread_id: String,
    /// Who wrote the message.
    pub role: ChatRole,
    /// The message text.
    pub text: String,
}

/// What a fact candidate's value is. Profile fields carry one value each;
/// found facts carry a statement each. The values are fixed for
/// `enrichment.fact_candidate.v1~`: a new field needs a new version of the
/// type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum FactField {
    OpenalexAuthorId,
    Affiliation,
    ResearchAreas,
    LinkedinUrl,
    LinkedinHeadline,
    Location,
    CurrentJobTitle,
    Skills,
    Role,
    News,
    Award,
    Board,
    Publication,
    Profile,
    Bio,
    Mention,
}

impl FactField {
    /// Whether the field's value is a list: `research_areas` and `skills`.
    #[must_use]
    pub fn is_list(self) -> bool {
        matches!(self, Self::ResearchAreas | Self::Skills)
    }
}

/// A fact candidate's value as the source states it: one string, or, for a
/// list field, every item the source gave, cleaned (each item stripped, empty
/// items dropped, order kept, nothing cut). Which form a field takes is
/// checked by [`check_type_rules`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FactValue {
    One(String),
    List(Vec<String>),
}

/// The schema of a [`FactValue`]: one schema over both forms, so a refusal
/// names its own keyword and place (`minItems` for an empty list, `pattern`
/// at the item) rather than one `anyOf` for the whole value.
fn fact_value_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["string", "array"],
        "minLength": 1,
        "pattern": "\\S",
        "minItems": 1,
        "items": {
            "type": "string",
            "minLength": 1,
            "pattern": "^\\S(?:[\\s\\S]*\\S)?$"
        }
    })
}

/// Trust classification assigned by the connector's tiering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum TrustTier {
    Found,
    Saved,
}

/// The payload of a fact candidate record. Its `provenance` and `version`
/// are SHA-256 digests, derived as `docs/fact-candidate-identity.md` in this
/// crate specifies; [`check_type_rules`] checks their form.
#[derive(Debug, Clone, PartialEq)]
#[gts_type_schema(
    dir_path = "schemas",
    base = RecordV1,
    type_id = gts_id!("cf.connectors.core.record.v1~cf.construct.enrichment.fact_candidate.v1~"),
    description = "One fact the Enrichment connector found about one person, in the connector's own vocabulary: this connector owns the payload and its field list. field names what the value is. value is the fact as the source states it: for research_areas and skills, a list of the items the source gave, cleaned; for every other field, one string. confidence, trust_tier and identity_confidence are the connector's own judgment. The record's subject_id is always set. observed_at is when the connector formed the record. Provenance and version are SHA-256 content digests, written sha256: plus 64 lower-case hex digits; the provenance covers the subject in its canonical lower-case form, the field and the normalized value, and the version covers the payload's canonical JSON, as the Construct SDK's fact-candidate-identity.md specifies. The results API checks the value's form per field and the digests' form, which this schema cannot express.",
    properties = "field,value,evidence,source_url,confidence,trust_tier,identity_confidence,rationale",
    traits = about_a_person()
)]
pub struct FactCandidateV1 {
    /// What the value is.
    pub field: FactField,
    /// The value of the field, or the found fact's statement, as the source
    /// states it.
    #[schemars(schema_with = "fact_value_schema")]
    pub value: FactValue,
    /// A verbatim span from the source that supports the value. Absent when
    /// the source gave none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String", length(min = 1), regex(pattern = r"\S"))]
    pub evidence: Option<String>,
    /// The page the value was found on. Absent when the value came without a
    /// page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String", extend("format" = "uri"), regex(pattern = r"^https?://"))]
    pub source_url: Option<String>,
    /// The connector's confidence in this value, from 0 to 1.
    #[schemars(range(min = 0, max = 1))]
    pub confidence: f64,
    /// Whether the connector asserts the value (`saved`) or only found it
    /// (`found`).
    pub trust_tier: TrustTier,
    /// The connector's identity-match confidence for the person, from 0 to 1:
    /// how sure it is that the sources it used are about this person.
    #[schemars(range(min = 0, max = 1))]
    pub identity_confidence: f64,
    /// The connector's short account, in prose, of how it matched the person
    /// and which sources it used. Never page content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String", length(min = 1), regex(pattern = r"\S"))]
    pub rationale: Option<String>,
}

/// Which kind of rule a [`TypeRuleViolation`] broke, named like the JSON
/// Schema keyword it stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeRule {
    /// The text does not have the required form.
    Pattern,
    /// The value is not of the required JSON type.
    Type,
    /// The value is not one of the allowed values.
    Enum,
}

impl TypeRule {
    /// The JSON Schema keyword this rule stands for, as a refusal names it.
    #[must_use]
    pub fn keyword(self) -> &'static str {
        match self {
            Self::Pattern => "pattern",
            Self::Type => "type",
            Self::Enum => "enum",
        }
    }
}

/// A rule of a record type that its generated schema cannot carry, broken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeRuleViolation {
    /// Where in the record, as a JSON pointer.
    pub place: String,
    /// Which rule.
    pub rule: TypeRule,
}

impl std::fmt::Display for TypeRuleViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} breaks {}", self.place, self.rule.keyword())
    }
}

impl std::error::Error for TypeRuleViolation {}

/// Checks the rules of `type_id` that its generated schema cannot carry: a
/// derived schema can only refine `payload`, and cannot tie one payload
/// member to another. Intake runs it for the record's type and every type it
/// derives from, after the schema. Only the fact candidate has such rules:
///
/// - `provenance` and `version` are `sha256:` plus 64 lower-case hex digits;
/// - `value` is a list for the list fields ([`FactField::is_list`]), and one
///   string for every other field;
/// - no value or list item is whitespace only, and no list item starts or
///   ends with whitespace, by the set of the identity derivation, which is wider than the schema's `\S` in a validator that
///   follows ECMA-262.
///
/// It does not rely on the schema having run: an unknown `field` or a list
/// item that is not a string is refused here too.
///
/// # Errors
///
/// The first rule the record breaks.
pub fn check_type_rules(
    type_id: &str,
    record: &serde_json::Value,
) -> Result<(), TypeRuleViolation> {
    if type_id != ENRICHMENT_FACT_CANDIDATE_TYPE {
        return Ok(());
    }
    let broken = |place: String, rule| Err(TypeRuleViolation { place, rule });
    for member in ["provenance", "version"] {
        if !record[member].as_str().is_some_and(is_sha256_digest) {
            return broken(format!("/{member}"), TypeRule::Pattern);
        }
    }
    let payload = &record["payload"];
    let Ok(field) = FactField::deserialize(&payload["field"]) else {
        return broken("/payload/field".to_owned(), TypeRule::Enum);
    };
    match (&payload["value"], field.is_list()) {
        (serde_json::Value::Array(items), true) => {
            for (index, item) in items.iter().enumerate() {
                let place = || format!("/payload/value/{index}");
                match item.as_str() {
                    None => return broken(place(), TypeRule::Type),
                    Some(item) if is_blank(item) || !is_stripped(item) => {
                        return broken(place(), TypeRule::Pattern);
                    }
                    Some(_) => {}
                }
            }
            Ok(())
        }
        (serde_json::Value::String(value), false) if is_blank(value) => {
            broken("/payload/value".to_owned(), TypeRule::Pattern)
        }
        (serde_json::Value::String(_), false) => Ok(()),
        _ => broken("/payload/value".to_owned(), TypeRule::Type),
    }
}

/// `sha256:` plus 64 lower-case hex digits.
fn is_sha256_digest(text: &str) -> bool {
    text.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Whether a text is whitespace only, by the identity derivation's set.
fn is_blank(text: &str) -> bool {
    text.chars().all(is_space)
}

/// Whether a text neither starts nor ends with whitespace, by the identity
/// derivation's set. The schema's item pattern says the same, but without a
/// lookahead (the GTS regular-expression profile has none), and a validator
/// whose `$` matches before a final newline would let one through.
fn is_stripped(text: &str) -> bool {
    !text.starts_with(is_space) && !text.ends_with(is_space)
}

/// The identity derivation's whitespace (Python's `str.isspace`): Unicode
/// white space, and the separators U+001C to U+001F.
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// One section of a course, the dashboard's module grouping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct CourseSection {
    /// The section's 0-based index within the course; concepts reference it.
    pub section_index: u32,
    /// The section title the dashboard shows as the module name.
    #[schemars(length(min = 1))]
    pub title: String,
}

/// One concept of a course, the label joined onto per-student scores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct CourseConcept {
    /// The concept's 0-based index within the course; the student mastery
    /// record scores this same index.
    pub concept_index: u32,
    /// The concept name the dashboard shows.
    #[schemars(length(min = 1))]
    pub name: String,
    /// The concept description shown on the concept-detail and aggregate
    /// reads. Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub description: Option<String>,
    /// The section this concept belongs to, referencing
    /// `sections[].section_index`. Optional: a concept may belong to no
    /// section. When set it must reference a section present in this
    /// payload; the schema cannot express that, so the producer must
    /// guarantee it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "u32")]
    pub section_index: Option<u32>,
}

/// The payload of a course catalog record. A catalog is about a course, not
/// a person, so it turns `subject_required` off.
#[derive(Debug, Clone, PartialEq, Eq)]
#[gts_type_schema(
    dir_path = "schemas",
    base = RecordV1,
    type_id = gts_id!("cf.connectors.core.record.v1~cf.construct.mastery.course_catalog.v1~"),
    description = "One course's concept catalog: the human-readable labels the Mastery connector pushes so a dashboard can name the scores it renders. The connector's extraction is the only writer of these labels, through this push. Course-scoped, not per-student: it carries no subject_id, and it is pushed on (re)extraction rather than on every student result, so the catalog is not repeated on every score push. provenance is the course locator, stable across re-pushes; version is a digest of the catalog content, so an unchanged re-extraction dedupes and a changed one is a new version the receiving side upserts onto the same course. observed_at is the connector-side timestamp when extraction finished. The concept_index and section_index here share the namespace the student mastery record's scores are keyed by, so a re-extraction that renumbers them must republish the catalog and the affected student mastery records together; the receiving side applies them consistently.",
    properties = "course_id,title,sections,concepts",
    traits = serde_json::json!({ SUBJECT_REQUIRED_TRAIT: false })
)]
pub struct CourseCatalogV1 {
    /// The external course identifier, stable across systems and the
    /// resolution key together with the tenant. Not an internal UUID.
    #[schemars(length(min = 1))]
    pub course_id: String,
    /// The course title the dashboard shows. Optional: the source may not
    /// have set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub title: Option<String>,
    /// The course's sections, ordered. Each `section_index` is unique within
    /// the course; the schema cannot express that, so the producer must
    /// guarantee it. May be empty for a course with no section structure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Vec<CourseSection>")]
    pub sections: Option<Vec<CourseSection>>,
    /// The course's concepts. Each `concept_index` is unique within the
    /// course and is the same index the student mastery record scores; the
    /// schema cannot express that, so the producer must guarantee it.
    #[schemars(length(min = 1))]
    pub concepts: Vec<CourseConcept>,
}

/// One concept's mastery estimate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct ConceptScore {
    /// The concept's stable index within the course.
    pub concept_index: u32,
    /// The mastery estimate, a probability in [0, 1].
    #[schemars(range(min = 0, max = 1))]
    pub score: f64,
    /// How many observations the estimate rests on.
    pub signal_count: u32,
}

/// The payload of a student mastery record.
#[derive(Debug, Clone, PartialEq)]
#[gts_type_schema(
    dir_path = "schemas",
    base = RecordV1,
    type_id = gts_id!("cf.connectors.core.record.v1~cf.construct.mastery.student_mastery.v1~"),
    description = "One student's computed concept mastery for one course under one model: the finished result the Mastery connector pushes. subject_id is always set (the record is about the student, its value the student's platform user uuid). The payload carries scores keyed by concept_index only; the concept name and description are joined on read from the course catalog record, which the connector pushes separately, not with this per-student push. provenance = the (course, model, student) locator, stable across re-pushes; version = a digest of the concept scores, so an identical recompute dedupes to one record and a changed recompute is a new version the receiving side upserts onto the same provenance. observed_at = the connector-side timestamp when the compute finished, not a watermark over the events read. The result is authoritative: the receiving side validates it against this type and stores it as-is, with no retain-or-drop decision.",
    properties = "course_id,model_name,model_version,concepts",
    traits = about_a_person()
)]
pub struct StudentMasteryV1 {
    /// The external course identifier, stable across systems. Not an
    /// internal UUID.
    #[schemars(length(min = 1))]
    pub course_id: String,
    /// The evaluation model that produced these scores. A student may have
    /// mastery under more than one model.
    #[schemars(length(min = 1))]
    pub model_name: String,
    /// The model's version. Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "u32", range(min = 1))]
    pub model_version: Option<u32>,
    /// Per-concept mastery, keyed by the concept's stable within-course
    /// index. Each `concept_index` appears at most once; the schema cannot
    /// express that, so the producer must guarantee it.
    #[schemars(length(min = 1))]
    pub concepts: Vec<ConceptScore>,
}

#[cfg(test)]
#[path = "gts_tests.rs"]
mod gts_tests;
