//! The inbox's boot configuration.

use serde::Deserialize;

/// Which gears the inbox asks, in the order it asks them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalsConfig {
    /// Stable source names (`pricing`, `products`). A name with no registration is unavailable.
    /// Each name is non-blank and unique; a duplicate or a blank name fails the boot.
    pub sources: Vec<String>,
}

/// Rejects a blank or repeated source name. The order of `sources` is the order the inbox asks.
///
/// # Errors
/// A name that is empty or only whitespace, or a name that appears twice.
pub fn checked_sources(sources: Vec<String>) -> anyhow::Result<Vec<String>> {
    let mut seen = std::collections::BTreeSet::new();
    for source in &sources {
        if source.trim().is_empty() {
            anyhow::bail!("bss-approvals: a source name is blank");
        }
        if !seen.insert(source.clone()) {
            anyhow::bail!("bss-approvals: source `{source}` is listed twice");
        }
    }
    Ok(sources)
}
