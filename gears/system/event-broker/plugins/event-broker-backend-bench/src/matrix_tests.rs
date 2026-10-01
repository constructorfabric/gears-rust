use crate::args::ScenarioId;
use crate::matrix::{CatchupCell, CellFilter, IngestCell, PipelineCell};

fn filter(patterns: &[&str]) -> CellFilter {
    CellFilter::new(patterns.iter().map(|p| (*p).to_owned()).collect())
}

#[test]
fn the_matrix_has_the_documented_cell_counts() {
    assert_eq!(IngestCell::all().len(), 8);
    assert_eq!(PipelineCell::all().len(), 16);
    assert_eq!(CatchupCell::all().len(), 4);
}

#[test]
fn cell_names_are_unique() {
    let mut names: Vec<String> = PipelineCell::all()
        .into_iter()
        .map(PipelineCell::name)
        .collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 16);
    assert_eq!(
        PipelineCell::SOAK.name(),
        "batch-16p16c-push:async-read:async"
    );
}

#[test]
fn star_admits_everything() {
    assert!(filter(&["*"]).admits(ScenarioId::B2, "single-1p1c-push:sync-read:sync"));
}

#[test]
fn patterns_match_the_qualified_name() {
    let f = filter(&["b2/batch-*-read:async"]);
    assert!(f.admits(ScenarioId::B2, "batch-16p16c-push:sync-read:async"));
    assert!(!f.admits(ScenarioId::B2, "batch-16p16c-push:sync-read:sync"));
    assert!(!f.admits(ScenarioId::B1, "batch-16p-push:sync"));
}

#[test]
fn any_pattern_admits() {
    let f = filter(&["b1/*", "*16c*"]);
    assert!(f.admits(ScenarioId::B1, "single-1p-push:sync"));
    assert!(f.admits(ScenarioId::B3, "16c-read:1"));
    assert!(!f.admits(ScenarioId::B3, "1c-read:1"));
}

#[test]
fn a_pattern_without_a_star_is_exact() {
    let f = filter(&["b3/1c-read:1"]);
    assert!(f.admits(ScenarioId::B3, "1c-read:1"));
    assert!(!f.admits(ScenarioId::B3, "1c-read:1000"));
}
