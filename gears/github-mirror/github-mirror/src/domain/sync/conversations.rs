//! Logical-conversation grouping, ported from the reference implementation's
//! `entity-refinement::logical`.
//!
//! GitHub models only inline review-comment threads; top-level issue and
//! pull-request comments are a flat list. After a sync this pass derives one
//! layer over both: **inline** conversations, review comments chained by
//! `in_reply_to_id`, and **toplevel** conversations, where a comment that
//! blockquotes at least [`MIN_QUOTE_OVERLAP`] consecutive characters of an
//! earlier comment on the same issue or pull request counts as its reply.
//! Every member comment gets its root comment's id as `conversation_id`.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash as _, Hasher as _};

use chrono::{DateTime, Utc};
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repo::{
    ConversationRepository, LogicalConversation, ReviewCommentForGrouping, ReviewThreadForGrouping,
};

/// The least a blockquote must share, after normalisation, with an earlier
/// comment to be read as a reply to it.
pub const MIN_QUOTE_OVERLAP: usize = 16;
const GROUPING_BATCH: usize = 500;
pub const INLINE: &str = "inline";
pub const TOPLEVEL: &str = "toplevel";
const PULL_REQUEST: &str = "pull_request";
const ISSUE: &str = "issue";

#[domain_model]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConversationStats {
    pub inline: u64,
    pub toplevel: u64,
}

/// Derive and store the conversations of `repo_id`. `changed_since == None`
/// re-derives every issue and pull request; `Some(instant)` only those whose
/// comments a sync wrote at or after it, leaving the rest untouched.
///
/// # Errors
/// Storage failures.
pub async fn group_conversations(
    repo: &dyn ConversationRepository,
    scope: &AccessScope,
    tenant_id: Uuid,
    repo_id: i64,
    changed_since: Option<DateTime<Utc>>,
) -> Result<ConversationStats, DomainError> {
    let pull_numbers: HashSet<i64> = repo
        .pull_numbers(scope, repo_id)
        .await?
        .into_iter()
        .collect();
    let mut stats = ConversationStats::default();
    let pulls = parents_to_group(repo, scope, repo_id, INLINE, changed_since).await?;
    for batch in pulls.chunks(GROUPING_BATCH) {
        stats.inline += group_inline(repo, scope, tenant_id, repo_id, batch).await?;
    }
    let issues = parents_to_group(repo, scope, repo_id, TOPLEVEL, changed_since).await?;
    for batch in issues.chunks(GROUPING_BATCH) {
        stats.toplevel +=
            group_toplevel(repo, scope, tenant_id, repo_id, &pull_numbers, batch).await?;
    }
    Ok(stats)
}

async fn parents_to_group(
    repo: &dyn ConversationRepository,
    scope: &AccessScope,
    repo_id: i64,
    conv_type: &str,
    changed_since: Option<DateTime<Utc>>,
) -> Result<Vec<i64>, DomainError> {
    let mut parents = if conv_type == INLINE {
        repo.pulls_with_review_comments_since(scope, repo_id, changed_since)
            .await?
    } else {
        repo.issues_with_comments_since(scope, repo_id, changed_since)
            .await?
    };
    if changed_since.is_none() {
        parents.extend(repo.conversation_parents(scope, repo_id, conv_type).await?);
        parents.sort_unstable();
        parents.dedup();
    }
    Ok(parents)
}

async fn group_inline(
    repo: &dyn ConversationRepository,
    scope: &AccessScope,
    tenant_id: Uuid,
    repo_id: i64,
    pulls: &[i64],
) -> Result<u64, DomainError> {
    let comments = repo
        .review_comments_for_grouping(scope, repo_id, pulls)
        .await?;
    let by_id: HashMap<i64, &ReviewCommentForGrouping> =
        comments.iter().map(|c| (c.id, c)).collect();
    let root_of: HashMap<i64, i64> = comments
        .iter()
        .map(|c| (c.id, inline_root(c, &by_id)))
        .collect();

    let mut conversations: HashMap<i64, Conversation> = HashMap::new();
    for c in &comments {
        conversations
            .entry(root_of.get(&c.id).copied().unwrap_or(c.id))
            .or_insert_with(|| Conversation::new(PULL_REQUEST, c.pull_number))
            .observe(&c.created_at);
    }

    let mut threads_by_pull: HashMap<i64, Vec<ReviewThreadForGrouping>> = HashMap::new();
    for thread in repo
        .review_threads_for_grouping(scope, repo_id, pulls)
        .await?
    {
        threads_by_pull
            .entry(thread.pull_number)
            .or_default()
            .push(thread);
    }
    let mut resolved_by_root: HashMap<i64, bool> = HashMap::new();
    let pulls_in_scope: HashSet<i64> = conversations.values().map(|c| c.parent_number).collect();
    for pull in pulls_in_scope {
        let Some(threads) = threads_by_pull.get_mut(&pull) else {
            continue;
        };
        let mut roots: Vec<i64> = conversations
            .iter()
            .filter(|(_, c)| c.parent_number == pull)
            .map(|(root, _)| *root)
            .collect();
        roots.sort_by_key(|root| {
            (
                std::cmp::Reverse(conversations.get(root).map_or(0, |c| c.count)),
                *root,
            )
        });
        threads.sort_by(|a, b| {
            b.comments_count
                .cmp(&a.comments_count)
                .then_with(|| a.id.cmp(&b.id))
        });
        for (root, thread) in roots.iter().zip(threads.iter()) {
            resolved_by_root.insert(*root, thread.is_resolved);
        }
    }

    let existing = existing_by_root(repo, scope, repo_id, INLINE, pulls).await?;
    for (root, conversation) in &conversations {
        let record =
            conversation.record(repo_id, INLINE, *root, resolved_by_root.get(root).copied());
        if existing.get(root) != Some(&record) {
            repo.upsert(scope, tenant_id, record).await?;
        }
    }
    remove_stale(repo, scope, repo_id, INLINE, &existing, &conversations).await?;
    for c in &comments {
        let desired = root_of.get(&c.id).copied();
        if c.conversation_id != desired {
            repo.set_review_comment_conversation(scope, repo_id, c.id, desired)
                .await?;
        }
    }
    Ok(count(conversations.len()))
}

async fn group_toplevel(
    repo: &dyn ConversationRepository,
    scope: &AccessScope,
    tenant_id: Uuid,
    repo_id: i64,
    pull_numbers: &HashSet<i64>,
    issues: &[i64],
) -> Result<u64, DomainError> {
    let mut comments = repo.comments_for_grouping(scope, repo_id, issues).await?;
    comments.sort_by(|a, b| {
        a.issue_number
            .cmp(&b.issue_number)
            .then_with(|| github_time(&a.created_at).cmp(&github_time(&b.created_at)))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut root_of: HashMap<i64, i64> = HashMap::new();
    let mut conversations: HashMap<i64, Conversation> = HashMap::new();
    let mut current_issue = comments.first().map_or(0, |c| c.issue_number);
    let mut newest_with: HashMap<u64, (usize, i64)> = HashMap::new();

    for (position, c) in comments.iter().enumerate() {
        if c.issue_number != current_issue {
            current_issue = c.issue_number;
            newest_with.clear();
            tokio::task::yield_now().await;
        }
        let body = c.body.as_deref().unwrap_or("");
        let earlier = window_hashes(&normalize(&quoted_lines(body)))
            .into_iter()
            .filter_map(|window| newest_with.get(&window).copied())
            .max()
            .map(|(_, id)| id);
        let root = earlier.map_or(c.id, |earlier| {
            root_of.get(&earlier).copied().unwrap_or(earlier)
        });
        root_of.insert(c.id, root);
        let kind = if pull_numbers.contains(&c.issue_number) {
            PULL_REQUEST
        } else {
            ISSUE
        };
        conversations
            .entry(root)
            .or_insert_with(|| Conversation::new(kind, c.issue_number))
            .observe(&c.created_at);
        for window in window_hashes(&normalize(body)) {
            newest_with.insert(window, (position, c.id));
        }
    }

    let existing = existing_by_root(repo, scope, repo_id, TOPLEVEL, issues).await?;
    for (root, conversation) in &conversations {
        let record = conversation.record(repo_id, TOPLEVEL, *root, None);
        if existing.get(root) != Some(&record) {
            repo.upsert(scope, tenant_id, record).await?;
        }
    }
    remove_stale(repo, scope, repo_id, TOPLEVEL, &existing, &conversations).await?;
    for c in &comments {
        let desired = root_of.get(&c.id).copied();
        if c.conversation_id != desired {
            repo.set_comment_conversation(scope, repo_id, c.id, desired)
                .await?;
        }
    }
    Ok(count(conversations.len()))
}

async fn remove_stale(
    repo: &dyn ConversationRepository,
    scope: &AccessScope,
    repo_id: i64,
    conv_type: &str,
    existing: &HashMap<i64, LogicalConversation>,
    derived: &HashMap<i64, Conversation>,
) -> Result<(), DomainError> {
    let stale: Vec<i64> = existing
        .keys()
        .filter(|root| !derived.contains_key(root))
        .copied()
        .collect();
    if !stale.is_empty() {
        repo.delete(scope, repo_id, conv_type, &stale).await?;
    }
    Ok(())
}

async fn existing_by_root(
    repo: &dyn ConversationRepository,
    scope: &AccessScope,
    repo_id: i64,
    conv_type: &str,
    parents: &[i64],
) -> Result<HashMap<i64, LogicalConversation>, DomainError> {
    Ok(repo
        .conversations_of(scope, repo_id, conv_type, parents)
        .await?
        .into_iter()
        .map(|row| (row.root_comment_id, row))
        .collect())
}

struct Conversation {
    parent_kind: &'static str,
    parent_number: i64,
    count: i64,
    earliest: Option<(DateTime<Utc>, String)>,
}

impl Conversation {
    fn new(parent_kind: &'static str, parent_number: i64) -> Self {
        Self {
            parent_kind,
            parent_number,
            count: 0,
            earliest: None,
        }
    }

    fn observe(&mut self, created_at: &str) {
        self.count += 1;
        if let Some(at) = github_time(created_at)
            && self
                .earliest
                .as_ref()
                .is_none_or(|(earliest, _)| at < *earliest)
        {
            self.earliest = Some((at, created_at.to_owned()));
        }
    }

    fn record(
        &self,
        repo_id: i64,
        conv_type: &str,
        root_comment_id: i64,
        is_resolved: Option<bool>,
    ) -> LogicalConversation {
        LogicalConversation {
            repo_id,
            conv_type: conv_type.to_owned(),
            root_comment_id,
            parent_kind: self.parent_kind.to_owned(),
            parent_number: self.parent_number,
            comment_count: self.count,
            is_resolved,
            created_at: self.earliest.as_ref().map(|(_, text)| text.clone()),
        }
    }
}

fn count(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

fn github_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

/// The root of a reply chain, following `in_reply_to_id`; the comment itself
/// when it replies to nothing, the missing parent's id when that parent was
/// not mirrored, and wherever the walk stood when a cycle closed.
fn inline_root(
    start: &ReviewCommentForGrouping,
    by_id: &HashMap<i64, &ReviewCommentForGrouping>,
) -> i64 {
    let mut current = start;
    let mut seen = HashSet::new();
    while let Some(parent_id) = current.in_reply_to_id {
        if !seen.insert(current.id) {
            break;
        }
        match by_id.get(&parent_id) {
            Some(parent) => current = parent,
            None => return parent_id,
        }
    }
    current.id
}

/// The blockquoted text of a Markdown body: every line whose first
/// non-blank character is `>`, with the markers removed.
fn quoted_lines(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        if let Some(rest) = line.trim_start().strip_prefix('>') {
            out.push_str(rest.trim_start_matches('>').trim_start());
            out.push('\n');
        }
    }
    out
}

fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn window_hashes(text: &str) -> Vec<u64> {
    let chars: Vec<char> = text.chars().collect();
    chars
        .windows(MIN_QUOTE_OVERLAP)
        .map(|window| {
            let mut hasher = DefaultHasher::new();
            window.hash(&mut hasher);
            hasher.finish()
        })
        .collect()
}
