use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Days, NaiveDate, TimeZone, Utc};
use github_mirror::api::rest::dto::{
    BranchDto, CommentDto, CommitDto, ContributorDto, IssueDto, IssueReactionDto,
    IssueTimelineEventDto, LabelDto, MilestoneDto, PullRequestDto, ReleaseDto, RepoDto,
    RepoSyncStatusDto, ReviewCommentDto, ReviewDto, ReviewThreadDto, SyncSessionDto,
    SyncSummaryDto, WorkflowRunDto,
};
use github_mirror::domain::ports::github::ForceMode;
use github_mirror::domain::repo::{
    ListingFilter, PageWindow, RepoSyncStatusRecord, SyncSessionRecord,
};
use github_mirror::domain::scope::{CollectionMode, SyncScope};
use github_mirror::domain::service::{Service, SyncRequest};
use serde::Serialize;
use serde_json::{Map, Value, json};
use toolkit_odata::{CursorV1, ODataQuery};
use toolkit_security::SecurityContext;

const PAGE_LIMIT: u64 = 100;

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Entity {
    Issues,
    Prs,
    Commits,
    Contributors,
    Repos,
    Comments,
    Conversations,
    ReviewThreads,
    Reviews,
    ReviewComments,
    Reactions,
    Timeline,
    Branches,
    Labels,
    Milestones,
    Releases,
    #[value(name = "workflow_runs")]
    WorkflowRuns,
}

pub struct SyncFlags<'a> {
    pub force: ForceMode,
    pub max_concurrent: Option<NonZeroUsize>,
    pub since: Option<&'a str>,
    pub include: Option<&'a str>,
    pub exclude: Option<&'a str>,
    pub actions_scope: Option<&'a str>,
    pub reactions_scope: Option<&'a str>,
    pub timeline_scope: Option<&'a str>,
    pub snippet_before: Option<i32>,
    pub snippet_after: Option<i32>,
}

pub fn sync_request(service: &Service, flags: &SyncFlags<'_>) -> Result<SyncRequest> {
    let narrows = flags.include.is_some()
        || flags.exclude.is_some()
        || flags.actions_scope.is_some()
        || flags.reactions_scope.is_some()
        || flags.timeline_scope.is_some()
        || flags.snippet_before.is_some()
        || flags.snippet_after.is_some();
    let scope = if narrows {
        let mut scope = service.default_scope();
        if let Some(include) = flags.include {
            scope.objects = SyncScope::parse_list(include)?;
        }
        if let Some(exclude) = flags.exclude {
            scope.objects = scope.objects.without(SyncScope::parse_list(exclude)?);
        }
        if let Some(mode) = flags.actions_scope {
            scope.collection.actions = CollectionMode::parse(mode)?;
        }
        if let Some(mode) = flags.reactions_scope {
            scope.collection.reactions = CollectionMode::parse(mode)?;
        }
        if let Some(mode) = flags.timeline_scope {
            scope.collection.timeline = CollectionMode::parse(mode)?;
        }
        let snippets = &mut scope.collection.inline_comment_snippets;
        if let Some(before) = flags.snippet_before {
            snippets.before = before;
        }
        if let Some(after) = flags.snippet_after {
            snippets.after = after;
        }
        snippets.validate()?;
        Some(scope)
    } else {
        None
    };
    Ok(SyncRequest {
        scope,
        force: flags.force,
        since: flags.since.map(parse_since).transpose()?,
        max_concurrent_tasks: flags.max_concurrent,
    })
}

fn parse_since(raw: &str) -> Result<DateTime<Utc>> {
    let value = raw.trim();
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return date
            .and_hms_opt(0, 0, 0)
            .map(|at| Utc.from_utc_datetime(&at))
            .ok_or_else(|| anyhow!("`{value}` is not a date"));
    }
    let (number, unit) = value.split_at(value.len().saturating_sub(1));
    let amount: u64 = number
        .parse()
        .ok()
        .filter(|amount| *amount > 0)
        .ok_or_else(|| anyhow!("`{value}` is not YYYY-MM-DD, Nd, Nw or Nm"))?;
    let days = match unit {
        "d" => amount,
        "w" => amount.saturating_mul(7),
        "m" => amount.saturating_mul(30),
        _ => bail!("`{value}` is not YYYY-MM-DD, Nd, Nw or Nm"),
    };
    Utc::now()
        .checked_sub_days(Days::new(days))
        .ok_or_else(|| anyhow!("`{value}` reaches back too far"))
}

fn parse_instant(raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim())
        .map(|at| at.with_timezone(&Utc))
        .map_err(|e| {
            anyhow!("`{raw}` is not an ISO 8601 instant such as 2026-10-08T12:00:00Z: {e}")
        })
}

pub async fn sync(
    service: &Service,
    ctx: &SecurityContext,
    repo: &str,
    request: SyncRequest,
    database_file: Option<&Path>,
) -> Result<Value> {
    let (owner, name) = split_repo(repo)?;
    let summary = SyncSummaryDto::from(service.sync_now(ctx, owner, name, None, request).await?);
    report(service, ctx, owner, name, Some(&summary), database_file).await
}

pub struct QueryFilters<'a> {
    pub since: Option<&'a str>,
    pub extracted_since: Option<&'a str>,
    pub subject_type: Option<&'a str>,
}

pub async fn query(
    service: &Service,
    ctx: &SecurityContext,
    entity: Entity,
    repo: &str,
    number: Option<i64>,
    limit: u64,
    filters: &QueryFilters<'_>,
) -> Result<Value> {
    let (owner, name) = split_repo(repo)?;
    let updated_since = filters.since.map(parse_since).transpose()?;
    let extracted_since = filters.extracted_since.map(parse_instant).transpose()?;
    if filters.subject_type.is_some() && !matches!(entity, Entity::ReviewComments) {
        bail!("--subject-type applies to review-comments only");
    }
    if (updated_since.is_some() || extracted_since.is_some())
        && matches!(entity, Entity::Repos | Entity::Conversations)
    {
        bail!("--since and --extracted-since do not apply to repos or conversations");
    }
    let window = PageWindow::bounded(limit, 0)?
        .with_updated_since(updated_since)
        .with_extracted_since(extracted_since);
    let filter = ListingFilter::default();
    let mut rows = match entity {
        Entity::Issues => rows(
            service
                .list_issues(ctx, owner, name, window, filter)
                .await?
                .0
                .items,
            IssueDto::from,
        )?,
        Entity::Prs => rows(
            service
                .list_pull_requests(ctx, owner, name, window, filter)
                .await?
                .0
                .items,
            PullRequestDto::from,
        )?,
        Entity::Commits => rows(
            service
                .list_commits(ctx, owner, name, window, None)
                .await?
                .0
                .items,
            CommitDto::from,
        )?,
        Entity::Contributors => rows(
            service
                .list_contributors(ctx, owner, name, window)
                .await?
                .items,
            ContributorDto::from,
        )?,
        Entity::Repos => vec![serde_json::to_value(RepoDto::from(
            service.get_repo(ctx, owner, name).await?,
        ))?],
        Entity::Branches => rows(
            service.list_branches(ctx, owner, name, window).await?.items,
            BranchDto::from,
        )?,
        Entity::Labels => rows(
            service.list_labels(ctx, owner, name, window).await?.items,
            LabelDto::from,
        )?,
        Entity::Milestones => rows(
            service
                .list_milestones(ctx, owner, name, window)
                .await?
                .items,
            MilestoneDto::from,
        )?,
        Entity::Releases => rows(
            service.list_releases(ctx, owner, name, window).await?.items,
            ReleaseDto::from,
        )?,
        Entity::WorkflowRuns => rows(
            service
                .list_workflow_runs(ctx, owner, name, window)
                .await?
                .0
                .items,
            WorkflowRunDto::from,
        )?,
        Entity::Conversations => conversations(service, ctx, (owner, name), number, limit).await?,
        Entity::Comments
        | Entity::ReviewThreads
        | Entity::Reviews
        | Entity::ReviewComments
        | Entity::Reactions
        | Entity::Timeline => {
            let number = number.ok_or_else(|| {
                anyhow!("this entity belongs to one issue or pull request: pass --number <N>")
            })?;
            query_one(service, ctx, entity, (owner, name), number, window).await?
        }
    };
    if let Some(subject_type) = filters.subject_type {
        rows.retain(|row| row.get("subject_type").and_then(Value::as_str) == Some(subject_type));
    }
    Ok(Value::Array(rows))
}

async fn query_one(
    service: &Service,
    ctx: &SecurityContext,
    entity: Entity,
    (owner, name): (&str, &str),
    number: i64,
    window: PageWindow,
) -> Result<Vec<Value>> {
    match entity {
        Entity::Comments => rows(
            service
                .list_comments(ctx, owner, name, number, window)
                .await?
                .items,
            CommentDto::from,
        ),
        Entity::ReviewThreads => rows(
            service
                .list_review_threads(
                    ctx,
                    owner,
                    name,
                    number,
                    &ODataQuery::new().with_limit(window.limit()),
                    window.extracted_since(),
                )
                .await?
                .items,
            ReviewThreadDto::from,
        ),
        Entity::Reviews => rows(
            service
                .list_reviews(ctx, owner, name, number, window)
                .await?
                .items,
            ReviewDto::from,
        ),
        Entity::ReviewComments => rows(
            service
                .list_review_comments(ctx, owner, name, number, window)
                .await?
                .items,
            ReviewCommentDto::from,
        ),
        Entity::Reactions => rows(
            service
                .list_issue_reactions(ctx, owner, name, number, window)
                .await?
                .items,
            IssueReactionDto::from,
        ),
        Entity::Timeline => rows(
            service
                .list_issue_timeline(ctx, owner, name, number, window)
                .await?
                .items,
            IssueTimelineEventDto::from,
        ),
        Entity::Issues
        | Entity::Prs
        | Entity::Commits
        | Entity::Contributors
        | Entity::Repos
        | Entity::Branches
        | Entity::Labels
        | Entity::Milestones
        | Entity::Releases
        | Entity::WorkflowRuns
        | Entity::Conversations => Ok(Vec::new()),
    }
}

async fn conversations(
    service: &Service,
    ctx: &SecurityContext,
    (owner, name): (&str, &str),
    number: Option<i64>,
    limit: u64,
) -> Result<Vec<Value>> {
    let mut parents: BTreeMap<(String, i64), Vec<Value>> = BTreeMap::new();
    let found = service.list_conversations(ctx, owner, name, number).await?;
    for conversation in found
        .into_iter()
        .take(usize::try_from(limit).unwrap_or(usize::MAX))
    {
        let members = service.conversation_comments(ctx, &conversation).await?;
        let comments = if members.review_comments.is_empty() {
            rows(members.comments, CommentDto::from)?
        } else {
            rows(members.review_comments, ReviewCommentDto::from)?
        };
        parents
            .entry((conversation.parent_kind, conversation.parent_number))
            .or_default()
            .push(json!({
                "conv_type": conversation.conv_type,
                "root_comment_id": conversation.root_comment_id,
                "comment_count": conversation.comment_count,
                "is_resolved": conversation.is_resolved,
                "created_at": conversation.created_at,
                "comments": comments,
            }));
    }
    Ok(parents
        .into_iter()
        .map(|((parent_kind, parent_number), conversations)| {
            json!({
                "parent_kind": parent_kind,
                "parent_number": parent_number,
                "conversations": conversations,
            })
        })
        .collect())
}

pub async fn check_rate_limit(service: &Service, ctx: &SecurityContext) -> Result<Value> {
    let quotas = service.rate_limit(ctx).await?;
    let now = chrono::Utc::now();
    let rows: Vec<Value> = quotas
        .iter()
        .map(|quota| {
            json!({
                "resource": quota.resource,
                "limit": quota.limit,
                "used": quota.used,
                "remaining": quota.remaining,
                "reset_at": quota.reset_at.map(|at| at.to_rfc3339()),
                "reset_in_seconds": quota.reset_at.map(|at| (at - now).num_seconds().max(0)),
            })
        })
        .collect();
    Ok(Value::Array(rows))
}

pub async fn clear_cache(service: &Service, ctx: &SecurityContext, repo: &str) -> Result<Value> {
    let (owner, name) = split_repo(repo)?;
    let removed = service.delete_repository(ctx, owner, name).await?;
    Ok(json!({ "repository": format!("{owner}/{name}"), "rows_removed": removed }))
}

pub async fn status(
    service: &Service,
    ctx: &SecurityContext,
    repo: &str,
    database_file: Option<&Path>,
) -> Result<Value> {
    let (owner, name) = split_repo(repo)?;
    report(service, ctx, owner, name, None, database_file).await
}

async fn report(
    service: &Service,
    ctx: &SecurityContext,
    owner: &str,
    name: &str,
    summary: Option<&SyncSummaryDto>,
    database_file: Option<&Path>,
) -> Result<Value> {
    let full_name = format!("{owner}/{name}");
    let run = run_status(service, ctx, &full_name)
        .await?
        .map(RepoSyncStatusDto::from);
    let session = latest_session(service, ctx, &full_name)
        .await?
        .map(SyncSessionDto::from);
    let summary = summary.or(session.as_ref().and_then(|s| s.summary.as_ref()));
    let storage = json!({
        "cache_bytes": service.cache_size(ctx, owner, name).await?,
        "database_bytes": database_file.map(database_bytes).transpose()?,
    });
    sections(&full_name, run.as_ref(), session.as_ref(), summary, storage)
}

fn sections(
    repository: &str,
    run: Option<&RepoSyncStatusDto>,
    session: Option<&SyncSessionDto>,
    summary: Option<&SyncSummaryDto>,
    storage: Value,
) -> Result<Value> {
    let repository = json!({
        "repository": repository,
        "session_id": session.map(|s| s.id.clone()),
        "status": session.map(|s| serde_json::to_value(s.status)).transpose()?,
        "progress_percent": session.map(|s| s.progress_percent),
        "started_at": session.and_then(|s| s.started_at.clone()),
        "ended_at": session.and_then(|s| s.ended_at.clone()),
        "duration_ms": session.and_then(|s| s.duration_ms),
        "error": session.and_then(|s| s.error.clone()),
    });
    let mut objects = Map::new();
    if let Some(summary) = summary
        && let Value::Object(fields) = serde_json::to_value(summary)?
    {
        for (key, value) in fields {
            if let Some(object) = key.strip_suffix("_synced") {
                objects.insert(object.to_owned(), value);
            } else if key == "stale_rows_deleted" || key == "accepted_drift_total" {
                objects.insert(key, value);
            }
        }
    }
    let api = session
        .and_then(|s| s.telemetry.as_ref())
        .map(serde_json::to_value)
        .transpose()?
        .unwrap_or_else(|| json!({}));
    let run = run
        .map(serde_json::to_value)
        .transpose()?
        .unwrap_or_else(|| json!({}));
    let mut report = Map::new();
    report.insert("repository".to_owned(), repository);
    report.insert("run".to_owned(), run);
    report.insert("objects".to_owned(), Value::Object(objects));
    report.insert("api".to_owned(), api);
    report.insert("storage".to_owned(), storage);
    Ok(Value::Object(report))
}

fn database_bytes(path: &Path) -> Result<u64> {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    Ok(file_size(path)? + file_size(Path::new(&wal))?)
}

fn file_size(path: &Path) -> Result<u64> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(meta.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

async fn run_status(
    service: &Service,
    ctx: &SecurityContext,
    full_name: &str,
) -> Result<Option<RepoSyncStatusRecord>> {
    let mut query = ODataQuery::new().with_limit(PAGE_LIMIT);
    loop {
        let page = service.list_repo_sync_status(ctx, &query, None).await?;
        if let Some(found) = page
            .items
            .into_iter()
            .find(|row| row.repo_full_name.eq_ignore_ascii_case(full_name))
        {
            return Ok(Some(found));
        }
        let Some(next) = page.page_info.next_cursor else {
            return Ok(None);
        };
        query = next_page(&next)?;
    }
}

async fn latest_session(
    service: &Service,
    ctx: &SecurityContext,
    full_name: &str,
) -> Result<Option<SyncSessionRecord>> {
    let mut query = ODataQuery::new().with_limit(PAGE_LIMIT);
    loop {
        let page = service.list_sessions(ctx, &query).await?;
        if let Some(found) = page
            .items
            .into_iter()
            .find(|row| row.repo_full_name.eq_ignore_ascii_case(full_name))
        {
            return Ok(Some(found));
        }
        let Some(next) = page.page_info.next_cursor else {
            return Ok(None);
        };
        query = next_page(&next)?;
    }
}

fn next_page(cursor: &str) -> Result<ODataQuery> {
    let cursor = CursorV1::decode(cursor)
        .map_err(|e| anyhow!("the next-page cursor did not decode: {e}"))?;
    Ok(ODataQuery::new().with_limit(PAGE_LIMIT).with_cursor(cursor))
}

fn rows<T, D: Serialize>(items: Vec<T>, to_dto: impl Fn(T) -> D) -> Result<Vec<Value>> {
    items
        .into_iter()
        .map(|item| serde_json::to_value(to_dto(item)).map_err(Into::into))
        .collect()
}

fn split_repo(repo: &str) -> Result<(&str, &str)> {
    repo.split_once('/')
        .filter(|(owner, name)| !owner.is_empty() && !name.is_empty() && !name.contains('/'))
        .ok_or_else(|| anyhow!("`{repo}` is not ORG/REPO"))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a panic in these tests is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn a_date_or_a_relative_span_becomes_a_cutoff() {
        assert_eq!(
            parse_since("2026-01-02").unwrap(),
            Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap()
        );
        for (span, days) in [("3d", 3), ("2w", 14), ("1m", 30)] {
            let cutoff = parse_since(span).unwrap();
            assert_eq!((Utc::now() - cutoff).num_days(), days, "{span}");
        }
        for bad in ["", "0d", "x", "3y", "2026-13-01"] {
            assert!(parse_since(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_instant_must_be_iso_8601() {
        assert_eq!(
            parse_instant("2026-10-08T12:00:00Z").unwrap(),
            Utc.with_ymd_and_hms(2026, 10, 8, 12, 0, 0).unwrap()
        );
        assert!(parse_instant("2026-10-08").is_err());
    }

    #[test]
    fn a_repository_argument_must_be_owner_slash_name() {
        assert_eq!(split_repo("dtolnay/itoa").unwrap(), ("dtolnay", "itoa"));
        for bad in ["itoa", "/itoa", "dtolnay/", "a/b/c"] {
            assert!(split_repo(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_report_always_has_its_five_sections() {
        let report = sections("dtolnay/itoa", None, None, None, json!({"cache_bytes": 0})).unwrap();
        let keys: Vec<&String> = report.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["api", "objects", "repository", "run", "storage"]);
        assert_eq!(report["repository"]["repository"], "dtolnay/itoa");
        assert_eq!(report["run"], json!({}));
        assert_eq!(report["objects"], json!({}));
        assert_eq!(report["storage"]["cache_bytes"], 0);
    }
}
