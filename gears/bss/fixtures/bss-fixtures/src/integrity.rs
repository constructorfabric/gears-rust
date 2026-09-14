//! Checks the corpus holds together on its own terms.
//!
//! The load-bearing one is [`IntegrityViolation::GatedKindUncovered`]: a family
//! may declare it gates a `modelKind` and carry no evaluation case for it, which
//! would let the registry report an `oracle` verdict over absent coverage — a
//! rule with a gate and no coverage is exactly the quiet hole this corpus exists
//! to close.
//!
//! Resolving each `provenance` id against the pricing design set is **not** done
//! here; that belongs with spec-check, which already declares and references
//! instruction ids and error codes. What is checked here is self-contained.

use crate::corpus::{Corpus, GateRole};
use crate::model::{Case, Family, ModelKind, PublishCase, PublishVerdict, Variant};

#[derive(Debug, PartialEq, Eq)]
pub enum IntegrityViolation {
    /// A family declares it gates this kind, but no **evaluation** case of that
    /// kind exercises it.
    ///
    /// The family's registry rows carry an `oracle` verdict, and only an
    /// evaluation case can earn one; the `publish` half is
    /// [`Self::ModelKindWithoutPublishCase`]'s subject and is counted per kind,
    /// not per family.
    GatedKindUncovered { family: Family, kind: ModelKind },
    /// Every case must cite the normative clause it encodes.
    MissingProvenance { case_id: String },
    /// A case exists for a family with no `_family.toml`.
    UndeclaredFamily { family: Family },
    /// A family claims the `Publish` role but lists no kinds, so it blocks
    /// nothing despite saying it does.
    FamilyGatesNothing { family: Family },
    /// A family claims the `Conformance` role yet lists gated kinds. The two
    /// readings contradict: either it gates publish or it does not.
    ConformanceFamilyGates { family: Family },
    /// A family claims the `Publish` role but maps to no registry
    /// [`Variant`], so the generator has no column to write its rows under and
    /// the gate can never ask about it.
    ///
    /// The silent form of [`Self::FamilyGatesNothing`]: that one catches a
    /// family that lists no kinds, this one catches a family that lists kinds
    /// nobody can look up.
    GatingFamilyWithoutVariant { family: Family },
    /// A case carries no assertions, so it proves nothing.
    CaseAssertsNothing { case_id: String },
    /// A publish case expects a rejection but names no error code.
    RejectionWithoutCode { case_id: String },
    /// A publish case declares itself unanswerable but names no slice.
    ///
    /// Same discipline as [`Self::RejectionWithoutCode`]: "nothing can answer
    /// this yet" without saying what would is not reviewable, and it is the one
    /// declaration in the corpus that suspends a case's evidence.
    DeclineWithoutSlice { case_id: String },
    /// A catalog `modelKind` no family gates. `inst-fx-gate` blocks publish of
    /// any kind without a green fixture, so an ungated kind is unpublishable.
    ModelKindUngated { kind: ModelKind },
    /// A catalog `modelKind` no publish case exercises.
    ///
    /// The registry's `publish` half is earned **per kind** by a passing run, so
    /// a kind the corpus asks no publish question of can never earn it: the gate
    /// stays shut for that kind forever. Exactly the shape of the `flat` hole —
    /// a rule whose gate nothing can open reads as a rule, and is a wall.
    ModelKindWithoutPublishCase { kind: ModelKind },
    /// A catalog `modelKind` whose only publish evidence is a **refusal**.
    ///
    /// One step past [`Self::ModelKindWithoutPublishCase`], and a subtler hole.
    /// `volume` earned its `publish` flag entirely from `kind-flip-rejected`
    /// and `package` entirely from `package-size-change-rejected` — both cases
    /// expecting a rejection. So for those two kinds `publish = true` meant
    /// "the gear reproduces one refusal", and **nothing in the corpus
    /// demonstrated that such a row can successfully publish at all**. A gear
    /// that rejected every `volume` supersession would have earned the identical
    /// flag and opened the identical gate.
    ///
    /// A flag a refusal alone can buy is not evidence that the kind is
    /// publishable; it is evidence that one guard fires. Both are needed, so a
    /// kind with no `accepted` case is a named build failure rather than a
    /// quietly weaker flag.
    ModelKindWithoutAcceptedPublishCase { kind: ModelKind },
}

/// Checks that every catalog `modelKind` is gated by some family.
///
/// Kept separate from [`check_integrity`] deliberately. Internal consistency
/// holds of any corpus, including the partial ones tests build; **completeness**
/// is a property of the committed corpus alone. Running them together would make
/// every partial fixture fail for being partial.
///
/// This is the check that would have caught `flat`: families and kinds are
/// different axes — nine families against five kinds, with `tier-boundary`
/// gating two — so a kind can quietly belong to no family at all.
///
/// It asks specifically for a family whose [`Variant`] is [`Variant::ModelKind`].
/// Any other family gating the kind is a *cross-cutting* fixture and is not the
/// kind's own: `supersession-continuity` gates `volume` (D-22) and would satisfy
/// a "gated by some family" reading while `tier-boundary` had quietly dropped
/// it, leaving the kind with a scenario fixture and no formula fixture.
#[must_use]
pub fn check_kind_coverage(corpus: &Corpus) -> Vec<IntegrityViolation> {
    ModelKind::ALL
        .iter()
        .filter(|kind| {
            !corpus.families.iter().any(|f| {
                f.family.variant() == Some(Variant::ModelKind) && f.gates.iter().any(|g| g == *kind)
            })
        })
        .map(|kind| IntegrityViolation::ModelKindUngated { kind: *kind })
        .collect()
}

/// Checks that every catalog `modelKind` is exercised by some publish case.
///
/// The sibling of [`check_kind_coverage`], on the other half of the registry and
/// for the same reason. That one asks whether a kind is *gated*; this one asks
/// whether the gate can ever **open**. `publish` is earned per kind by a passing
/// run over that kind's publish cases, so a kind with none earns nothing — and
/// "earns nothing" is indistinguishable, in the committed file, from "failed".
/// `flat` and `per_unit` sat in exactly that state: gated by a family, green on
/// their oracle half, and permanently unpublishable because no case existed to
/// pass.
///
/// A case marked `declined_until` does **not** count. It is authored against a
/// slice nothing has built, so it cannot pass and cannot earn the flag either;
/// counting it would let a kind's only coverage be a case that can never be
/// answered — the same silence, one level down.
///
/// ## And at least one of them must expect `accepted`
///
/// Requiring merely *a* publish case per kind was the same mistake one level in.
/// `volume`'s flag rested entirely on `kind-flip-rejected` and `package`'s on
/// `package-size-change-rejected`, both of which expect a **rejection** — so the
/// flag said "the gear reproduces one refusal" and nothing in the corpus said a
/// `volume` or `package` row can be published at all. A gear that refused every
/// such row would have earned the identical flag.
///
/// So a kind whose answerable publish cases are all refusals is
/// [`IntegrityViolation::ModelKindWithoutAcceptedPublishCase`] — a named build
/// failure in the generator, exactly like a kind with no case. The negative and
/// the positive pin different things and neither substitutes for the other: the
/// refusal pins where the guard bites, the acceptance pins that the guard has a
/// far side.
///
/// Kept out of [`check_integrity`] for the same reason as its sibling:
/// completeness is a property of the committed corpus, not of every partial one
/// a test builds.
#[must_use]
pub fn check_publish_case_coverage(corpus: &Corpus) -> Vec<IntegrityViolation> {
    let mut out = Vec::new();

    for kind in ModelKind::ALL {
        let answerable: Vec<&PublishCase> = corpus
            .cases
            .iter()
            .filter_map(|case| match case {
                Case::Publish(p)
                    if p.declined_until.is_none() && p.successor.model_kind == kind =>
                {
                    Some(p.as_ref())
                }
                _ => None,
            })
            .collect();

        if answerable.is_empty() {
            // Reported alone: "no publish case" and "no accepted publish case"
            // are one fault, and naming both would make one hole read as two.
            out.push(IntegrityViolation::ModelKindWithoutPublishCase { kind });
            continue;
        }

        let demonstrates_publish = answerable.iter().any(|p| {
            p.assert
                .iter()
                .any(|a| matches!(&a.expect, PublishVerdict::Accepted))
        });
        if !demonstrates_publish {
            out.push(IntegrityViolation::ModelKindWithoutAcceptedPublishCase { kind });
        }
    }

    out
}

#[must_use]
pub fn check_integrity(corpus: &Corpus) -> Vec<IntegrityViolation> {
    let mut out = Vec::new();

    for case in &corpus.cases {
        if case.provenance().is_empty() {
            out.push(IntegrityViolation::MissingProvenance {
                case_id: case.id().to_owned(),
            });
        }
        if case.assertion_count() == 0 {
            out.push(IntegrityViolation::CaseAssertsNothing {
                case_id: case.id().to_owned(),
            });
        }
        // A rejection that names no code is not reviewable, and the codes are
        // themselves part of the published contract.
        if let Case::Publish(p) = case {
            for a in &p.assert {
                if matches!(&a.expect, PublishVerdict::Rejected { error_code } if error_code.trim().is_empty())
                {
                    out.push(IntegrityViolation::RejectionWithoutCode {
                        case_id: p.id.clone(),
                    });
                }
            }
            // A decline suspends the case's evidence, so the slice that would
            // restore it has to be named.
            if p.declined_until
                .as_ref()
                .is_some_and(|slice| slice.trim().is_empty())
            {
                out.push(IntegrityViolation::DeclineWithoutSlice {
                    case_id: p.id.clone(),
                });
            }
        }
        if !corpus.families.iter().any(|f| f.family == case.family()) {
            let v = IntegrityViolation::UndeclaredFamily {
                family: case.family(),
            };
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }

    for meta in &corpus.families {
        match meta.role {
            GateRole::Publish if meta.gates.is_empty() => {
                out.push(IntegrityViolation::FamilyGatesNothing {
                    family: meta.family,
                });
            }
            GateRole::Conformance if !meta.gates.is_empty() => {
                out.push(IntegrityViolation::ConformanceFamilyGates {
                    family: meta.family,
                });
            }
            _ => {}
        }
        // A family that gates publish must map to a registry variant, or the
        // generator has no `(kind, variant)` key to write its rows under and the
        // gate can never ask about it -- gating, silently, nothing.
        if matches!(meta.role, GateRole::Publish) && meta.family.variant().is_none() {
            out.push(IntegrityViolation::GatingFamilyWithoutVariant {
                family: meta.family,
            });
        }
        for kind in &meta.gates {
            // **Coverage is an evaluation case, not any case at all.** `model_kind()`
            // alone admits a publish case, which contributes no oracle outcome and —
            // when it carries `declined_until` — by `check_publish_case_coverage`'s
            // own argument "cannot pass and cannot earn the flag either". A family
            // gating a kind on those alone would read as covered while the oracle
            // half of the gate is empty. Only `PublishCase` can be declined, so
            // excluding the variant covers both shapes; the publish half of coverage
            // stays `check_publish_case_coverage`'s.
            let covered = corpus
                .cases_for(meta.family)
                .any(|c| matches!(c, Case::Evaluation(_)) && c.model_kind() == *kind);
            if !covered {
                out.push(IntegrityViolation::GatedKindUncovered {
                    family: meta.family,
                    kind: *kind,
                });
            }
        }
    }

    out
}

#[cfg(test)]
#[path = "integrity_tests.rs"]
mod integrity_tests;
