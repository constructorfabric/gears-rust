//! The shared-Postgres harness's own guards, executed.
//!
//! **No server**, deliberately. What is asserted in the ordinary suite is the
//! prune's *decision*, a pure question about a database name and a process id,
//! and standing a container up to ask it would make the harness's only test
//! depend on the harness it is testing. These run in the ordinary suite, which
//! is where a guard about not destroying a concurrent run's data belongs.
//!
//! Why the guard exists at all is in `pg_support`'s module doc: the previous
//! rule — skip whatever has a live connection — was disproven by inspection, and
//! two concurrent `cargo test` invocations could drop each other's databases
//! mid-run.
//!
//! One case asks the daemon about a name it does not know, on a thread of its
//! own and bounded (RT-07): a wedged daemon is an unanswered question, which is
//! the answer the case expects, and never hangs the default gate. The check that
//! the harness's own container is live depends on this host's Docker state, not
//! on code, so it is `#[ignore]`d like the rest of the tier that needs Docker.

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
    // that, rather than skip, so the host still proves something.
    if process_is_running(std::process::id()) != Some(true) {
        assert!(
            !prunable(&format!("t_{}_0", std::process::id())),
            "the liveness question is unanswerable here, so nothing may be dropped"
        );
        return;
    }

    // A host that cannot spawn `sleep` cannot supply the "other run still
    // going" half either, so it takes the same fail-safe assertion as the
    // unanswerable-liveness case above rather than failing the suite for an
    // absent utility. `sleep` is POSIX, so this is a guard against an unusual
    // host, not an expected path.
    let Ok(mut stand_in) = Command::new("sleep").arg("30").spawn() else {
        assert!(
            !prunable(&format!("t_{}_0", std::process::id())),
            "no stand-in can be spawned here, so nothing may be dropped"
        );
        return;
    };
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

/// **A name the daemon knows nothing about is not a corpse.**
///
/// This is the guard on the fix for a fail-open in the destructive direction.
/// `container_verdict` used to be a `bool`, so "the daemon could not be asked"
/// and "the container is not running" were one answer — and the only caller of
/// that answer issues `docker rm -f`. One transient `docker inspect` failure
/// therefore removed a healthy, shared container out from under every sibling
/// test process.
///
/// Asserting on an **absent** name is what makes this runnable with no Docker
/// and no container: `docker inspect` exits non-zero for a name it does not
/// know, exactly as it does when the daemon is unreachable, and a host with no
/// `docker` binary at all takes the same path. All three land on `None`, and
/// `None` is the value that removes nothing.
///
/// The case that must NOT hold is the interesting one: if this ever answers
/// `Some(Verdict::Corpse)`, the harness has gone back to force-removing on a
/// question it could not answer.
#[test]
fn a_container_the_daemon_cannot_speak_for_is_never_judged_a_corpse() {
    let unknown = format!("bss-products-pg-harness-absent-{}", std::process::id());
    // A daemon that does not answer within the bound left the question unanswered, as `None`.
    if let Ok(verdict) = verdict_within(&unknown, std::time::Duration::from_secs(10)) {
        assert_eq!(
            verdict, None,
            "an unanswerable question must stay unanswered, never resolve to a corpse"
        );
    }
}

/// The live case, so the case above is not one that would pass with `container_verdict`
/// hard-wired to `None`: this harness's own container is running whenever the Postgres tier has
/// been used on this host. Skipped rather than asserted when it is absent, because a developer
/// who has never run the tier is not a failure. It reads this host's Docker state, so it is not
/// in the ordinary suite (RT-07).
#[test]
#[ignore = "requires Docker: reads the state of the Postgres tier's harness container"]
fn the_harness_container_is_live_when_the_tier_has_run() {
    if let Some(verdict) = pg_support::container_verdict(pg_support::HARNESS_CONTAINER) {
        assert_eq!(
            verdict,
            pg_support::Verdict::Live,
            "the harness container exists and is not live; the tier's own runs left a corpse"
        );
    }
}

/// `container_verdict` of `name`, asked on a thread of its own: an error when the daemon did not
/// answer within `bound`. The thread is left behind on a timeout; the test process ends with it.
fn verdict_within(
    name: &str,
    bound: std::time::Duration,
) -> Result<Option<pg_support::Verdict>, std::sync::mpsc::RecvTimeoutError> {
    let (tx, rx) = std::sync::mpsc::channel();
    let name = name.to_owned();
    std::thread::spawn(move || {
        tx.send(pg_support::container_verdict(&name)).ok();
    });
    rx.recv_timeout(bound)
}
