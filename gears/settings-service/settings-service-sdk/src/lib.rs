// Created: 2026-08-11 by Constructor Tech
//! Settings Service SDK
//!
//! Public contract for the `settings-service` gear.
//!
//! - [`SettingKey`] — the parsed setting key value object
//! - the opaque [`SecretHandle`] and the [`EffectiveSource`] vocabulary
//! - [`SettingsReaderClient`] and [`SettingsContributionClient`], whose every
//!   fallible method returns the platform-wide `CanonicalError`
//! - [`SettingsError`], the opt-in typed projection over that canonical error
//!
//! # Setting key shape
//!
//! A setting's key is a GTS **type** identifier: the abstract base this gear
//! owns, then the setting's own derived half. Both end with `~`.
//!
//! ```text
//! gts.cf.core.settings.setting_type.v1~acme.settings.network.enable_proxy.v1~
//! └──────── base type, owned by this gear ────────┘└── derived half, a type ──┘
//! ```
//!
//! The derived half carries four name tokens with the **category third**; an
//! admin key is composed as `<vendor>.settings.<category>.<name>.v1~`, a module
//! supplies its own. The value's shape is *not* in the key: it is a separate
//! curated catalog type (`gts.cf.toolkit.settings.type_*~`) named by the
//! declaration's `value_type_id`. Registered when the declaration is created
//! (ADR-002).
//!
//! Only the first segment carries the `gts.` prefix, and each segment holds
//! exactly four name tokens before its version — that grammar is enforced by
//! `gts-id`, not re-implemented here.
#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod activation;
pub mod api;
pub mod catalogue;
pub mod error;
pub mod gts;
pub mod key;
pub mod models;
pub mod odata;
pub mod precondition;

pub use activation::{
    ActivationOutcome, SettingChangeHandler, SettingChangeNotification, SettingsActivationClient,
    SubscriptionHandle,
};
pub use api::{BulkOutcome, BulkSelector, SettingsContributionClient, SettingsReaderClient};
pub use error::SettingsError;
pub use key::{SettingKey, SettingKeyError};
pub use models::{EffectiveSource, SecretHandle};
