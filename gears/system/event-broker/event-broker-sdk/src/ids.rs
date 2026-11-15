use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::gts::CONSUMER_GROUP_RESOURCE_TYPE;

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
pub struct EventTypeId(pub Uuid);

impl EventTypeId {
    pub fn new(id: Uuid) -> Self {
        Self(id)
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
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

    /// The group's GTS instance id on the wire. Anonymous groups are identified
    /// by `gts.cf.core.events.consumer_group.v1~<uuid>`, so the uuid this carries
    /// is the instance suffix - the broker minted it the same way (`GtsInstanceId::
    /// new(<type>, <uuid>)`), so this reproduces exactly what a JOIN/GET/DELETE
    /// path needs.
    pub fn to_gts(self) -> String {
        format!("{CONSUMER_GROUP_RESOURCE_TYPE}{}", self.0)
    }

    /// Parses an anonymous group's GTS instance id back to its uuid. Returns
    /// `None` for a named group (a non-uuid instance suffix; named groups are
    /// tracked in #5702).
    pub fn try_from_gts(gts: &str) -> Option<Self> {
        gts.strip_prefix(CONSUMER_GROUP_RESOURCE_TYPE)
            .and_then(|suffix| Uuid::parse_str(suffix).ok())
            .map(Self)
    }
}

impl std::fmt::Display for ConsumerGroupId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_group_id_round_trips_through_its_gts_form() {
        let id =
            ConsumerGroupId::new(Uuid::parse_str("b7e63bcf-2e68-5fba-a1b1-0b5355b0f6d7").unwrap());
        assert_eq!(
            id.to_gts(),
            format!("{CONSUMER_GROUP_RESOURCE_TYPE}b7e63bcf-2e68-5fba-a1b1-0b5355b0f6d7")
        );
        assert_eq!(ConsumerGroupId::try_from_gts(&id.to_gts()), Some(id));
    }

    #[test]
    fn named_group_gts_id_has_no_uuid_and_is_rejected() {
        assert_eq!(
            ConsumerGroupId::try_from_gts(&format!(
                "{CONSUMER_GROUP_RESOURCE_TYPE}vendor.audit-processor.v1"
            )),
            None
        );
    }
}
