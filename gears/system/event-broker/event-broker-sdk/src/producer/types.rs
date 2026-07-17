use crate::api::ProducerMode;
use crate::ids::ProducerId;

/// Controls when producer schemas are fetched. Validation is always enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ValidationTiming {
    #[default]
    Eager,
    Lazy,
}

/// Event authoring and broker registration diagnostic identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducerIdentity {
    source: String,
    client_agent: Option<String>,
}

impl ProducerIdentity {
    #[must_use]
    pub fn new() -> Self {
        Self {
            source: String::new(),
            client_agent: None,
        }
    }

    #[must_use]
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    #[must_use]
    pub fn client_agent(mut self, client_agent: impl Into<String>) -> Self {
        self.client_agent = Some(client_agent.into());
        self
    }

    pub(crate) fn validate(&self) -> Result<(), crate::error::EventBrokerError> {
        if self.source.trim().is_empty() {
            return Err(crate::error::EventBrokerError::InvalidProducerOptions {
                detail: "producer identity source is required".to_owned(),
                instance: String::new(),
            });
        }
        Ok(())
    }

    pub fn source_ref(&self) -> &str {
        &self.source
    }

    pub fn client_agent_ref(&self) -> Option<&str> {
        self.client_agent.as_deref()
    }
}

impl Default for ProducerIdentity {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectDeduplication {
    Stateless,
    RegisterOnStart {
        mode: ProducerMode,
    },
    Reuse {
        mode: ProducerMode,
        producer_id: ProducerId,
    },
}

impl DirectDeduplication {
    #[must_use]
    pub fn stateless() -> Self {
        Self::Stateless
    }

    #[must_use]
    pub fn register_on_start(mode: ProducerMode) -> Self {
        Self::RegisterOnStart { mode }
    }

    #[must_use]
    pub fn reuse(mode: ProducerMode, producer_id: ProducerId) -> Self {
        Self::Reuse { mode, producer_id }
    }

    pub(crate) fn validate(&self) -> Result<(), crate::error::EventBrokerError> {
        match *self {
            Self::Stateless => Ok(()),
            Self::RegisterOnStart {
                mode: ProducerMode::Stateless,
            }
            | Self::Reuse {
                mode: ProducerMode::Stateless,
                ..
            } => Err(crate::error::EventBrokerError::InvalidProducerOptions {
                detail: "registration-backed deduplication requires monotonic or chained mode"
                    .to_owned(),
                instance: String::new(),
            }),
            Self::RegisterOnStart { .. } | Self::Reuse { .. } => Ok(()),
        }
    }
}

#[cfg(feature = "db")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbDeduplication {
    Stateless,
    Managed(ManagedDeduplication),
}

#[cfg(feature = "db")]
impl DbDeduplication {
    #[must_use]
    pub fn stateless() -> Self {
        Self::Stateless
    }

    #[must_use]
    pub fn managed(mode: ProducerMode) -> ManagedDeduplicationBuilder {
        ManagedDeduplicationBuilder {
            mode,
            key: None,
            on_missing: MissingProducerRegistration::Fail,
            on_unknown: UnknownProducerRegistration::Fail,
        }
    }

    pub(crate) fn mode(&self) -> ProducerMode {
        match self {
            Self::Stateless => ProducerMode::Stateless,
            Self::Managed(managed) => managed.mode,
        }
    }
}

#[cfg(feature = "db")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedDeduplication {
    pub(crate) mode: ProducerMode,
    pub(crate) key: String,
    pub(crate) on_missing: MissingProducerRegistration,
    pub(crate) on_unknown: UnknownProducerRegistration,
}

#[cfg(feature = "db")]
pub struct ManagedDeduplicationBuilder {
    mode: ProducerMode,
    key: Option<String>,
    on_missing: MissingProducerRegistration,
    on_unknown: UnknownProducerRegistration,
}

#[cfg(feature = "db")]
impl ManagedDeduplicationBuilder {
    #[must_use]
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    #[must_use]
    pub fn on_missing(mut self, policy: MissingProducerRegistration) -> Self {
        self.on_missing = policy;
        self
    }

    #[must_use]
    pub fn on_unknown(mut self, policy: UnknownProducerRegistration) -> Self {
        self.on_unknown = policy;
        self
    }

    pub fn build(self) -> Result<DbDeduplication, crate::error::EventBrokerError> {
        if self.mode == ProducerMode::Stateless {
            return Err(crate::error::EventBrokerError::InvalidProducerOptions {
                detail: "managed producer registration requires monotonic or chained mode"
                    .to_owned(),
                instance: String::new(),
            });
        }
        let key =
            self.key
                .ok_or_else(|| crate::error::EventBrokerError::InvalidProducerOptions {
                    detail: "managed producer registration key is required".to_owned(),
                    instance: String::new(),
                })?;
        if key.trim().is_empty() {
            return Err(crate::error::EventBrokerError::InvalidProducerOptions {
                detail: "managed producer registration key must not be empty".to_owned(),
                instance: String::new(),
            });
        }
        Ok(DbDeduplication::Managed(ManagedDeduplication {
            mode: self.mode,
            key,
            on_missing: self.on_missing,
            on_unknown: self.on_unknown,
        }))
    }
}

#[cfg(feature = "db")]
impl From<ManagedDeduplicationBuilder> for DbDeduplication {
    fn from(builder: ManagedDeduplicationBuilder) -> Self {
        builder
            .build()
            .expect("managed producer deduplication must be complete")
    }
}

#[cfg(feature = "db")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingProducerRegistration {
    Fail,
    RegisterNew,
}

#[cfg(feature = "db")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownProducerRegistration {
    Fail,
    RegisterNew,
}
