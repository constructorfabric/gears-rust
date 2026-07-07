//! Tests for the v1 default resolvers: the deferral resolver defaults a missing
//! `first_period_id` from the invoice period and rejects a malformed one; the SSP
//! resolver enforces the multi-PO presence gate; the VC resolver echoes the refs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::domain::error::DomainError;
use crate::domain::recognition::input::{RecognitionInput, RecognitionTiming};

fn input_with(timing: RecognitionTiming) -> RecognitionInput {
    RecognitionInput {
        policy_ref: "policy.straightline.v1".to_owned(),
        timing,
        po_allocation_group: Some("grp".to_owned()),
        multi_po: false,
        ssp_snapshot_ref: None,
        subscription_ref: None,
        vc_estimate_ref: None,
        vc_method_ref: None,
        immaterial_one_shot_sku: false,
    }
}

fn ctx<'a>(input: &'a RecognitionInput, invoice_period: &'a str) -> RecognitionContext<'a> {
    RecognitionContext {
        input,
        invoice_period_id: invoice_period,
        item_amount_minor_ex_tax: 12_000,
        invoice_total_minor: 12_000,
        currency: "USD",
        revenue_stream: "recurring",
    }
}

#[test]
fn default_resolver_fills_first_period_from_invoice_period() {
    let input = input_with(RecognitionTiming::StraightLine {
        periods: 12,
        first_period_id: None,
    });
    let resolved = DefaultDeferralPolicyResolver
        .resolve(&ctx(&input, "202606"))
        .unwrap();
    assert_eq!(resolved.policy_ref, "policy.straightline.v1");
    match resolved.timing {
        RecognitionTiming::StraightLine {
            periods,
            first_period_id,
        } => {
            assert_eq!(periods, 12);
            assert_eq!(first_period_id.as_deref(), Some("202606"));
        }
        RecognitionTiming::PointInTime => panic!("expected straight-line"),
    }
}

#[test]
fn default_resolver_keeps_explicit_first_period() {
    let input = input_with(RecognitionTiming::StraightLine {
        periods: 3,
        first_period_id: Some("202701".to_owned()),
    });
    let resolved = DefaultDeferralPolicyResolver
        .resolve(&ctx(&input, "202606"))
        .unwrap();
    match resolved.timing {
        RecognitionTiming::StraightLine {
            first_period_id, ..
        } => assert_eq!(first_period_id.as_deref(), Some("202701")),
        RecognitionTiming::PointInTime => panic!("expected straight-line"),
    }
}

#[test]
fn default_resolver_rejects_malformed_first_period() {
    let input = input_with(RecognitionTiming::StraightLine {
        periods: 3,
        first_period_id: Some("nope".to_owned()),
    });
    let err = DefaultDeferralPolicyResolver
        .resolve(&ctx(&input, "202606"))
        .unwrap_err();
    assert!(matches!(err, DomainError::RecognitionPolicyConflict(_)));
}

#[test]
fn default_resolver_passes_point_in_time_through() {
    let input = input_with(RecognitionTiming::PointInTime);
    let resolved = DefaultDeferralPolicyResolver
        .resolve(&ctx(&input, "202606"))
        .unwrap();
    assert_eq!(resolved.timing, RecognitionTiming::PointInTime);
}

#[test]
fn ssp_resolver_blocks_multi_po_without_ref() {
    let input = RecognitionInput {
        multi_po: true,
        ssp_snapshot_ref: None,
        ..input_with(RecognitionTiming::PointInTime)
    };
    let err = DefaultSspResolver
        .resolve(&ctx(&input, "202606"))
        .unwrap_err();
    assert!(matches!(err, DomainError::SspSnapshotRequired(_)));
}

#[test]
fn ssp_resolver_passes_multi_po_with_ref() {
    let input = RecognitionInput {
        multi_po: true,
        ssp_snapshot_ref: Some("ssp.v9".to_owned()),
        ..input_with(RecognitionTiming::PointInTime)
    };
    let got = DefaultSspResolver.resolve(&ctx(&input, "202606")).unwrap();
    assert_eq!(got.as_deref(), Some("ssp.v9"));
}

#[test]
fn ssp_resolver_passes_single_po_none() {
    let input = input_with(RecognitionTiming::PointInTime);
    let got = DefaultSspResolver.resolve(&ctx(&input, "202606")).unwrap();
    assert_eq!(got, None);
}

#[test]
fn vc_resolver_echoes_refs() {
    let input = RecognitionInput {
        vc_estimate_ref: Some("vc.est.1".to_owned()),
        vc_method_ref: Some("vc.method.1".to_owned()),
        ..input_with(RecognitionTiming::PointInTime)
    };
    let (est, method) = DefaultVcResolver.resolve(&ctx(&input, "202606")).unwrap();
    assert_eq!(est.as_deref(), Some("vc.est.1"));
    assert_eq!(method.as_deref(), Some("vc.method.1"));
}
