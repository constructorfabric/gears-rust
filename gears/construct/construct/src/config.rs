use serde::Deserialize;
use uuid::Uuid;

/// Default for [`ConstructConfig::personalization_default`]: on, so a host
/// without the settings service takes records for new subjects. A deployment
/// that wants new subjects to start with personalization off sets it to false.
pub const DEFAULT_PERSONALIZATION: bool = true;

pub const DEFAULT_MODEL_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "adapter", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelConfig {
    ChatCompletions {
        upstream_alias: String,
        model: String,
        #[serde(default = "default_model_timeout_ms")]
        timeout_ms: u64,
    },
}

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
    #[serde(default)]
    pub model: Option<ModelConfig>,
}

impl Default for ConstructConfig {
    fn default() -> Self {
        Self {
            personalization_default: DEFAULT_PERSONALIZATION,
            connectors_off: Vec::new(),
            model: None,
        }
    }
}

fn default_personalization() -> bool {
    DEFAULT_PERSONALIZATION
}

fn default_model_timeout_ms() -> u64 {
    DEFAULT_MODEL_TIMEOUT_MS
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
        assert!(config.model.is_none());
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
    fn the_chat_completions_model_is_read_with_a_default_timeout() {
        let config = parse(serde_json::json!({
            "model": { "adapter": "chat_completions", "upstream_alias": "llm.example", "model": "fact-planner" },
        }))
        .expect("a model config");
        assert_eq!(
            config.model,
            Some(ModelConfig::ChatCompletions {
                upstream_alias: "llm.example".to_owned(),
                model: "fact-planner".to_owned(),
                timeout_ms: DEFAULT_MODEL_TIMEOUT_MS,
            })
        );
    }

    #[test]
    fn an_unknown_adapter_or_model_key_is_rejected() {
        assert!(
            parse(serde_json::json!({ "model": { "adapter": "telepathy", "model": "x" } }))
                .is_err(),
            "unknown adapter"
        );
        assert!(
            parse(serde_json::json!({
                "model": { "adapter": "chat_completions", "upstream_alias": "a", "model": "m", "temprature": 0 },
            }))
            .is_err(),
            "typo in a model key"
        );
    }

    #[test]
    fn a_connector_in_another_spelling_is_the_same_connector() {
        let id = Uuid::new_v4();
        let upper = id.to_string().to_uppercase();
        let config = parse(serde_json::json!({ "connectors_off": [upper] })).expect("a UUID");
        assert_eq!(config.connectors_off, vec![id]);
    }
}
