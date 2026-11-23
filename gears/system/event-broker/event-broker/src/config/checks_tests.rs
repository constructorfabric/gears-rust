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
fn memory_process_profiles_and_sweep_interval_are_validated_at_startup() {
    let outcomes = [
        ("one profile at 1ms", json!(["secondary"]), "1ms"),
        (
            "64 profiles",
            json!((0..64).map(|i| format!("p{i:02}")).collect::<Vec<_>>()),
            "1s",
        ),
        ("255-byte name", json!(["a".repeat(255)]), "1s"),
        ("empty list", json!([]), "1s"),
        (
            "65 profiles",
            json!((0..65).map(|i| format!("p{i:02}")).collect::<Vec<_>>()),
            "1s",
        ),
        ("duplicate", json!(["a", "b", "a"]), "1s"),
        ("empty name", json!([""]), "1s"),
        ("scoped name", json!(["a/b"]), "1s"),
        ("256-byte name", json!(["a".repeat(256)]), "1s"),
        ("zero interval", json!(["secondary"]), "0s"),
    ]
    .map(|(case, profiles, sweep_interval)| {
        let cfg: EventBrokerConfig = serde_json::from_value(json!({
            "mode": "standalone",
            "backends": { "main": { "type": MEMORY_BACKEND } },
            "backend_routing": { TOPIC_TYPE: "main" },
            "process": {
                "cluster": {
                    "mode": "memory",
                    "profiles": profiles,
                    "sweep_interval": sweep_interval,
                },
            },
        }))
        .expect("fixture deserializes before startup validation");
        (
            case,
            check(&cfg, cfg.mode).map(|deployment| deployment.is_some()),
        )
    });

    assert_eq!(
        outcomes,
        [
            ("one profile at 1ms", Ok(true)),
            ("64 profiles", Ok(true)),
            ("255-byte name", Ok(true)),
            ("empty list", Err(Error::MemoryProcessProfileCount)),
            ("65 profiles", Err(Error::MemoryProcessProfileCount)),
            ("duplicate", Err(Error::DuplicateMemoryProcessProfile)),
            ("empty name", Err(Error::InvalidMemoryProcessProfile)),
            ("scoped name", Err(Error::InvalidMemoryProcessProfile)),
            ("256-byte name", Err(Error::InvalidMemoryProcessProfile)),
            ("zero interval", Err(Error::ZeroMemoryProcessSweepInterval)),
        ]
    );
}

#[test]
fn only_standalone_accepts_a_memory_process_and_every_role_accepts_remote_or_absence() {
    let outcomes = [
        "standalone",
        "cluster_ingest",
        "cluster_delivery",
        "cluster_dispatcher",
    ]
    .map(|mode| {
        let input = json!({
            "mode": mode,
            "backends": { "main": { "type": MEMORY_BACKEND } },
            "backend_routing": { TOPIC_TYPE: "main" },
        });
        let [memory, remote, absent] = [
            Some(json!({"cluster": {"mode": "memory", "profiles": ["event-broker"]}})),
            Some(json!({"cluster": {"mode": "remote"}})),
            None,
        ]
        .map(|process| {
            let mut input = input.clone();
            if let Some(process) = process {
                input["process"] = process;
            }
            let cfg: EventBrokerConfig =
                serde_json::from_value(input).expect("fixture deserializes");
            check(&cfg, cfg.mode).map(|deployment| deployment.is_some())
        });
        (mode, memory, remote, absent)
    });

    assert_eq!(
        outcomes,
        [
            ("standalone", Ok(true), Ok(true), Ok(true)),
            (
                "cluster_ingest",
                Err(Error::MemoryProcessOutsideStandalone),
                Ok(true),
                Ok(true)
            ),
            (
                "cluster_delivery",
                Err(Error::MemoryProcessOutsideStandalone),
                Ok(true),
                Ok(true)
            ),
            (
                "cluster_dispatcher",
                Err(Error::MemoryProcessOutsideStandalone),
                Ok(false),
                Ok(false)
            ),
        ]
    );
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

/// A valid single-backend deployment in `mode`, with the synchronous-publish
/// cap set to `max_wait_secs`.
fn config_with_publish_wait(mode: &str, max_wait_secs: u32) -> EventBrokerConfig {
    serde_json::from_value(json!({
        "mode": mode,
        "publish": { "max_wait_secs": max_wait_secs },
        "backends": { "main": { "type": MEMORY_BACKEND } },
        "backend_routing": { TOPIC_TYPE: "main" },
    }))
    .expect("test configuration deserializes")
}

/// B2: every ingest-serving mode refuses a synchronous-publish hold above the
/// cap, so a publish cannot be made to hold its response head past the
/// api-gateway deadline (which would answer `504` after the event committed).
#[test]
fn an_ingest_mode_refuses_a_publish_wait_above_the_cap() {
    for (mode_str, mode) in [
        ("standalone", DeploymentMode::Standalone),
        ("cluster_ingest", DeploymentMode::ClusterIngest),
    ] {
        let cfg = config_with_publish_wait(mode_str, super::MAX_PUBLISH_WAIT_SECS + 1);

        assert_eq!(
            check(&cfg, mode).err(),
            Some(Error::PublishMaxWaitTooLarge {
                secs: super::MAX_PUBLISH_WAIT_SECS + 1,
                max: super::MAX_PUBLISH_WAIT_SECS,
            }),
            "{mode_str}"
        );
    }
}

fn config_with_scope_timing(mode: &str, ttl_secs: u32, deadline_secs: u32) -> EventBrokerConfig {
    serde_json::from_value(json!({
        "mode": mode,
        "streaming": {
            "authz_scope_ttl_secs": ttl_secs,
            "authz_eval_deadline_secs": deadline_secs,
        },
        "backends": { "main": { "type": MEMORY_BACKEND } },
        "backend_routing": { TOPIC_TYPE: "main" },
    }))
    .expect("test configuration deserializes")
}

/// Every delivery-serving mode refuses a scope TTL at `2 x deadline + 2`, the
/// `ignore` refresh lead: a refreshed group would be due again at once.
#[test]
fn a_scope_ttl_within_two_deadlines_of_the_refresh_lead_is_refused() {
    for (mode_str, mode) in [
        ("standalone", DeploymentMode::Standalone),
        ("cluster_delivery", DeploymentMode::ClusterDelivery),
    ] {
        let cfg = config_with_scope_timing(mode_str, 22, 10);

        assert_eq!(
            check(&cfg, mode).err(),
            Some(Error::ScopeTtlTooShort {
                ttl_secs: 22,
                deadline_secs: 10,
            }),
            "{mode_str}"
        );
        assert_eq!(
            Error::ScopeTtlTooShort {
                ttl_secs: 22,
                deadline_secs: 10,
            }
            .to_string(),
            "streaming: authz_scope_ttl_secs is 22 and authz_eval_deadline_secs is 10; in \
             standalone and cluster_delivery modes authz_scope_ttl_secs must be greater than 2 x \
             authz_eval_deadline_secs + 2, so a stream's consume scope is refreshed ahead of its \
             expiry"
        );
    }
}

/// One second past the `ignore` refresh lead passes where delivery runs.
/// Both modes run a backend, so a passing check returns the proved
/// deployment: its shape tells the pass from the `Ok(None)` of a mode with
/// no backend, as `Deployment` has no `PartialEq`.
#[test]
fn a_scope_ttl_past_the_refresh_lead_is_accepted() {
    for (mode_str, mode) in [
        ("standalone", DeploymentMode::Standalone),
        ("cluster_delivery", DeploymentMode::ClusterDelivery),
    ] {
        let cfg = config_with_scope_timing(mode_str, 23, 10);
        let checked = check(&cfg, mode);
        let Ok(Some(_)) = &checked else {
            panic!("{mode_str}: expected the proved deployment, got {checked:?}");
        };
    }
}

/// The rule binds the delivery-serving modes only: `cluster_ingest` opens no
/// stream, so it carries the `streaming` section without being subject to it.
/// It runs a backend, so the check returns the proved deployment; its shape
/// is the observable, as `Deployment` has no `PartialEq`.
#[test]
fn a_mode_without_delivery_accepts_a_short_scope_ttl() {
    let cfg = config_with_scope_timing("cluster_ingest", 22, 10);
    let checked = check(&cfg, DeploymentMode::ClusterIngest);
    let Ok(Some(_)) = &checked else {
        panic!("expected the proved deployment, got {checked:?}");
    };
}

/// The cap itself, and a zero wait (every sync publish answered `202` at once),
/// are within bounds in every ingest-serving mode.
#[test]
fn an_ingest_mode_accepts_the_publish_wait_cap_and_zero() {
    for (mode_str, mode) in [
        ("standalone", DeploymentMode::Standalone),
        ("cluster_ingest", DeploymentMode::ClusterIngest),
    ] {
        for wait in [super::MAX_PUBLISH_WAIT_SECS, 0] {
            let cfg = config_with_publish_wait(mode_str, wait);
            assert!(
                check(&cfg, mode).is_ok(),
                "{mode_str}: wait={wait} is within bounds"
            );
        }
    }
}

/// The ordering that keeps a synchronous publish from being answered `504`
/// after it already committed: the hold cap stays below the ingest publish
/// deadline (so the broker never holds past it), which stays below the
/// dispatcher's outer deadline (so a two-gateway cluster publish is bounded,
/// not cut). Inverting either relationship reopens that window, so it is pinned
/// at compile time.
#[test]
fn the_publish_deadlines_keep_the_hold_inside_the_declared_route_deadlines() {
    const {
        assert!(
            (super::MAX_PUBLISH_WAIT_SECS as u64) < super::PUBLISH_ROUTE_TIMEOUT_SECS,
            "the hold cap must stay below the ingest publish route deadline",
        );
        assert!(
            super::PUBLISH_ROUTE_TIMEOUT_SECS < super::DISPATCHER_PUBLISH_ROUTE_TIMEOUT_SECS,
            "the dispatcher's outer deadline must exceed the ingest publish deadline",
        );
    }
}

/// The cap binds the ingest-serving modes only: `cluster_delivery` never
/// enqueues, so it carries the `publish` section without being subject to the
/// rule.
#[test]
fn a_non_ingest_mode_is_not_subject_to_the_publish_wait_cap() {
    let cfg = config_with_publish_wait("cluster_delivery", super::MAX_PUBLISH_WAIT_SECS * 100);

    assert!(
        check(&cfg, DeploymentMode::ClusterDelivery).is_ok(),
        "a non-ingest role is unrestricted"
    );
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
