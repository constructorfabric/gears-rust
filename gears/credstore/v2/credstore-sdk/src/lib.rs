// Updated: 2026-10-06 by Constructor Tech
#![doc = include_str!("../README.md")]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
pub mod api;
pub mod error;
pub mod gts;
pub mod models;
pub mod plugin_api;
pub mod types;

pub use ::gts::GtsId;
pub use api::CredStoreClientV1;
pub use error::CredStoreError;
pub use gts::{CREDENTIAL_RESOURCE_TYPE, CredStorePluginSpecV1, CredentialV1, SecretTypeTraits};
pub use models::{
    Credential, CredentialListItem, CredentialPatch, CredentialStatus, CredentialWrite, Fallback,
    InheritanceStatus, OwnerId, PatchField, PutOutcome, PutPrecondition, Secret, SecretRef,
    SecretValue, SharingMode, StoreKey, TenantId, Validator, ValueVersion, WritePrecondition,
};
pub use plugin_api::{CredStorePluginClientV2, DestroySelector};
pub use types::{SECRET_TYPE_CATALOG, SecretType, SecretTypeDescriptor};
