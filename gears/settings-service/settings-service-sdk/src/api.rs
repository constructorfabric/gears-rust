// Created: 2026-08-12 by Virtuozzo International GmbH
//! The client traits consuming gears bind through `ClientHub`.
//!
//! # Error contract
//!
//! Every fallible method returns [`CanonicalError`], the platform-wide error
//! type — never a settings-specific one. That keeps the boundary uniform across
//! every gear SDK and means a new failure mode here is not a breaking change
//! for consumers.
//!
//! For flat typed dispatch see [`crate::SettingsError`], an opt-in projection
//! that lives beside these traits rather than inside them.
//!
//! # There is no `watch`
//!
//! Change notification is not a reader-trait method. A consumer that must
//! actively re-apply a setting -- a connection pool, a listening socket --
//! subscribes through [`crate::activation`]: `subscribe(keys, handler)` plus
//! `report_outcome(...)`, specified by DESIGN-activation §4.5 *In-Process SDK —
//! Consumer Activation*. An earlier draft listed a `watch` change stream on this
//! trait; it named the same capability twice, and its shape could not carry the
//! per-setting acknowledgement the activation contract requires.

use async_trait::async_trait;
use secrecy::SecretString;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::SettingKey;
use crate::models::{EffectiveValueResponse, GetEffectiveRequest};

/// The most settings one bulk read resolves. An explicit key list longer than
/// this, or a category that expands past it, is answered with a per-key
/// `bulk_too_large` error rather than a partial set — the same bound the batch
/// set carries.
pub const BULK_LIMIT: usize = 500;

/// What a bulk read asks for.
///
/// Both forms share one ancestry walk per scope; they differ only in how the
/// caller names the settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkSelector {
    /// An explicit set of keys.
    Keys(Vec<SettingKey>),
    /// Every setting in a category.
    ///
    /// The caller does not know the resulting key set in advance, which is why
    /// [`BulkOutcome`] carries the key rather than relying on position.
    Category(String),
}

/// One setting's outcome within a bulk read.
///
/// The key is carried explicitly on **both** the success and the failure side.
/// Positional correspondence to the request would not work: a
/// [`BulkSelector::Category`] read has no caller-supplied key list to align
/// against, and while a success could be self-identified from
/// `EffectiveValueResponse.key`, a `CanonicalError` carries no key at all. An
/// unattributable failure is not actionable — a consumer that cannot tell
/// *which* of twenty settings failed can only give up on all of them.
#[derive(Debug)]
pub struct BulkOutcome {
    /// The setting this outcome is for.
    pub key: SettingKey,
    /// Its independently resolved result.
    pub result: Result<EffectiveValueResponse, CanonicalError>,
}

/// The platform's in-process hot read path for effective setting values.
///
/// `ClientHub` binds this to the in-process implementation when the gear is
/// co-located, or to the same trait over REST when it runs out of process; a
/// consumer's code does not change between the two.
#[async_trait]
pub trait SettingsReaderClient: Send + Sync {
    /// Resolve one setting's effective value for a scope.
    ///
    /// A successful read always carries a value: every declaration has a Schema
    /// Default, so resolution terminates in one. On failure the service does
    /// **not** substitute that default — it lives in the same database and is
    /// equally unreachable — so the caller receives a distinguishable error and
    /// owns its own degradation posture.
    ///
    /// See [`crate::SettingsError`] for typed dispatch over the failure cases.
    async fn get_effective(
        &self,
        ctx: &SecurityContext,
        req: GetEffectiveRequest,
    ) -> Result<EffectiveValueResponse, CanonicalError>;

    /// Resolve several settings for one scope, sharing a single ancestry walk.
    ///
    /// Each outcome is independently `Ok` or `Err`: one failing key never fails
    /// the others, because a consumer reading twenty settings at boot should not
    /// lose nineteen of them to one retired key.
    ///
    /// Every outcome names its own key — see [`BulkOutcome`] for why that is
    /// carried explicitly rather than left to positional correspondence.
    ///
    /// Bounded at [`BULK_LIMIT`] settings: a longer key list, or a category
    /// that expands past it, is answered with a `bulk_too_large` error on
    /// every key it names — never a partial set.
    ///
    /// A [`BulkSelector::Category`] whose enumeration fails — the id is not a
    /// category id, the store is unreachable, a stored key does not parse —
    /// is answered with an **empty** batch: there is no key to hang the
    /// failure on, and a shortened batch would pass for a complete one. The
    /// gear logs the failure at `warn`; a consumer that must tell "failed"
    /// from "configures nothing" reads by explicit keys, and a top-level
    /// result for the category selector is a follow-up revision of this
    /// trait.
    ///
    /// See [`crate::SettingsError`] for typed dispatch over the failure cases.
    async fn get_effective_bulk(
        &self,
        ctx: &SecurityContext,
        selector: BulkSelector,
        scope: String,
    ) -> Vec<BulkOutcome>;

    /// Resolve a secret-backed setting to plaintext.
    ///
    /// The only plaintext path, and machine-only: the caller is authorized
    /// against this specific setting and the resolution is audited. A setting
    /// with no credential configured anywhere fails rather than returning the
    /// declaration's non-secret placeholder.
    ///
    /// That failure is [`crate::SettingsError::SecretNotConfigured`], and it is
    /// deliberately not [`crate::SettingsError::NotFound`]: the declaration
    /// resolved, only the credential is absent. Conflating the two is what lets
    /// a consumer hand a placeholder to a backend believing it is a credential.
    ///
    /// The plaintext comes wrapped. A [`SecretString`] prints `[REDACTED]` in
    /// `Debug`, zeroes its bytes when dropped, and yields them only through an
    /// explicit `expose_secret()` — so a consumer's log line, error context or
    /// panic message cannot carry a credential by accident, and every place
    /// that reads it is visible. The Credential Store hands the bytes to this
    /// service in the same kind of wrapper; nothing along the way unwraps them.
    async fn resolve_secret(
        &self,
        ctx: &SecurityContext,
        handle: crate::SecretHandle,
    ) -> Result<SecretString, CanonicalError>;
}

/// Registration surface for gears that contribute their own declarations.
#[async_trait]
pub trait SettingsContributionClient: Send + Sync {
    /// Register or reconcile this module's declarations at install or upgrade.
    async fn register_declarations(
        &self,
        ctx: &SecurityContext,
        owner_module: String,
        declarations: Vec<crate::models::ContributedDeclaration>,
    ) -> Result<crate::models::ReconcileResult, CanonicalError>;

    /// Retire declarations this module previously registered.
    async fn retire_declarations(
        &self,
        ctx: &SecurityContext,
        owner_module: String,
        keys: Vec<SettingKey>,
    ) -> Result<crate::models::ReconcileResult, CanonicalError>;
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod api_tests;
