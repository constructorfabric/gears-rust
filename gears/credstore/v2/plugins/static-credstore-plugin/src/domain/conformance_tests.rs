// Created: 2026-10-04 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Runs the SDK's plugin conformance suite against the static plugin: one
//! test per check, each against a fresh, empty store.
use credstore_sdk::credstore_plugin_conformance;

use crate::config::StaticCredStorePluginConfig;
use crate::domain::service::Service;

fn fresh_plugin() -> Service {
    Service::from_config(&StaticCredStorePluginConfig::default()).expect("config builds")
}

credstore_plugin_conformance!(fresh_plugin());
