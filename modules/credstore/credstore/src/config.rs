//! Configuration for the credstore module.

use serde::Deserialize;

/// Module configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CredStoreConfig {
    /// Vendor selector used to pick a plugin implementation.
    ///
    /// The module queries types-registry for plugin instances matching
    /// this vendor and selects the one with lowest priority number.
    pub vendor: String,
}

impl Default for CredStoreConfig {
    fn default() -> Self {
        Self {
            vendor: "hyperspot".to_owned(),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn vendor_can_be_overridden_via_serde() {
        let json = r#"{"vendor": "acme"}"#;
        let cfg: CredStoreConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.vendor, "acme");
    }

    #[test]
    fn serde_default_applies_default_vendor() {
        let cfg: CredStoreConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(
            cfg.vendor, "hyperspot",
            "serde(default) must use Default impl"
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let json = r#"{"vendor": "x", "unexpected": true}"#;
        assert!(serde_json::from_str::<CredStoreConfig>(json).is_err());
    }
}
