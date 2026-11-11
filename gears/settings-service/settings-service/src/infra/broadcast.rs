//! The `cache_invalidate` events of a request: the broker-native envelope, the
//! size accounting against the broker's event limit, and the split of a key set
//! that would not fit.
//!
//! The domain gathers what a request touched ([`TenantKeys`]) and decides what
//! becomes of each publish; what an event looks like on the wire is the broker
//! client's, so it is built here.

use event_broker_sdk::validate::is_trace_parent;
use event_broker_sdk::{EventBrokerError, EventV1, GtsTypeId};
use settings_service_sdk::gts::{
    ACTIVATION_SUBJECT_TYPE, CACHE_INVALIDATE_EVENT_TYPE, CHANGE_NOTIFICATION_EVENT_TYPE,
};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::domain::activation::{BuiltNotification, NotifiedChange, SubscriberTenant};
use crate::domain::coherence::{BroadcastContext, TenantKeys};
use crate::infra::event_schemas::{CacheInvalidation, ChangeNotification, NotifiedKey};

/// The envelope `source` of every broadcast: the gear's name.
pub const EVENT_SOURCE: &str = "settings-service";

/// The broker's event size limit, measured as the broker measures it: the
/// event's JSON form, every member included ([`EventV1::encoded_len`]). The
/// broker refuses a larger event, so a broadcast stays strictly under it, with
/// [`HEADROOM`] to spare.
pub const BROKER_EVENT_LIMIT_BYTES: usize = EventV1::MAX_ENCODED_BYTES;

/// What a part keeps free below the limit. The base of a part is measured on
/// the envelope of its first event, so the room left covers everything that can
/// differ between the events of one request (the timestamp's fractional digits,
/// at most nine) by a margin far wider than any of it; the maximal-width
/// timestamp is not measured.
pub const HEADROOM: usize = 1024;

/// The size of `event` as the broker's limit counts it. An event that cannot
/// be measured, which no JSON `data` is, counts as over any limit, so the split
/// keeps each key of it apart.
#[must_use]
pub fn serialised_len(event: &EventV1) -> usize {
    event.encoded_len().unwrap_or(usize::MAX)
}

/// The request's trace context as the envelope may carry it: the broker refuses
/// an event whose `trace_parent` is not a W3C traceparent, and a broadcast is
/// worth more than the trace link, so a malformed one is left out.
fn envelope_trace_parent(ctx: &BroadcastContext) -> Option<&str> {
    let trace_parent = ctx.trace_parent.as_deref()?;
    if is_trace_parent(trace_parent) {
        Some(trace_parent)
    } else {
        debug!(
            change_set_id = %ctx.change_set_id,
            "malformed trace context left out of the cache_invalidate envelope"
        );
        None
    }
}

fn envelope(
    ctx: &BroadcastContext,
    trace_parent: Option<&str>,
    tenant: &str,
    keys: Vec<String>,
) -> Result<EventV1, EventBrokerError> {
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
    // The builder fixes the event's id (a fresh UUID v7) and `occurred_at`
    // (now) when it builds.
    let event_type = GtsTypeId::new(CACHE_INVALIDATE_EVENT_TYPE);
    let builder = match serde_json::to_value(&body) {
        Ok(data) => EventV1::builder_for(event_type, data),
        Err(_) => EventV1::builder_without_data(event_type),
    }
    .tenant_id(ctx.root_tenant)
    .source(EVENT_SOURCE)
    .subject(tenant)
    .subject_type_id(GtsTypeId::new(ACTIVATION_SUBJECT_TYPE));
    match trace_parent {
        Some(trace_parent) => builder.trace_parent(trace_parent),
        None => builder,
    }
    .build()
    // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-7
}

/// Keep a built part. Every member of the envelope is the gear's own and the
/// tenant's envelope already built once, so a refusal here is a broken
/// invariant; it is logged rather than published broken.
fn keep(events: &mut Vec<EventV1>, tenant: &str, built: Result<EventV1, EventBrokerError>) {
    match built {
        Ok(event) => events.push(event),
        Err(error) => warn!(
            %tenant,
            %error,
            "cache_invalidate not built; peers converge within cache_ttl_seconds"
        ),
    }
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
        if !part.is_empty() && len.saturating_add(cost) >= budget {
            parts.push(std::mem::take(&mut part));
            len = base;
        }
        len = len.saturating_add(quoted + usize::from(!part.is_empty()));
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
pub fn build_broadcasts(touched: &[TenantKeys], ctx: &BroadcastContext) -> Vec<EventV1> {
    let trace_parent = envelope_trace_parent(ctx);
    let mut events = Vec::new();
    for tenant in touched {
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-4
        let part_of = |keys| envelope(ctx, trace_parent, &tenant.tenant, keys);
        let base = match part_of(Vec::new()) {
            Ok(empty) => serialised_len(&empty),
            Err(error) => {
                keep(&mut events, &tenant.tenant, Err(error));
                continue;
            }
        };
        let parts = pack(&tenant.keys, base, |key| {
            serde_json::to_string(key).map_or(key.len() + 2, |s| s.len())
        });
        for keys in parts {
            keep(&mut events, &tenant.tenant, part_of(keys));
        }
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
) -> Result<EventV1, EventBrokerError> {
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
    // The builder fixes the event's id (a fresh UUID v7) and `occurred_at`
    // (now) when it builds.
    let event_type = GtsTypeId::new(CHANGE_NOTIFICATION_EVENT_TYPE);
    let builder = match serde_json::to_value(&body) {
        Ok(data) => EventV1::builder_for(event_type, data),
        Err(_) => EventV1::builder_without_data(event_type),
    }
    .tenant_id(tenant)
    .source(EVENT_SOURCE)
    .subject(pair.tenant.as_str())
    .subject_type_id(GtsTypeId::new(ACTIVATION_SUBJECT_TYPE));
    match trace_parent {
        Some(trace_parent) => builder.trace_parent(trace_parent),
        None => builder,
    }
    .build()
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-8
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-8
}

/// The trace context a notification's envelope may carry: the broker refuses
/// an event whose `trace_parent` is not a W3C traceparent, and a notification
/// is worth more than the trace link, so a malformed one is left out.
fn notification_trace_parent<'a>(
    pair: &SubscriberTenant,
    trace_parent: Option<&'a str>,
) -> Option<&'a str> {
    let trace_parent = trace_parent?;
    if is_trace_parent(trace_parent) {
        Some(trace_parent)
    } else {
        debug!(
            subscriber = %pair.subscriber,
            tenant = %pair.tenant,
            "malformed trace context left out of the change_notification envelope"
        );
        None
    }
}

/// The notifications of one pair, from its states in key order: one event or —
/// when the serialised event, envelope included, would reach the broker's limit
/// less [`HEADROOM`] — several, entries packed in order until the next would
/// exceed it, each kept whole, each with the same `tenant` and `subscriber` and
/// an `id` of its own, together carrying every entry once.
///
/// A pair whose tenant is not a UUID builds nothing: the tables hold bare UUID
/// strings, so this does not occur, and an event the broker would refuse is not
/// worth publishing. A part whose envelope the broker SDK refuses is logged and
/// left out the same way; its states stay `awaiting` for the delivery loop.
#[must_use]
pub fn build_notifications(
    pair: &SubscriberTenant,
    changes: &[NotifiedChange],
    trace_parent: Option<&str>,
) -> Vec<BuiltNotification> {
    let Ok(tenant) = Uuid::parse_str(&pair.tenant) else {
        return Vec::new();
    };
    let trace_parent = notification_trace_parent(pair, trace_parent);
    // Every member of the envelope is the gear's own, so a refusal is a broken
    // invariant: logged, and the part left to the delivery loop rather than
    // published broken.
    let refused = |error: &EventBrokerError| {
        warn!(
            subscriber = %pair.subscriber,
            tenant = %pair.tenant,
            %error,
            "change_notification not built; the delivery loop keeps its states awaiting"
        );
    };
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-bundle:p1:inst-ca-bundle-4
    let base = match notification_envelope(pair, tenant, &[], trace_parent) {
        Ok(empty) => serialised_len(&empty),
        Err(error) => {
            refused(&error);
            return Vec::new();
        }
    };
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
        .filter_map(
            |part| match notification_envelope(pair, tenant, &part, trace_parent) {
                Ok(event) => Some(BuiltNotification {
                    event,
                    changes: part,
                }),
                Err(error) => {
                    refused(&error);
                    None
                }
            },
        )
        .collect()
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-bundle:p1:inst-ca-bundle-5
}

#[cfg(test)]
#[path = "broadcast_tests.rs"]
mod broadcast_tests;
