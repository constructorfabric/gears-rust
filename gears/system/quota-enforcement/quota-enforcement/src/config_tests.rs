use std::time::Duration;

use super::{
    CatalogSection, ElectionTimingConfig, GaugesSection, LeasesSection, MetricsConfig,
    PoliciesSection, QuotaEnforcementConfig, QuotasSection,
};

#[test]
fn defaults_select_the_platform_vendor_and_the_cluster_election_defaults() {
    let cfg = QuotaEnforcementConfig::default();
    assert_eq!(cfg.storage_vendor, "constructorfabric");
    assert_eq!(cfg.election.ttl(), Duration::from_secs(30));
    assert_eq!(cfg.election.max_missed_renewals, 2);
    assert_eq!(cfg.sweeper_stop_timeout(), Duration::from_secs(10));
    assert_eq!(cfg.metrics.instrument_name("denial_total"), "denial_total");
    cfg.validate().expect("defaults are valid");
}

#[test]
fn a_blank_vendor_and_zero_timings_are_rejected_with_the_field_name() {
    let cases: Vec<(QuotaEnforcementConfig, &str)> = vec![
        (
            QuotaEnforcementConfig {
                storage_vendor: " ".to_owned(),
                ..QuotaEnforcementConfig::default()
            },
            "storage_vendor",
        ),
        (
            QuotaEnforcementConfig {
                election: ElectionTimingConfig {
                    ttl_secs: 0,
                    ..ElectionTimingConfig::default()
                },
                ..QuotaEnforcementConfig::default()
            },
            "ttl_secs",
        ),
        (
            QuotaEnforcementConfig {
                election: ElectionTimingConfig {
                    max_missed_renewals: 0,
                    ..ElectionTimingConfig::default()
                },
                ..QuotaEnforcementConfig::default()
            },
            "max_missed_renewals",
        ),
        (
            QuotaEnforcementConfig {
                sweeper_stop_timeout_secs: 0,
                ..QuotaEnforcementConfig::default()
            },
            "sweeper_stop_timeout_secs",
        ),
    ];
    for (cfg, field) in cases {
        let err = cfg.validate().expect_err("invalid config rejected");
        assert!(err.to_string().contains(field), "{field}: {err}");
    }
}

#[test]
fn metrics_prefix_is_validated_and_applied() {
    let empty = MetricsConfig::default();
    empty.validate().expect("empty prefix is valid");
    let spaced = MetricsConfig {
        prefix: "  qe ".to_owned(),
    };
    spaced
        .validate()
        .expect("surrounding whitespace is trimmed");
    assert_eq!(spaced.instrument_name("denial_total"), "qe_denial_total");
    for bad in ["1qe", "qe-x", "qe x", "qe.x"] {
        let cfg = MetricsConfig {
            prefix: bad.to_owned(),
        };
        assert!(cfg.validate().is_err(), "prefix {bad:?} must be rejected");
    }
}

#[test]
fn unknown_keys_are_rejected_and_partial_configs_use_defaults() {
    let cfg: QuotaEnforcementConfig =
        serde_json::from_str(r#"{ "storage_vendor": "acme" }"#).expect("partial config");
    assert_eq!(cfg.storage_vendor, "acme");
    assert_eq!(cfg.election.ttl_secs, 30);
    let timing: QuotaEnforcementConfig =
        serde_json::from_str(r#"{ "election": { "ttl_secs": 5 } }"#).expect("partial election");
    assert_eq!(timing.election.ttl(), Duration::from_secs(5));
    assert_eq!(timing.election.max_missed_renewals, 2);
    assert!(serde_json::from_str::<QuotaEnforcementConfig>(r#"{ "vendor": "acme" }"#).is_err());
    assert!(
        serde_json::from_str::<QuotaEnforcementConfig>(r#"{ "coordination_vendor": "acme" }"#)
            .is_err(),
        "the retired coordination plugin selector is rejected"
    );
}

#[test]
fn the_catalog_section_accepts_type_ids_and_rejects_instances_and_duplicates() {
    let section = CatalogSection {
        subject_projections: vec![
            "gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~".to_owned(),
            "gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.tenant.v1~".to_owned(),
        ],
        resource_projections: vec![
            "gts.cf.core.qe.res.v1~cf.genai.llm_gateway.model.v1~".to_owned(),
        ],
    };
    section.validate().expect("valid");
    let domain = section.to_domain().expect("lowers");
    assert_eq!(domain.subject_projections.len(), 2);
    assert_eq!(domain.resource_projections.len(), 1);
    assert!(
        CatalogSection::default().validate().is_ok(),
        "empty is a valid catalogue"
    );

    let instance = CatalogSection {
        subject_projections: vec!["gts.cf.core.qe.scope.v1~cf.core.qe.user.v1".to_owned()],
        ..CatalogSection::default()
    };
    let err = instance
        .validate()
        .expect_err("an instance id is not a type id");
    assert!(err.to_string().contains("subject_projections[0]"), "{err}");

    let duplicate = CatalogSection {
        resource_projections: vec![
            "gts.cf.core.qe.res.v1~cf.genai.llm_gateway.model.v1~".to_owned(),
            "gts.cf.core.qe.res.v1~cf.genai.llm_gateway.model.v1~".to_owned(),
        ],
        ..CatalogSection::default()
    };
    let err = duplicate.validate().expect_err("duplicates rejected");
    assert!(err.to_string().contains("twice"), "{err}");

    let cfg: QuotaEnforcementConfig = serde_json::from_str(
        r#"{ "catalog": { "subject_projections": ["gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~"] } }"#,
    )
    .expect("partial catalog section");
    assert_eq!(cfg.catalog.subject_projections.len(), 1);
    assert!(cfg.catalog.resource_projections.is_empty());
    assert!(
        serde_json::from_str::<QuotaEnforcementConfig>(
            r#"{ "catalog": { "request_contracts": [] } }"#
        )
        .is_err(),
        "request contracts are discovered, never configured"
    );
}

#[test]
fn the_quotas_section_defaults_to_the_prd_bounds_and_rejects_each_zero_or_oversize() {
    let section = QuotasSection::default();
    section.validate().expect("defaults are valid");
    let limits = section.to_limits();
    assert_eq!(limits.metadata_max_bytes, 4096);
    assert_eq!(limits.list_max_limit, 500);
    assert_eq!(limits.list_max_ids, 100);
    assert_eq!(section.metric_cache_ttl(), Duration::from_mins(1));
    assert_eq!(section.metric_cache_stale_grace(), Duration::from_mins(5));

    let cases: Vec<(QuotasSection, &str)> = vec![
        (
            QuotasSection {
                metadata_max_bytes: 1,
                ..QuotasSection::default()
            },
            "metadata_max_bytes",
        ),
        (
            QuotasSection {
                metadata_max_bytes: QuotasSection::MAX_METADATA_BYTES + 1,
                ..QuotasSection::default()
            },
            "metadata_max_bytes",
        ),
        (
            QuotasSection {
                metric_cache_entries: 0,
                ..QuotasSection::default()
            },
            "metric_cache_entries",
        ),
        (
            QuotasSection {
                metric_cache_ttl_secs: 0,
                ..QuotasSection::default()
            },
            "metric_cache_ttl_secs",
        ),
        (
            QuotasSection {
                list_max_limit: 0,
                ..QuotasSection::default()
            },
            "list_max_limit",
        ),
        (
            QuotasSection {
                list_max_ids: 0,
                ..QuotasSection::default()
            },
            "list_max_ids",
        ),
    ];
    for (section, field) in cases {
        let err = section.validate().expect_err("rejected");
        assert!(err.to_string().contains(field), "{field}: {err}");
    }
    assert!(
        QuotasSection {
            metric_cache_stale_grace_secs: 0,
            ..QuotasSection::default()
        }
        .validate()
        .is_ok(),
        "a zero grace serves nothing stale and is allowed"
    );
}

#[test]
fn the_policies_section_defaults_to_the_feature_bounds_and_rejects_each_zero_or_oversize() {
    let section = PoliciesSection::default();
    let limits = section.to_limits().expect("defaults are valid");
    assert_eq!(
        limits.evaluation.upper_timeout_ms.get(),
        5,
        "feature default 5 ms"
    );
    assert_eq!(limits.evaluation.cost_limit.get(), 10_000);
    assert_eq!(limits.authoring.config_bytes, 16_384);
    assert_eq!(limits.authoring.comment_bytes, 1_024);
    assert_eq!(limits.authoring.list_limit, 50);
    assert_eq!(limits.artifact_cache_entries.get(), 512);
    assert_eq!(limits.preparation_max_attempts.get(), 3);
    assert_eq!(limits.snapshot.bytes, 65_536);
    assert_eq!(limits.snapshot.schemas, 32);
    assert_eq!(limits.snapshot.depth, 32);

    // The clamp is what the budget is built from: a persisted request above
    // it is cut down, a request below it is honoured, no request means 5 ms.
    let budget = limits.evaluation.budget(Some(500)).expect("clamped");
    assert_eq!(budget.timeout(), Duration::from_millis(5));
    let budget = limits.evaluation.budget(Some(2)).expect("under the clamp");
    assert_eq!(budget.timeout(), Duration::from_millis(2));

    let cases: Vec<(PoliciesSection, &str)> = vec![
        (
            PoliciesSection {
                evaluation_timeout_upper_ms: 0,
                ..PoliciesSection::default()
            },
            "evaluation_timeout_upper_ms",
        ),
        (
            PoliciesSection {
                evaluation_timeout_upper_ms: PoliciesSection::MAX_EVALUATION_TIMEOUT_MS + 1,
                ..PoliciesSection::default()
            },
            "evaluation_timeout_upper_ms",
        ),
        (
            PoliciesSection {
                evaluation_cost_limit: 0,
                ..PoliciesSection::default()
            },
            "evaluation_cost_limit",
        ),
        (
            PoliciesSection {
                config_max_bytes: 0,
                ..PoliciesSection::default()
            },
            "config_max_bytes",
        ),
        (
            PoliciesSection {
                config_max_bytes: PoliciesSection::MAX_CONFIG_BYTES + 1,
                ..PoliciesSection::default()
            },
            "config_max_bytes",
        ),
        (
            PoliciesSection {
                comment_max_bytes: 0,
                ..PoliciesSection::default()
            },
            "comment_max_bytes",
        ),
        (
            PoliciesSection {
                list_max_limit: 0,
                ..PoliciesSection::default()
            },
            "list_max_limit",
        ),
        (
            PoliciesSection {
                artifact_cache_entries: 0,
                ..PoliciesSection::default()
            },
            "artifact_cache_entries",
        ),
        (
            PoliciesSection {
                preparation_max_attempts: 0,
                ..PoliciesSection::default()
            },
            "preparation_max_attempts",
        ),
        (
            PoliciesSection {
                snapshot_max_bytes: 0,
                ..PoliciesSection::default()
            },
            "snapshot_max_bytes",
        ),
        (
            PoliciesSection {
                snapshot_max_bytes: PoliciesSection::MAX_SNAPSHOT_BYTES + 1,
                ..PoliciesSection::default()
            },
            "snapshot_max_bytes",
        ),
        (
            PoliciesSection {
                snapshot_max_schemas: 0,
                ..PoliciesSection::default()
            },
            "snapshot_max_schemas",
        ),
        (
            PoliciesSection {
                snapshot_max_depth: 0,
                ..PoliciesSection::default()
            },
            "snapshot_max_depth",
        ),
    ];
    for (section, field) in cases {
        let err = section.validate().expect_err("rejected");
        assert!(err.to_string().contains(field), "{field}: {err}");
    }

    let whole: QuotaEnforcementConfig =
        serde_json::from_str(r#"{ "policies": { "evaluation_timeout_upper_ms": 20 } }"#)
            .expect("partial section");
    whole.validate().expect("valid");
    assert_eq!(whole.policies.evaluation_timeout_upper_ms, 20);
    assert_eq!(
        whole.policies.list_max_limit, 50,
        "untouched fields keep defaults"
    );
    assert!(
        serde_json::from_str::<QuotaEnforcementConfig>(r#"{ "policies": { "timeout": 1 } }"#)
            .is_err(),
        "unknown keys are rejected"
    );
}

#[test]
fn the_gauges_section_keeps_the_sample_fresher_than_the_refresh_interval() {
    let section = GaugesSection::default();
    section.validate().expect("defaults are valid");
    let timing = section.to_timing();
    assert_eq!(timing.refresh, Duration::from_secs(30));
    assert_eq!(timing.refresh_deadline, Duration::from_secs(10));
    assert_eq!(timing.stale_after, Duration::from_mins(3));

    let cases: Vec<(GaugesSection, &str)> = vec![
        (
            GaugesSection {
                refresh_secs: 0,
                ..GaugesSection::default()
            },
            "refresh_secs",
        ),
        (
            GaugesSection {
                refresh_deadline_secs: 0,
                ..GaugesSection::default()
            },
            "refresh_deadline_secs",
        ),
        (
            GaugesSection {
                refresh_deadline_secs: 31,
                ..GaugesSection::default()
            },
            "refresh_deadline_secs",
        ),
        (
            GaugesSection {
                stale_after_secs: 30,
                ..GaugesSection::default()
            },
            "stale_after_secs",
        ),
    ];
    for (section, field) in cases {
        let err = section.validate().expect_err("rejected");
        assert!(err.to_string().contains(field), "{field}: {err}");
    }

    let whole: QuotaEnforcementConfig = serde_json::from_str(
        r#"{ "quotas": { "metadata_max_bytes": 8192 }, "gauges": { "refresh_secs": 5, "refresh_deadline_secs": 2, "stale_after_secs": 20 } }"#,
    )
    .expect("partial sections");
    whole.validate().expect("valid");
    assert_eq!(whole.quotas.metadata_max_bytes, 8192);
    assert_eq!(
        whole.quotas.list_max_limit, 500,
        "untouched fields keep defaults"
    );
    assert_eq!(whole.gauges.refresh_secs, 5);
    assert!(
        serde_json::from_str::<QuotaEnforcementConfig>(r#"{ "quotas": { "max_bytes": 1 } }"#)
            .is_err(),
        "unknown keys are rejected"
    );
}

#[test]
fn the_lease_section_defaults_to_the_platform_window_and_rejects_a_bad_one() {
    let section = LeasesSection::default();
    section.validate().expect("defaults are valid");
    let limits = section.to_limits();
    assert_eq!(limits.min_ttl, Duration::from_secs(1));
    assert_eq!(limits.max_ttl, Duration::from_hours(1));
    assert_eq!(
        section.to_sweep_timing().expect("timing").interval,
        Duration::from_mins(1)
    );

    for bad in [
        LeasesSection {
            min_ttl_secs: 0,
            ..LeasesSection::default()
        },
        LeasesSection {
            min_ttl_secs: 10,
            max_ttl_secs: 9,
            ..LeasesSection::default()
        },
        LeasesSection {
            sweep_interval_secs: 0,
            ..LeasesSection::default()
        },
        LeasesSection {
            sweep_batch_size: 0,
            ..LeasesSection::default()
        },
    ] {
        assert!(bad.validate().is_err(), "{bad:?} must be rejected");
    }
}
