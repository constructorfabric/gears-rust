//! `gears.<gear>.clients`: settings for the clients a gear builds through
//! wiring, kept apart from the gear's own `config`.
//!
//! ```yaml
//! gears:
//!   orders:
//!     clients:
//!       defaults:            # tuning for every client of this gear
//!         timeout: "5s"
//!       consumes:
//!         billing:           # #[toolkit::consumes(from = "billing")]
//!           timeout: "2s"
//!       provides:
//!         payment_api:       # #[toolkit::provides], contract in snake case
//!           transport: rest
//!           endpoint: "http://payments:8080"
//! ```
//!
//! `defaults` holds tuning keys only and is merged under each entry key by
//! key, nested objects included; the entry wins. Unknown keys are rejected
//! with their full path.

use serde_json::{Map, Value};
use toolkit_contract::wiring::ClientTuning;
pub use toolkit_contract::wiring::{ClientWiring, ConsumerWiring};

use crate::config::ConfigProvider;

/// Key of the section in a gear's entry.
pub const CLIENTS_KEY: &str = "clients";

const SECTION_KEYS: &[&str] = &["defaults", "consumes", "provides"];
const CONSUMER_KEYS: &[&str] = &["endpoint", "grpc_endpoint"];
const PROVIDED_KEYS: &[&str] = &["transport", "endpoint"];

/// The settings for the client gear `owner` uses to consume `dep`:
/// `clients.consumes.<dep>` over `clients.defaults`. `None` when neither is
/// set (the dep keeps discovery and default tuning).
///
/// # Errors
/// The section or the entry is malformed: not an object, an unknown key, or a
/// value that does not deserialize.
pub fn consumer_wiring(
    cfg: &dyn ConfigProvider,
    owner: &str,
    dep: &str,
) -> anyhow::Result<Option<ConsumerWiring>> {
    let Some(section) = section(cfg, owner)? else {
        return Ok(None);
    };
    let path = format!("gears.{owner}.clients.consumes.{dep}");
    let entry = entry(section, "consumes", dep, owner)?;
    if entry.is_none() && section.get("defaults").is_none() {
        return Ok(None);
    }
    let merged = merged(section, entry, &path, CONSUMER_KEYS)?;
    serde_json::from_value(Value::Object(merged))
        .map(Some)
        .map_err(|e| anyhow::anyhow!("{path}: {e}"))
}

/// The wiring gear `gear` uses for the contract it provides under `key` (the
/// trait name in snake case): `clients.provides.<key>` over
/// `clients.defaults`. [`ClientWiring::Local`] when the entry is absent;
/// `defaults` alone does not make a contract remote.
///
/// # Errors
/// The section or the entry is malformed: not an object, an unknown key, or a
/// value that does not deserialize.
pub fn provided_wiring(
    cfg: &dyn ConfigProvider,
    gear: &str,
    key: &str,
) -> anyhow::Result<ClientWiring> {
    let Some(section) = section(cfg, gear)? else {
        return Ok(ClientWiring::Local);
    };
    let Some(entry) = entry(section, "provides", key, gear)? else {
        return Ok(ClientWiring::Local);
    };
    let path = format!("gears.{gear}.clients.provides.{key}");
    let merged = merged(section, Some(entry), &path, PROVIDED_KEYS)?;
    serde_json::from_value(Value::Object(merged)).map_err(|e| anyhow::anyhow!("{path}: {e}"))
}

/// The gear's `clients` object, with its own keys and `defaults` checked.
fn section<'a>(
    cfg: &'a dyn ConfigProvider,
    gear: &str,
) -> anyhow::Result<Option<&'a Map<String, Value>>> {
    let Some(value) = cfg
        .get_gear_config(gear)
        .and_then(|entry| entry.get(CLIENTS_KEY))
        .filter(|v| !v.is_null())
    else {
        return Ok(None);
    };
    let section = as_object(value, &format!("gears.{gear}.clients"))?;
    check_keys(section, &format!("gears.{gear}.clients"), SECTION_KEYS, &[])?;
    if let Some(defaults) = section.get("defaults") {
        let path = format!("gears.{gear}.clients.defaults");
        check_keys(as_object(defaults, &path)?, &path, ClientTuning::KEYS, &[])?;
    }
    Ok(Some(section))
}

/// `clients.<group>.<name>`, required to be an object when present.
fn entry<'a>(
    section: &'a Map<String, Value>,
    group: &str,
    name: &str,
    gear: &str,
) -> anyhow::Result<Option<&'a Map<String, Value>>> {
    let Some(entries) = section.get(group) else {
        return Ok(None);
    };
    let entries = as_object(entries, &format!("gears.{gear}.clients.{group}"))?;
    entries
        .get(name)
        .map(|e| as_object(e, &format!("gears.{gear}.clients.{group}.{name}")))
        .transpose()
}

/// `defaults` with `entry` merged over it, after checking the entry's keys.
fn merged(
    section: &Map<String, Value>,
    entry: Option<&Map<String, Value>>,
    path: &str,
    own_keys: &[&str],
) -> anyhow::Result<Map<String, Value>> {
    let mut merged = section
        .get("defaults")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(entry) = entry {
        check_keys(entry, path, ClientTuning::KEYS, own_keys)?;
        merge_into(&mut merged, entry);
    }
    Ok(merged)
}

/// Merge `over` into `base` key by key; nested objects merge, anything else
/// replaces.
fn merge_into(base: &mut Map<String, Value>, over: &Map<String, Value>) {
    for (key, value) in over {
        match (base.get_mut(key), value) {
            (Some(Value::Object(b)), Value::Object(o)) => merge_into(b, o),
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

fn as_object<'a>(value: &'a Value, path: &str) -> anyhow::Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{path} must be an object"))
}

fn check_keys(
    object: &Map<String, Value>,
    path: &str,
    known: &[&str],
    also: &[&str],
) -> anyhow::Result<()> {
    for key in object.keys() {
        if !known.contains(&key.as_str()) && !also.contains(&key.as_str()) {
            let mut all: Vec<&str> = known.iter().chain(also).copied().collect();
            all.sort_unstable();
            anyhow::bail!("{path}.{key}: unknown key; known keys: {}", all.join(", "));
        }
    }
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "clients_config_tests.rs"]
mod clients_config_tests;
