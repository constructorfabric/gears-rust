//! `cluster-oop` - the cluster gear as a Profile 3 deployable
//! (DESIGN-DEPLOYABLE-GEAR §4.3, §12.8).
//!
//! # What this binary is
//!
//! A `clap` CLI over `toolkit::bootstrap::oop::run_oop_with_options`, and nothing
//! else. Everything a cluster pod needs at runtime - the Axum listener with
//! `/healthz`, `/readyz`, `/health` and `/openapi.json` served **as soon as the
//! listener binds**, before the gear's `start`; background self-registration with
//! backoff; dependency resolution; the presence loop; the drain sequence;
//! `DirectoryService` deregistration on shutdown - is supplied by that call and is
//! **not cluster's code to write** (§4.3).
//!
//! That absence is the point, and it is checkable: ADR-0005's confirmation step is
//! a code review asserting **no registration or dependency retry loop appears in a
//! gear's own code**. There is no `loop`, no `retry`, no backoff and no sleep in
//! this file or in `registered_gears.rs`, and the gear crate holds none either.
//! `tests/oop_bootstrap.rs` carries the same assertion mechanically over the
//! sources so a later edit cannot reintroduce one quietly.
//!
//! # Deployment profile
//!
//! The *binary* is what selects Profile 3; the gear does not know which profile it
//! is in and has no configuration that would tell it (invariant I9). A Profile 1
//! monolith links the same `cluster` library through its own
//! `registered_gears.rs` and never builds this target. The only thing this file
//! decides is the gear name the bootstrap registers under - `"cluster"`, matching
//! `#[toolkit::gear(name = "cluster")]`, which is the key both `ctx.config()` and
//! the operator's `gears.cluster.config` block are read by.
//!
//! # Configuration
//!
//! Resolved by the bootstrap, not here: `--config` if given, else the
//! `MODULE_CONFIG_PATH` environment variable (`OopRunOptions::default`), merged
//! under any `TOOLKIT_MODULE_CONFIG` rendered config a master host supplied. The
//! probes come up only when the config carries an `oop_http` section - without one
//! the bootstrap takes its legacy gRPC-only path and serves no HTTP at all.

mod registered_gears;

// Two flags, both consumed by the bootstrap rather than by cluster.
//
// `D3` adds `cluster-oop migrate` here as a `#[command(subcommand)]` - the one
// place this binary will ever have a branch, since a migration run must dedupe
// bindings by the `R3` instance key and exit rather than serve (DESIGN section
// 4.10.1).
//
// Help text is given as explicit `about` / `help` strings rather than as doc
// comments, and that is a workspace-lint constraint rather than a style choice.
// clap's derive turns doc comments into `--help` output *and* re-emits them as
// string literals, so a doc comment here has to satisfy `clippy::doc_markdown`
// (which wants `MODULE_CONFIG_PATH` in backticks) and `clippy::non_ascii_literal`
// (which forbids `§`) while also reading well to an operator - and backticks in a
// terminal help pane read as noise. Splitting the two audiences settles it: these
// `//` comments are for a reader of the source, the `help = "..."` strings are for
// the operator.
//
// `D3` adds `cluster-oop migrate` as a `#[command(subcommand)]` here - the one
// place this binary will ever have a branch, since a migration run must dedupe
// bindings by the `R3` instance key and exit rather than serve (DESIGN section
// 4.10.1).
#[derive(clap::Parser)]
#[command(
    name = "cluster-oop",
    about = "The cluster gear as an out-of-process service"
)]
struct Cli {
    #[arg(
        short,
        long,
        help = "Path to the configuration file. Falls back to MODULE_CONFIG_PATH"
    )]
    config: Option<std::path::PathBuf>,

    #[arg(
        short,
        long,
        action = clap::ArgAction::Count,
        help = "Log verbosity (-v debug, -vv trace)"
    )]
    verbose: u8,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use clap::Parser as _;
    use toolkit::bootstrap::oop::{OopRunOptions, run_oop_with_options};

    let cli = Cli::parse();

    run_oop_with_options(OopRunOptions {
        // Must equal `#[toolkit::gear(name = ...)]`: the bootstrap registers the
        // instance under this name and reads `gears.<name>.config` by it.
        gear_name: "cluster".to_owned(),
        verbose: cli.verbose,
        config_path: cli.config,
        // Every other field - instance id, directory endpoint, heartbeat
        // interval - is the platform's to default (§4.3). Naming any of them
        // here would be cluster-side client configuration, which invariant I9
        // forbids.
        ..Default::default()
    })
    .await
}
