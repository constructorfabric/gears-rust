//! Unit cases for [`super`] — D-76's live-history rule (its tier *order* went with
//! tier 2 under D-330), D-81's two instants, `inst-sy-firstrating`'s never-inline
//! rule and clause (3)'s fail-closed refusal.

use uuid::Uuid;

use super::{
    LiveCandidate, SelectedRow, SelectionTier, SynthesisOutcome, SynthesisTrigger, UnresolvedKey,
    select_rows,
};
use crate::domain::error::DomainError;

fn live(plan_revision: Option<u64>) -> LiveCandidate {
    LiveCandidate {
        price_id: Uuid::now_v7(),
        plan_revision,
    }
}

// ---------------------------------------------------------------------------
// D-76's live-history rule, as narrowed by D-330.
//
// Four cases stood here for tier 2 — its short-circuit, its greatest-
// `effective_from` order, its at-most-one answer and the "the seam still holds"
// case that asserted the empty-input shape every real call made. All four are
// deleted with the tier (D-330), not retargeted: a probe kept pointing at a
// deleted rule is the shape that reads as coverage to a grep and asserts nothing.
// ---------------------------------------------------------------------------

#[test]
fn live_history_resolves_and_carries_the_revision_it_was_given() {
    let candidate = live(Some(3));
    let selected = select_rows(&[candidate])
        .into_iter()
        .next()
        .expect("live history answers");

    assert_eq!(selected.row_id, candidate.price_id);
    assert_eq!(selected.tier, SelectionTier::LiveHistory);
    assert_eq!(selected.plan_revision, Some(3));
}

#[test]
fn no_live_row_answering_is_empty_and_never_the_current_row() {
    // Clause (3). The whole rule exists for this: the current row is precisely
    // the price the subscriber was **not** paying. With clause (2) struck this is
    // the *only* other outcome, so it is the one every unresolvable key takes.
    assert!(select_rows(&[]).is_empty());
    // ...while a covered key still resolves, which is what makes the refusal a
    // refusal rather than a broken rule.
    let candidate = live(Some(0));
    assert_eq!(
        select_rows(&[candidate]),
        vec![SelectedRow {
            row_id: candidate.price_id,
            tier: SelectionTier::LiveHistory,
            plan_revision: Some(0),
        }]
    );
}

/// **A market with more than one live line freezes all of them.**
///
/// A [`FrozenKey`](crate::infra::synthesis::FrozenKey) is D-76's frozen
/// `(currency, region)` pair — a market, not a canonical scope key — and a market
/// legitimately carries several lines: `inst-cs-hybrid` exists to sanction a
/// recurring row beside a usage row, and an `existing_grandfathered` generation
/// sits beside an `all_subscriptions` row on the same pair.
///
/// This rule answered `Option` until 2026-08-11 and the caller took the first
/// candidate, so every other line on the market was dropped in silence. D-87 makes
/// the frozen payload self-contained — Rating evaluates from it and Billing posts
/// from it, resolving nothing through the read model — so **a dropped line is a
/// charge that never happens**, on a record with a seven-year horizon.
#[test]
fn every_live_line_on_one_market_is_frozen_not_the_first() {
    let recurring = live(Some(0));
    let usage = live(Some(0));

    let selected = select_rows(&[recurring, usage]);

    assert_eq!(
        selected.len(),
        2,
        "both lines of the market are the subscriber's economics, not one of them"
    );
    let frozen: Vec<Uuid> = selected.iter().map(|row| row.row_id).collect();
    assert!(frozen.contains(&recurring.price_id));
    assert!(frozen.contains(&usage.price_id));
    assert!(
        selected
            .iter()
            .all(|row| row.tier == SelectionTier::LiveHistory),
        "every resolved row carries the one rule that can resolve one (D-76 as narrowed by D-330)"
    );
}

// ---------------------------------------------------------------------------
// D-81's triggers, and `inst-sy-firstrating`.
// ---------------------------------------------------------------------------

/// `inst-sy-firstrating`, **as a statement rather than as enforcement.**
///
/// The rating line fails closed into the exception path, synthesis runs as a
/// separate audited step, and rating retries. Nothing in this crate consults
/// `runs_inline` — see the module doc — so this case would stay green if every
/// caller ran synthesis inline on `FirstRating`. What it holds is that the
/// predicate keeps saying what the instruction says, which is what a caller
/// landing later reads it for.
#[test]
fn first_rating_never_runs_inline_and_migration_does() {
    assert!(!SynthesisTrigger::FirstRating.runs_inline());
    assert!(SynthesisTrigger::Migration.runs_inline());
}

#[test]
fn every_trigger_round_trips_and_an_unknown_token_does_not_resolve() {
    for &trigger in SynthesisTrigger::ALL {
        assert_eq!(SynthesisTrigger::parse(trigger.as_str()), Some(trigger));
    }
    // The design set's prose spells it `first-rating`; the stored token is
    // `first_rating`, and the hyphenated form must not resolve - a snapshot
    // attributed to the wrong instant rule is worse than one that fails to read.
    assert_eq!(SynthesisTrigger::parse("first-rating"), None);
    assert_eq!(SynthesisTrigger::parse(""), None);
}

#[test]
fn every_selection_tier_round_trips_in_d76s_own_spelling() {
    for &tier in SelectionTier::ALL {
        assert_eq!(SelectionTier::parse(tier.as_str()), Some(tier));
    }
    assert_eq!(SelectionTier::LiveHistory.as_str(), "live_history");
    assert_eq!(SelectionTier::parse("live-history"), None);
    // **`historical_import` must not resolve.** D-330 struck the tier; a stored
    // token that still parsed would let a hand-written or migrated provenance row
    // claim a rule this system cannot perform, on a record with a seven-year
    // horizon. The `source` field survives the strike (S11 §4 clause 2) with
    // exactly one admissible value.
    assert_eq!(SelectionTier::parse("historical_import"), None);
    assert_eq!(SelectionTier::ALL.len(), 1);
}

// ---------------------------------------------------------------------------
// Clause (3) at the outcome level.
// ---------------------------------------------------------------------------

#[test]
fn a_fully_resolved_outcome_freezes() {
    let outcome = SynthesisOutcome {
        selected: vec![SelectedRow {
            row_id: Uuid::now_v7(),
            tier: SelectionTier::LiveHistory,
            plan_revision: Some(0),
        }],
        unresolved: Vec::new(),
    };
    assert!(outcome.ensure_complete(Uuid::now_v7()).is_ok());
}

#[test]
fn one_unresolved_key_refuses_the_whole_snapshot() {
    // **Partial synthesis is the one outcome that must not exist.** A snapshot
    // missing a key is a subscription that will fail to rate on it later, with a
    // frozen record asserting its economics were captured.
    let subscription = Uuid::now_v7();
    let outcome = SynthesisOutcome {
        selected: vec![SelectedRow {
            row_id: Uuid::now_v7(),
            tier: SelectionTier::LiveHistory,
            plan_revision: Some(0),
        }],
        unresolved: vec![UnresolvedKey {
            currency: "JPY".to_owned(),
            region: "APAC".to_owned(),
        }],
    };

    let err = outcome.ensure_complete(subscription).unwrap_err();
    let DomainError::PriceRowAbsent(detail) = err else {
        panic!("expected PriceRowAbsent, got {err:?}");
    };
    assert!(detail.contains("JPY"), "{detail}");
    assert!(detail.contains("APAC"), "{detail}");
    assert!(detail.contains(&subscription.to_string()), "{detail}");
    // The refusal says why it did not simply use the current row.
    assert!(detail.contains("were not paying"), "{detail}");
}

#[test]
fn an_empty_resolution_set_is_refused_with_its_own_sentence() {
    // Belt and braces with `chk_pricing_snapshot_provenance_resolved`: a snapshot
    // resolving nothing is not a snapshot.
    //
    // **The text is asserted, not merely `is_err()`.** This arm has nothing in
    // `unresolved` to render from, so rendering it the ordinary way produced
    // "covers subscription <id> on 0 scope key(s) at the synthesis instant: " -
    // a fail-closed refusal on a seven-year path that names nothing to go and fix
    // and lists none of it. `is_err()` was green through the whole of that, which
    // is why the degenerate sentence stood.
    let subscription = Uuid::now_v7();
    let outcome = SynthesisOutcome {
        selected: Vec::new(),
        unresolved: Vec::new(),
    };

    let err = outcome.ensure_complete(subscription).unwrap_err();
    let DomainError::PriceRowAbsent(detail) = err else {
        panic!("expected PriceRowAbsent, got {err:?}");
    };
    assert!(
        detail.contains("named no scope keys to resolve"),
        "{detail}"
    );
    assert!(detail.contains(&subscription.to_string()), "{detail}");
    assert!(
        !detail.contains("0 scope key(s)"),
        "the degenerate rendering is back: {detail}"
    );
}
