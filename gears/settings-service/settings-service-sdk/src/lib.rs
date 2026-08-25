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
//! A setting's key is a GTS **instance** identifier of the form
//! `<value-type>~<setting-instance-id>`:
//!
//! ```text
//! gts.cf.settings.types.bool_flag.v1~acme.settings.network.enable_proxy.v1
//! └────── value type, ends `~` ─────┘└──── instance id, no trailing `~` ───┘
//! ```
//!
//! The left half is a curated value type from the `gts.cf.settings.types.*~`
//! catalog and is the **only** part registered in GTS. The setting itself is an
//! unregistered GTS instance living in the Settings DB.
//!
//! Only the first segment carries the `gts.` prefix, and each segment holds
//! exactly four name tokens before its version — that grammar is enforced by
//! `gts-id`, not re-implemented here.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod activation;
pub mod api;
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
