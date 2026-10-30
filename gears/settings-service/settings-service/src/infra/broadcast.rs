//! The `cache_invalidate` events of a request: the broker-native envelope, the
//! size accounting against the broker's event limit, and the split of a key set
//! that would not fit.
//!
//! The domain gathers what a request touched ([`TenantKeys`]) and decides what
//! becomes of each publish; what an event looks like on the wire is the broker
//! client's, so it is built here.

use chrono::Utc;
use event_broker_sdk::{Event, GtsTypeId};
use settings_service_sdk::gts::{ACTIVATION_SUBJECT_TYPE, CACHE_INVALIDATE_EVENT_TYPE};
use uuid::Uuid;

use crate::domain::coherence::{BroadcastContext, TenantKeys};
use crate::infra::event_schemas::CacheInvalidation;

/// The envelope `source` of every broadcast: the gear's name.
pub const EVENT_SOURCE: &str = "settings-service";

/// The broker's event size limit, measured on the serialised event, envelope
/// included. A broadcast stays strictly under it, with [`HEADROOM`] to spare.
pub const BROKER_EVENT_LIMIT_BYTES: usize = 64 * 1024;

/// What a part keeps free below the limit. The base of a part is measured on
/// the envelope of its first event, so the room left covers everything that can
/// differ between the events of one request (the timestamp's fractional digits,
/// at most nine, and the fields the broker adds on its side) by a margin far
/// wider than any of them; the maximal-width timestamp is not measured.
pub const HEADROOM: usize = 1024;

/// The size of `event` as it travels: its envelope members and `data`, as JSON.
#[must_use]
pub fn serialised_len(event: &Event) -> usize {
    serde_json::json!({
        "id": event.id,
        "type_id": event.type_id.as_ref() as &str,
        "tenant_id": event.tenant_id,
        "source": event.source,
        "subject": event.subject,
        "subject_type": event.subject_type.as_ref() as &str,
        "occurred_at": event.occurred_at,
        "trace_parent": event.trace_parent,
        "data": event.data,
    })
    .to_string()
    .len()
}

fn envelope(ctx: &BroadcastContext, tenant: &str, keys: Vec<String>) -> Event {
    // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-3
    let body = CacheInvalidation {
        change_set_id: ctx.change_set_id,
        tenant: tenant.to_owned(),
        changed_keys: keys,
        subject_type: None,
        subject_id: None,
    };
    // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-3
    // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-7
    Event {
        id: Uuid::new_v4(),
        type_id: GtsTypeId::new(CACHE_INVALIDATE_EVENT_TYPE),
        tenant_id: ctx.root_tenant,
        source: EVENT_SOURCE.to_owned(),
        subject: tenant.to_owned(),
        subject_type: GtsTypeId::new(ACTIVATION_SUBJECT_TYPE),
        occurred_at: Utc::now(),
        trace_parent: ctx.trace_parent.clone(),
        data: serde_json::to_value(&body).ok(),
        partition: None,
        sequence: None,
        sequence_time: None,
        meta: None,
    }
    // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-7
}

/// The broadcasts of a request: per tenant, in first-touch order, one event or
/// — when its key set would not fit the broker's limit — several, keys packed
/// in order until the next would reach the limit less [`HEADROOM`], each part
/// under the limit, with its own `id`.
#[must_use]
pub fn build_broadcasts(touched: &[TenantKeys], ctx: &BroadcastContext) -> Vec<Event> {
    let budget = BROKER_EVENT_LIMIT_BYTES - HEADROOM;
    let mut events = Vec::new();
    for tenant in touched {
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-4
        let base = serialised_len(&envelope(ctx, &tenant.tenant, Vec::new()));
        let mut part: Vec<String> = Vec::new();
        let mut len = base;
        for key in &tenant.keys {
            let quoted = serde_json::to_string(key).map_or(key.len() + 2, |s| s.len());
            // The comma between two keys of one part.
            let cost = quoted + usize::from(!part.is_empty());
            if !part.is_empty() && len + cost >= budget {
                events.push(envelope(ctx, &tenant.tenant, std::mem::take(&mut part)));
                len = base;
            }
            len += quoted + usize::from(!part.is_empty());
            part.push(key.clone());
        }
        if !part.is_empty() {
            events.push(envelope(ctx, &tenant.tenant, part));
        }
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-4
    }
    events
}

#[cfg(test)]
#[path = "broadcast_tests.rs"]
mod broadcast_tests;
