//! Each configuration-level rule fails on its own violation and passes
//! otherwise (feature 0005 §8, "config checks").

use std::collections::BTreeMap;

use serde_json::json;

use sea_orm::DbBackend;

use super::{Error, check, gear_database, routing};
use crate::config::{DeploymentMode, EventBrokerConfig};
use crate::domain::backend::{Name, Routing};
use crate::domain::outbox::OutboxPartitions;

const MEMORY_BACKEND: &str = "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~";
const SQLITE_BACKEND: &str = "gts.cf.core.events.backend.v1~cf.core.backend.sqlite.v1~";
const TOPIC_TYPE: &str = "gts.cf.core.events.topic.v1~";
const AUDIT_TOPIC: &str = "gts.cf.core.events.topic.v1~vendor.app.audit.topic.v1";

fn config(backends: &serde_json::Value, backend_routing: &serde_json::Value) -> EventBrokerConfig {
    serde_json::from_value(json!({
        "mode": "standalone",
        "backends": backends.clone(),
        "backend_routing": backend_routing.clone(),
    }))
    .expect("test configuration deserializes")
}

fn name(raw: &str) -> Name {
    Name::parse(raw).expect("a valid name")
}

/// Two entries, one of them carrying a settings value that must never appear
/// in an error.
fn two_backends() -> serde_json::Value {
    json!({
        "main": { "type": MEMORY_BACKEND },
        "audit": { "type": MEMORY_BACKEND, "secret": "hunter2" },
    })
}

#[test]
fn a_configuration_keeping_every_rule_passes_with_the_routing_it_states() {
    let cfg = config(
        &two_backends(),
        &json!({ TOPIC_TYPE: "main", AUDIT_TOPIC: "audit" }),
    );

    let deployment = check(&cfg, DeploymentMode::Standalone)
        .expect("every rule holds")
        .expect("standalone holds a registry");
    assert_eq!(
        deployment.backends.keys().cloned().collect::<Vec<_>>(),
        vec![name("audit"), name("main")]
    );
    assert_eq!(
        deployment.routing,
        Routing::new(
            name("main"),
            BTreeMap::from([(AUDIT_TOPIC.to_owned(), name("audit"))])
        )
    );
    assert_eq!(deployment.outbox_partitions, OutboxPartitions::DEFAULT);
}

/// A key naming a topic type derived from the base is a topic key.
#[test]
fn a_derived_topic_type_key_is_accepted() {
    let special = "gts.cf.core.events.topic.v1~vendor.app.special.topic.v1~";
    let cfg = config(
        &two_backends(),
        &json!({ TOPIC_TYPE: "main", special: "audit" }),
    );

    let deployment = check(&cfg, DeploymentMode::Standalone)
        .expect("every rule holds")
        .expect("standalone holds a registry");
    assert_eq!(
        deployment.routing,
        Routing::new(
            name("main"),
            BTreeMap::from([(special.to_owned(), name("audit"))])
        )
    );
}

/// Ingest and delivery are subject to the same rules as standalone.
#[test]
fn ingest_and_delivery_are_checked_too() {
    let cfg = config(&json!({ "main": { "type": MEMORY_BACKEND } }), &json!({}));

    for mode in [
        DeploymentMode::ClusterIngest,
        DeploymentMode::ClusterDelivery,
    ] {
        assert_eq!(
            check(&cfg, mode).err(),
            Some(Error::MissingBaseRoute),
            "{mode:?}"
        );
    }
}

/// A dispatcher holds no registry: it carries the sections without being
/// subject to their rules.
#[test]
fn a_dispatcher_is_not_subject_to_the_registry_rules() {
    let cfg = config(
        &json!({ "bad name": { "type": MEMORY_BACKEND } }),
        &json!({}),
    );

    assert!(
        check(&cfg, DeploymentMode::ClusterDispatcher)
            .expect("nothing to check")
            .is_none()
    );
}

#[test]
fn a_backends_key_that_is_not_a_registry_name_is_refused_without_reproducing_it() {
    let cfg = config(
        &json!({ "main": { "type": MEMORY_BACKEND }, "postgres://eb@db": { "type": MEMORY_BACKEND } }),
        &json!({ TOPIC_TYPE: "main" }),
    );

    let err = check(&cfg, DeploymentMode::Standalone).expect_err("the key breaks section 2.1");
    assert_eq!(err, Error::InvalidBackendName { position: 1 });
    assert_eq!(
        err.to_string(),
        "backends: key 1 (in sorted order) is not a registry name; a registry name is 1 to 255 characters of [a-zA-Z0-9_-] (feature 0005 section 2.1)"
    );
}

/// A key that is not a GTS identifier under the topic base - here an event
/// type, and a plain string - would never match a topic.
#[test]
fn a_routing_key_that_is_not_a_topic_key_is_refused_without_reproducing_it() {
    for key in [
        "gts.cf.core.events.event.v1~vendor.audit.login.v1~",
        "audit-topic",
    ] {
        let cfg = config(
            &two_backends(),
            &json!({ TOPIC_TYPE: "main", key: "audit" }),
        );

        let err = check(&cfg, DeploymentMode::Standalone).expect_err("the key breaks section 2.2");
        assert_eq!(err, Error::InvalidRoutingKey { position: 0 }, "{key}");
        assert_eq!(
            err.to_string(),
            "backend_routing: key 0 (in sorted order) is not a topic key; a key is `gts.cf.core.events.topic.v1~`, the instance-less key of a topic type derived from it, or a topic instance identifier (feature 0005 section 2.2)"
        );
    }
}

/// C3. A value that is not even a valid name is the same mistake: it names
/// no entry.
#[test]
fn a_route_naming_no_registry_entry_is_refused() {
    for target in ["archive", "not a name"] {
        let cfg = config(
            &two_backends(),
            &json!({ TOPIC_TYPE: "main", AUDIT_TOPIC: target }),
        );

        let err = check(&cfg, DeploymentMode::Standalone).expect_err("C3");
        assert_eq!(
            err,
            Error::UnknownBackend {
                key: AUDIT_TOPIC.to_owned()
            },
            "{target}"
        );
        assert_eq!(
            err.to_string(),
            "backend_routing: the route for 'gts.cf.core.events.topic.v1~vendor.app.audit.topic.v1' names no registry entry; every route names an entry in `backends` (feature 0005 section 2.4, C3)"
        );
    }
}

/// C4.
#[test]
fn a_missing_instance_less_route_is_refused() {
    let cfg = config(&two_backends(), &json!({ AUDIT_TOPIC: "audit" }));

    let err = check(&cfg, DeploymentMode::Standalone).expect_err("C4");
    assert_eq!(err, Error::MissingBaseRoute);
    assert_eq!(
        err.to_string(),
        "backend_routing: there is no route for `gts.cf.core.events.topic.v1~`; it is required so that every topic resolves to a registry entry (feature 0005 section 2.4, C4)"
    );
}

/// C5.
#[test]
fn a_registry_entry_no_route_names_is_refused() {
    let cfg = config(&two_backends(), &json!({ TOPIC_TYPE: "main" }));

    let err = check(&cfg, DeploymentMode::Standalone).expect_err("C5");
    assert_eq!(
        err,
        Error::UnreferencedBackend {
            name: name("audit")
        }
    );
    assert_eq!(
        err.to_string(),
        "backends: entry 'audit' is named by no route; every registry entry is named by at least one route (feature 0005 section 2.4, C5)"
    );
}

/// The specification refresh needs the routing alone, and an unreferenced
/// entry does not stop it from resolving a topic.
#[test]
fn the_routing_alone_is_not_subject_to_c5() {
    let cfg = config(&two_backends(), &json!({ TOPIC_TYPE: "main" }));

    assert_eq!(
        routing(&cfg).expect("C5 is not a routing rule"),
        Routing::new(name("main"), BTreeMap::new())
    );
}

// ── SQLite only in standalone (feature 0005 section 2.4, C10) ─────────────

/// A `SQLite` entry next to a memory one, both routed to.
fn with_a_sqlite_entry(mode: &str) -> EventBrokerConfig {
    serde_json::from_value(json!({
        "mode": mode,
        "backends": {
            "main": { "type": MEMORY_BACKEND },
            "local": { "type": SQLITE_BACKEND, "path": "/var/lib/eb/events.db" },
        },
        "backend_routing": { TOPIC_TYPE: "main", AUDIT_TOPIC: "local" },
    }))
    .expect("test configuration deserializes")
}

#[test]
fn a_sqlite_entry_is_accepted_in_standalone() {
    let cfg = with_a_sqlite_entry("standalone");

    let deployment = check(&cfg, DeploymentMode::Standalone)
        .expect("every rule holds")
        .expect("standalone holds a registry");
    assert_eq!(
        deployment.backends.keys().cloned().collect::<Vec<_>>(),
        vec![name("local"), name("main")]
    );
}

#[test]
fn a_sqlite_entry_is_refused_in_cluster_ingest() {
    let cfg = with_a_sqlite_entry("cluster_ingest");

    let err = check(&cfg, DeploymentMode::ClusterIngest).expect_err("C10");
    assert_eq!(
        err,
        Error::SqliteBackendOutsideStandalone {
            name: name("local")
        }
    );
    assert_eq!(
        err.to_string(),
        "backends: entry 'local' has the SQLite backend type; a SQLite backend serves a single process, so only `standalone` mode runs one (feature 0005 section 2.4, C10)"
    );
}

#[test]
fn a_sqlite_entry_is_refused_in_cluster_delivery() {
    let cfg = with_a_sqlite_entry("cluster_delivery");

    let err = check(&cfg, DeploymentMode::ClusterDelivery).expect_err("C10");
    assert_eq!(
        err,
        Error::SqliteBackendOutsideStandalone {
            name: name("local")
        }
    );
}

/// A dispatcher builds no backend, so the registry's types are not its
/// concern.
#[test]
fn a_dispatcher_carries_a_sqlite_entry_unchecked() {
    let cfg = with_a_sqlite_entry("cluster_dispatcher");

    assert!(
        check(&cfg, DeploymentMode::ClusterDispatcher)
            .expect("nothing to check")
            .is_none()
    );
}

#[test]
fn a_sqlite_gear_database_is_accepted_in_standalone() {
    assert_eq!(
        gear_database(DeploymentMode::Standalone, DbBackend::Sqlite),
        Ok(())
    );
}

/// Every cluster mode opens the gear's database, the dispatcher included.
#[test]
fn a_sqlite_gear_database_is_refused_in_every_cluster_mode() {
    for mode in [
        DeploymentMode::ClusterIngest,
        DeploymentMode::ClusterDelivery,
        DeploymentMode::ClusterDispatcher,
    ] {
        let err = gear_database(mode, DbBackend::Sqlite).expect_err("C10");
        assert_eq!(err, Error::SqliteGearDatabaseOutsideStandalone, "{mode:?}");
        assert_eq!(
            err.to_string(),
            "database: the gear's database is SQLite; a SQLite database serves a single process, so only `standalone` mode runs on one (feature 0005 section 2.4, C10)",
            "{mode:?}"
        );
    }
}

#[test]
fn a_server_gear_database_is_accepted_in_every_cluster_mode() {
    for mode in [
        DeploymentMode::ClusterIngest,
        DeploymentMode::ClusterDelivery,
        DeploymentMode::ClusterDispatcher,
    ] {
        for engine in [DbBackend::Postgres, DbBackend::MySql] {
            assert_eq!(gear_database(mode, engine), Ok(()), "{mode:?} {engine:?}");
        }
    }
}

// ── Outbox groups (feature 0005 section 2.5, C8) ───────────────────────────

fn with_outbox_groups(mode: &str, outbox_groups: &serde_json::Value) -> EventBrokerConfig {
    serde_json::from_value(json!({
        "mode": mode,
        "backends": { "main": { "type": MEMORY_BACKEND } },
        "backend_routing": { TOPIC_TYPE: "main" },
        "outbox_groups": outbox_groups.clone(),
    }))
    .expect("test configuration deserializes")
}

fn outbox_partitions(mode: DeploymentMode, cfg: &EventBrokerConfig) -> Result<u16, Error> {
    check(cfg, mode).map(|deployment| {
        deployment
            .expect("holds a registry")
            .outbox_partitions
            .get()
    })
}

/// Absent, the `default` group has 16 partitions; stated, it has the count it
/// states.
#[test]
fn the_default_group_has_16_partitions_unless_it_states_a_count() {
    let absent = with_outbox_groups("standalone", &json!({}));
    assert_eq!(
        outbox_partitions(DeploymentMode::Standalone, &absent),
        Ok(16)
    );

    let stated = with_outbox_groups("cluster_ingest", &json!({ "default": { "partitions": 4 } }));
    assert_eq!(
        outbox_partitions(DeploymentMode::ClusterIngest, &stated),
        Ok(4)
    );
}

/// C8: a power of 2 from 1 to 64, the toolkit-db outbox's limit.
#[test]
fn a_default_group_partition_count_outside_c8_is_refused() {
    for partitions in [0, 3, 65, 128] {
        let cfg = with_outbox_groups(
            "standalone",
            &json!({ "default": { "partitions": partitions } }),
        );

        let err = check(&cfg, DeploymentMode::Standalone).expect_err("C8");
        assert_eq!(
            err,
            Error::OutboxPartitionsOutOfRange { partitions },
            "{partitions}"
        );
        assert_eq!(
            err.to_string(),
            format!(
                "outbox_groups: group 'default' has {partitions} partitions; `partitions` is a power of 2 from 1 to 64 (feature 0005 section 2.4, C8)"
            )
        );
    }
}

/// C8: `default` always carries every topic, so stating some is a mistake
/// rather than a narrowing.
#[test]
fn a_default_group_stating_topics_is_refused() {
    let cfg = with_outbox_groups(
        "standalone",
        &json!({ "default": { "partitions": 16, "topics": [AUDIT_TOPIC] } }),
    );

    let err = check(&cfg, DeploymentMode::Standalone).expect_err("C8");
    assert_eq!(err, Error::DefaultOutboxGroupTopics);
    assert_eq!(
        err.to_string(),
        "outbox_groups: group 'default' states `topics`; `default` always carries every topic and sets nothing but `partitions` (feature 0005 section 2.4, C8)"
    );
}

/// Named groups would need tables created at runtime; until they are
/// implemented, one is refused without reproducing its unvalidated name.
#[test]
fn a_named_outbox_group_is_refused_without_reproducing_its_name() {
    let cfg = with_outbox_groups(
        "standalone",
        &json!({
            "audit": { "partitions": 4, "topics": [AUDIT_TOPIC] },
            "default": { "partitions": 16 },
        }),
    );

    let err = check(&cfg, DeploymentMode::Standalone).expect_err("not supported yet");
    assert_eq!(err, Error::UnsupportedOutboxGroup { position: 0 });
    assert_eq!(
        err.to_string(),
        "outbox_groups: key 0 (in sorted order) is not `default`; only the reserved group `default` is supported, and named outbox groups are not implemented yet (feature 0005 section 2.5)"
    );
}

/// C8 is an ingest rule: a delivery instance enqueues nothing, carries the
/// section unchecked, and holds the default.
#[test]
fn delivery_is_not_subject_to_c8() {
    let cfg = with_outbox_groups(
        "cluster_delivery",
        &json!({ "default": { "partitions": 3 } }),
    );

    assert_eq!(
        outbox_partitions(DeploymentMode::ClusterDelivery, &cfg),
        Ok(16)
    );
}

// ── Which rule is reported when several are broken ─────────────────────────

/// Routing keys are checked one at a time in sorted order, form then target,
/// so the first key's bad target is reported before a later key's bad form.
#[test]
fn routing_keys_are_checked_in_sorted_order_form_then_target() {
    let cfg = config(
        &two_backends(),
        &json!({ TOPIC_TYPE: "missing", "zz-not-a-topic-key": "main" }),
    );

    assert_eq!(
        check(&cfg, DeploymentMode::Standalone).err(),
        Some(Error::UnknownBackend {
            key: TOPIC_TYPE.to_owned()
        })
    );
}

/// The partition count is judged before `topics`.
#[test]
fn an_out_of_range_count_is_reported_before_topics_on_default() {
    let cfg = with_outbox_groups(
        "standalone",
        &json!({ "default": { "partitions": 3, "topics": [AUDIT_TOPIC] } }),
    );

    assert_eq!(
        check(&cfg, DeploymentMode::Standalone).err(),
        Some(Error::OutboxPartitionsOutOfRange { partitions: 3 })
    );
}
