//! Regenerates the joint corpus's `registry.toml`.
//!
//! Run: `cargo run -p cf-gears-bss-pricing --example regen_registry`
//!
//! **An example target, not a binary of either crate, and that is the design.**
//! The registry records two independently earned halves: `oracle`, which only
//! `bss-fixtures-conformance` can run, and `publish`, which only this gear's
//! validator can run. Neither crate may take the other as a normal dependency —
//! the corpus's invariant is that no evaluator reaches a gear even transitively,
//! which is why the harness is a dev-dependency here. An example compiles with
//! dev-dependencies, so this is the one build in which both halves are visible,
//! and therefore the one place that can write the file.
//!
//! The freshness assertion lives beside it in `tests/corpus_publish.rs`. There
//! is exactly **one** authority on the file's content: two crates each asserting
//! a different expectation is how a generated file starts flapping.
//!
//! Commit a regeneration on its own.

mod validator;

use bss_fixtures::Corpus;
use bss_fixtures_conformance::registry_gen;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Corpus::corpus_root();
    let corpus = Corpus::load(&root)?;

    let report = validator::publish_report(&corpus);
    let earned = report.earned_kinds();

    std::fs::write(
        root.join("registry.toml"),
        registry_gen::render_for(&corpus, &earned)?,
    )?;

    // The unearned half is the interesting output, so it goes to the operator
    // rather than only into a `false` in the file. A case the gear answers
    // differently from the corpus is a disagreement between two independent
    // readings of one design set, and it is resolved by a human deciding which
    // side is wrong -- never by adjusting either to match the other.
    for outcome in report.failures() {
        eprintln!(
            "publish case `{}` assertion {}: corpus expects {}, gear answers {}",
            outcome.case_id,
            outcome.index,
            validator::describe_verdict(&outcome.expected),
            validator::describe_answer(&outcome.actual),
        );
    }

    // Suspended coverage is reported too, and separately. A case the corpus
    // itself declares unanswerable is not a disagreement -- but it is absent
    // evidence, and absent evidence that nobody prints is absent evidence
    // nobody notices.
    for outcome in report.declined() {
        eprintln!(
            "publish case `{}` is declined until {}: {}",
            outcome.case_id,
            outcome.declined_until.as_deref().unwrap_or("?"),
            validator::describe_answer(&outcome.actual),
        );
    }
    for outcome in report.stale_declines() {
        eprintln!(
            "publish case `{}` is marked declined until {} and was answered anyway: \
             the declaration is stale and must come out of the case file",
            outcome.case_id,
            outcome.declined_until.as_deref().unwrap_or("?"),
        );
    }

    Ok(())
}
