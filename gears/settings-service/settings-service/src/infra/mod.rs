//! Adapters to the outside world.

pub mod broadcast;
pub mod cache_invalidation;
pub mod coherence_metrics;
pub mod contribution_client;
pub mod event_broker;
pub mod event_schemas;
pub mod lifecycle_metrics;
pub mod platform_scope;
pub mod reader_client;
pub mod sdk_error_mapping;
pub mod secret_manager;
pub mod service_identity;
pub mod setting_type_registrar;
pub mod step_up;
pub mod storage;
pub mod tenant_hierarchy;
pub mod type_validator;
pub mod value_writes;
pub mod write_metrics;
