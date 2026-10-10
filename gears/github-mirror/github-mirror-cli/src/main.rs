mod app;
mod registered_gears;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use authn_resolver_sdk::AuthNResolverClient;
use clap::{Parser, Subcommand};
use figment::Figment;
use figment::providers::Serialized;
use github_mirror::domain::ports::github::ForceMode;
use github_mirror::domain::service::{Service, SyncRequest};
use serde_json::Value;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use toolkit::bootstrap::AppConfig;
use toolkit::runtime::{DbOptions, HostRuntime};
use toolkit::{ClientHub, GearRegistry};
use toolkit_db::DbManager;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::app::commands::{self, Entity};
use crate::app::config::{self, DatabasePlacement, StorageFlags};
use crate::app::output::{self, OutputFormat};

#[derive(Parser)]
#[command(
    name = "github-mirror",
    version,
    about = "Mirror GitHub repositories into a local database and query them"
)]
struct Cli {
    #[arg(
        long,
        global = true,
        default_value = "./github-mirror.toml",
        help = "TOML configuration file"
    )]
    config: PathBuf,

    #[arg(
        long,
        global = true,
        env = "GITHUB_TOKEN",
        hide_env_values = true,
        help = "GitHub token; falls back to GITHUB_TOKEN, then ~/.github-mirror/gh_token.txt"
    )]
    token: Option<String>,

    #[arg(
        long,
        global = true,
        env = "CF_PLATFORM_TOKEN",
        hide_env_values = true,
        help = "Platform login token; falls back to CF_PLATFORM_TOKEN, then ~/.github-mirror/platform_token.txt"
    )]
    platform_token: Option<String>,

    #[arg(
        long,
        global = true,
        env = "GITHUB_MIRROR_STORAGE_DIR",
        value_name = "DIR",
        help = "Root folder for the databases and caches; replaces server.home_dir (default ~/.github-mirror)"
    )]
    storage_dir: Option<PathBuf>,

    #[arg(
        long,
        global = true,
        env = "GITHUB_MIRROR_DATABASE_URL",
        value_name = "URL",
        help = "Database URL (sqlite://, postgres:// or mysql://); replaces gears.github-mirror.database and makes --database-placement moot"
    )]
    database_url: Option<String>,

    #[arg(
        long,
        global = true,
        value_enum,
        value_name = "MODE",
        help = "Where the SQLite file goes: per_repo (default) one per repository, per_org one per owner, shared one for all"
    )]
    database_placement: Option<DatabasePlacement>,

    #[arg(short, long, global = true, action = clap::ArgAction::Count, help = "More log output (-v, -vv, -vvv)")]
    verbose: u8,

    #[arg(
        long,
        global = true,
        help = "Log filter, e.g. info or github_mirror=debug"
    )]
    log_level: Option<String>,

    #[arg(short, long, global = true, help = "Only errors in the log output")]
    quiet: bool,

    #[arg(
        long,
        global = true,
        value_enum,
        help = "table (default for sync and status) or json (default for query)"
    )]
    output_format: Option<OutputFormat>,

    #[arg(long, help = "Print the built-in configuration template and exit")]
    print_config: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Synchronize a repository")]
    Sync {
        #[arg(help = "ORG/REPO")]
        repo: String,
        #[arg(
            long,
            help = "Walk every listing and refine every entity, keeping the HTTP cache"
        )]
        force_full: bool,
        #[arg(long, help = "Like --force-full, and also bypass the HTTP cache")]
        force: bool,
        #[arg(long, value_name = "N", help = "Tasks in flight inside this sync")]
        max_concurrent: Option<std::num::NonZeroUsize>,
        #[arg(
            long,
            value_name = "CUTOFF",
            help = "Skip closed issues and pull requests older than this: YYYY-MM-DD, Nd, Nw or Nm"
        )]
        since: Option<String>,
        #[arg(
            long,
            value_name = "TYPES",
            help = "Collect only these object types, comma-separated: issues, pull_requests (prs, pulls), commits, releases, branches, labels, milestones, github_actions (actions, gha), contributors"
        )]
        include: Option<String>,
        #[arg(
            long,
            value_name = "TYPES",
            help = "Object types to leave out, comma-separated"
        )]
        exclude: Option<String>,
        #[arg(
            long,
            value_name = "MODE",
            help = "Workflow runs and CI checks: open, all or none"
        )]
        actions_scope: Option<String>,
        #[arg(long, value_name = "MODE", help = "Reactions: open, all or none")]
        reactions_scope: Option<String>,
        #[arg(long, value_name = "MODE", help = "Timeline events: open, all or none")]
        timeline_scope: Option<String>,
        #[arg(
            long,
            value_name = "N",
            help = "Code lines kept above each inline review comment, from its diff hunk: a count, or -1 for all"
        )]
        inline_comment_snippet_before: Option<i32>,
        #[arg(
            long,
            value_name = "N",
            help = "Code lines kept below each inline review comment: a count, or -1 for all"
        )]
        inline_comment_snippet_after: Option<i32>,
    },
    #[command(about = "Continue an interrupted synchronization")]
    Resume {
        #[arg(help = "ORG/REPO")]
        repo: String,
        #[arg(
            long,
            help = "Walk every listing and refine every entity, keeping the HTTP cache"
        )]
        force_full: bool,
        #[arg(long, help = "Like --force-full, and also bypass the HTTP cache")]
        force: bool,
    },
    #[command(about = "Read mirrored data from the local database")]
    Query {
        #[arg(value_enum)]
        entity: Entity,
        #[arg(help = "ORG/REPO")]
        repo: String,
        #[arg(
            long,
            help = "Issue or pull request number, for entities that belong to one"
        )]
        number: Option<i64>,
        #[arg(long, default_value_t = 30, help = "Most rows to return")]
        limit: u64,
        #[arg(
            long,
            value_name = "CUTOFF",
            help = "Only rows GitHub changed at or after this: YYYY-MM-DD, Nd, Nw or Nm"
        )]
        since: Option<String>,
        #[arg(
            long,
            value_name = "ISO8601",
            help = "Only rows a sync wrote at or after this instant, e.g. 2026-10-08T12:00:00Z"
        )]
        extracted_since: Option<String>,
        #[arg(
            long,
            value_name = "TYPE",
            help = "Review comments on a line or a file: line or file"
        )]
        subject_type: Option<String>,
    },
    #[command(about = "Show the synchronization status of a repository")]
    Status {
        #[arg(help = "ORG/REPO")]
        repo: String,
    },
    #[command(
        name = "check-rate-limit",
        about = "Show the GitHub token's remaining REST and GraphQL quotas"
    )]
    CheckRateLimit,
    #[command(
        name = "clear-cache",
        about = "Remove everything mirrored for a repository: its data, change-detection state and cached responses"
    )]
    ClearCache {
        #[arg(help = "ORG/REPO")]
        repo: String,
    },
}

impl Command {
    fn repo(&self) -> Option<&str> {
        match self {
            Self::Sync { repo, .. }
            | Self::Resume { repo, .. }
            | Self::Query { repo, .. }
            | Self::Status { repo }
            | Self::ClearCache { repo } => Some(repo),
            Self::CheckRateLimit => None,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(&cli);
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    if cli.print_config {
        print!("{}", config::DEFAULT_TEMPLATE);
        return Ok(());
    }
    let Some(command) = cli.command else {
        bail!("no command given; see --help");
    };

    let platform_token = config::resolve_token(cli.platform_token, "platform_token.txt").ok_or_else(|| {
        anyhow!(
            "no platform token: pass --platform-token, set CF_PLATFORM_TOKEN or write ~/.github-mirror/platform_token.txt"
        )
    })?;
    let github_token = config::resolve_token(cli.token, "gh_token.txt");
    let loaded = config::load(
        &cli.config,
        github_token.as_deref(),
        StorageFlags {
            storage_dir: cli.storage_dir,
            database_url: cli.database_url,
            database_placement: cli.database_placement,
            repo: command.repo(),
        },
    )?;
    let format = cli.output_format.unwrap_or(match command {
        Command::Query { .. } => OutputFormat::Json,
        Command::Sync { .. }
        | Command::Resume { .. }
        | Command::Status { .. }
        | Command::CheckRateLimit
        | Command::ClearCache { .. } => OutputFormat::Table,
    });

    let database_file = config::sqlite_file(&loaded.app);
    let runtime = Runtime::start(loaded.app)?;
    let outcome = execute(
        &runtime,
        command,
        &platform_token,
        loaded.tenant_id,
        database_file.as_deref(),
    )
    .await;
    let stopped = runtime.stop().await;
    let value = match (outcome, stopped) {
        (Ok(value), Ok(())) => value,
        (Err(e), Ok(())) | (_, Err(e)) => return Err(e),
    };
    output::print(&value, format)
}

async fn execute(
    runtime: &Runtime,
    command: Command,
    platform_token: &str,
    tenant_id: Option<Uuid>,
    database_file: Option<&Path>,
) -> Result<Value> {
    let service = runtime.service().await?;
    let ctx = runtime.authenticate(platform_token).await?;
    if let Some(expected) = tenant_id
        && expected != ctx.subject_tenant_id()
    {
        bail!(
            "cli.tenant_id is {expected}, but the platform token belongs to tenant {}",
            ctx.subject_tenant_id()
        );
    }
    match command {
        Command::Sync {
            repo,
            force_full,
            force,
            max_concurrent,
            since,
            include,
            exclude,
            actions_scope,
            reactions_scope,
            timeline_scope,
            inline_comment_snippet_before,
            inline_comment_snippet_after,
        } => {
            let request = commands::sync_request(
                &service,
                &commands::SyncFlags {
                    force: ForceMode::from_flags(force, force_full),
                    max_concurrent,
                    since: since.as_deref(),
                    include: include.as_deref(),
                    exclude: exclude.as_deref(),
                    actions_scope: actions_scope.as_deref(),
                    reactions_scope: reactions_scope.as_deref(),
                    timeline_scope: timeline_scope.as_deref(),
                    snippet_before: inline_comment_snippet_before,
                    snippet_after: inline_comment_snippet_after,
                },
            )?;
            commands::sync(&service, &ctx, &repo, request, database_file).await
        }
        Command::Resume {
            repo,
            force_full,
            force,
        } => {
            let request = SyncRequest {
                force: ForceMode::from_flags(force, force_full),
                ..SyncRequest::default()
            };
            commands::sync(&service, &ctx, &repo, request, database_file).await
        }
        Command::Query {
            entity,
            repo,
            number,
            limit,
            since,
            extracted_since,
            subject_type,
        } => {
            commands::query(
                &service,
                &ctx,
                entity,
                &repo,
                number,
                limit,
                &commands::QueryFilters {
                    since: since.as_deref(),
                    extracted_since: extracted_since.as_deref(),
                    subject_type: subject_type.as_deref(),
                },
            )
            .await
        }
        Command::Status { repo } => commands::status(&service, &ctx, &repo, database_file).await,
        Command::ClearCache { repo } => commands::clear_cache(&service, &ctx, &repo).await,
        Command::CheckRateLimit => commands::check_rate_limit(&service, &ctx).await,
    }
}

fn init_logging(cli: &Cli) {
    let level = cli.log_level.clone().unwrap_or_else(|| {
        let preset = match (cli.quiet, cli.verbose) {
            (true, _) => "error",
            (false, 0) => "warn",
            (false, 1) => "info",
            (false, 2) => "debug",
            (false, _) => "trace",
        };
        preset.to_owned()
    });
    let filter = tracing_subscriber::EnvFilter::try_new(&level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_EVERY: Duration = Duration::from_millis(50);

struct Runtime {
    hub: Arc<ClientHub>,
    cancel: CancellationToken,
    phases: JoinHandle<Result<()>>,
}

impl Runtime {
    fn start(config: AppConfig) -> Result<Self> {
        toolkit::bootstrap::init_crypto_provider()?;
        let db = database(&config)?;
        let hub = Arc::new(ClientHub::default());
        let cancel = CancellationToken::new();
        let host = HostRuntime::new(
            GearRegistry::discover_and_build()?,
            Arc::new(config),
            db,
            Arc::clone(&hub),
            cancel.clone(),
            Uuid::new_v4(),
            None,
        );
        let phases = tokio::spawn(host.run_gear_phases());
        Ok(Self {
            hub,
            cancel,
            phases,
        })
    }

    async fn authenticate(&self, platform_token: &str) -> Result<SecurityContext> {
        let authn = self
            .wait_for(ClientHub::try_get::<dyn AuthNResolverClient>)
            .await?;
        let result = authn
            .authenticate(platform_token)
            .await
            .map_err(|e| anyhow!("the platform token was refused: {e}"))?;
        Ok(result.security_context)
    }

    async fn service(&self) -> Result<Arc<Service>> {
        self.wait_for(|hub| hub.try_get::<Service>().filter(|service| service.started()))
            .await
    }

    async fn stop(self) -> Result<()> {
        self.cancel.cancel();
        self.phases
            .await
            .context("the gear runtime task failed")?
            .context("the gear runtime failed")
    }

    async fn wait_for<T>(&self, find: impl Fn(&ClientHub) -> Option<Arc<T>>) -> Result<Arc<T>>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
        loop {
            if let Some(found) = find(&self.hub) {
                return Ok(found);
            }
            if self.phases.is_finished() {
                bail!("the gear runtime stopped before it was ready");
            }
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "the gear runtime was not ready after {} seconds",
                    STARTUP_TIMEOUT.as_secs()
                );
            }
            tokio::time::sleep(POLL_EVERY).await;
        }
    }
}

fn database(config: &AppConfig) -> Result<DbOptions> {
    if config.database.is_none() {
        return Ok(DbOptions::None);
    }
    let figment = Figment::new().merge(Serialized::defaults(config));
    let manager = DbManager::from_figment(figment, config.server.home_dir.clone())?;
    Ok(DbOptions::Manager(Arc::new(manager)))
}
