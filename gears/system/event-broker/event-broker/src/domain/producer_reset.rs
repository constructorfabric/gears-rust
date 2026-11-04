//! Which of a producer's sequence chains a reset clears.

use toolkit::domain_model;

/// Which `event_broker_producer_sequence` rows `IngestService::reset_producer` clears:
/// the producer's chain on every topic, or on one.
#[domain_model]
#[derive(Debug, Clone)]
pub enum ProducerResetScope {
    All,
    Topic { topic: String },
}

/// The optional selector a `:reset` request carries, before the domain decides
/// which [`ProducerResetScope`] it means. Its own type so the REST layer hands
/// it over without making the decision itself.
#[domain_model]
#[derive(Debug)]
pub struct ProducerResetSelector {
    pub topic: Option<String>,
}

impl From<ProducerResetSelector> for ProducerResetScope {
    fn from(selector: ProducerResetSelector) -> Self {
        match selector.topic {
            Some(topic) => Self::Topic { topic },
            None => Self::All,
        }
    }
}
