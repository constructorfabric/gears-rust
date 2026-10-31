//! The `cache_invalidate` events of a request: the broker-native envelope, the
//! size accounting against the broker's event limit, and the split of a key set
//! that would not fit.
//!
//! The domain gathers what a request touched ([`TenantKeys`]) and decides what
//! becomes of each publish; what an event looks like on the wire is the broker
//! client's, so it is built here.

use chrono::Utc;
use event_broker_sdk::{Event, GtsTypeId};
use settings_service_sdk::gts::{
    ACTIVATION_SUBJECT_TYPE, CACHE_INVALIDATE_EVENT_TYPE, CHANGE_NOTIFICATION_EVENT_TYPE,
};
use uuid::Uuid;

use crate::domain::activation::{BuiltNotification, NotifiedChange, SubscriberTenant};
use crate::domain::coherence::{BroadcastContext, TenantKeys};
use crate::infra::event_schemas::{CacheInvalidation, ChangeNotification, NotifiedKey};

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

/// Split `items` into parts that each fit under the limit less [`HEADROOM`]:
/// entries are packed in order until the next would reach it, each kept whole.
/// `base` is the size of the event with no entry, and `size` the serialised
/// size of one entry. Every part holds at least one entry, so an entry larger
/// than the budget travels alone rather than being lost.
fn pack<T: Clone>(items: &[T], base: usize, size: impl Fn(&T) -> usize) -> Vec<Vec<T>> {
    let budget = BROKER_EVENT_LIMIT_BYTES - HEADROOM;
    let mut parts = Vec::new();
    let mut part: Vec<T> = Vec::new();
    let mut len = base;
    for item in items {
        let quoted = size(item);
        // The comma between two entries of one part.
        let cost = quoted + usize::from(!part.is_empty());
        if !part.is_empty() && len + cost >= budget {
            parts.push(std::mem::take(&mut part));
            len = base;
        }
        len += quoted + usize::from(!part.is_empty());
        part.push(item.clone());
    }
    if !part.is_empty() {
        parts.push(part);
    }
    parts
}

/// The broadcasts of a request: per tenant, in first-touch order, one event or
/// — when its key set would not fit the broker's limit — several, keys packed
/// in order until the next would reach the limit less [`HEADROOM`], each part
/// under the limit, with its own `id`.
#[must_use]
pub fn build_broadcasts(touched: &[TenantKeys], ctx: &BroadcastContext) -> Vec<Event> {
    let mut events = Vec::new();
    for tenant in touched {
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-4
        let base = serialised_len(&envelope(ctx, &tenant.tenant, Vec::new()));
        let parts = pack(&tenant.keys, base, |key| {
            serde_json::to_string(key).map_or(key.len() + 2, |s| s.len())
        });
        events.extend(
            parts
                .into_iter()
                .map(|keys| envelope(ctx, &tenant.tenant, keys)),
        );
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-4
    }
    events
}

/// The envelope of one `change_notification`: `tenant_id`, `subject` the
/// subscription's tenant, `subject_type` the activation subject type, `source`
/// the gear, an `id` of its own, `occurred_at` now, and `trace_parent` from the
/// current span when there is one.
fn notification_envelope(
    pair: &SubscriberTenant,
    tenant: Uuid,
    changes: &[NotifiedChange],
    trace_parent: Option<&str>,
) -> Event {
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-bundle:p1:inst-ca-bundle-1
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-bundle:p1:inst-ca-bundle-2
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-bundle:p1:inst-ca-bundle-3
    // The body names the tenant, the subscriber and each key with its
    // `change_ref`, and nothing else: no value, no expected value, no hash, no
    // actor, no key outside the pair's rows; the optional subject pair stays
    // absent in v1.
    let body = ChangeNotification {
        tenant: pair.tenant.clone(),
        subscriber: pair.subscriber.clone(),
        changes: changes
            .iter()
            .map(|c| NotifiedKey {
                key: c.key.clone(),
                change_ref: c.change_ref,
            })
            .collect(),
        subject_type: None,
        subject_id: None,
    };
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-bundle:p1:inst-ca-bundle-3
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-bundle:p1:inst-ca-bundle-2
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-bundle:p1:inst-ca-bundle-1
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-8
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-8
    Event {
        id: Uuid::new_v4(),
        type_id: GtsTypeId::new(CHANGE_NOTIFICATION_EVENT_TYPE),
        tenant_id: tenant,
        source: EVENT_SOURCE.to_owned(),
        subject: pair.tenant.clone(),
        subject_type: GtsTypeId::new(ACTIVATION_SUBJECT_TYPE),
        occurred_at: Utc::now(),
        trace_parent: trace_parent.map(str::to_owned),
        data: serde_json::to_value(&body).ok(),
        partition: None,
        sequence: None,
        sequence_time: None,
        meta: None,
    }
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-8
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-8
}

/// The notifications of one pair, from its states in key order: one event or —
/// when the serialised event, envelope included, would reach the broker's limit
/// less [`HEADROOM`] — several, entries packed in order until the next would
/// exceed it, each kept whole, each with the same `tenant` and `subscriber` and
/// an `id` of its own, together carrying every entry once.
///
/// A pair whose tenant is not a UUID builds nothing: the tables hold bare UUID
/// strings, so this does not occur, and an event the broker would refuse is not
/// worth publishing.
#[must_use]
pub fn build_notifications(
    pair: &SubscriberTenant,
    changes: &[NotifiedChange],
    trace_parent: Option<&str>,
) -> Vec<BuiltNotification> {
    let Ok(tenant) = Uuid::parse_str(&pair.tenant) else {
        return Vec::new();
    };
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-bundle:p1:inst-ca-bundle-4
    let base = serialised_len(&notification_envelope(pair, tenant, &[], trace_parent));
    let parts = pack(changes, base, |change| {
        // `{"key":"…","change_ref":"…"}`.
        serde_json::to_string(&NotifiedKey {
            key: change.key.clone(),
            change_ref: change.change_ref,
        })
        .map_or(change.key.len() + 64, |s| s.len())
    });
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-bundle:p1:inst-ca-bundle-4
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-bundle:p1:inst-ca-bundle-5
    parts
        .into_iter()
        .map(|part| BuiltNotification {
            event: notification_envelope(pair, tenant, &part, trace_parent),
            changes: part,
        })
        .collect()
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-bundle:p1:inst-ca-bundle-5
}

#[cfg(test)]
#[path = "broadcast_tests.rs"]
mod broadcast_tests;
