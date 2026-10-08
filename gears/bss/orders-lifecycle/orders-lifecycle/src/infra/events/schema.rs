//! GTS documents for the Orders event contract (DESIGN §4.7): the Orders topic instance, the
//! abstract order-event base and the eleven final event types. Orders registers them at init;
//! Event Broker loads them from `types-registry` and the producer prepares them before
//! readiness. The schemas are the published contract consumers validate against.
use serde_json::{Value, json};

use super::payload::{
    FailureReason, MAX_CATEGORY_CHARS, MAX_DURATION_CHARS, MAX_EXTERNAL_REFERENCE_CHARS, MAX_LINES,
    MAX_REASON_CHARS, MAX_REFERENCE_CHARS,
};
use super::{EVENT_BASE, ORDER_SUBJECT_TYPE, PARTITION_KEY, TOPIC};

const DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";
const PLATFORM_EVENT_BASE: &str = "gts.cf.core.events.event.v1~";
const UUID_PATTERN: &str = "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$";
const STATES: [&str; 11] = [
    "draft",
    "submitted",
    "pending_approval",
    "approved",
    "in_fulfillment",
    "on_hold",
    "completed",
    "rejected",
    "cancelled",
    "fulfillment_failed",
    "expired",
];
/// Members of the common summary, in §4.4 order; required unless listed in `OPTIONAL_COMMON`.
const COMMON: [&str; 11] = [
    "orderId",
    "orderVersion",
    "occurredAt",
    "correlationId",
    "category",
    "state",
    "resourceTenantId",
    "sellerTenantId",
    "payerTenantId",
    "contractId",
    "externalReference",
];
const OPTIONAL_COMMON: [&str; 2] = ["contractId", "externalReference"];

fn gts_uri(id: &str) -> String {
    format!("gts://{id}")
}
fn uuid() -> Value {
    json!({"type": "string", "pattern": UUID_PATTERN})
}
fn version() -> Value {
    json!({"type": "integer", "minimum": 1, "maximum": i32::MAX})
}
fn revision() -> Value {
    json!({"type": "integer", "minimum": 1})
}
fn instant() -> Value {
    json!({"type": "string", "format": "date-time", "maxLength": 64})
}
fn state() -> Value {
    json!({"type": "string", "enum": STATES})
}
/// The row-21 hold sources, which are also the only states row 22 restores.
fn holdable_state() -> Value {
    json!({"type": "string", "enum": ["submitted", "pending_approval", "approved", "in_fulfillment"]})
}
/// Caller text: no control characters other than tab/CR/LF (audit `caller_reason` rule).
fn reason() -> Value {
    json!({
        "type": "string", "minLength": 1, "maxLength": MAX_REASON_CHARS,
        "pattern": "^[^\\u0000-\\u0008\\u000B\\u000C\\u000E-\\u001F\\u007F-\\u009F]+$",
    })
}
fn reference() -> Value {
    json!({
        "type": "string", "minLength": 1, "maxLength": MAX_REFERENCE_CHARS,
        "pattern": "^[\\x20-\\x7E]+$",
    })
}
fn uuid_list(min: usize) -> Value {
    json!({"type": "array", "items": uuid(), "minItems": min, "maxItems": MAX_LINES, "uniqueItems": true})
}

/// The common order summary every event's `data` carries.
fn common_data() -> Value {
    let required: Vec<&str> = COMMON
        .iter()
        .copied()
        .filter(|m| !OPTIONAL_COMMON.contains(m))
        .collect();
    json!({
        "type": "object",
        "required": required,
        "properties": {
            "orderId": uuid(),
            "orderVersion": version(),
            "occurredAt": instant(),
            "correlationId": uuid(),
            "category": {
                "type": "string", "minLength": 1, "maxLength": MAX_CATEGORY_CHARS,
                "pattern": "^gts\\.cf\\.bss\\.orders\\.category\\.v1~[\\x21-\\x7E]+$",
            },
            "state": state(),
            "resourceTenantId": uuid(),
            "sellerTenantId": uuid(),
            "payerTenantId": uuid(),
            "contractId": uuid(),
            "externalReference": {
                "type": "string", "minLength": 1, "maxLength": MAX_EXTERNAL_REFERENCE_CHARS,
            },
        },
    })
}

/// The closed DESIGN §3.7 compensation evidence; `forced` selects the D-182 variant and
/// `no_active_true` the acknowledgement/cancel guard that no active subscription remains.
fn compensation(forced: bool, no_active_true: bool) -> Value {
    let assertion = if forced {
        json!({"const": "unknown"})
    } else if no_active_true {
        json!({"const": true})
    } else {
        json!({"type": "boolean"})
    };
    let at_sale = if forced {
        json!({"const": "unknown"})
    } else {
        json!({"type": "boolean"})
    };
    let mut properties = json!({
        "drafts_voided": uuid_list(0),
        "activated_rolled_back": uuid_list(0),
        "activation_dispatched": if forced { json!({"const": true}) } else { json!({"type": "boolean"}) },
        "at_sale_facts_emitted": at_sale,
        "no_active_subscription_remains": assertion,
    });
    let mut required = vec![
        "drafts_voided",
        "activated_rolled_back",
        "activation_dispatched",
        "at_sale_facts_emitted",
        "no_active_subscription_remains",
    ];
    if forced {
        properties["operator_attestation"] = json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["requested_by", "request_audit_id", "requested_at", "approved_by"],
            "properties": {
                "requested_by": uuid(),
                "request_audit_id": uuid(),
                "requested_at": instant(),
                "approved_by": uuid(),
            },
        });
        required.push("operator_attestation");
    }
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": properties,
    })
}

/// A concrete event's `data` narrowing: its own members plus the inherited common members,
/// restated with their exact base schemas (a derived type may only narrow), then closed.
fn closed(required: &[&str], mut specific: Value) -> Value {
    let common = common_data();
    if let Some(properties) = specific.as_object_mut() {
        for member in COMMON {
            properties.insert(member.to_owned(), common["properties"][member].clone());
        }
    }
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": specific,
    })
}

/// `(final type id, description, data narrowing)` for the eleven events, in §4.4 order.
fn concrete() -> Vec<(&'static str, &'static str, Value)> {
    use super::payload::{
        EventKind, OrderAcceptanceRecorded, OrderAmended, OrderApproved, OrderCancelled,
        OrderCompleted, OrderExpired, OrderFulfillmentFailed, OrderHeld, OrderRejected,
        OrderResumed, OrderSubmitted,
    };
    let failure_reasons: Vec<&str> = FailureReason::ALL.iter().map(|r| r.token()).collect();
    vec![
        (
            OrderSubmitted::TYPE_ID,
            "Orders: an order was submitted (row 4).",
            closed(
                &["lines"],
                json!({
                    "lines": {
                        "type": "array", "minItems": 1, "maxItems": MAX_LINES, "uniqueItems": true,
                        "items": {
                            "type": "object", "additionalProperties": false,
                            "required": ["lineId"], "properties": {"lineId": uuid()},
                        },
                    },
                    "acceptance": {
                        "type": "object", "additionalProperties": false,
                        "required": ["acceptedVersion", "acceptedAt"],
                        "properties": {
                            "acceptedVersion": {"type": "integer", "minimum": 2, "maximum": i32::MAX},
                            "acceptedAt": instant(),
                        },
                    },
                }),
            ),
        ),
        (
            OrderApproved::TYPE_ID,
            "Orders: an order was approved (rows 8, 9).",
            closed(
                &["decidingAuthority", "approvedVersion"],
                json!({"decidingAuthority": reference(), "approvedVersion": version()}),
            ),
        ),
        (
            OrderRejected::TYPE_ID,
            "Orders: an order was rejected (row 10).",
            closed(
                &["decidingAuthority", "denialReason"],
                json!({"decidingAuthority": reference(), "denialReason": reason()}),
            ),
        ),
        (
            OrderAmended::TYPE_ID,
            "Orders: an amendment appended a new order version (rows 18-20).",
            closed(
                &["supersedesVersion"],
                json!({"supersedesVersion": version()}),
            ),
        ),
        (
            OrderHeld::TYPE_ID,
            "Orders: an order was put on hold (row 21).",
            closed(
                &["previousState"],
                json!({"previousState": holdable_state(), "holdReason": reason()}),
            ),
        ),
        (
            OrderResumed::TYPE_ID,
            "Orders: a held order resumed (row 22).",
            closed(
                &["restoredState"],
                json!({"restoredState": holdable_state()}),
            ),
        ),
        (
            OrderCancelled::TYPE_ID,
            "Orders: an order was cancelled (rows 5, 15-17, 23, 27).",
            closed(
                &["cancellingActor", "cancelReason"],
                json!({
                    "cancellingActor": reference(),
                    "cancelReason": reason(),
                    "compensationEvidence": compensation(false, true),
                }),
            ),
        ),
        (
            OrderExpired::TYPE_ID,
            "Orders: an order expired by its state TTL (rows 6, 24).",
            closed(
                &[
                    "expiredState",
                    "ttl",
                    "ttlPolicyId",
                    "ttlPolicyRevision",
                    "platformPolicyRevision",
                ],
                json!({
                    "expiredState": {
                        "type": "string",
                        "enum": ["draft", "submitted", "pending_approval", "approved", "on_hold"],
                    },
                    "ttl": {
                        "type": "string", "minLength": 2, "maxLength": MAX_DURATION_CHARS,
                        "pattern": "^P[0-9A-Za-z]+$",
                    },
                    "ttlPolicyId": uuid(),
                    "ttlPolicyRevision": revision(),
                    "platformPolicyRevision": revision(),
                }),
            ),
        ),
        (
            OrderCompleted::TYPE_ID,
            "Orders: fulfillment completed with the per-line subscription mapping (row 13).",
            closed(
                &["lines"],
                json!({
                    "lines": {
                        "type": "array", "minItems": 1, "maxItems": MAX_LINES, "uniqueItems": true,
                        "items": {
                            "type": "object", "additionalProperties": false,
                            "required": ["lineId", "subscriptionId"],
                            "properties": {"lineId": uuid(), "subscriptionId": uuid()},
                        },
                    },
                }),
            ),
        ),
        (
            OrderFulfillmentFailed::TYPE_ID,
            "Orders: fulfillment failed (rows 14, 26, 28, 29).",
            closed(
                &["failureReason", "compensationEvidence"],
                json!({
                    "failureReason": {"type": "string", "enum": failure_reasons},
                    "forcedReason": reason(),
                    // The registry's compatibility checker admits no `if/then/else`; the
                    // reason-to-variant coupling (D-182) is enforced by Orders before enqueue.
                    "compensationEvidence": {
                        "oneOf": [compensation(false, true), compensation(true, false)],
                    },
                }),
            ),
        ),
        (
            OrderAcceptanceRecorded::TYPE_ID,
            "Orders: customer acceptance of a version was recorded (row 25).",
            closed(
                &[
                    "acceptedVersion",
                    "acceptedAt",
                    "recordingActor",
                    "requirementSource",
                ],
                json!({
                    "acceptedVersion": {"type": "integer", "minimum": 2, "maximum": i32::MAX},
                    "acceptedAt": instant(),
                    "recordingActor": reference(),
                    "requirementSource": {
                        "type": "string",
                        "enum": ["contract", "seller", "platform_default", "volunteered"],
                    },
                }),
            ),
        ),
    ]
}

/// The Orders topic instance (an instance of `gts.cf.core.events.topic.v1~`). Retention is the
/// broker's configured default; the partition count is broker deployment configuration.
pub fn topic_instance() -> Value {
    json!({
        "id": TOPIC,
        "description": "Orders Lifecycle internal lifecycle events: platform-root tenancy, one partition stream per order (D-95, D-200).",
    })
}

/// The abstract Orders event base: narrows `data` to the common summary. It declares no
/// traits, so Event Broker never resolves it as a publishable type.
pub fn abstract_base() -> Value {
    json!({
        "$id": gts_uri(EVENT_BASE),
        "$schema": DRAFT_07,
        "description": "Orders Lifecycle event family (abstract): the common order summary every Orders lifecycle event carries.",
        "x-gts-abstract": true,
        "type": "object",
        "allOf": [
            {"$ref": gts_uri(PLATFORM_EVENT_BASE)},
            {"type": "object", "required": ["data"], "properties": {"data": common_data()}},
        ],
    })
}

/// The eleven final event types.
pub fn final_types() -> Vec<Value> {
    concrete()
        .into_iter()
        .map(|(id, description, data)| {
            json!({
                "$id": gts_uri(id),
                "$schema": DRAFT_07,
                "description": description,
                "x-gts-final": true,
                "x-gts-traits": {
                    "topic": TOPIC,
                    "allowed_subject_types": [ORDER_SUBJECT_TYPE],
                    "partition_key": PARTITION_KEY,
                },
                "type": "object",
                "allOf": [
                    {"$ref": gts_uri(EVENT_BASE)},
                    {"type": "object", "required": ["data"], "properties": {"data": data}},
                ],
            })
        })
        .collect()
}

/// Every event-contract document Orders registers, base before derived types.
pub fn documents() -> Vec<Value> {
    let mut documents = vec![topic_instance(), abstract_base()];
    documents.extend(final_types());
    documents
}
