//! The shared-Postgres harness's own guards, executed.
//!
//! **No Docker and no server**, deliberately. What is asserted here is the
//! prune's *decision* — a pure question about a database name and a process id —
//! and standing a container up to ask it would make the harness's only test
//! depend on the harness it is testing. `#[ignore]` is therefore absent too:
//! these run in the ordinary suite, which is where a guard about not destroying
//! a concurrent run's data belongs.
//!
//! Why the guard exists at all is in `pg_support`'s module doc: the previous
//! rule — skip whatever has a live connection — was disproven by inspection, and
//! two concurrent `cargo test` invocations could drop each other's databases
//! mid-run.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod pg_support;

use std::process::Command;

use pg_support::{owning_pid, process_is_running, prunable};

/// Only a name this harness minted names a run, and only a named run can be
/// judged finished.
///
/// The counter half is parsed and discarded on purpose: `t_12_x` is somebody
/// else's database that happens to start the way ours do, and reading it as pid
/// 12's would be a guess.
#[test]
fn a_name_this_harness_did_not_mint_is_never_prunable() {
    assert_eq!(owning_pid("t_4321_0"), Some(4321));
    assert_eq!(owning_pid("t_4321_17"), Some(4321));

    for foreign in [
        "postgres",
        "template1",
        "t_",
        "t_4321",
        "t_4321_x",
        "t_x_0",
        "tenant_4321_0",
    ] {
        assert_eq!(owning_pid(foreign), None, "{foreign} was read as a run's");
        assert!(!prunable(foreign), "{foreign} would have been dropped");
    }
}

/// **The guard.** A database whose run is still going is left alone; one whose
/// run has ended is the leak the prune exists to clear.
///
/// The live run is a real second process rather than this one, because this
/// process is the case the old rule accidentally got right — a stand-in child
/// makes the question "is that pid alive" rather than "is that pid mine". Both
/// directions are one assertion pair over the **same** name, so the difference
/// between them is only that the process ended.
#[test]
fn a_running_runs_database_is_left_alone_and_a_finished_ones_is_not() {
    // Where the liveness question cannot be answered about a process this test
    // knows is alive - its own - there is no "finished run" half to assert: the
    // guard answers *keep* for everything, which is the whole fail-safe. Assert
    // that, rather than skip, so the host still proves something. Windows is the
    // live case: Git-Bash's `ps` answers about MSYS pids, so a Win32 pid is
    // nobody, and both halves below would be measuring the namespace mismatch
    // instead of the guard.
    if process_is_running(std::process::id()) != Some(true) {
        assert!(
            !prunable(&format!("t_{}_0", std::process::id())),
            "the liveness question is unanswerable here, so nothing may be dropped"
        );
        return;
    }

    let mut stand_in = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn a stand-in for a concurrent run");
    let name = format!("t_{}_0", stand_in.id());

    let while_running = prunable(&name);
    stand_in.kill().expect("stop the stand-in");
    // Reaped, or `ps` still lists it as a zombie and the second half of this
    // test would be asserting the wrong thing.
    stand_in.wait().expect("reap the stand-in");

    assert!(
        !while_running,
        "{name} would have been dropped out from under a run that was still going"
    );
    assert!(
        prunable(&name),
        "{name} outlived its run and nothing would ever clear it"
    );

    // And this run's own, which need no special case: this process is running.
    assert!(!prunable(&format!("t_{}_0", std::process::id())));
}
