use super::*;

fn satisfied() -> PredicateAnswer {
    PredicateAnswer::Satisfied
}

fn failed() -> PredicateAnswer {
    PredicateAnswer::Failed {
        detail: "the window does not cover t".to_owned(),
    }
}

fn unevaluable() -> PredicateAnswer {
    PredicateAnswer::NotEvaluable { owed_to: "Slice 4" }
}

fn outcome(predicate: Predicate, answer: PredicateAnswer) -> PredicateOutcome {
    PredicateOutcome { predicate, answer }
}

fn component(id: u128, verdict: PlanMarketVerdict) -> ComponentSellability {
    ComponentSellability {
        component_plan_id: Uuid::from_u128(id),
        verdict,
    }
}

// ---------------------------------------------------------------------------
// Predicate (6) is excluded from a component reference.
// ---------------------------------------------------------------------------

/// D-46's flag applies to the bundle SKU, never to a component reference:
/// `sellable = false` components are exactly the composition-only SKUs bundles
/// exist to package.
#[test]
fn a_component_failing_the_registry_flag_still_passes() {
    let verdict = component_verdict(&[
        outcome(Predicate::ActiveWindowWithHorizon, satisfied()),
        outcome(Predicate::CommittedVersion, satisfied()),
        outcome(Predicate::AvailabilityDates, satisfied()),
        outcome(Predicate::PlanLifecycleState, satisfied()),
        outcome(Predicate::GaGateFlags, satisfied()),
        outcome(
            Predicate::RegistrySellable,
            PredicateAnswer::Failed {
                detail: "sellable = false".to_owned(),
            },
        ),
    ]);

    assert_eq!(verdict, PlanMarketVerdict::Sellable);
}

/// And every other failing predicate still refuses.
#[test]
fn a_component_failing_any_of_one_to_five_is_not_sellable() {
    for predicate in [
        Predicate::ActiveWindowWithHorizon,
        Predicate::CommittedVersion,
        Predicate::AvailabilityDates,
        Predicate::PlanLifecycleState,
        Predicate::GaGateFlags,
    ] {
        assert_eq!(
            component_verdict(&[outcome(predicate, failed())]),
            PlanMarketVerdict::NotSellable,
            "{predicate:?} must still refuse"
        );
    }
}

/// `Failed` dominates `NotEvaluable`: an operator told to wait for a slice that
/// cannot change the answer has been told the wrong thing.
#[test]
fn a_failure_beside_an_unevaluable_predicate_is_not_sellable() {
    let verdict = component_verdict(&[
        outcome(Predicate::ActiveWindowWithHorizon, failed()),
        outcome(Predicate::GaGateFlags, unevaluable()),
    ]);

    assert_eq!(verdict, PlanMarketVerdict::NotSellable);
}

#[test]
fn nothing_failed_and_something_unevaluable_is_not_evaluable() {
    let verdict = component_verdict(&[
        outcome(Predicate::ActiveWindowWithHorizon, satisfied()),
        outcome(Predicate::GaGateFlags, unevaluable()),
    ]);

    assert_eq!(verdict, PlanMarketVerdict::NotEvaluable);
}

// ---------------------------------------------------------------------------
// The conjunction.
// ---------------------------------------------------------------------------

#[test]
fn every_component_sellable_makes_the_bundle_sellable() {
    let verdict = bundle_verdict(
        PriceBasis::SumOfParts,
        None,
        &satisfied(),
        &[
            component(1, PlanMarketVerdict::Sellable),
            component(2, PlanMarketVerdict::Sellable),
        ],
    );

    assert_eq!(verdict, PlanMarketVerdict::Sellable);
}

/// D-94, one level up: one unsellable component makes the bundle unsellable,
/// never partially sellable.
#[test]
fn one_unsellable_component_makes_the_whole_bundle_unsellable() {
    let verdict = bundle_verdict(
        PriceBasis::SumOfParts,
        None,
        &satisfied(),
        &[
            component(1, PlanMarketVerdict::Sellable),
            component(2, PlanMarketVerdict::NotSellable),
        ],
    );

    assert_eq!(verdict, PlanMarketVerdict::NotSellable);
}

/// The bundle's own availability is in the conjunction and no component carries
/// it.
#[test]
fn the_bundles_own_availability_can_refuse_on_its_own() {
    let verdict = bundle_verdict(
        PriceBasis::SumOfParts,
        None,
        &failed(),
        &[component(1, PlanMarketVerdict::Sellable)],
    );

    assert_eq!(verdict, PlanMarketVerdict::NotSellable);
}

/// An empty conjunction is not vacuously true: a bundle binding no key on this
/// market is not sellable on it.
#[test]
fn a_bundle_referencing_no_component_is_not_sellable() {
    let verdict = bundle_verdict(PriceBasis::SumOfParts, None, &satisfied(), &[]);

    assert_eq!(verdict, PlanMarketVerdict::NotSellable);
}

/// For `own_price` the bundle's **own** rows must pass as well.
#[test]
fn an_own_price_bundles_own_rows_are_in_the_conjunction() {
    let verdict = bundle_verdict(
        PriceBasis::OwnPrice,
        Some(PlanMarketVerdict::NotSellable),
        &satisfied(),
        &[component(1, PlanMarketVerdict::Sellable)],
    );

    assert_eq!(verdict, PlanMarketVerdict::NotSellable);
}

/// For `sum_of_parts` there are no own rows (`inst-bb-rowless`), so an own
/// verdict handed in anyway is ignored rather than folded in.
#[test]
fn a_sum_of_parts_bundle_ignores_an_own_verdict() {
    let verdict = bundle_verdict(
        PriceBasis::SumOfParts,
        Some(PlanMarketVerdict::NotSellable),
        &satisfied(),
        &[component(1, PlanMarketVerdict::Sellable)],
    );

    assert_eq!(verdict, PlanMarketVerdict::Sellable);
}

/// The other half of the same caller error, and the half that fails **open**.
///
/// `sum_of_parts` carrying an own verdict drops a fact about rows that do not
/// exist, which cannot make an unsellable bundle sell. `own_price` carrying
/// **none** drops the bundle's own rows out of the conjunction, so a bundle whose
/// own rows fail predicates (1)-(5) would answer `Sellable` on its components
/// alone. `NotEvaluable` is what the gate says when it was not given an input it
/// needs.
#[test]
fn an_own_price_bundle_with_no_own_verdict_is_not_evaluable() {
    let verdict = bundle_verdict(
        PriceBasis::OwnPrice,
        None,
        &satisfied(),
        &[component(1, PlanMarketVerdict::Sellable)],
    );

    assert_eq!(verdict, PlanMarketVerdict::NotEvaluable);
}

/// And that missing input does not outrank a definite failure beside it.
///
/// `fold`'s ordering is forced rather than chosen: a `Failed` beside a
/// `NotEvaluable` is a fact that will not improve by waiting. The absent own
/// verdict is a `NotEvaluable` like any other, so answering it while the bundle's
/// own availability window has already failed — or while a component is already
/// unsellable — tells an operator to wait for a slice that cannot change the
/// answer.
#[test]
fn a_definite_failure_dominates_a_missing_own_verdict() {
    let past_its_window = bundle_verdict(
        PriceBasis::OwnPrice,
        None,
        &failed(),
        &[component(1, PlanMarketVerdict::Sellable)],
    );
    assert_eq!(past_its_window, PlanMarketVerdict::NotSellable);

    // The component plane is the second input the early return outranked, and it
    // would survive a fix that only moved the availability test.
    let beside_an_unsellable_component = bundle_verdict(
        PriceBasis::OwnPrice,
        None,
        &satisfied(),
        &[component(1, PlanMarketVerdict::NotSellable)],
    );
    assert_eq!(
        beside_an_unsellable_component,
        PlanMarketVerdict::NotSellable
    );
}

#[test]
fn an_unevaluable_component_makes_the_bundle_unevaluable() {
    let verdict = bundle_verdict(
        PriceBasis::SumOfParts,
        None,
        &satisfied(),
        &[
            component(1, PlanMarketVerdict::Sellable),
            component(2, PlanMarketVerdict::NotEvaluable),
        ],
    );

    assert_eq!(verdict, PlanMarketVerdict::NotEvaluable);
}

/// And a failure beside it still dominates, at the bundle level too.
#[test]
fn a_failure_dominates_an_unevaluable_component() {
    let verdict = bundle_verdict(
        PriceBasis::SumOfParts,
        None,
        &satisfied(),
        &[
            component(1, PlanMarketVerdict::NotEvaluable),
            component(2, PlanMarketVerdict::NotSellable),
        ],
    );

    assert_eq!(verdict, PlanMarketVerdict::NotSellable);
}

// ---------------------------------------------------------------------------
// The subsystem has no caller, and that is checked rather than asserted in prose.
// ---------------------------------------------------------------------------

/// One of this crate's non-test sources, with its comments and string literals
/// blanked — `crate::source_scan`'s single reading of *"is this token in the
/// crate's code, or only in its prose"*, borrowed rather than re-implemented for
/// D-321 clause (4)'s reason: two readings of "code" would be two answers to one
/// question.
struct Source {
    /// The path, relative to the crate root, for the diagnostic.
    label: String,
    /// The file with comments and literals blanked.
    code: String,
}

/// Every `.rs` file under `src/` that is not itself a test module.
///
/// `_tests.rs` is excluded for the trigger census's reason and it is the point of
/// the instrument: a construction exercised only by the cases asserting about it
/// is exactly the state this module is in, so counting its own suite as a caller
/// would make the census unable to see it.
fn crate_sources(dir: &std::path::Path, out: &mut Vec<Source>) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for entry in std::fs::read_dir(dir).expect("the crate's source tree is readable") {
        let path = entry.expect("a readable directory entry").path();
        if path.is_dir() {
            crate_sources(&path, out);
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
            || name.ends_with("_tests.rs")
        {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a readable source file");
        out.push(Source {
            // `/` on every host, because the lookups below are written with `/`
            // and `Display` uses the platform separator. On Windows the label
            // read `infra\window.rs`, `source_ending_in("infra/window.rs")`
            // matched nothing, and the panic reported "the walk did not reach
            // infra/window.rs; it saw 295 sources" — a census failure that says
            // nothing about the census's subject. `Path::ends_with` would also
            // fix it, but the label is carried into diagnostics too, and one
            // spelling of a path in a message is worth more than two.
            label: path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string()
                .replace('\\', "/"),
            code: crate::source_scan::blank_comments_and_literals(&text),
        });
    }
}

/// Every file outside this module that names `needle` in code.
fn naming_sites<'a>(sources: &'a [Source], needle: &str) -> Vec<&'a str> {
    sources
        .iter()
        .filter(|source| !source.label.ends_with("bundle_sellability.rs"))
        .filter(|source| source.code.contains(needle))
        .map(|source| source.label.as_str())
        .collect()
}

/// **`inst-sg-bundle` is unevaluated because this module has no caller**, and this
/// is what makes that a fact of the build rather than an impression a reader forms
/// by grepping once.
///
/// It reddens the day a caller lands — which is the day
/// [`crate::domain::sellability`]'s `inst-sg-bundle` section and this module's own
/// "no caller" paragraph both become false and have to be rewritten. That
/// coupling is deliberate: three accounts of this surface were stale for eight
/// days precisely because nothing was holding them.
///
/// **The positive control is not optional.** A walk that found nothing because the
/// blanking ate every file, or because the source root moved, would be green for
/// the wrong reason and would stay green forever. `plan_market_verdict` is the
/// control: it is the conjunction this one is the bundle-level twin of, and it has
/// exactly one naming site outside its own module — `api::rest::windows`, where
/// the surface is rendered. If the walk cannot see that, it cannot see anything.
#[test]
fn nothing_in_this_crate_reaches_the_bundle_conjunction() {
    let mut sources = Vec::new();
    crate_sources(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut sources,
    );

    // The control first: a green refusal below means nothing until the walk is
    // shown to read code at all.
    let control = naming_sites(&sources, "plan_market_verdict");
    assert!(
        control.iter().any(|at| at.ends_with("windows.rs")),
        "the walk cannot see `plan_market_verdict`'s one production caller, so its \
         silence about the bundle conjunction proves nothing; it saw {} sources and \
         found {control:?}",
        sources.len()
    );

    for needle in [
        "bundle_verdict",
        "component_verdict",
        "ComponentSellability",
    ] {
        let sites = naming_sites(&sources, needle);
        assert!(
            sites.is_empty(),
            "`{needle}` is named in this crate's code at {sites:?}. If that is a real \
             caller, the bundle conjunction is wired and three accounts now lie: this \
             module's \"no caller\" paragraph, `domain::sellability`'s `inst-sg-bundle` \
             section, and `api::rest::bundles`' note that a composition publish freezes \
             nothing. Fix them with the wiring, not this assertion."
        );
    }
}

/// The source that ends in `suffix`, or a panic naming what the walk did see.
fn source_ending_in<'a>(sources: &'a [Source], suffix: &str) -> &'a Source {
    sources
        .iter()
        .find(|source| source.label.ends_with(suffix))
        .unwrap_or_else(|| {
            panic!(
                "the walk did not reach {suffix}; it saw {} sources",
                sources.len()
            )
        })
}

/// **The act that would freeze a component set does not exist**, and this is the
/// half of the blocker that no amount of work in `domain::` can repair.
///
/// `inst-ba-return` says a bundle publish leaves the composition *"frozen into the
/// read model / snapshot"*. A publish unit freezes anything in this gear by
/// recording a `PendingVersionRow`, which is what the projector later drives; the
/// composition act records none, so no `CatalogVersion` advances and no subject
/// re-projects. Until that changes, `inst-sg-bundle`'s *"frozen component key
/// set"* has nowhere to be frozen **into**, whatever
/// [`crate::domain::bundle_sellability`] is able to compute over it.
///
/// `infra::window` is the positive control and the closest sibling: it is the
/// other Slice-7 mutation path, it is also a 202, and it *does* record one. A
/// refusal with no control here would be green the day `PendingVersionRow` is
/// renamed.
#[test]
fn a_composition_publish_advances_no_catalog_version() {
    let mut sources = Vec::new();
    crate_sources(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut sources,
    );

    assert!(
        source_ending_in(&sources, "infra/window.rs")
            .code
            .contains("PendingVersionRow"),
        "the control failed: a publish unit that does record a pending version no \
         longer names one, so this walk cannot tell a frozen act from an unfrozen one"
    );

    assert!(
        !source_ending_in(&sources, "infra/bundle.rs")
            .code
            .contains("PendingVersionRow"),
        "`infra::bundle` now records a pending `CatalogVersion`, so a composition \
         publish re-projects its subject. That is the operand `inst-sg-bundle` has \
         been waiting for: `PlanSubjectDelta` may now carry a bundle member, and \
         `domain::sellability`'s `inst-sg-bundle` section, this module's \"no \
         caller\" paragraph and `api::rest::bundles`' 202 note are all now wrong."
    );
}
