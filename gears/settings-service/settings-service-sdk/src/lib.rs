//! Settings Service SDK
//!
//! Public contract for the `settings-service` gear.
//!
//! - [`SettingKey`] — the parsed setting key value object
//! - the opaque [`SecretHandle`] and the [`EffectiveSource`] vocabulary
//! - [`SettingsReaderClient`] and [`SettingsContributionClient`], whose every
//!   fallible method returns the platform-wide `CanonicalError`
//! - [`SettingsActivationClient`], for a consumer that must actively re-apply a
//!   setting: per-key subscription states, fresh re-reads, reported outcomes
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
//! value type named by the declaration's `value_type_id` — one of this SDK's
//! catalogue (`gts.cf.core.settings.type_*~`) or one a module registered. Registered when the declaration is created
//! (ADR-002).
//!
//! Only the first segment carries the `gts.` prefix, and each segment holds
//! exactly four name tokens before its version — that grammar is enforced by
//! `gts-id`, not re-implemented here.
#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod activation;
pub mod api;
pub mod canonical;
pub mod catalogue;
pub mod error;
pub mod gts;
pub mod key;
pub mod models;
pub mod odata;
pub mod precondition;

pub use activation::{
    ActivationOutcome, BundledKey, ChangeBundle, ChangeNotification, EncodedOutcome,
    InstanceIdentity, KeyState, MAX_INSTANCE_LABEL_CHARS, NotifiedKey, PendingReason,
    RefusedReason, ReportResult, ReportStatus, SettingChangeHandler, SettingsActivationClient,
    SubscriptionControl, SubscriptionHandle, UnsubscribeResult,
};
pub use api::{BulkOutcome, BulkSelector, SettingsContributionClient, SettingsReaderClient};
pub use error::SettingsError;
pub use key::{SettingKey, SettingKeyError};
pub use models::{EffectiveSource, Freshness, SecretHandle, TrailEntry};
// The wrapper `resolve_secret` returns, re-exported so a consumer reads the
// plaintext with the same `secrecy` this crate compiled against.
pub use secrecy::{ExposeSecret, SecretString};
