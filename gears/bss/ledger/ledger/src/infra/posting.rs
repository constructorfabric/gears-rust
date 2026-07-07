//! Transactional posting-engine components (DB-touching, so under `infra`,
//! never `domain` — dylint DE0301). Each runs inside one passed-in secure
//! transaction (`toolkit_db::secure::DbTx`): the idempotency gate, the
//! fiscal-period guard, and the balance projector.

pub(crate) mod chain;
pub mod chart;
pub(crate) mod freeze;
pub mod idempotency;
pub mod period;
pub mod projector;
pub mod service;
