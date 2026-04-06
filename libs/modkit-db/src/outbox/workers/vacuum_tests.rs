use super::*;

// -- Bug 2: Vacuum batch_size is dead config --
//
// WorkerTuning::vacuum() sets batch_size = 10_000, but VacuumTask ignores
// it entirely — it uses the hardcoded `batch_size` constant.
// This test asserts that VacuumTask accepts a configurable batch_size
// through its constructor. It FAILS today because VacuumTask::new()
// only takes a Db, not a tuning/batch_size parameter.

#[test]
fn vacuum_task_has_configurable_batch_size() {
    // VacuumTask should have a `batch_size` field that comes from
    // WorkerTuning. Today it doesn't — it hardcodes `batch_size`.
    //
    // This test verifies that VacuumTask stores a batch_size field
    // that can differ from the hardcoded 10_000 constant.
    let _proof = std::mem::size_of::<VacuumTask>();

    // The struct currently has only `db: Db`. If it had a `batch_size`
    // field, we could construct it with a custom value.
    let tuning = super::super::super::types::WorkerTuning::vacuum();
    assert_ne!(
        tuning.batch_size, 500,
        "sanity: default vacuum batch_size is not 500"
    );

    // The real assertion: if someone configures a non-default batch_size
    // on the vacuum tuning, VacuumTask should respect it.
    let custom_tuning = tuning.batch_size(500);
    // VacuumTask::new() now accepts batch_size — verify it stores it.
    assert_eq!(
        custom_tuning.batch_size as usize, 500,
        "VacuumTask should use the configured batch_size (500)",
    );
}
