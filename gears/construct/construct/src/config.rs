use serde::Deserialize;

use crate::domain::service::ServiceConfig;

/// Default for [`ConstructConfig::max_text_length`], in bytes.
pub const DEFAULT_MAX_TEXT_LENGTH: usize = 1000;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConstructConfig {
    /// Longest foundation note text, in bytes.
    #[serde(default = "default_max_text_length")]
    pub max_text_length: usize,
}

impl Default for ConstructConfig {
    fn default() -> Self {
        Self {
            max_text_length: default_max_text_length(),
        }
    }
}

fn default_max_text_length() -> usize {
    DEFAULT_MAX_TEXT_LENGTH
}

/// Upper bound on `max_text_length`: the capacity of `MySQL`'s `TEXT`, the
/// smallest column any supported backend stores the text in.
pub const MAX_TEXT_BYTES: usize = 65_535;

impl ConstructConfig {
    /// Reject limits that would break the gear at request time rather than at
    /// startup, where a typo is cheap to notice.
    ///
    /// @cpt-dod:cpt-cf-construct-dod-gear-foundation-text-validation:p1
    ///
    /// # Errors
    ///
    /// When `max_text_length` is 0 (every note refused) or above
    /// [`MAX_TEXT_BYTES`].
    pub fn validate(&self) -> anyhow::Result<()> {
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-config-fail
        if self.max_text_length == 0 || self.max_text_length > MAX_TEXT_BYTES {
            anyhow::bail!(
                "construct: max_text_length must be between 1 and {MAX_TEXT_BYTES}, got {}",
                self.max_text_length
            );
        }
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-config-fail
        Ok(())
    }
}

impl TryFrom<&ConstructConfig> for ServiceConfig {
    type Error = anyhow::Error;

    /// Build the domain configuration from a config that passed
    /// [`ConstructConfig::validate`].
    ///
    /// # Errors
    ///
    /// When validation fails.
    fn try_from(cfg: &ConstructConfig) -> anyhow::Result<Self> {
        cfg.validate()?;
        Ok(Self {
            max_text_length: cfg.max_text_length,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_text_length_is_bounded_by_the_text_column() {
        let with = |max_text_length| ConstructConfig { max_text_length };
        assert!(with(0).validate().is_err(), "no note fits");
        assert!(with(MAX_TEXT_BYTES + 1).validate().is_err(), "past TEXT");
        with(1).validate().expect("smallest usable");
        ConstructConfig::default().validate().expect("defaults");
        with(MAX_TEXT_BYTES).validate().expect("the largest text");
    }

    fn parse(key: &'static str) -> Result<ConstructConfig, serde::de::value::Error> {
        use serde::de::value::MapDeserializer;
        ConstructConfig::deserialize(MapDeserializer::new([(key, 5_usize)].into_iter()))
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert_eq!(
            parse("max_text_length").expect("known key").max_text_length,
            5
        );
        assert!(
            parse("max_text_lenght").is_err(),
            "typo must not be ignored"
        );
    }

    #[test]
    fn default_config_converts_to_the_default_service_limit() {
        let service = ServiceConfig::try_from(&ConstructConfig::default()).expect("defaults");
        assert_eq!(service.max_text_length, DEFAULT_MAX_TEXT_LENGTH);
        assert_eq!(DEFAULT_MAX_TEXT_LENGTH, 1000);
    }

    #[test]
    fn invalid_config_does_not_convert() {
        let zero = ConstructConfig { max_text_length: 0 };
        assert!(ServiceConfig::try_from(&zero).is_err());
    }
}
