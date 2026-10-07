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
//! The Docker half is guarded twice without a daemon: the harness's source may
//! execute no program but `ps` and must reach Docker through testcontainers' own
//! client, and a client that cannot reach any daemon must leave the liveness
//! question unanswered rather than judge a corpse. The positive half, that a
//! daemon which answers "no such container" is read as absent, needs a daemon,
//! so it is `#[ignore]`d like the rest of the tier that needs Docker.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod pg_support;

use std::process::Command;

use pg_support::{Named, owning_pid, process_is_running, prunable};
use testcontainers_modules::testcontainers::bollard::{self, Docker};

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

/// **The guard for the channel defect**, which no green run on a developer
/// machine can see.
///
/// The defect was never a wrong answer — it was a *second* way of asking. The
/// adoption, liveness and force-remove questions shelled out to a `docker` CLI
/// while the container was created through bollard, and where the two do not
/// resolve to one daemon they can never agree. In a downstream CI image there is
/// no `docker` binary at all: the first test process created the container and
/// every later one burned the whole boot budget on `409 Conflict`, ninety seconds
/// per test.
///
/// Every machine that runs this suite by hand has a `docker` on its path, which
/// is why no run this repository performs is able to fail on it. What *can* see
/// it is the text. The harness asks Docker exactly one way, through the client
/// `.start()` builds the container with, and a subprocess is the shape the
/// defect takes.
///
/// So the assertion is over the whole census rather than a denylist: `ps` is the
/// one program this harness may execute, which also refuses
/// `Command::new("/usr/local/bin/docker")` and every other spelling a denylist
/// would let through. `ps` is sanctioned because it is asked about processes and
/// not about Docker — see the prune guards above, which are what it serves.
#[test]
fn the_harness_executes_no_program_but_ps_and_reaches_docker_one_way() {
    let harness = include_str!("pg_support/mod.rs");

    let executed: Vec<&str> = harness
        .split("Command::new(")
        .skip(1)
        .map(|rest| rest.split(')').next().unwrap_or(rest).trim())
        .collect();
    assert_eq!(
        executed,
        vec!["\"ps\""],
        "the harness must execute nothing but `ps`: a `docker` subprocess is a \
         second channel, and two channels cannot be kept in agreement"
    );

    // The positive half: a refusal alone would pass just as well on a harness
    // that had stopped asking Docker anything.
    assert!(
        harness.contains("docker_client_instance()"),
        "the harness must reach Docker through testcontainers' own client"
    );
}

/// **A daemon that cannot be asked is never judged a corpse.**
///
/// The only caller of this answer force-removes the container, so the one value
/// it must never produce on an unanswered question is [`Named::Corpse`]: one
/// transient error under a dozen concurrent test processes would remove a
/// sibling's booting container, and every red after it would say nothing about
/// any schema.
///
/// Runnable with no Docker at all, because no daemon is involved: the client
/// points at a loopback port this test has just released, so the inspect fails
/// on the connection, which is exactly what an unreachable or wedged daemon looks
/// like to the harness.
#[tokio::test]
async fn a_daemon_that_cannot_be_asked_is_never_judged_a_corpse() {
    let silent = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("find a loopback port nothing listens on");
    let unreachable =
        Docker::connect_with_http(&format!("http://{silent}"), 4, bollard::API_DEFAULT_VERSION)
            .expect("build a client for an address nothing serves");

    assert_eq!(
        pg_support::inspect_named(&unreachable, pg_support::HARNESS_CONTAINER).await,
        Named::Unknown,
        "an unanswerable question must stay unanswered, never resolve to a corpse"
    );
}

/// The positive half, so the case above is not one that would pass with
/// `inspect_named` hard-wired to [`Named::Unknown`]: a daemon that answers "no
/// such container" has answered. The name is absent, which removes nothing — a
/// removal on it would race a sibling that starts the container under the name
/// first.
#[tokio::test]
#[ignore = "requires Docker: asks the daemon about a name it has never seen"]
async fn a_name_the_daemon_does_not_know_is_absent() {
    let docker = pg_support::daemon().await;
    let absent = format!(
        "{}-absent-{}",
        pg_support::HARNESS_CONTAINER,
        std::process::id()
    );

    assert_eq!(
        pg_support::inspect_named(&docker, &absent).await,
        Named::Absent,
        "a 404 from the daemon is an answer: there is nothing under that name"
    );
}
