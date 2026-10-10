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

use chrono::{DateTime, Utc};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repo::{
    ConversationRepository, LogicalConversation, ReviewCommentForGrouping, ReviewThreadForGrouping,
};

/// The least a blockquote must share, after normalisation, with an earlier
/// comment to be read as a reply to it.
pub const MIN_QUOTE_OVERLAP: usize = 16;
pub const INLINE: &str = "inline";
pub const TOPLEVEL: &str = "toplevel";
const PULL_REQUEST: &str = "pull_request";
const ISSUE: &str = "issue";

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
    let inline = group_inline(repo, scope, tenant_id, repo_id, changed_since).await?;
    let toplevel = group_toplevel(
        repo,
        scope,
        tenant_id,
        repo_id,
        &pull_numbers,
        changed_since,
    )
    .await?;
    Ok(ConversationStats { inline, toplevel })
}

async fn group_inline(
    repo: &dyn ConversationRepository,
    scope: &AccessScope,
    tenant_id: Uuid,
    repo_id: i64,
    changed_since: Option<DateTime<Utc>>,
) -> Result<u64, DomainError> {
    let in_scope: Option<Vec<i64>> = match changed_since {
        None => None,
        Some(since) => {
            let pulls = repo
                .pulls_with_review_comments_since(scope, repo_id, since)
                .await?;
            if pulls.is_empty() {
                return Ok(0);
            }
            Some(pulls)
        }
    };

    let comments = repo
        .review_comments_for_grouping(scope, repo_id, in_scope.as_deref())
        .await?;
    if comments.is_empty() {
        return Ok(0);
    }
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
        .review_threads_for_grouping(scope, repo_id, in_scope.as_deref())
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

    let existing = existing_by_root(repo, scope, repo_id, INLINE, in_scope.as_deref()).await?;
    for (root, conversation) in &conversations {
        let record =
            conversation.record(repo_id, INLINE, *root, resolved_by_root.get(root).copied());
        if existing.get(root) != Some(&record) {
            repo.upsert(scope, tenant_id, record).await?;
        }
    }
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
    changed_since: Option<DateTime<Utc>>,
) -> Result<u64, DomainError> {
    let in_scope: Option<Vec<i64>> = match changed_since {
        None => None,
        Some(since) => {
            let issues = repo
                .issues_with_comments_since(scope, repo_id, since)
                .await?;
            if issues.is_empty() {
                return Ok(0);
            }
            Some(issues)
        }
    };

    let mut comments = repo
        .comments_for_grouping(scope, repo_id, in_scope.as_deref())
        .await?;
    comments.sort_by(|a, b| {
        a.issue_number
            .cmp(&b.issue_number)
            .then_with(|| github_time(&a.created_at).cmp(&github_time(&b.created_at)))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut root_of: HashMap<i64, i64> = HashMap::new();
    let mut conversations: HashMap<i64, Conversation> = HashMap::new();
    let Some(first) = comments.first() else {
        return Ok(0);
    };
    let mut current_issue = first.issue_number;
    let mut prior: Vec<(i64, String)> = Vec::new();

    for c in &comments {
        if c.issue_number != current_issue {
            current_issue = c.issue_number;
            prior.clear();
        }
        let body = c.body.as_deref().unwrap_or("");
        let normalized = normalize(body);
        let quoted = normalize(&quoted_lines(body));
        let root = if !quoted.is_empty()
            && let Some((earlier, _)) = prior
                .iter()
                .rev()
                .find(|(_, earlier_body)| overlaps(&quoted, earlier_body, MIN_QUOTE_OVERLAP))
        {
            root_of.get(earlier).copied().unwrap_or(*earlier)
        } else {
            c.id
        };
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
        prior.push((c.id, normalized));
    }

    let existing = existing_by_root(repo, scope, repo_id, TOPLEVEL, in_scope.as_deref()).await?;
    for (root, conversation) in &conversations {
        let record = conversation.record(repo_id, TOPLEVEL, *root, None);
        if existing.get(root) != Some(&record) {
            repo.upsert(scope, tenant_id, record).await?;
        }
    }
    for c in &comments {
        let desired = root_of.get(&c.id).copied();
        if c.conversation_id != desired {
            repo.set_comment_conversation(scope, repo_id, c.id, desired)
                .await?;
        }
    }
    Ok(count(conversations.len()))
}

async fn existing_by_root(
    repo: &dyn ConversationRepository,
    scope: &AccessScope,
    repo_id: i64,
    conv_type: &str,
    in_scope: Option<&[i64]>,
) -> Result<HashMap<i64, LogicalConversation>, DomainError> {
    let rows = match in_scope {
        None => repo.list(scope, repo_id, None).await?,
        Some(parents) => {
            let mut out = Vec::new();
            for &parent in parents {
                out.extend(repo.list(scope, repo_id, Some(parent)).await?);
            }
            out
        }
    };
    Ok(rows
        .into_iter()
        .filter(|row| row.conv_type == conv_type)
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

/// Whether `quoted` and `body`, both normalised, share a run of at least
/// `min` consecutive characters.
fn overlaps(quoted: &str, body: &str, min: usize) -> bool {
    let chars: Vec<char> = quoted.chars().collect();
    if chars.len() < min {
        return false;
    }
    chars.windows(min).any(|window| {
        let needle: String = window.iter().collect();
        body.contains(&needle)
    })
}
