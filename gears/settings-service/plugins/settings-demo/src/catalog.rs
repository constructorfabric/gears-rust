// Created: 2026-09-06 by Constructor Tech
//! The sample catalogue: one declaration per value type the SDK ships.

use std::num::NonZeroU32;

use serde_json::{Value, json};
use settings_service_sdk::catalogue;
use settings_service_sdk::models::{
    ContributedClassification, ContributedDeclaration, ScopeClass, SettingMode,
};
use settings_service_sdk::{SettingKey, SettingKeyError};

/// The vendor segment of every demo key.
pub const VENDOR: &str = "cf";
/// The package segment of every demo key.
pub const PACKAGE: &str = "settings_demo";

fn key(category: &str, name: &str) -> Result<SettingKey, SettingKeyError> {
    SettingKey::contributed(VENDOR, PACKAGE, category, name, NonZeroU32::MIN)
}

fn declare(
    category: &str,
    name: &str,
    value_type_id: &str,
    default: Value,
    scope: ScopeClass,
    description: &str,
) -> Result<ContributedDeclaration, SettingKeyError> {
    let mut d = ContributedDeclaration::new(
        key(category, name)?,
        value_type_id.to_owned(),
        default,
        scope,
    );
    d.description = Some(description.to_owned());
    // Demo settings are written from the example server, which has no identity
    // provider to re-authenticate against, so they opt out of step-up; the
    // secret below keeps it, as a real deployment's settings would by default.
    d.requires_step_up = Some(false);
    Ok(d)
}

/// The declarations this gear contributes, in catalogue order.
///
/// # Errors
/// [`SettingKeyError`] if a demo key does not parse — a defect in this file,
/// caught by its tests before any boot sees it.
pub fn declarations() -> Result<Vec<ContributedDeclaration>, SettingKeyError> {
    let mut all = vec![
        declare(
            "network",
            "proxy_enabled",
            catalogue::BOOL_FLAG,
            json!(false),
            ScopeClass::Cascading,
            "Route outbound traffic through the configured proxy.",
        )?,
        declare(
            "network",
            "proxy_url",
            catalogue::URL,
            json!("http://proxy.internal:3128"),
            ScopeClass::Cascading,
            "The proxy every outbound request goes through when the proxy is enabled.",
        )?,
        declare(
            "network",
            "listen_port",
            catalogue::PORT,
            json!(8080),
            ScopeClass::Global,
            "The port the demo service listens on.",
        )?,
        declare(
            "network",
            "upstream_host",
            catalogue::HOSTNAME,
            json!("upstream.internal"),
            ScopeClass::Cascading,
            "The upstream the demo service forwards to.",
        )?,
        declare(
            "network",
            "admin_ip",
            catalogue::IPV4,
            json!("10.0.0.1"),
            ScopeClass::Local,
            "The address administrative traffic is accepted from.",
        )?,
        declare(
            "notifications",
            "digest_cron",
            catalogue::CRON,
            json!("0 9 * * 1"),
            ScopeClass::Cascading,
            "When the weekly digest is sent.",
        )?,
        declare(
            "notifications",
            "sender_name",
            catalogue::STRING,
            json!("Demo Team"),
            ScopeClass::Cascading,
            "The name notifications are sent under.",
        )?,
        declare(
            "limits",
            "max_sessions",
            catalogue::INTEGER,
            json!(100),
            ScopeClass::Cascading,
            "How many concurrent sessions a tenant may hold.",
        )?,
        declare(
            "limits",
            "session_ttl_seconds",
            catalogue::DURATION_SECONDS,
            json!(3600),
            ScopeClass::Cascading,
            "How long an idle session stays valid.",
        )?,
        declare(
            "limits",
            "cpu_share",
            catalogue::NUMBER,
            json!(0.5),
            ScopeClass::Local,
            "The share of a node's CPU the demo workload may use.",
        )?,
        declare(
            "security",
            "hostname_pattern",
            catalogue::REGEX,
            json!("^[a-z0-9-]+$"),
            ScopeClass::Global,
            "The pattern a node hostname must match to join.",
        )?,
    ];

    // A PII value: classified by the caller, since no trait carries it.
    let mut support_email = declare(
        "notifications",
        "support_email",
        catalogue::EMAIL,
        json!("support@example.com"),
        ScopeClass::Cascading,
        "Where users are told to write for help.",
    )?;
    support_email.data_classification = Some(ContributedClassification::Pii);
    all.push(support_email);

    // Exposable on the anonymous surface: public by classification, and the
    // sign-in page shows it before anyone signs in.
    let mut banner = declare(
        "notifications",
        "banner",
        catalogue::TEXT,
        json!("Welcome to the demo."),
        ScopeClass::Cascading,
        "The notice shown on the sign-in page.",
    )?;
    banner.anonymous_exposable = Some(true);
    banner.requires_step_up = Some(false);
    all.push(banner);

    // A secret: the default is the empty placeholder; the credential is set as
    // a value at a scope and never lives in a declaration.
    let mut token = declare(
        "security",
        "api_token",
        catalogue::SECRET_STRING,
        json!(""),
        ScopeClass::Cascading,
        "The token the demo service presents to its upstream.",
    )?;
    token.requires_step_up = Some(true);
    all.push(token);

    // A secret without step-up: what the credential path looks like when the
    // declaration does not also demand a fresh authentication.
    all.push(declare(
        "security",
        "webhook_secret",
        catalogue::SECRET_STRING,
        json!(""),
        ScopeClass::Cascading,
        "The shared secret the demo service signs outbound webhooks with.",
    )?);

    // Advanced mode: hidden from the standard settings view.
    let mut flags = declare(
        "limits",
        "feature_flags",
        catalogue::JSON,
        json!({}),
        ScopeClass::Cascading,
        "Free-form feature flags for the demo workload.",
    )?;
    flags.mode = Some(SettingMode::Advanced);
    all.push(flags);

    Ok(all)
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod catalog_tests;
