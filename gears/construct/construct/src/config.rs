use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};

use serde::Deserialize;
use uuid::Uuid;

/// Default for [`ConstructConfig::personalization_default`]: on, so a host
/// without the settings service takes records for new subjects. A deployment
/// that wants new subjects to start with personalization off sets it to false.
pub const DEFAULT_PERSONALIZATION: bool = true;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConstructConfig {
    /// Whether personalization is on for a new subject, used only while the
    /// settings service gives no tenant default: no settings client in the
    /// host, or the setting not declared there.
    #[serde(default = "default_personalization")]
    pub personalization_default: bool,
    /// Connectors that are off, by connector identity: the subject id of the
    /// connector's login, as a UUID. Every other connector is on. A value that
    /// is not a UUID stops the gear from starting, so a typo cannot leave a
    /// connector on.
    #[serde(default)]
    pub connectors_off: Vec<Uuid>,
    /// Construct's processing service account: the identity Construct reads
    /// and writes the profiles in graph storage as, in every tenant. Without
    /// it the profile read answers unavailable and received records are
    /// dropped.
    #[serde(default)]
    pub processing_identity: Option<Uuid>,
    /// The model the planner calls. Without it, received records are dropped
    /// with an event, as before the planner existed.
    #[serde(default)]
    pub model: Option<ModelConfig>,
    #[serde(default)]
    pub processing: ProcessingConfig,
    /// The admission rules of every tenant, until Construct's settings are
    /// declared in the settings service.
    #[serde(default)]
    pub admission: AdmissionConfig,
    /// How the profile read asks the platform which categories a caller may
    /// read: `one_call` (a category constraint in one policy answer) or
    /// `per_category` (one policy call per category).
    #[serde(default)]
    pub profile_categories: CategoryCheckConfig,
}

impl Default for ConstructConfig {
    fn default() -> Self {
        Self {
            personalization_default: DEFAULT_PERSONALIZATION,
            connectors_off: Vec::new(),
            processing_identity: None,
            model: None,
            processing: ProcessingConfig::default(),
            admission: AdmissionConfig::default(),
            profile_categories: CategoryCheckConfig::default(),
        }
    }
}

/// The planner's model: an OAGW upstream with the `OpenAI` chat completions
/// API.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    /// The OAGW upstream alias, one plain path segment.
    pub upstream_alias: String,
    /// The model name the upstream expects.
    pub model: String,
    /// The limit for one model call, in seconds.
    #[serde(default = "default_model_timeout_secs")]
    pub timeout_secs: NonZeroU64,
}

fn default_model_timeout_secs() -> NonZeroU64 {
    NonZeroU64::new(60).unwrap_or(NonZeroU64::MIN)
}

/// How received records are processed.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ProcessingConfig {
    /// The most records waiting; a full queue refuses a record with 503.
    pub queue_capacity: NonZeroUsize,
    /// The most records processed at once.
    pub workers: NonZeroUsize,
    /// The planner's cap on model rounds for one record.
    pub max_rounds: NonZeroU32,
    /// The planner's cap on tokens for one record.
    pub max_tokens: NonZeroU64,
    /// How many times a record is planned again after a write conflict.
    pub max_reruns: u32,
}

impl Default for ProcessingConfig {
    fn default() -> Self {
        Self {
            queue_capacity: NonZeroUsize::new(256).unwrap_or(NonZeroUsize::MIN),
            workers: NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN),
            max_rounds: NonZeroU32::new(8).unwrap_or(NonZeroU32::MIN),
            max_tokens: NonZeroU64::new(32_000).unwrap_or(NonZeroU64::MIN),
            max_reruns: 2,
        }
    }
}

/// The admission rules. An absent list lets every member through.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AdmissionConfig {
    /// The connectors whose records are trusted, by connector identity: the
    /// subject id of the connector's login, as a UUID.
    pub trusted_connectors: Option<Vec<Uuid>>,
    /// The lowest confidence, from 0 to 1, a model's step may carry.
    pub confidence_floor: f32,
    /// The entity kinds a plan may store.
    pub entity_kinds: Option<Vec<String>>,
    /// The most steps one plan may hold.
    pub max_steps: usize,
    /// The longest text piece, in characters, a plan may store.
    pub max_value_chars: usize,
}

impl Default for AdmissionConfig {
    fn default() -> Self {
        Self {
            trusted_connectors: None,
            confidence_floor: 0.6,
            entity_kinds: None,
            max_steps: 100,
            max_value_chars: 1000,
        }
    }
}

/// The most steps one plan may be capped at.
pub const MAX_STEPS_LIMIT: usize = 10_000;
/// The longest text piece the value cap may allow.
pub const MAX_VALUE_CHARS_LIMIT: usize = 100_000;

/// A config the gear refuses to start with.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("construct config: {0}")]
pub struct InvalidConfig(pub String);

impl ConstructConfig {
    /// Check the values serde cannot: the admission rules' ranges and the
    /// model's upstream alias.
    ///
    /// # Errors
    ///
    /// [`InvalidConfig`] naming the first bad value.
    pub fn validate(&self) -> Result<(), InvalidConfig> {
        let admission = &self.admission;
        let floor = admission.confidence_floor;
        if !floor.is_finite() || !(0.0..=1.0).contains(&floor) {
            return Err(InvalidConfig(format!(
                "admission.confidence_floor must be from 0 to 1, not {floor}"
            )));
        }
        if !(1..=MAX_STEPS_LIMIT).contains(&admission.max_steps) {
            return Err(InvalidConfig(format!(
                "admission.max_steps must be from 1 to {MAX_STEPS_LIMIT}, not {}",
                admission.max_steps
            )));
        }
        if !(1..=MAX_VALUE_CHARS_LIMIT).contains(&admission.max_value_chars) {
            return Err(InvalidConfig(format!(
                "admission.max_value_chars must be from 1 to {MAX_VALUE_CHARS_LIMIT}, not {}",
                admission.max_value_chars
            )));
        }
        if let Some(model) = &self.model
            && !is_upstream_alias(&model.upstream_alias)
        {
            return Err(InvalidConfig(format!(
                "model.upstream_alias {:?} must be 1 to 63 letters, digits, `-` or `_`, starting with a letter or digit",
                model.upstream_alias
            )));
        }
        Ok(())
    }
}

/// Whether `alias` is a plain OAGW upstream alias: 1 to 63 ASCII letters,
/// digits, `-` or `_`, starting with a letter or digit. It becomes the path of
/// every model call, so anything else is refused.
fn is_upstream_alias(alias: &str) -> bool {
    let mut chars = alias.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && alias.len() <= 63
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The config spelling of [`crate::domain::profile_read::CategoryCheck`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryCheckConfig {
    #[default]
    OneCall,
    PerCategory,
}

fn default_personalization() -> bool {
    DEFAULT_PERSONALIZATION
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::value::{Error, MapDeserializer};

    fn parse(json: serde_json::Value) -> Result<ConstructConfig, serde_json::Error> {
        serde_json::from_value(json)
    }

    #[test]
    fn an_empty_config_takes_the_defaults() {
        let config = ConstructConfig::deserialize(MapDeserializer::<_, Error>::new(
            std::iter::empty::<(&str, bool)>(),
        ))
        .expect("an empty config is valid");
        assert!(config.personalization_default);
        assert!(config.connectors_off.is_empty());
        assert!(config.processing_identity.is_none() && config.model.is_none());
        assert_eq!(config.processing.max_reruns, 2);
        assert!((config.admission.confidence_floor - 0.6).abs() < f32::EPSILON);
        assert_eq!(config.profile_categories, CategoryCheckConfig::OneCall);
    }

    #[test]
    fn the_processing_keys_are_read() {
        let config = parse(serde_json::json!({
            "processing_identity": "8b0b4c4e-0000-4000-8000-000000000001",
            "model": { "upstream_alias": "llm", "model": "gpt-x" },
            "processing": { "workers": 2 },
            "admission": { "entity_kinds": ["skill"], "max_steps": 10 },
            "profile_categories": "per_category",
        }))
        .expect("known keys");
        assert!(config.processing_identity.is_some());
        let model = config.model.expect("a model");
        assert_eq!(model.timeout_secs.get(), 60);
        assert_eq!(config.processing.workers.get(), 2);
        assert_eq!(config.processing.queue_capacity.get(), 256);
        assert_eq!(
            config.admission.entity_kinds,
            Some(vec!["skill".to_owned()])
        );
        assert_eq!(config.admission.max_steps, 10);
        assert_eq!(config.profile_categories, CategoryCheckConfig::PerCategory);
    }

    fn valid(json: serde_json::Value) -> Result<(), InvalidConfig> {
        parse(json).expect("parses").validate()
    }

    #[test]
    fn the_defaults_are_valid() {
        assert_eq!(ConstructConfig::default().validate(), Ok(()));
    }

    #[test]
    fn a_confidence_floor_outside_zero_to_one_is_refused() {
        for floor in [-0.1, 1.5, 60.0] {
            assert!(
                valid(serde_json::json!({ "admission": { "confidence_floor": floor } })).is_err(),
                "{floor}"
            );
        }
        assert!(valid(serde_json::json!({ "admission": { "confidence_floor": 0.0 } })).is_ok());
        assert!(valid(serde_json::json!({ "admission": { "confidence_floor": 1.0 } })).is_ok());
    }

    #[test]
    fn caps_of_zero_or_past_their_limit_are_refused() {
        for admission in [
            serde_json::json!({ "max_steps": 0 }),
            serde_json::json!({ "max_steps": MAX_STEPS_LIMIT + 1 }),
            serde_json::json!({ "max_value_chars": 0 }),
            serde_json::json!({ "max_value_chars": MAX_VALUE_CHARS_LIMIT + 1 }),
        ] {
            assert!(
                valid(serde_json::json!({ "admission": admission })).is_err(),
                "{admission}"
            );
        }
    }

    #[test]
    fn an_upstream_alias_that_is_not_a_plain_name_is_refused() {
        for alias in [
            "",
            "..",
            "a/b",
            "a%2Fb",
            "a\\b",
            "a b",
            "-llm",
            &"x".repeat(64),
        ] {
            let model = serde_json::json!({ "model": { "upstream_alias": alias, "model": "m" } });
            assert!(valid(model).is_err(), "{alias:?}");
        }
        let model =
            serde_json::json!({ "model": { "upstream_alias": "llm_gateway-1", "model": "m" } });
        assert!(valid(model).is_ok());
    }

    #[test]
    fn a_trusted_connector_that_is_not_a_uuid_is_rejected() {
        assert!(
            parse(serde_json::json!({ "admission": { "trusted_connectors": ["connector-a"] } }))
                .is_err()
        );
        let id = Uuid::new_v4();
        let config = parse(serde_json::json!({ "admission": { "trusted_connectors": [id.to_string().to_uppercase()] } }))
            .expect("a UUID in another spelling");
        assert_eq!(config.admission.trusted_connectors, Some(vec![id]));
    }

    #[test]
    fn a_processing_key_with_a_typo_or_a_zero_is_rejected() {
        assert!(parse(serde_json::json!({ "processing": { "worker": 2 } })).is_err());
        assert!(parse(serde_json::json!({ "processing": { "workers": 0 } })).is_err());
        assert!(parse(serde_json::json!({ "model": { "upstream_alias": "llm" } })).is_err());
        assert!(parse(serde_json::json!({ "processing_identity": "nobody" })).is_err());
    }

    #[test]
    fn both_keys_are_read() {
        let config = parse(serde_json::json!({
            "personalization_default": false,
            "connectors_off": ["8b0b4c4e-0000-4000-8000-000000000001"],
        }))
        .expect("known keys");
        assert!(!config.personalization_default);
        assert_eq!(config.connectors_off.len(), 1);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(
            parse(serde_json::json!({ "personalisation_default": false })).is_err(),
            "typo must not be ignored"
        );
        assert!(
            parse(serde_json::json!({ "max_text_length": 10 })).is_err(),
            "the foundation's key is gone"
        );
    }

    #[test]
    fn a_connector_that_is_not_a_uuid_is_rejected() {
        assert!(parse(serde_json::json!({ "connectors_off": ["  "] })).is_err());
        assert!(parse(serde_json::json!({ "connectors_off": ["connector-a"] })).is_err());
    }

    #[test]
    fn a_connector_in_another_spelling_is_the_same_connector() {
        let id = Uuid::new_v4();
        let upper = id.to_string().to_uppercase();
        let config = parse(serde_json::json!({ "connectors_off": [upper] })).expect("a UUID");
        assert_eq!(config.connectors_off, vec![id]);
    }
}
