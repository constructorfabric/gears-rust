use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
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
    1000
}

/// Upper bound on `max_text_length`: the capacity of `MySQL`'s `TEXT`, the
/// smallest column any supported backend stores the text in.
pub const MAX_TEXT_BYTES: usize = 65_535;

impl ConstructConfig {
    /// Reject limits that would break the gear at request time rather than at
    /// startup, where a typo is cheap to notice.
    ///
    /// # Errors
    ///
    /// When `max_text_length` is 0 (every note refused) or above
    /// [`MAX_TEXT_BYTES`].
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.max_text_length == 0 || self.max_text_length > MAX_TEXT_BYTES {
            anyhow::bail!(
                "construct: max_text_length must be between 1 and {MAX_TEXT_BYTES}, got {}",
                self.max_text_length
            );
        }
        Ok(())
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
}
