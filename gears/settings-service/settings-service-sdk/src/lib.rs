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
//! value type named by the declaration's `value_type_id` — one of this SDK's
//! catalogue (`gts.cf.core.settings.type_*~`) or one a module registered. Registered when the declaration is created
//! (ADR-002).
//!
//! Only the first segment carries the `gts.` prefix, and each segment holds
//! exactly four name tokens before its version — that grammar is enforced by
//! `gts-id`, not re-implemented here.
#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

// The gear publishes this crate's GTS declarations through its
// `gts_publication(...)`: this collector, `gts_declarations()`, is what it
// reads. Every declaration below reaches it and the process-wide inventory.
toolkit_gts::declare_gts_inventory!();

/// Submit a hand-written type schema to the process-wide inventory and to this
/// crate's collector: what `toolkit_gts::local::gts_type_schema` does for a
/// derived one. The two records are built from the same two expressions.
///
/// There is no public `toolkit-gts` macro for a hand-written schema, so this
/// names the `#[doc(hidden)]` collector module that `declare_gts_inventory!()`
/// generates (`crate::__toolkit_gts_inventory::TypeSchema`): a change to that
/// internal shape breaks the macro. A schema submitted the plain way (only to
/// the global inventory) is silently missing from the published manifest; the
/// test `every_settings_schema_in_the_inventory_is_in_the_collector` catches it
/// for the `gts.cf.core.settings.` namespace.
macro_rules! submit_type_schema {
    ($type_id:expr, $schema_fn:expr) => {
        toolkit_gts::inventory::submit! {
            toolkit_gts::InventoryTypeSchema {
                type_id: $type_id,
                schema_fn: $schema_fn,
            }
        }
        toolkit_gts::inventory::submit! {
            crate::__toolkit_gts_inventory::TypeSchema(toolkit_gts::InventoryTypeSchema {
                type_id: $type_id,
                schema_fn: $schema_fn,
            })
        }
    };
}

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
pub use models::{EffectiveSource, SecretHandle, TrailEntry};
// The wrapper `resolve_secret` returns, re-exported so a consumer reads the
// plaintext with the same `secrecy` this crate compiled against.
pub use secrecy::{ExposeSecret, SecretString};
