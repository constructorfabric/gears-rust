//! Operator-facing configuration for the Event Broker module
//! (`DESIGN.md` §4.1 "Deployment Modes", which carries the example YAML),
//! and the startup checks that turn its raw sections into a validated
//! deployment (feature 0005 §2.4).

pub mod checks;
mod model;

pub(crate) use model::MAX_REQUEST_BODY_BYTES;
pub use model::{
    BUILT_IN_PARTITIONS, BackendEntry, BatchConfig, DEFAULT_RETENTION_DURATION, DeploymentMode,
    EventBrokerConfig, LoaderConfig, OutboxGroupEntry, PollingConfig, ProvisioningConfig,
    RegistrationConfig, RetentionSettings, RetentionSettingsEntry, RetentionSize, StreamingConfig,
    SubscriptionConfig, TopicSettingsEntry, TopicSettingsError, TopicSettingsMap, WorkersConfig,
};
