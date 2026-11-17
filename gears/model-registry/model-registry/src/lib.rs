//! Model Registry gear.
//!
//! The public contract lives in `cf-gears-model-registry-sdk` and is
//! re-exported here. The gear implements [`ModelRegistryClientV1`], registers
//! it in `ClientHub`, and exposes the REST API of DESIGN §3.3.

pub use model_registry_sdk::{
    ApprovalState, ApprovalV1, CreateModelRequestV1, CreateProviderRequestV1, ModelEntryV1,
    ModelInfoV1, ModelRegistryClientV1, ModelRegistryError, ModelV1, ProviderEntryV1, ProviderV1,
    SharingMode, UpdateModelRequestV1, UpdateProviderRequestV1,
};

pub mod gear;
pub use gear::ModelRegistryGear;

#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod gts;
#[doc(hidden)]
pub mod infra;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod testing;
