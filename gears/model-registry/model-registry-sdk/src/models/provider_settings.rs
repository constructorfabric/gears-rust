// Created: 2026-05-06 by Constructor Tech
//! [`ProviderSettings`] marker trait for typed provider-settings types
//! (e.g. `OpenAiSettingsV1`; the shipped set lives in
//! [`crate::models::providers`] and is open-ended).
//!
//! There is **no** tagged-enum carrier and no hand-written newtype here —
//! `Model<P>` defaults to `Model<serde_json::Value>` (which implements
//! `gts::GtsSchema` upstream), i.e. the provider settings ride as a raw JSON
//! blob until the consumer narrows to a typed view via
//! [`crate::models::Model::try_into_typed`] (a thin wrapper over
//! [`gts::try_narrow`]). Resolution is by GTS schema id: each typed settings
//! type's `GtsSchema::TYPE_ID` is matched against the model's `info.gts_type`
//! before the JSON value is deserialized into the typed shape.

use gts::GtsSchema;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Marker trait implemented by every typed provider-settings type shipped
/// in [`crate::models::providers`] (e.g. `OpenAiSettingsV1`).
///
/// Acts purely as documentation that a type is a typed provider-settings
/// payload — there are no required methods. Provider family identification
/// is done via `ModelInfoV1::gts_type` (a [`gts::GtsSchemaId`]).
pub trait ProviderSettings:
    std::fmt::Debug + Clone + PartialEq + Send + Sync + 'static + GtsSchema
{
}
