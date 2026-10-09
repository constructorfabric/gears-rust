//! Asks every configured source and merges what the caller can read.

use std::collections::HashMap;
use std::sync::Arc;

use bss_approvals_sdk::{
    ApprovalSourceV1, InboxUnit, SortKey, SourceCounts, SourceNarrowing, SourcePage,
    SourcePageQuery, VoteAction, VoteRequest, VoteResponse,
};
use toolkit::ClientHub;
use toolkit::client_hub::ClientScope;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::cursor;
use super::error;
use super::merge::{self, SourceAnswer};
use super::owner::{self, SourceGet};
use super::query::PreparedList;

/// Whether a source contributed to the answer.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceHealth {
    /// The source answered.
    Ok,
    /// The source refused the caller. Its units are not in the answer.
    Forbidden,
    /// The source did not answer, or the cursor already recorded it as down.
    Unavailable,
}

/// One source in a list or counts answer.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    /// The configured name.
    pub name: String,
    /// `ok`, `forbidden` or `unavailable`.
    pub status: SourceHealth,
}

/// One merged page.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// The page, in the asked order.
    pub units: Vec<InboxUnit>,
    /// Present when any source had more, or returned a unit this page did not take.
    pub next_cursor: Option<String>,
    /// Every source that was readable or forbidden, in config order.
    pub sources: Vec<SourceRow>,
}

/// The counts of the readable sources, and which sources those are.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Counted {
    /// Summed over the readable sources only.
    pub counts: SourceCounts,
    /// Every source that was readable or forbidden, in config order.
    pub sources: Vec<SourceRow>,
}

/// Reads one merged page.
///
/// # Errors
/// Every source is down or unregistered, every source forbids the caller, or a source refuses
/// the narrowing. A down source beside a source that answered is named `unavailable` and omitted.
/// The cursor errors are the caller's, from [`PreparedList`].
pub async fn list_page(
    hub: &ClientHub,
    names: &[String],
    ctx: &SecurityContext,
    prepared: &PreparedList,
) -> Result<Listed, CanonicalError> {
    let frozen: std::collections::BTreeSet<&str> =
        prepared.unavailable.iter().map(String::as_str).collect();
    let asked: Vec<&String> = names
        .iter()
        .filter(|name| !frozen.contains(name.as_str()))
        .collect();
    let pending = asked.iter().map(|name| {
        let after = prepared.keys.get(name.as_str()).copied().flatten();
        ask_page(hub, name, ctx, prepared, after)
    });
    let answered = futures::future::join_all(pending).await;
    let mut by_name: HashMap<&str, Result<Ask, CanonicalError>> = HashMap::new();
    for (name, result) in asked.into_iter().zip(answered) {
        by_name.insert(name.as_str(), result);
    }
    let mut pages = Vec::new();
    let mut sources = Vec::new();
    let mut down = Vec::new();
    let mut hard: Option<CanonicalError> = None;
    for name in names {
        if frozen.contains(name.as_str()) {
            sources.push(SourceRow {
                name: name.clone(),
                status: SourceHealth::Unavailable,
            });
            down.push(name.clone());
            continue;
        }
        match by_name
            .remove(name.as_str())
            .unwrap_or(Ok(Ask::Unavailable))
        {
            Ok(Ask::Ready(page)) => {
                sources.push(SourceRow {
                    name: name.clone(),
                    status: SourceHealth::Ok,
                });
                pages.push((name.clone(), page));
            }
            Ok(Ask::Forbidden) => sources.push(SourceRow {
                name: name.clone(),
                status: SourceHealth::Forbidden,
            }),
            Ok(Ask::Unavailable) => {
                sources.push(SourceRow {
                    name: name.clone(),
                    status: SourceHealth::Unavailable,
                });
                down.push(name.clone());
            }
            Err(err) => {
                if hard.is_none() {
                    hard = Some(err);
                }
            }
        }
    }
    if let Some(err) = hard {
        return Err(err);
    }
    if !down.is_empty() && pages.is_empty() && !any_forbidden(&sources) {
        return Err(error::source_unavailable(&down));
    }
    if pages.is_empty() && all_forbidden(&sources) {
        return Err(error::forbidden());
    }
    let views: Vec<SourceAnswer> = pages
        .into_iter()
        .map(|(name, page)| SourceAnswer {
            source: name,
            units: page.units,
            has_more: page.has_more,
        })
        .collect();
    let merged = merge::merge(prepared.order, prepared.limit, &prepared.keys, views);
    let unavailable: Vec<String> = sources
        .iter()
        .filter(|row| row.status == SourceHealth::Unavailable)
        .map(|row| row.name.clone())
        .collect();
    let next_cursor = if merged.has_more {
        Some(cursor::encode(
            prepared.order,
            &prepared.hash,
            &merged.keys,
            &unavailable,
        )?)
    } else {
        None
    };
    Ok(Listed {
        units: merged.units,
        next_cursor,
        sources,
    })
}

/// Sums the counts of the readable sources.
///
/// # Errors
/// Every source is down or unregistered, every source forbids the caller, or a source refuses
/// the narrowing. A down source beside a source that answered is named `unavailable` and left
/// out of the sum.
pub async fn count_all(
    hub: &ClientHub,
    names: &[String],
    ctx: &SecurityContext,
    narrowing: &SourceNarrowing,
) -> Result<Counted, CanonicalError> {
    let pending = names
        .iter()
        .map(|name| ask_counts(hub, name, ctx, narrowing));
    let answered = futures::future::join_all(pending).await;
    let mut counts = SourceCounts::default();
    let mut sources = Vec::new();
    let mut down = Vec::new();
    let mut hard: Option<CanonicalError> = None;
    for (name, result) in names.iter().zip(answered) {
        match result {
            Ok(AskCount::Ready(page)) => {
                counts.add(&page);
                sources.push(SourceRow {
                    name: name.clone(),
                    status: SourceHealth::Ok,
                });
            }
            Ok(AskCount::Forbidden) => sources.push(SourceRow {
                name: name.clone(),
                status: SourceHealth::Forbidden,
            }),
            Ok(AskCount::Unavailable) => {
                sources.push(SourceRow {
                    name: name.clone(),
                    status: SourceHealth::Unavailable,
                });
                down.push(name.clone());
            }
            Err(err) => {
                if hard.is_none() {
                    hard = Some(err);
                }
            }
        }
    }
    if let Some(err) = hard {
        return Err(err);
    }
    if !down.is_empty() && counts.total == 0 && !any_forbidden(&sources) && !any_ok(&sources) {
        return Err(error::source_unavailable(&down));
    }
    if all_forbidden(&sources) {
        return Err(error::forbidden());
    }
    Ok(Counted { counts, sources })
}

/// The card: one unit, or the owner-resolution refusal.
///
/// # Errors
/// See [`owner::require_one`].
pub async fn get_unit(
    hub: &ClientHub,
    names: &[String],
    ctx: &SecurityContext,
    id: Uuid,
    impact: bool,
) -> Result<InboxUnit, CanonicalError> {
    let resolved = resolve_owner(hub, names, ctx, id, impact).await?;
    owner::require_one(resolved).map(|(_, unit)| unit)
}

/// The owner, then that source's vote door. The answer is the door's own.
///
/// # Errors
/// The card's refusal, or the source's failure to call its door.
pub async fn vote_unit(
    hub: &ClientHub,
    names: &[String],
    ctx: &SecurityContext,
    id: Uuid,
    action: VoteAction,
    request: VoteRequest,
) -> Result<VoteResponse, CanonicalError> {
    let resolved = resolve_owner(hub, names, ctx, id, false).await?;
    let (owner, _) = owner::require_one(resolved)?;
    let Some(source) = lookup(hub, &owner) else {
        return Err(error::source_unavailable(std::slice::from_ref(&owner)));
    };
    source.vote(ctx, id, action, request).await
}

fn all_forbidden(sources: &[SourceRow]) -> bool {
    !sources.is_empty()
        && sources
            .iter()
            .all(|row| row.status == SourceHealth::Forbidden)
}

fn any_forbidden(sources: &[SourceRow]) -> bool {
    sources
        .iter()
        .any(|row| row.status == SourceHealth::Forbidden)
}

fn any_ok(sources: &[SourceRow]) -> bool {
    sources.iter().any(|row| row.status == SourceHealth::Ok)
}

fn is_forbidden(err: &CanonicalError) -> bool {
    matches!(err, CanonicalError::PermissionDenied { .. })
}

fn is_unavailable(err: &CanonicalError) -> bool {
    matches!(err, CanonicalError::ServiceUnavailable { .. })
}

async fn resolve_owner(
    hub: &ClientHub,
    names: &[String],
    ctx: &SecurityContext,
    id: Uuid,
    impact: bool,
) -> Result<owner::Resolved, CanonicalError> {
    let mut answers = Vec::with_capacity(names.len());
    for name in names {
        let answer = match lookup(hub, name) {
            None => {
                tracing::warn!(source = %name, "bss-approvals: source is not registered");
                SourceGet::Unavailable
            }
            Some(source) => classify_get(name, source.get(ctx, id, impact).await),
        };
        answers.push((name.clone(), answer));
    }
    Ok(owner::resolve(answers))
}

fn classify_get(name: &str, result: Result<Option<InboxUnit>, CanonicalError>) -> SourceGet {
    match result {
        Ok(Some(unit)) => SourceGet::Found(Box::new(unit)),
        Ok(None) => SourceGet::Absent,
        Err(err) if is_forbidden(&err) => {
            tracing::warn!(source = %name, error = %err, "bss-approvals: source forbidden");
            SourceGet::Forbidden
        }
        Err(err) if is_unavailable(&err) => {
            tracing::warn!(source = %name, error = %err, "bss-approvals: source unavailable");
            SourceGet::Unavailable
        }
        Err(err) => SourceGet::Failed(Box::new(err)),
    }
}

enum Ask {
    Ready(SourcePage),
    Forbidden,
    Unavailable,
}

#[expect(
    clippy::cognitive_complexity,
    reason = "each source answer is logged on the arm that classifies it"
)]
async fn ask_page(
    hub: &ClientHub,
    name: &str,
    ctx: &SecurityContext,
    prepared: &PreparedList,
    after: Option<SortKey>,
) -> Result<Ask, CanonicalError> {
    let Some(source) = lookup(hub, name) else {
        tracing::warn!(source = %name, "bss-approvals: source is not registered");
        return Ok(Ask::Unavailable);
    };
    let query = SourcePageQuery {
        narrowing: prepared.narrowing.clone(),
        order: prepared.order,
        limit: prepared.limit,
        after,
        impact: prepared.impact,
    };
    match source.page(ctx, &query).await {
        Ok(page) => Ok(Ask::Ready(page)),
        Err(err) if is_forbidden(&err) => {
            tracing::warn!(source = %name, error = %err, "bss-approvals: source forbidden");
            Ok(Ask::Forbidden)
        }
        Err(err) if is_unavailable(&err) => {
            tracing::warn!(source = %name, error = %err, "bss-approvals: source unavailable");
            Ok(Ask::Unavailable)
        }
        Err(err) => Err(err),
    }
}

enum AskCount {
    Ready(SourceCounts),
    Forbidden,
    Unavailable,
}

#[expect(
    clippy::cognitive_complexity,
    reason = "each source answer is logged on the arm that classifies it"
)]
async fn ask_counts(
    hub: &ClientHub,
    name: &str,
    ctx: &SecurityContext,
    narrowing: &SourceNarrowing,
) -> Result<AskCount, CanonicalError> {
    let Some(source) = lookup(hub, name) else {
        tracing::warn!(source = %name, "bss-approvals: source is not registered");
        return Ok(AskCount::Unavailable);
    };
    match source.counts(ctx, narrowing).await {
        Ok(counts) => Ok(AskCount::Ready(counts)),
        Err(err) if is_forbidden(&err) => {
            tracing::warn!(source = %name, error = %err, "bss-approvals: source forbidden");
            Ok(AskCount::Forbidden)
        }
        Err(err) if is_unavailable(&err) => {
            tracing::warn!(source = %name, error = %err, "bss-approvals: source unavailable");
            Ok(AskCount::Unavailable)
        }
        Err(err) => Err(err),
    }
}

/// The system actors the configured sources declare (AP-D-11). A source not registered now
/// declares none.
#[must_use]
pub fn system_actors(hub: &ClientHub, names: &[String]) -> Vec<Uuid> {
    names
        .iter()
        .filter_map(|name| lookup(hub, name))
        .flat_map(|source| source.system_actors().to_vec())
        .collect()
}

fn lookup(hub: &ClientHub, name: &str) -> Option<Arc<dyn ApprovalSourceV1>> {
    hub.try_get_scoped(&ClientScope::new(name))
}
