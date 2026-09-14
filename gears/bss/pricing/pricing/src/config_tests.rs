//! Tests for [`crate::config`].

use std::path::PathBuf;
use std::time::Duration;

use super::{
    BssPricingConfig, ConfigError, FixturesConfig, JobsConfig, LimitsConfig,
    MAX_IDEMPOTENCY_KEY_TTL_HOURS, MAX_REGISTRY_CALL_TIMEOUT_SECS,
};

#[test]
fn defaults_are_the_ratified_launch_values() {
    let cfg = BssPricingConfig::default();

    assert!(!cfg.events_enabled, "no broker is wired in this repository");
    assert_eq!(cfg.jobs.readmodel_warm_tick_secs, 5);
    assert_eq!(cfg.jobs.catalog_version_overdue_secs, 300);
    // The two thresholds D-166 separated. They differ by two orders of magnitude
    // because they measure different waits: 300s is D-47's max batching delay,
    // and 5s is sec 1.2's publish->read-model propagation SLO. Equal values here
    // would be the merged predicate back, and neither signal would discriminate.
    assert_eq!(cfg.jobs.readmodel_degraded_after_secs, 5);
    assert!(
        cfg.jobs.readmodel_degraded_after_secs < cfg.jobs.catalog_version_overdue_secs,
        "the post-commit SLO must be the tighter of the two"
    );
    // The window plane's two. 300s is the order of D-47's max batching delay (the
    // design set names no activation SLO at all - `jobs::window_activation`'s
    // module doc reports it), and 60s is an order of magnitude inside the 5-minute
    // changeover floors `inst-gc-compose` / `inst-su-instant` impose.
    assert_eq!(cfg.jobs.window_activation_tick_secs, 60);
    assert_eq!(cfg.jobs.window_activation_overdue_secs, 300);
    // The relation between them is what actually matters - a sweep must be able to
    // cross a boundary before the boundary is late - and it is asserted here of the
    // **defaults only**. Every other pair is refused by `validate` itself; see
    // `a_cadence_at_or_past_the_overdue_threshold_is_rejected`, which is where the
    // invariant lives now. This line was the whole of it while the validator
    // refused nothing but the two zeroes.
    assert!(
        cfg.jobs.window_activation_tick_secs < cfg.jobs.window_activation_overdue_secs,
        "a sweep must be able to cross a boundary before the boundary is late"
    );
    assert_eq!(cfg.limits.max_tier_bands_per_row, 100);
    assert_eq!(cfg.limits.max_price_rows_per_plan, 500);
    // PRD 14: `customEveryNDays <= 366`, `customEveryNMonths <= 24`, ratified
    // 2026-07-28. Unlike the two above these are hard caps -- P1 rejects an
    // over-cap interval at authoring rather than clamping it.
    assert_eq!(cfg.limits.max_custom_interval_days, 366);
    assert_eq!(cfg.limits.max_custom_interval_months, 24);
    assert_eq!(cfg.limits.idempotency_key_ttl_hours, 24);
    // The registry budget, pinned as a literal like every value above it. Compared
    // with `DEFAULT_REGISTRY_CALL_TIMEOUT_SECS` instead, both sides of the
    // comparison would come from that one constant and the ratified number would be
    // pinned nowhere: editing the constant would move the default and keep the suite
    // green.
    assert_eq!(cfg.limits.registry_call_timeout_secs, 5);
    assert_eq!(
        cfg.fixtures.registry_path,
        PathBuf::from("gears/bss/fixtures/corpus/registry.toml")
    );
    assert!(cfg.validate().is_ok());
}

/// The `CatalogVersion` source: fail-closed unless a deployment says otherwise in
/// words.
///
/// Three properties, and the second is the point of the whole shape. A boolean
/// would let the dev source be selected by a `true` sitting next to something
/// else; the named value cannot be arrived at by accident, and it leaves the
/// admission in the deployment's own file.
#[test]
fn the_catalog_version_source_is_fail_closed_and_opting_out_must_be_spelled() {
    use super::CatalogVersionSource;

    assert_eq!(
        BssPricingConfig::default().catalog_version_registry.mode,
        CatalogVersionSource::Unconfigured,
        "a deployment that says nothing must not publish invented versions"
    );

    let opted: BssPricingConfig = serde_json::from_str(
        "{\"catalog_version_registry\": {\"mode\": \"local_dev_invented_versions\"}}",
    )
    .expect("the documented spelling parses");
    assert_eq!(
        opted.catalog_version_registry.mode,
        CatalogVersionSource::LocalDevInventedVersions
    );

    // A boolean, or any other near-miss, is refused rather than coerced. Without
    // this the "cannot be set by accident" claim is prose.
    for near_miss in [
        "{\"catalog_version_registry\": {\"mode\": true}}",
        "{\"catalog_version_registry\": {\"mode\": \"local_dev\"}}",
        "{\"catalog_version_registry\": {\"enabled\": true}}",
    ] {
        assert!(
            serde_json::from_str::<BssPricingConfig>(near_miss).is_err(),
            "expected {near_miss} to be refused"
        );
    }
}

#[test]
fn the_fixtures_section_carries_only_a_path() {
    // The gate has no off switch, so the section has no boolean. Anything that
    // looked like `enabled: false` would be a way to publish an ungated
    // `modelKind`, which is the one thing the corpus exists to prevent.
    let err = serde_json::from_str::<FixturesConfig>("{\"enabled\": false}")
        .expect_err("unknown fixture keys are denied");

    assert!(
        err.to_string().contains("enabled"),
        "the error names the offending key: {err}"
    );
}

#[test]
fn an_overridden_registry_path_is_taken_verbatim() {
    let cfg: BssPricingConfig =
        serde_json::from_str("{\"fixtures\": {\"registry_path\": \"/opt/bss/registry.toml\"}}")
            .expect("a fixtures override is a valid section");

    assert_eq!(
        cfg.fixtures.registry_path,
        PathBuf::from("/opt/bss/registry.toml")
    );
    assert!(cfg.validate().is_ok());
}

#[test]
fn an_empty_registry_path_is_rejected_at_boot() {
    // Left unchecked this boots a gate that is closed for every kind and logs
    // "file not found" for a path the operator never wrote, so it is caught
    // where the mistake is legible.
    let fixtures = FixturesConfig {
        registry_path: PathBuf::new(),
    };

    assert_eq!(
        fixtures.validate(),
        Err(ConfigError::EmptyPath {
            field: "fixtures.registry_path"
        })
    );
}

#[test]
fn an_empty_section_deserializes_to_the_defaults() {
    let cfg: BssPricingConfig = serde_json::from_str("{}").expect("empty map is a valid section");

    assert_eq!(
        cfg.jobs.readmodel_warm_tick_secs,
        JobsConfig::default().readmodel_warm_tick_secs
    );
    assert_eq!(
        cfg.limits.max_price_rows_per_plan,
        LimitsConfig::default().max_price_rows_per_plan
    );
}

#[test]
fn an_unknown_field_is_rejected_rather_than_ignored() {
    // A typo in a limit must not silently leave the launch default in force:
    // the operator believes they raised a cap that never moved.
    let err = serde_json::from_str::<BssPricingConfig>("{\"max_price_rows\": 900}")
        .expect_err("unknown fields are denied");

    assert!(
        err.to_string().contains("max_price_rows"),
        "the error names the offending key: {err}"
    );
}

#[test]
fn a_zero_cadence_is_rejected_by_field() {
    let jobs = JobsConfig {
        readmodel_warm_tick_secs: 0,
        ..JobsConfig::default()
    };

    assert_eq!(
        jobs.validate(),
        Err(ConfigError::ZeroInterval {
            field: "jobs.readmodel_warm_tick_secs"
        })
    );
}

#[test]
fn a_zero_overdue_threshold_is_rejected() {
    let jobs = JobsConfig {
        catalog_version_overdue_secs: 0,
        ..JobsConfig::default()
    };

    assert_eq!(
        jobs.validate(),
        Err(ConfigError::ZeroInterval {
            field: "jobs.catalog_version_overdue_secs"
        })
    );
}

#[test]
fn a_zero_degraded_threshold_is_rejected() {
    // Zero would mark every publish degraded in the pass that first observes its
    // commit - the same shape of noise measuring the 5s rule from `requested_at`
    // produced, and the reason D-166 exists.
    let jobs = JobsConfig {
        readmodel_degraded_after_secs: 0,
        ..JobsConfig::default()
    };

    assert_eq!(
        jobs.validate(),
        Err(ConfigError::ZeroInterval {
            field: "jobs.readmodel_degraded_after_secs"
        })
    );
}

/// The four cadences and bounds whose zero-checks had no test of their own.
///
/// A table rather than four functions, in `zero_limits_are_rejected_by_field`'s
/// shape, and it exists because this file already carried the defect it closes:
/// three of `JobsConfig`'s seven fields had a per-field test and the rest had a
/// `validate` arm and nothing asserting it. Two of the four are the window
/// plane's, added with the sweep; the other two are D-163's and were unproved
/// before it — reported rather than left, since the file is one roster and a
/// roster that covers half its struct is the shape a later field is added under.
#[test]
fn the_remaining_zero_cadences_and_bounds_are_rejected_by_field() {
    for (jobs, field) in [
        (
            JobsConfig {
                pending_refs_per_tenant: 0,
                ..JobsConfig::default()
            },
            "jobs.pending_refs_per_tenant",
        ),
        (
            JobsConfig {
                pending_tenants_per_pass: 0,
                ..JobsConfig::default()
            },
            "jobs.pending_tenants_per_pass",
        ),
        (
            // Zero would raise the Warn for every window the sweep ever flips:
            // a boundary is always at least zero seconds old by the time a pass
            // reads it.
            JobsConfig {
                window_activation_overdue_secs: 0,
                ..JobsConfig::default()
            },
            "jobs.window_activation_overdue_secs",
        ),
        (
            // `tokio::time::interval` panics on a zero period, so this one is
            // the difference between a boot that fails and a lifecycle that dies
            // on its first tick.
            JobsConfig {
                window_activation_tick_secs: 0,
                ..JobsConfig::default()
            },
            "jobs.window_activation_tick_secs",
        ),
    ] {
        assert_eq!(
            jobs.validate(),
            Err(ConfigError::ZeroInterval { field }),
            "{field} must be rejected by name"
        );
    }
}

/// A sweep cadence at or past the overdue threshold is refused, by **both**
/// field names.
///
/// The relation is not a taste: the overdue condition is read off the due set, so
/// a window whose boundary arrived just after a pass is found by the *next* pass
/// up to one whole tick later, and with `tick >= threshold` that first, entirely
/// healthy attempt is already late. `600 / 300` was accepted by `validate()` and
/// reported `activated: 1` and `overdue: 1` for the same window —
/// `pricing.window.activation_overdue`, whose meaning per §7 is *the lease
/// singleton is stalled*, raised once per window on every ordinary flip, forever.
///
/// Equality is refused with the same arm and for the same reason: at `tick ==
/// threshold` a boundary that arrives one instant after a pass is exactly a
/// threshold old when the next pass finds it, and the comparison that raises the
/// alarm is `>=`.
#[test]
fn a_cadence_at_or_past_the_overdue_threshold_is_rejected() {
    let expected = ConfigError::CadenceNotInsideThreshold {
        cadence: "jobs.window_activation_tick_secs",
        threshold: "jobs.window_activation_overdue_secs",
    };

    for tick in [300, 600] {
        let jobs = JobsConfig {
            window_activation_overdue_secs: 300,
            window_activation_tick_secs: tick,
            ..JobsConfig::default()
        };

        assert_eq!(
            jobs.validate(),
            Err(expected),
            "a {tick}s cadence under a 300s threshold alarms on every healthy flip"
        );
    }

    // One second inside it is accepted: the arm refuses a **relation** between two
    // knobs, not a value of either, so an operator who slows the sweep and moves
    // the alarm bar with it is still free to.
    let inside = JobsConfig {
        window_activation_overdue_secs: 300,
        window_activation_tick_secs: 299,
        ..JobsConfig::default()
    };
    assert!(inside.validate().is_ok());
}

/// The **warm** sweep's cadence is inside its own overdue threshold too, and the
/// relation holds on the pair whose clock the sweep does not start.
///
/// `pricing.catalogversion.commit_overdue` is measured from `requested_at` — an
/// instant a *publish* stamps, in some other process, between two passes — so the
/// cadence eats into the budget exactly as it does on the window plane: a ref
/// requested one instant after a pass is a whole tick old when the next pass first
/// looks at it, and at `tick >= threshold` the registry is reported as not having
/// answered on the first occasion anybody asked. That is
/// [`ReadModelWarmJob::observe_commit_overdue`](crate::infra::jobs::readmodel_warm)'s
/// `waited >= threshold` compare, so equality is refused with the same arm and for
/// the same reason as its window sibling.
///
/// **Not `readmodel_degraded_after_secs`**, and the review entry that asked for that
/// pair had it the wrong way round — see `JobsConfig::validate`'s own note. The
/// ratified defaults are `readmodel_warm_tick_secs: 5` and
/// `readmodel_degraded_after_secs: 5`, so an arm over that pair would refuse the
/// out-of-the-box configuration; the case below is what proves the defaults stay
/// valid under the arm that *was* added.
#[test]
fn the_warm_cadence_at_or_past_the_commit_overdue_threshold_is_rejected() {
    let expected = ConfigError::CadenceNotInsideThreshold {
        cadence: "jobs.readmodel_warm_tick_secs",
        threshold: "jobs.catalog_version_overdue_secs",
    };

    for tick in [300, 600] {
        let jobs = JobsConfig {
            readmodel_warm_tick_secs: tick,
            catalog_version_overdue_secs: 300,
            ..JobsConfig::default()
        };

        assert_eq!(
            jobs.validate(),
            Err(expected),
            "a {tick}s warm cadence under a 300s batching-delay threshold reports every \
             healthy publish as unanswered"
        );
    }

    // One second inside it is accepted, for the window arm's reason: the refusal is
    // of a relation, not of a value.
    let inside = JobsConfig {
        readmodel_warm_tick_secs: 299,
        catalog_version_overdue_secs: 300,
        ..JobsConfig::default()
    };
    assert!(inside.validate().is_ok());
}

/// **The ratified defaults pass `validate()`**, and this is the case that keeps a
/// later cadence relation from being written against them.
///
/// `readmodel_warm_tick_secs` and `readmodel_degraded_after_secs` are **equal** at
/// 5s out of the box — §1.2's propagation SLO on both sides — so any arm demanding
/// the warm tick be strictly inside *that* threshold would make the default
/// configuration unbootable. A relation is only a relation if the shipped values
/// satisfy it.
#[test]
fn the_default_cadences_are_accepted_including_the_equal_warm_pair() {
    let defaults = JobsConfig::default();

    assert_eq!(
        defaults.readmodel_warm_tick_secs, defaults.readmodel_degraded_after_secs,
        "the two are equal by ratification, which is what makes this case load-bearing"
    );
    assert!(
        defaults.validate().is_ok(),
        "the shipped defaults must boot"
    );
}

#[test]
fn zero_limits_are_rejected_by_field() {
    for (limits, field) in [
        (
            LimitsConfig {
                max_tier_bands_per_row: 0,
                ..LimitsConfig::default()
            },
            "limits.max_tier_bands_per_row",
        ),
        (
            LimitsConfig {
                max_price_rows_per_plan: 0,
                ..LimitsConfig::default()
            },
            "limits.max_price_rows_per_plan",
        ),
        // A zero interval cap is not a permissive one: P1 also requires
        // `n > 0`, so no `n` at all could satisfy both bounds and every custom
        // frequency would be unpublishable.
        (
            LimitsConfig {
                max_custom_interval_days: 0,
                ..LimitsConfig::default()
            },
            "limits.max_custom_interval_days",
        ),
        (
            LimitsConfig {
                max_custom_interval_months: 0,
                ..LimitsConfig::default()
            },
            "limits.max_custom_interval_months",
        ),
        (
            LimitsConfig {
                idempotency_key_ttl_hours: 0,
                ..LimitsConfig::default()
            },
            "limits.idempotency_key_ttl_hours",
        ),
        // A zero budget refuses every publish before the registry is dialled,
        // which is a fail-closed gear that cannot be talked out of it.
        (
            LimitsConfig {
                registry_call_timeout_secs: 0,
                ..LimitsConfig::default()
            },
            "limits.registry_call_timeout_secs",
        ),
    ] {
        assert_eq!(limits.validate(), Err(ConfigError::ZeroLimit { field }));
    }
}

/// **The registry budget is bounded above as well as below.**
///
/// The bound is what keeps a config value from becoming a transaction held open
/// for minutes: the budget is spent inside an open write transaction on ten of
/// the twelve call sites, so an over-large value pins row locks and a pool
/// connection for as long as it names.
#[test]
fn validate_rejects_a_registry_budget_above_the_maximum() {
    let limits = LimitsConfig {
        registry_call_timeout_secs: MAX_REGISTRY_CALL_TIMEOUT_SECS + 1,
        ..LimitsConfig::default()
    };

    assert_eq!(
        limits.validate(),
        Err(ConfigError::LimitAboveMaximum {
            field: "limits.registry_call_timeout_secs",
            maximum: MAX_REGISTRY_CALL_TIMEOUT_SECS,
        })
    );
    // The boundary is admissible, so the refusal above is the bound and not an
    // off-by-one that also refuses the largest legal value.
    assert_eq!(
        LimitsConfig {
            registry_call_timeout_secs: MAX_REGISTRY_CALL_TIMEOUT_SECS,
            ..LimitsConfig::default()
        }
        .validate(),
        Ok(())
    );
}

/// **The multiplication the accessor performs, bounded where every other limit is.**
///
/// `idempotency_key_ttl` renders the knob as seconds, and `validate` refused only
/// zero — so a value above the arithmetic bound aborted a debug boot and wrapped a
/// release one to a tiny window, disabling idempotency replay in the direction
/// nobody checks. The cap is arithmetic rather than policy: the largest TTL whose
/// seconds fit a `u64`.
#[test]
fn validate_rejects_an_idempotency_ttl_whose_seconds_do_not_fit() {
    let limits = LimitsConfig {
        idempotency_key_ttl_hours: MAX_IDEMPOTENCY_KEY_TTL_HOURS + 1,
        ..LimitsConfig::default()
    };

    assert_eq!(
        limits.validate(),
        Err(ConfigError::LimitAboveMaximum {
            field: "limits.idempotency_key_ttl_hours",
            maximum: MAX_IDEMPOTENCY_KEY_TTL_HOURS,
        })
    );

    // The boundary is admissible and the accessor is total on it — the whole point
    // of the bound being this number and not a smaller one.
    let at_the_bound = LimitsConfig {
        idempotency_key_ttl_hours: MAX_IDEMPOTENCY_KEY_TTL_HOURS,
        ..LimitsConfig::default()
    };
    assert_eq!(at_the_bound.validate(), Ok(()));
    assert_eq!(
        at_the_bound.idempotency_key_ttl(),
        std::time::Duration::from_secs(MAX_IDEMPOTENCY_KEY_TTL_HOURS * 3_600)
    );
}

/// The configured budget reads as a duration, and the default is one the bound
/// admits.
///
/// **The value under test is not the default**, which is the whole of what makes
/// this falsifiable: over `LimitsConfig::default()` both operands derive from
/// `DEFAULT_REGISTRY_CALL_TIMEOUT_SECS` — the field is set from it and the accessor
/// is `Duration::from_secs` of the field — so the comparison is `X == X` and an
/// accessor returning the wrong unit, or the constant outright, passes it. A number
/// the default does not carry is the one thing the accessor cannot answer by
/// accident.
///
/// The ratified default is pinned as a literal in
/// [`defaults_are_the_ratified_launch_values`], where every neighbouring limit is.
#[test]
fn the_registry_budget_reads_as_a_duration_inside_its_own_bound() {
    let configured = LimitsConfig {
        registry_call_timeout_secs: 11,
        ..LimitsConfig::default()
    };

    assert_eq!(configured.registry_call_timeout(), Duration::from_secs(11));
    assert_eq!(configured.validate(), Ok(()));
    assert!(
        LimitsConfig::default().registry_call_timeout().as_secs() <= MAX_REGISTRY_CALL_TIMEOUT_SECS,
        "and the shipped default is inside the bound the validator enforces"
    );
}

#[test]
fn validate_rejects_the_whole_config_when_a_sub_section_is_invalid() {
    let cfg = BssPricingConfig {
        limits: LimitsConfig {
            idempotency_key_ttl_hours: 0,
            ..LimitsConfig::default()
        },
        ..BssPricingConfig::default()
    };

    assert!(cfg.validate().is_err());
}

#[test]
fn the_idempotency_ttl_reads_as_a_duration() {
    let ttl = LimitsConfig::default().idempotency_key_ttl();

    assert_eq!(ttl.as_secs(), 24 * 3_600);
}
