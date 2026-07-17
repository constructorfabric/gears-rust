use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ProducerId(pub Uuid);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SubscriptionId(pub Uuid);

impl std::fmt::Display for SubscriptionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TopicId(pub Uuid);

impl TopicId {
    pub fn new(id: Uuid) -> Self {
        Self(id)
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    pub fn from_gts(gts: &str) -> Self {
        Self(Uuid::new_v5(&Uuid::NAMESPACE_OID, gts.as_bytes()))
    }
}

impl std::fmt::Display for TopicId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct EventTypeId(pub Uuid);

impl EventTypeId {
    pub fn new(id: Uuid) -> Self {
        Self(id)
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    pub fn from_gts(gts: &str) -> Self {
        Self(Uuid::new_v5(&Uuid::NAMESPACE_OID, gts.as_bytes()))
    }
}

impl std::fmt::Display for EventTypeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ConsumerGroupId(pub Uuid);

impl ConsumerGroupId {
    pub fn new(id: Uuid) -> Self {
        Self(id)
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    pub fn from_gts(gts: &str) -> Self {
        Self(Uuid::new_v5(&Uuid::NAMESPACE_OID, gts.as_bytes()))
    }
}

impl std::fmt::Display for ConsumerGroupId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
