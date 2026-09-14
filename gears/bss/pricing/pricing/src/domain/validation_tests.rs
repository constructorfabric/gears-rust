//! Tests for the validation pipeline framework.

use super::{ValidationPipeline, ValidationReport, ValidationRule};

struct AlwaysViolates(&'static str);
struct AlwaysWarns(&'static str);
struct Silent(&'static str);

impl ValidationRule<u32> for AlwaysViolates {
    fn name(&self) -> &'static str {
        self.0
    }

    fn evaluate(&self, subject: &u32, report: &mut ValidationReport) {
        report.violate(self.0, subject.to_string(), "blocked");
    }
}

impl ValidationRule<u32> for AlwaysWarns {
    fn name(&self) -> &'static str {
        self.0
    }

    fn evaluate(&self, subject: &u32, report: &mut ValidationReport) {
        report.warn(self.0, subject.to_string(), "advisory");
    }
}

impl ValidationRule<u32> for Silent {
    fn name(&self) -> &'static str {
        self.0
    }

    fn evaluate(&self, _subject: &u32, _report: &mut ValidationReport) {}
}

#[test]
fn an_empty_report_is_publishable() {
    assert!(ValidationReport::default().is_publishable());
}

#[test]
fn a_warning_alone_never_blocks() {
    // If an advisory could block, the distinction between the two lists would
    // be decorative and a fail-closed rule could hide behind a soft word.
    let pipeline = ValidationPipeline::new().with_rule(Box::new(AlwaysWarns("W1")));

    let report = pipeline.run(&7);

    assert!(report.is_publishable());
    assert_eq!(report.warnings.len(), 1);
    assert!(report.violations.is_empty());
}

#[test]
fn one_violation_blocks_the_publish() {
    let pipeline = ValidationPipeline::new().with_rule(Box::new(AlwaysViolates("V1")));

    assert!(!pipeline.run(&7).is_publishable());
}

#[test]
fn every_rule_runs_even_after_a_failure() {
    // The aggregate report is the point: an author remediates a plan in one
    // pass, so a run must not stop at the first blocking rule.
    let pipeline = ValidationPipeline::new()
        .with_rule(Box::new(AlwaysViolates("V1")))
        .with_rule(Box::new(AlwaysViolates("V2")))
        .with_rule(Box::new(AlwaysWarns("W1")));

    let report = pipeline.run(&7);

    assert_eq!(report.violations.len(), 2);
    assert_eq!(report.warnings.len(), 1);
}

#[test]
fn findings_appear_in_registration_order() {
    let pipeline = ValidationPipeline::new()
        .with_rule(Box::new(AlwaysViolates("first")))
        .with_rule(Box::new(Silent("quiet")))
        .with_rule(Box::new(AlwaysViolates("second")));

    let report = pipeline.run(&1);

    assert_eq!(report.violations[0].code, "first");
    assert_eq!(report.violations[1].code, "second");
    assert_eq!(pipeline.rule_names(), vec!["first", "quiet", "second"]);
}

#[test]
fn a_run_carries_nothing_into_the_next_one() {
    // The same pipeline runs as a submit-time pre-check and again inside the
    // publish commit. If a run kept state, the commit-time verdict would depend
    // on how many times the plan had been submitted.
    let pipeline = ValidationPipeline::new().with_rule(Box::new(AlwaysViolates("V1")));

    let first = pipeline.run(&7);
    let second = pipeline.run(&7);

    assert_eq!(first, second);
    assert_eq!(second.violations.len(), 1);
}

#[test]
fn a_pipeline_with_no_rules_publishes_everything() {
    // Recorded deliberately: this is why the Foundation registers the money and
    // rounding rules itself instead of relying on a slice to have loaded.
    let pipeline: ValidationPipeline<u32> = ValidationPipeline::new();

    assert!(pipeline.run(&7).is_publishable());
    assert!(pipeline.rule_names().is_empty());
}

#[test]
fn absorb_concatenates_both_lists() {
    let mut left = ValidationReport::default();
    left.violate("A", "row-1", "x");
    let mut right = ValidationReport::default();
    right.violate("B", "row-2", "y");
    right.warn("C", "row-2", "z");

    left.absorb(right);

    assert_eq!(left.violations.len(), 2);
    assert_eq!(left.warnings.len(), 1);
    assert_eq!(left.violations[1].code, "B");
}

#[test]
fn the_report_renders_its_blocking_count() {
    let mut report = ValidationReport::default();
    report.violate("A", "row-1", "x");
    report.violate("B", "row-2", "y");
    report.warn("C", "row-3", "z");

    assert_eq!(report.to_string(), "2");
}

// ---------------------------------------------------------------------------
// The write-stage census, derived from the source rather than transcribed.
// ---------------------------------------------------------------------------

/// Every `.rs` file under `src`, recursively, that is not a test module.
///
/// `approval_repo_tests::production_sources`' construction and for its reason: a
/// scan that reads one directory level is a guard a reorganisation switches off
/// silently.
fn production_sources() -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("the crate's own source tree") {
            let path = entry.expect("a readable dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with("_tests.rs"))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The three ways a [`Violation`](super::Violation) acquires
/// [`Stage::Write`](super::Stage::Write), as source text.
///
/// The third one is why this is a scan and not a grep for `violate_at_write`:
/// `taxonomy.rs` builds its `Violation` as a literal and calls no `violate*`
/// method at all, so a derivation over the two methods **cannot see it** — which
/// is precisely how the census in [`super::Stage`]'s doc went wrong the second
/// time.
const WRITE_STAMPING_FORMS: [&str; 3] = ["violate_at_write(", "violate_at(", "stage: Stage::Write"];

/// The file stems that can stamp a violation `Stage::Write`, read off the source.
fn write_stamping_sites() -> std::collections::BTreeSet<String> {
    production_sources()
        .into_iter()
        .filter(|path| {
            // `validation.rs` declares the vocabulary; it is not a site that uses it.
            path.file_stem().and_then(|s| s.to_str()) != Some("validation") && {
                let text = std::fs::read_to_string(path).expect("a readable source file");
                WRITE_STAMPING_FORMS.iter().any(|form| text.contains(form))
            }
        })
        .map(|path| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .expect("a utf-8 file stem")
                .to_owned()
        })
        .collect()
}

/// Every module that can stamp a violation at the write stage is one somebody
/// decided should.
///
/// **This replaces a number in prose.** [`super::Stage`]'s doc carried a count of
/// the rules that emit a write-stage violation, and it has been wrong twice — "three
/// of the four" when five of seven was the fact, then "the eight" when the 2026-08-17
/// review found eight of at least fifteen. A count of code sites, maintained by hand
/// beside the code, is the defect; the number is a symptom.
///
/// The operand here moves independently of the expectation: the left side is a walk
/// of the crate's own sources, the right side is a list with a reason per entry. A
/// module that starts stamping `Stage::Write` reddens this and has to say why it
/// may. A module that stops reddens it too, which is the direction that catches a
/// door being quietly removed.
///
/// What it deliberately does **not** claim is completeness over *rules*. The
/// check-function plane refuses at authoring doors without a stage at all — see
/// [`super::Stage`]'s table — and no scan of stamping sites can see a door chosen by
/// call site.
#[test]
fn every_write_stamping_site_is_accounted_for() {
    let sites = write_stamping_sites();

    let expected: std::collections::BTreeSet<String> = [
        // The doors themselves: the three callers of `write_stage_only()`. The
        // plan door hand-builds a report from two `phase_graph` rules and passes
        // `Stage::Write` to them, so it names the value.
        "plans",
        // Slice 3's row rules, where the doctrine was written: a fault whose every
        // operand is in the request. This read "and one of them is frozen by the
        // scope key" until 2026-08-21, a day after D-312's amendment dropped that
        // half; `model_kind` holds the two arms that moved, and neither has such
        // an operand.
        "model_kind",
        "allowance",
        "level_aggregation",
        "reservation",
        // The fifth member of that family, and the one that had no such arm until
        // 2026-08-18 (review B3-1): `package_size` / `package_price_minor` beside
        // a frozen non-usage `chargeKind`. `ModelKind::Package` is legal only on a
        // usage row, so no later call can legalise either field and the sole
        // resolution retracts the one just sent — the doctrine's criterion,
        // met exactly as its four siblings meet it.
        "package",
        // Slice 2's plan-name rule (`inst-cmp-planname`).
        "composition",
        // The two per-instance rules whose stage is a parameter of the door that
        // asks — `inst-ph-row-attached` (D-342) and `inst-ph-graph`.
        "phase_graph",
        // The one hand-constructed `Violation { stage: Stage::Write }` in the
        // crate: `RegionsDeclared`, whose write door is
        // `api::rest::prices::require_declared_region`. Its sibling
        // `RoundingPolicyDeclared` carried the same stamp with no door at all and
        // lost it on 2026-08-18 (review B5-2).
        "taxonomy",
        // Slice 3's band geometry, added 2026-08-19 by the W1 e2e wave. A
        // zero-width band is read from `from_qty == to_qty` alone: no operand
        // lies outside the request, and no later call can give a band of no
        // width a quantity to price. Its write door is the same one its four
        // siblings use, `api::rest::prices`'s `require_no_key_contradiction`.
        //
        // It is here because the **store** was judging it first: the row's
        // insert trips `chk_pricing_price_tier_band_width` and the caller got a
        // 500 rather than a violation. A rule that only runs at publish cannot
        // protect a write the store refuses.
        "tier_bands",
        // The overlay authoring edge (review Z3-5): `check_authored_shape`'s three
        // faults are decidable from the authored document alone, and stamping them
        // is what makes `write_stage_only()` safe on that report instead of
        // silently answering `None`.
        "overlay_rules",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();

    assert_eq!(
        sites, expected,
        "the modules that can stamp a violation Stage::Write have moved; add the new one here \
         with the door that judges it, or record why one stopped"
    );
}

// ---------------------------------------------------------------------------
// The wire spelling of every domain rule code.
// ---------------------------------------------------------------------------

/// The `pub const NAME: &str = "VALUE";` declarations under `src/domain`, as
/// `(name, value, file)`.
fn domain_string_constants() -> Vec<(String, String, String)> {
    let domain = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/domain");
    production_sources()
        .into_iter()
        .filter(|path| path.starts_with(&domain))
        .flat_map(|path| {
            let file = path
                .file_name()
                .and_then(|n| n.to_str())
                .expect("a utf-8 file name")
                .to_owned();
            std::fs::read_to_string(&path)
                .expect("a readable source file")
                .lines()
                .filter_map(|line| line.strip_prefix("pub const "))
                .filter_map(|rest| rest.split_once(": &str = \""))
                .filter_map(|(name, rest)| rest.split_once("\";").map(|(v, _)| (name, v)))
                .map(|(name, value)| (name.to_owned(), value.to_owned(), file.clone()))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The constants whose value is deliberately **not** their identifier, each with
/// what it actually is.
///
/// Every one is a stored or wire **value token**, not a rule code — a thing the
/// catalog writes into a column or a payload, whose spelling belongs to the
/// design set's vocabulary rather than to this crate's naming. They are the
/// exception this census has to name, and naming them is the half that carries
/// meaning: an entry added here is a decision that a string on the wire may
/// diverge from the name the code refers to it by.
const VALUE_TOKENS: &[(&str, &str)] = &[
    // The evaluation policy's generation tag (D-133), which is a version and not
    // a name.
    ("EVALUATION_POLICY_GENERATION", "ep-4"),
    // D-07's reserved absorber, refused as a party id.
    ("PLATFORM_SENTINEL", "platform"),
    // The classless overlay scope's stored value.
    ("GLOBAL_SCOPE", "global"),
    // The change policy a cross-boundary plan change publishes (D-114).
    ("CROSS_BOUNDARY_CHANGE_POLICY", "cancel_plus_new"),
    // The two grant-key prefixes the migration delta renders (D-41).
    ("GRANT_KEY_QUOTA", "quota:"),
    ("GRANT_KEY_FLAG", "flag:"),
    // §6's two `billing_timing` column values.
    ("BILLING_TIMING_ADVANCE", "advance"),
    ("BILLING_TIMING_ARREARS", "arrears"),
    // The filler the canonical scope-key rendering writes for an absent axis. It
    // is a rendered value, and the two free-form axes refuse it as a value of
    // their own so the rendering stays injective.
    ("ABSENT_AXIS_TOKEN", "none"),
];

/// Every domain rule code is spelled exactly as its identifier.
///
/// **This replaces 45 spelling pins nobody wrote.** The 2026-08-17 review found
/// that 45 of the crate's 138 domain rule codes had no byte-for-byte literal
/// anywhere in `src/` or `tests/`, so their wire spelling rested on the identifier
/// alone: a constant whose value drifted from its name would still compile, would
/// still be referenced at every `report.violate` site, and would be wrong on the
/// wire — `plan_rules_tests`' own words about why its `DECLARED` table exists.
/// Eleven of Slice 3's codes were in that state while `rules.rs` made the same
/// claim `DECLARED` makes.
///
/// One `assert_eq!(CODE, "CODE")` per code would have been 45 lines that go stale
/// the moment somebody mints the 46th. This is over the **whole** set, derived,
/// and it grows by itself.
///
/// What it does **not** replace is `DECLARED`, which is stronger in the direction
/// that matters most for a slice with a §5 table: there the right-hand side is
/// transcribed from the design document, so it catches a constant and a document
/// that disagree. This one only catches a constant that disagrees with **itself**
/// — which is precisely the exposure the review measured.
#[test]
fn every_domain_rule_code_is_spelled_as_its_own_name() {
    let constants = domain_string_constants();

    assert!(
        constants.len() >= 130,
        "the scan found {} declarations, fewer than the crate had when it was written — a scan \
         that silently matched nothing asserts this vacuously",
        constants.len()
    );

    let exempt: std::collections::BTreeMap<&str, &str> = VALUE_TOKENS.iter().copied().collect();

    for (name, value, file) in &constants {
        if let Some(expected) = exempt.get(name.as_str()) {
            assert_eq!(
                value, expected,
                "{file}: {name} is a declared value token, and its token moved; if that is \
                 intended, move it here with the reason"
            );
            continue;
        }
        assert_eq!(
            value, name,
            "{file}: the rule code {name} is spelled {value} on the wire. A code whose value \
             drifts from its name compiles, is referenced at every violate site, and is wrong \
             for every consumer; if this one is a value token rather than a code, declare it in \
             VALUE_TOKENS with what it is"
        );
    }
}

/// No two domain rule codes share a spelling, with one argued exception.
///
/// A discriminator that names two rules cannot discriminate.
/// `CURRENCY_NOT_COVERED` is shared on purpose — §5 declares one code for all
/// three enumerated configurations because the operator's remedy is the same —
/// and since 2026-08-18 it is shared by **re-export** rather than by two
/// declarations, so this census sees it once.
#[test]
fn no_two_domain_rule_codes_share_a_spelling() {
    let mut by_value: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (name, value, file) in domain_string_constants() {
        by_value
            .entry(value)
            .or_default()
            .push(format!("{file}:{name}"));
    }
    let shared: Vec<(&String, &Vec<String>)> = by_value
        .iter()
        .filter(|(_, sites)| sites.len() > 1)
        .collect();

    assert!(
        shared.is_empty(),
        "two declarations render one string, so a consumer cannot tell the two rules apart: \
         {shared:?}"
    );
}
