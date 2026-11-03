use std::collections::BTreeMap;

use toolkit::domain_model;
use toolkit_gts::GtsInstanceId;

use super::Name;
use crate::domain::resolution::Sourced;

/// Which registry entry holds each topic's log (feature 0005 §2.2).
///
/// Built only by `crate::config::checks`, which proves every name is a
/// registry entry and that the instance-less topic key is present, so
/// resolving a topic never fails: a topic no specific key names takes the
/// instance-less route.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routing {
    /// The route for `gts.cf.core.events.topic.v1~`, the one every topic
    /// falls back to.
    base: Name,
    /// Every other route, keyed by a topic instance identifier or by the
    /// instance-less key of a topic type derived from the base.
    keyed: BTreeMap<String, Name>,
}

impl Routing {
    #[must_use]
    pub fn new(base: Name, keyed: BTreeMap<String, Name>) -> Self {
        Self { base, keyed }
    }

    /// The registry entry `topic` routes to. An entry keyed by the topic's own
    /// identifier wins over the one keyed by its type, and a type no entry
    /// names takes the instance-less route.
    #[must_use]
    pub fn resolve(&self, topic: &GtsInstanceId) -> Sourced<Name> {
        if let Some(name) = self.keyed.get(topic.as_ref()) {
            return Sourced::configured_for_topic(name.clone());
        }
        let type_route = gts::GtsId::try_new(topic.as_ref())
            .ok()
            .and_then(|id| id.get_type_id())
            .and_then(|type_id| self.keyed.get(&type_id));
        Sourced::configured_for_type(type_route.unwrap_or(&self.base).clone())
    }

    /// The instance-less route's entry.
    #[must_use]
    pub fn base(&self) -> &Name {
        &self.base
    }

    /// Every registry entry some route names, the instance-less route's
    /// included.
    pub fn names(&self) -> impl Iterator<Item = &Name> {
        std::iter::once(&self.base).chain(self.keyed.values())
    }

    /// The keys that name one topic rather than a type. Each should match a
    /// registered topic; one that does not is most likely a mistyped key that
    /// leaves its topic on the instance-less route.
    pub fn topic_keys(&self) -> impl Iterator<Item = &str> {
        self.keyed
            .keys()
            .filter(|key| !key.ends_with('~'))
            .map(String::as_str)
    }
}
