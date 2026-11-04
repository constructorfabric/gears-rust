//! Reporting of ancestor credentials the merge did not send (`inst-merge-3a0`).
//!
//! The caller only sees a 401 from its upstream or a 403 from OAGW, so the
//! operator needs the server side: a counter per request and a warning per
//! binding, at most once a minute.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use toolkit_macros::domain_model;
use uuid::Uuid;

use super::merge::{Withheld, WithheldKind};
use crate::domain::ports::OagwMetricsPort;
use crate::domain::ports::metric_labels::withheld_outcome;

/// How often one binding may log the same warning.
const WARN_EVERY: Duration = Duration::from_mins(1);

/// Bindings remembered. When all are within their quiet minute, further
/// bindings do not warn until one expires.
const MAX_REMEMBERED: usize = 1024;

/// What a warning is about: the owner, the upstream, and what was withheld.
type Key = (Uuid, Uuid, WithheldKind);

/// Counter and rate-limited warnings for withheld credentials.
#[domain_model]
#[derive(Default)]
pub(super) struct WithheldLog {
    last_warned: Mutex<HashMap<Key, Instant>>,
}

impl WithheldLog {
    /// Count `withheld` and warn about it unless the same binding warned less
    /// than [`WARN_EVERY`] ago.
    pub(super) fn report(&self, metrics: Option<&dyn OagwMetricsPort>, withheld: &Withheld) {
        let (outcome, what) = match withheld.kind {
            WithheldKind::Auth => (withheld_outcome::DROPPED_AUTH, "auth dropped"),
            WithheldKind::Headers => (withheld_outcome::DROPPED_HEADERS, "headers dropped"),
            WithheldKind::EnforcedAuth => (
                withheld_outcome::ENFORCED_REFUSED,
                "request refused (enforced auth)",
            ),
        };
        if let Some(metrics) = metrics {
            metrics.record_credential_withheld(&withheld.alias, outcome);
        }
        if self.warns(withheld) {
            tracing::warn!(
                alias = %withheld.alias,
                owner_upstream_id = %withheld.owner_id,
                upstream_id = %withheld.upstream_id,
                outcome,
                "ancestor {what}: the upstream serves the alias on endpoints the owner of \
                 the credential did not configure"
            );
        }
    }

    /// Whether `withheld` may warn now; if so, remember that it did.
    pub(super) fn warns(&self, withheld: &Withheld) -> bool {
        let key: Key = (withheld.owner_id, withheld.upstream_id, withheld.kind);
        let now = Instant::now();
        let mut last = self
            .last_warned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last
            .get(&key)
            .is_some_and(|at| now.duration_since(*at) < WARN_EVERY)
        {
            return false;
        }
        if last.len() >= MAX_REMEMBERED {
            last.retain(|_, at| now.duration_since(*at) < WARN_EVERY);
            if last.len() >= MAX_REMEMBERED {
                // Every remembered binding is still quiet; a new one waits
                // for a free slot. The counter still counts it.
                return false;
            }
        }
        last.insert(key, now);
        true
    }
}
