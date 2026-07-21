// Created: 2026-07-14 by Constructor Tech
//! Open-extension support for the `type`-discriminated model families.
//!
//! The Open Responses wire protocol is an open set of types keyed by a
//! namespaced `type` string (`{provider_slug}:{type}`, `cf_gears:…`). Each core
//! family ([`OutputItem`](super::items::OutputItem),
//! [`Tool`](super::tools::Tool), [`StreamingEvent`](super::streaming::StreamingEvent),
//! …) is a flat enum with one variant per core-owned type plus an `Other`
//! variant holding an [`Extension`]. Any `type` the core does not own
//! deserializes into `Other` verbatim and is forwarded without interpretation
//! (per `principle-content-non-interpretation`); a consumer that has the
//! provider's crate projects it into a typed view with [`Extension::decode`].
//!

use serde::de::DeserializeOwned;

/// A `type`-discriminated value whose `type` is not one the core owns — a
/// provider or third-party plugin extension.
///
/// Preserved verbatim so the gateway can forward it unchanged. Decode into a
/// provider-defined type with [`Extension::decode`] after checking [`kind`](Self::kind).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct Extension(pub serde_json::Value);

impl Extension {
    /// The `type` discriminator, if present.
    #[must_use]
    pub fn kind(&self) -> Option<&str> {
        self.0.get("type").and_then(serde_json::Value::as_str)
    }

    /// Project into a provider-defined typed view. The caller is expected to
    /// have matched [`kind`](Self::kind) first.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if the preserved value does not conform
    /// to `T`.
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        T::deserialize(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_returns_type_discriminator() {
        let ext = Extension(serde_json::json!({"type": "my_provider:foo", "data": 1}));
        assert_eq!(ext.kind(), Some("my_provider:foo"));
    }

    #[test]
    fn decode_projects_into_typed_view() {
        let ext = Extension(serde_json::json!({"name": "hello"}));
        let val: serde_json::Value = ext.decode().unwrap();
        assert_eq!(val["name"], "hello");
    }
}
