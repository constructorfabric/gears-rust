#[cfg(feature = "outbox")]
use crate::api::ProducerMode;

/// Controls when producer schemas are fetched. Validation is always enabled.
#[cfg(feature = "outbox")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ValidationTiming {
    #[default]
    Eager,
    Lazy,
}

/// How an outbox producer is known to the broker.
#[cfg(feature = "outbox")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboxRegistration {
    /// No producer id: the broker does not deduplicate the producer's events.
    Stateless,
    /// One producer id per outbox partition, registered on the partition's
    /// first drain and stored under the registration's key.
    Managed(ManagedRegistration),
}

#[cfg(feature = "outbox")]
impl OutboxRegistration {
    /// A managed registration in `mode`, stored under `key`. Both are checked
    /// by `OutboxSetupBuilder::build`.
    #[must_use]
    pub fn managed(mode: ProducerMode, key: impl Into<String>) -> Self {
        Self::Managed(ManagedRegistration {
            mode,
            key: key.into(),
        })
    }

    /// The producer mode this registration implies (`Stateless` when unmanaged).
    pub(crate) fn mode(&self) -> ProducerMode {
        match self {
            Self::Stateless => ProducerMode::Stateless,
            Self::Managed(managed) => managed.mode,
        }
    }
}

/// The mode and key of a managed outbox registration.
#[cfg(feature = "outbox")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedRegistration {
    mode: ProducerMode,
    key: String,
}

#[cfg(feature = "outbox")]
impl ManagedRegistration {
    /// The producer mode the broker enforces for this producer.
    pub(crate) fn mode(&self) -> ProducerMode {
        self.mode
    }

    /// The stable producer key the registration is stored under.
    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    /// Refuses a registration in `Stateless` mode, or with a key the
    /// registration row cannot take.
    ///
    /// # Errors
    ///
    /// [`InvalidProducerOptions`](crate::error::EventBrokerError::InvalidProducerOptions)
    /// on the first failing check, in this order: a `Stateless` mode, a blank
    /// key, a key with leading or trailing whitespace, and a key longer than
    /// the registration row holds.
    pub(super) fn check(&self) -> Result<(), crate::error::EventBrokerError> {
        let refused =
            |detail: String| crate::error::EventBrokerError::InvalidProducerOptions { detail };
        if self.mode == ProducerMode::Stateless {
            return Err(refused(
                "managed producer registration requires monotonic or chained mode".to_owned(),
            ));
        }
        if self.key.trim().is_empty() {
            return Err(refused(
                "managed producer registration key must not be empty".to_owned(),
            ));
        }
        // MySQL's PAD SPACE collation equates names that differ only in
        // trailing spaces; edge whitespace is refused on every engine so all
        // engines agree on which names collide.
        if self.key != self.key.trim() {
            return Err(refused(
                "managed producer registration key must not have leading or trailing whitespace"
                    .to_owned(),
            ));
        }
        if self.key.len() > super::registration::MAX_NAME_LEN {
            return Err(refused(format!(
                "managed producer registration key must be at most {} bytes",
                super::registration::MAX_NAME_LEN
            )));
        }
        Ok(())
    }
}
