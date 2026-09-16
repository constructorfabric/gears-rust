//! The instrument contract: rendered names, label keys, label values and bucket layouts.

use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{
    InMemoryMetricExporter, InMemoryMetricExporterBuilder, PeriodicReader, SdkMeterProvider,
    Temporality,
};

use super::{
    ACTIVATION_WRITE_SET_BUCKETS, AdmissionMetricsMeter, OPERATION_DURATION_BUCKETS_SECONDS, SCOPE,
};
use crate::domain::admission::vector::{VectorDrift, VectorRole};
use crate::domain::enums::OperationKind;
use crate::domain::ports::metrics::{AdmissionMetrics, PassLabels, RefusalStage, TerminalStatus};
use gts::CompatibilityVerdict;

/// The labels the pre-T20 tests are not about: a committing registration.
/// Named rather than inlined so those tests keep reading as assertions about
/// `status`, `stage` and `reason`.
fn commit() -> PassLabels {
    PassLabels::new(OperationKind::Registration, false)
}

fn default_prefix() -> String {
    crate::config::MetricsConfig::default().effective_prefix("types-registry")
}

fn recorder() -> (
    SdkMeterProvider,
    InMemoryMetricExporter,
    AdmissionMetricsMeter,
) {
    let exporter = InMemoryMetricExporterBuilder::new()
        .with_temporality(Temporality::Delta)
        .build();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let metrics = AdmissionMetricsMeter::new(&provider.meter(SCOPE), &default_prefix());
    (provider, exporter, metrics)
}

fn counter_sum(exporter: &InMemoryMetricExporter, name: &str) -> u64 {
    let metrics = exporter.get_finished_metrics().unwrap();
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data()
                {
                    return sum
                        .data_points()
                        .map(opentelemetry_sdk::metrics::data::SumDataPoint::value)
                        .sum();
                }
            }
        }
    }
    0
}

fn counter_sum_where(
    exporter: &InMemoryMetricExporter,
    name: &str,
    labels: &[(&str, &str)],
) -> u64 {
    let metrics = exporter.get_finished_metrics().unwrap();
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data()
                {
                    return sum
                        .data_points()
                        .filter(|dp| {
                            labels.iter().all(|(key, value)| {
                                dp.attributes().any(|kv| {
                                    kv.key.as_str() == *key && kv.value.as_str() == *value
                                })
                            })
                        })
                        .map(opentelemetry_sdk::metrics::data::SumDataPoint::value)
                        .sum();
                }
            }
        }
    }
    0
}

fn histogram_bounds(exporter: &InMemoryMetricExporter, name: &str) -> Option<Vec<f64>> {
    let metrics = exporter.get_finished_metrics().unwrap();
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::F64(MetricData::Histogram(h)) = metric.data()
                {
                    return h.data_points().next().map(|dp| dp.bounds().collect());
                }
            }
        }
    }
    None
}

fn histogram_sum(exporter: &InMemoryMetricExporter, name: &str) -> Option<f64> {
    let metrics = exporter.get_finished_metrics().unwrap();
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::F64(MetricData::Histogram(h)) = metric.data()
                {
                    return h
                        .data_points()
                        .next()
                        .map(opentelemetry_sdk::metrics::data::HistogramDataPoint::sum);
                }
            }
        }
    }
    None
}

fn histogram_count(exporter: &InMemoryMetricExporter, name: &str) -> u64 {
    let metrics = exporter.get_finished_metrics().unwrap();
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::F64(MetricData::Histogram(h)) = metric.data()
                {
                    return h
                        .data_points()
                        .map(opentelemetry_sdk::metrics::data::HistogramDataPoint::count)
                        .sum();
                }
            }
        }
    }
    0
}

#[test]
fn unchanged_probes_are_counted_separately_by_hit_and_miss() {
    let (provider, exporter, metrics) = recorder();
    metrics.unchanged_probe(true);
    metrics.unchanged_probe(false);
    metrics.unchanged_probe(false);
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_unchanged_probes_total",
            &[("hit", "true")]
        ),
        1
    );
    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_unchanged_probes_total",
            &[("hit", "false")]
        ),
        2
    );
    assert_eq!(counter_sum(&exporter, "types_registry_candidates_total"), 0);
}

#[test]
fn candidates_are_counted_by_their_terminal_status() {
    let (provider, exporter, metrics) = recorder();

    metrics.candidate_terminalized(TerminalStatus::Succeeded, commit());
    metrics.candidate_terminalized(TerminalStatus::Succeeded, commit());
    metrics.candidate_terminalized(TerminalStatus::Unchanged, commit());
    metrics.candidate_terminalized(TerminalStatus::Failed, commit());
    provider.force_flush().unwrap();

    assert_eq!(counter_sum(&exporter, "types_registry_candidates_total"), 4);
    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_candidates_total",
            &[("status", "succeeded")],
        ),
        2,
    );
    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_candidates_total",
            &[("status", "unchanged")],
        ),
        1,
    );
    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_candidates_total",
            &[("status", "failed")],
        ),
        1,
    );
}

#[test]
fn a_non_terminal_status_does_not_convert_to_a_terminal_one() {
    use crate::domain::enums::OperationItemStatus;

    assert!(TerminalStatus::try_from(OperationItemStatus::Pending).is_err());
    assert!(TerminalStatus::try_from(OperationItemStatus::Running).is_err());
    for status in [
        OperationItemStatus::Succeeded,
        OperationItemStatus::Unchanged,
        OperationItemStatus::Failed,
    ] {
        assert!(
            TerminalStatus::try_from(status).is_ok(),
            "{status:?} is terminal and must convert"
        );
    }
}

#[test]
fn refusals_carry_their_stage_and_reason() {
    let (provider, exporter, metrics) = recorder();

    metrics.refused(RefusalStage::Acceptance, "empty_batch", commit());
    metrics.refused(RefusalStage::Admission, "precondition_failed", commit());
    metrics.refused(RefusalStage::Admission, "precondition_failed", commit());
    provider.force_flush().unwrap();

    assert_eq!(counter_sum(&exporter, "types_registry_refusals_total"), 3);
    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_refusals_total",
            &[("stage", "acceptance"), ("reason", "empty_batch")],
        ),
        1,
    );
    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_refusals_total",
            &[("stage", "admission"), ("reason", "precondition_failed")],
        ),
        2,
    );
}

#[test]
fn two_reasons_at_one_stage_are_two_series() {
    let (provider, exporter, metrics) = recorder();

    metrics.refused(RefusalStage::Acceptance, "empty_batch", commit());
    metrics.refused(RefusalStage::Acceptance, "duplicate_candidate", commit());
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_refusals_total",
            &[("reason", "empty_batch")],
        ),
        1,
    );
    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_refusals_total",
            &[("reason", "duplicate_candidate")],
        ),
        1,
    );
}

#[test]
fn revalidation_retries_are_counted_by_drift_shape() {
    let (provider, exporter, metrics) = recorder();

    metrics.revalidation_retried(&VectorDrift::Moved {
        gts_id: "x".to_owned(),
        role: VectorRole::Dependency,
        recorded: 1,
        found: 2,
    });
    metrics.revalidation_retried(&VectorDrift::Appeared {
        gts_id: "y".to_owned(),
        role: VectorRole::Dependent,
    });
    metrics.revalidation_retried(&VectorDrift::Vanished {
        gts_id: "z".to_owned(),
        role: VectorRole::Dependency,
    });
    metrics.revalidation_retried(&VectorDrift::Refreshed {
        gts_id: "w".to_owned(),
    });
    metrics.revalidation_retried(&VectorDrift::CurrentProjectionMoved {
        gts_id: "v".to_owned(),
    });
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum(&exporter, "types_registry_revalidations_total"),
        5,
    );
    for shape in [
        "moved",
        "appeared",
        "vanished",
        "refreshed",
        "current_projection_moved",
    ] {
        assert_eq!(
            counter_sum_where(
                &exporter,
                "types_registry_revalidations_total",
                &[("drift", shape)],
            ),
            1,
            "one retry per drift shape, missing {shape}",
        );
    }
}

#[test]
#[allow(clippy::cast_precision_loss)]
fn activation_write_set_buckets_reach_the_configured_default_bound() {
    let (provider, exporter, metrics) = recorder();

    metrics.observe_activation_write_set(3, commit());
    provider.force_flush().unwrap();

    assert_eq!(
        histogram_bounds(&exporter, "types_registry_activation_write_set"),
        Some(ACTIVATION_WRITE_SET_BUCKETS.to_vec()),
    );
    assert_eq!(
        ACTIVATION_WRITE_SET_BUCKETS.last().copied(),
        Some(crate::config::Limits::default().activation_write_set as f64),
        "the top bucket tracks the default limits.activation_write_set",
    );
    assert_eq!(
        histogram_sum(&exporter, "types_registry_activation_write_set"),
        Some(3.0),
    );
}

#[test]
fn an_empty_activation_write_set_is_still_observed() {
    let (provider, exporter, metrics) = recorder();

    metrics.observe_activation_write_set(0, commit());
    provider.force_flush().unwrap();

    assert_eq!(
        histogram_count(&exporter, "types_registry_activation_write_set"),
        1,
    );
    assert_eq!(
        histogram_sum(&exporter, "types_registry_activation_write_set"),
        Some(0.0),
    );
}

#[test]
fn operation_duration_is_recorded_in_seconds() {
    let (provider, exporter, metrics) = recorder();

    metrics.observe_operation_duration(std::time::Duration::from_millis(250));
    provider.force_flush().unwrap();

    assert_eq!(
        histogram_bounds(&exporter, "types_registry_operation_duration_seconds"),
        Some(OPERATION_DURATION_BUCKETS_SECONDS.to_vec()),
    );
    let sum = histogram_sum(&exporter, "types_registry_operation_duration_seconds")
        .expect("the duration must be observed");
    assert!(
        (sum - 0.25).abs() < 1e-9,
        "250ms must be recorded as 0.25s, got {sum}",
    );
}

#[test]
fn the_default_prefix_is_the_gear_name_in_snake_case() {
    assert_eq!(default_prefix(), "types_registry");
}

#[test]
fn a_configured_prefix_renames_every_series() {
    let exporter = InMemoryMetricExporterBuilder::new()
        .with_temporality(Temporality::Delta)
        .build();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let metrics = AdmissionMetricsMeter::new(&provider.meter(SCOPE), "tenant_a_tr");

    metrics.candidate_terminalized(TerminalStatus::Succeeded, commit());
    metrics.refused(RefusalStage::Acceptance, "zero_precondition", commit());
    metrics.observe_activation_write_set(1, commit());
    metrics.observe_operation_duration(std::time::Duration::from_millis(5));
    provider.force_flush().unwrap();

    let names = recorded_names(&exporter);
    for suffix in [
        "candidates_total",
        "refusals_total",
        "activation_write_set",
        "operation_duration_seconds",
    ] {
        assert!(
            names.contains(&format!("tenant_a_tr_{suffix}")),
            "expected tenant_a_tr_{suffix} among {names:?}"
        );
        assert!(
            !names.contains(&format!("types_registry_{suffix}")),
            "the default prefix must not survive a configured one: {names:?}"
        );
    }
}

fn recorded_names(exporter: &InMemoryMetricExporter) -> Vec<String> {
    let mut names = Vec::new();
    for rm in &exporter.get_finished_metrics().unwrap() {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                names.push(metric.name().to_owned());
            }
        }
    }
    names
}

// ---------------------------------------------------------------------------
// `types_registry_compat_verdicts_total{verdict,forced}` (T17, P16)
// ---------------------------------------------------------------------------

/// The instrument's **rendered name**, which no other test in this repository
/// would notice losing its `_total`.
#[test]
fn the_verdict_counter_renders_under_its_prefixed_total_name() {
    let (provider, exporter, metrics) = recorder();

    metrics.compat_verdict(CompatibilityVerdict::Compatible, false, commit());
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum(&exporter, "types_registry_compat_verdicts_total"),
        1,
        "the name is what a dashboard queries; a renamed instrument is an empty panel",
    );
}

/// All three verdicts have separate series, including non-refused compatible results.
#[test]
fn all_three_verdicts_are_counted_under_their_own_label_value() {
    let (provider, exporter, metrics) = recorder();

    metrics.compat_verdict(CompatibilityVerdict::Compatible, false, commit());
    metrics.compat_verdict(CompatibilityVerdict::Incompatible, false, commit());
    metrics.compat_verdict(CompatibilityVerdict::Incompatible, false, commit());
    metrics.compat_verdict(CompatibilityVerdict::Unknown, false, commit());
    provider.force_flush().unwrap();

    for (verdict, expected) in [("compatible", 1), ("incompatible", 2), ("unknown", 1)] {
        assert_eq!(
            counter_sum_where(
                &exporter,
                "types_registry_compat_verdicts_total",
                &[("verdict", verdict), ("forced", "false")],
            ),
            expected,
            "verdict={verdict}",
        );
    }
    assert_eq!(
        counter_sum(&exporter, "types_registry_compat_verdicts_total"),
        4,
    );
}

/// Forced verdicts have a separate series.
#[test]
fn a_waived_verdict_is_its_own_series_under_forced_true() {
    let (provider, exporter, metrics) = recorder();

    metrics.compat_verdict(CompatibilityVerdict::Incompatible, true, commit());
    metrics.compat_verdict(CompatibilityVerdict::Incompatible, false, commit());
    provider.force_flush().unwrap();

    for (forced, expected) in [("true", 1), ("false", 1)] {
        assert_eq!(
            counter_sum_where(
                &exporter,
                "types_registry_compat_verdicts_total",
                &[("verdict", "incompatible"), ("forced", forced)],
            ),
            expected,
            "forced={forced}: a waiver must not be blended into the unwaived series",
        );
    }
}

/// Every label **key** is present on every data point, and each vocabulary is
/// closed. A dropped key silently merges series; a stray value silently splits
/// one. T20 added `dry_run` here, and this count is the deliberation it forced.
#[test]
fn the_verdict_counter_carries_exactly_three_closed_label_keys() {
    let (provider, exporter, metrics) = recorder();

    for verdict in [
        CompatibilityVerdict::Compatible,
        CompatibilityVerdict::Incompatible,
        CompatibilityVerdict::Unknown,
    ] {
        for forced in [true, false] {
            metrics.compat_verdict(verdict, forced, commit());
        }
    }
    provider.force_flush().unwrap();

    let mut seen: Vec<(String, String)> = Vec::new();
    let finished = exporter.get_finished_metrics().unwrap();
    for rm in &finished {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                if metric.name() != "types_registry_compat_verdicts_total" {
                    continue;
                }
                let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data() else {
                    panic!("the verdict instrument must be a u64 sum");
                };
                for dp in sum.data_points() {
                    let keys: Vec<&str> = dp.attributes().map(|kv| kv.key.as_str()).collect();
                    assert_eq!(
                        keys.len(),
                        3,
                        "exactly `verdict`, `forced` and `dry_run`, got {keys:?}",
                    );
                    assert!(
                        keys.contains(&"verdict")
                            && keys.contains(&"forced")
                            && keys.contains(&"dry_run"),
                        "{keys:?}"
                    );
                    assert!(
                        !keys.contains(&"kind"),
                        "a verdict is only computed for a registration: {keys:?}",
                    );
                    let value_of = |key: &str| {
                        dp.attributes()
                            .find(|kv| kv.key.as_str() == key)
                            .map(|kv| kv.value.as_str().to_string())
                            .unwrap_or_default()
                    };
                    seen.push((value_of("verdict"), value_of("forced")));
                }
            }
        }
    }

    seen.sort_unstable();
    seen.dedup();
    let expected: Vec<(String, String)> = ["compatible", "incompatible", "unknown"]
        .into_iter()
        .flat_map(|v| {
            ["false", "true"]
                .into_iter()
                .map(move |f| (v.to_owned(), f.to_owned()))
        })
        .collect();
    let mut expected_sorted = expected;
    expected_sorted.sort_unstable();
    assert_eq!(
        seen, expected_sorted,
        "the two vocabularies are closed at three verdicts and two booleans",
    );
}

// ---------------------------------------------------------------------------
// T20's label sweep: mode and kind on the per-candidate series (P16 rule 2)
// ---------------------------------------------------------------------------

/// Sorted distinct values a label key took, for vocabulary assertions that do
/// not depend on counts.
fn label_values_of(exporter: &InMemoryMetricExporter, name: &str, key: &str) -> Vec<String> {
    let metrics = exporter.get_finished_metrics().unwrap();
    let mut values = Vec::new();
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data()
                {
                    for dp in sum.data_points() {
                        for kv in dp.attributes() {
                            if kv.key.as_str() == key {
                                values.push(kv.value.as_str().into_owned());
                            }
                        }
                    }
                }
            }
        }
    }
    values.sort();
    values.dedup();
    values
}

/// A dry-run pass must not be indistinguishable from one that wrote. This
/// is the whole point of the label: "how many registrations succeeded today"
/// must not answer with a number that includes dry runs.
#[test]
fn a_dry_run_candidate_is_a_different_series_from_a_committed_one() {
    let (provider, exporter, metrics) = recorder();

    metrics.candidate_terminalized(
        TerminalStatus::Succeeded,
        PassLabels::new(OperationKind::Registration, false),
    );
    metrics.candidate_terminalized(
        TerminalStatus::Succeeded,
        PassLabels::new(OperationKind::Registration, true),
    );
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_candidates_total",
            &[("status", "succeeded"), ("dry_run", "false")],
        ),
        1,
    );
    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_candidates_total",
            &[("status", "succeeded"), ("dry_run", "true")],
        ),
        1,
    );
}

/// Deletions are rare and irreversible; a success series that blends them cannot
/// answer *what did this deployment delete*.
#[test]
fn a_deletion_is_a_different_series_from_a_registration() {
    let (provider, exporter, metrics) = recorder();

    metrics.candidate_terminalized(
        TerminalStatus::Succeeded,
        PassLabels::new(OperationKind::Deletion, false),
    );
    metrics.refused(
        RefusalStage::Admission,
        "has_registered_dependents",
        PassLabels::new(OperationKind::Deletion, false),
    );
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_candidates_total",
            &[("kind", "deletion"), ("status", "succeeded")],
        ),
        1,
    );
    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_refusals_total",
            &[
                ("kind", "deletion"),
                ("reason", "has_registered_dependents")
            ],
        ),
        1,
    );
    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_candidates_total",
            &[("kind", "registration")],
        ),
        0,
        "nothing in this pass was a registration",
    );
}

/// Both label keys, and both vocabularies, on both per-candidate series.
#[test]
fn the_two_new_label_keys_carry_closed_vocabularies() {
    let (provider, exporter, metrics) = recorder();

    for kind in [OperationKind::Registration, OperationKind::Deletion] {
        for dry_run in [false, true] {
            let labels = PassLabels::new(kind, dry_run);
            metrics.candidate_terminalized(TerminalStatus::Succeeded, labels);
            metrics.refused(RefusalStage::Admission, "precondition_failed", labels);
        }
    }
    provider.force_flush().unwrap();

    for series in [
        "types_registry_candidates_total",
        "types_registry_refusals_total",
    ] {
        assert_eq!(
            label_values_of(&exporter, series, "kind"),
            vec!["deletion", "registration"],
            "{series} must carry exactly the two operation kinds",
        );
        assert_eq!(
            label_values_of(&exporter, series, "dry_run"),
            vec!["false", "true"],
            "{series} must distinguish a dry-run pass from a committing one",
        );
    }
}

/// The verdict counter takes the mode and **not** the kind: a compatibility
/// verdict is only ever computed for a registration, so a `kind` label there
/// would be one constant series — noise rather than signal.
#[test]
fn the_verdict_counter_carries_the_mode_but_not_the_kind() {
    let (provider, exporter, metrics) = recorder();

    metrics.compat_verdict(
        CompatibilityVerdict::Compatible,
        false,
        PassLabels::new(OperationKind::Registration, true),
    );
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_compat_verdicts_total",
            &[("verdict", "compatible"), ("dry_run", "true")],
        ),
        1,
    );
    assert!(
        label_values_of(&exporter, "types_registry_compat_verdicts_total", "kind").is_empty(),
        "a verdict is only computed for a registration; a constant label is not a signal",
    );
}

/// A dry run's hypothetical write set is **not observed**, and that is a
/// decision rather than an omission: the histogram answers "how close does this
/// deployment run to `limits.activation_write_set`", and a pass that wrote
/// nothing is not a data point about pressure on that bound. The *exceeded*
/// case stays visible — it is a refusal, and refusals carry `dry_run`.
#[test]
fn a_dry_run_write_set_is_not_observed() {
    let (provider, exporter, metrics) = recorder();

    metrics.observe_activation_write_set(7, PassLabels::new(OperationKind::Registration, true));
    provider.force_flush().unwrap();
    assert_eq!(
        histogram_count(&exporter, "types_registry_activation_write_set"),
        0,
        "a dry-run pass records no write set",
    );

    metrics.observe_activation_write_set(7, PassLabels::new(OperationKind::Registration, false));
    provider.force_flush().unwrap();
    assert_eq!(
        histogram_count(&exporter, "types_registry_activation_write_set"),
        1,
        "a committing pass still records one",
    );
}
