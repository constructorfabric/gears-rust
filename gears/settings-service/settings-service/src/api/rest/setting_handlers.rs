//! Handlers for the administrative read surface over effective values.

use std::sync::Arc;

use axum::extract::{Path, Query};
use axum::http::header;
use axum::{Extension, Json};
use serde::Deserialize;
use settings_service_sdk::SettingKey;
use toolkit::api::canonical_prelude::*;
use toolkit::api::odata::OData;
use toolkit_odata::{CursorV1, ODataQuery, PageInfo};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::api::authz::{self, resource};
use crate::api::rest::browse_narrowing::{
    browse_binding, candidate_batch, page_size, value_part_matches,
};
use crate::api::rest::page_dto::{PageDto, PageInfoDto};
use crate::api::rest::setting_dto::{
    AuditRecordDto, EffectiveValueDto, Reader, ReviewItemDto, SettingItemDto, is_shown_unmasked,
    render, render_flagged, render_record,
};
use crate::api::rest::setting_filter::interpret;
pub(crate) use crate::api::rest::setting_filter::unsupported;
use crate::domain::category::visibility;
use crate::domain::declaration::{BrowseNarrowing, Declaration};
use crate::domain::error::DomainError;
use crate::domain::resolution::{EffectiveValue, SUBTREE_BUDGET, ScopeTarget, subtree_too_large};
use crate::domain::search::{Corpus, Needle};
use crate::domain::value::{ReviewRequest, review_binding};
use crate::field;
use crate::gear::ConcreteResolver;
use crate::log_text::LogSafe;

const READ: &str = "read";

/// The entitlement that unmasks `pii` values on the administrative surface.
const READ_UNMASKED: &str = "read_unmasked";

/// The optional `tenant` query parameter: the target scope as a tenant id.
#[derive(Debug, Default, Deserialize)]
pub struct TenantParam {
    /// The target tenant's id, a UUID. Omitted, the caller's own tenant — for
    /// a platform administrator the root, and therefore platform scope.
    #[serde(default)]
    pub tenant: Option<String>,
}

/// The query parameters of the browse: the target tenant and the optional text
/// that narrows the page.
#[derive(Debug, Default, Deserialize)]
pub struct BrowseParams {
    /// The target tenant's id, a UUID, as [`TenantParam::tenant`].
    #[serde(default)]
    pub tenant: Option<String>,
    /// Free text narrowing the page; trimmed, 2..200 characters.
    #[serde(default)]
    pub q: Option<String>,
}

/// Parses the optional `tenant` query parameter; absent or empty means "not given".
pub(crate) fn parse_tenant(raw: Option<&str>) -> Result<Option<Uuid>, DomainError> {
    match raw {
        None | Some("") => Ok(None),
        Some(raw) => Uuid::parse_str(raw)
            .map(Some)
            .map_err(|_| DomainError::Validation {
                field: "tenant".to_owned(),
                code: field::TENANT_PARAM,
                message: format!("`{raw}` is not a tenant id"),
            }),
    }
}

fn parse_key(raw: &str) -> Result<SettingKey, DomainError> {
    SettingKey::parse(raw).map_err(|e| DomainError::Validation {
        field: "key".to_owned(),
        code: field::VALIDATION,
        message: e.to_string(),
    })
}

/// Maps a failure to obtain a database connection to an internal domain error.
pub(crate) fn conn_error(err: &toolkit_db::DbError) -> DomainError {
    DomainError::Internal {
        diagnostic: err.to_string(),
    }
}

/// The target of an administrative read, gated to the caller's subtree.
///
/// The requested tenant, or the caller's own; it must be the caller itself or
/// a descendant that is not standalone. Anything else is a denial: an
/// administrator above a standalone tenant may not read its values, and a
/// sibling or an ancestor is not the caller's to read at all.
pub(crate) async fn gate_target(
    resolver: &ConcreteResolver,
    ctx: &SecurityContext,
    requested: Option<Uuid>,
) -> Result<ScopeTarget, DomainError> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-1
    let root = resolver.root_tenant().await?;
    let caller = ctx.subject_tenant_id();
    let target = requested.unwrap_or(caller);
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-1
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-4
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-4
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-2
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-3
    if target != caller {
        let hierarchy = resolver.hierarchy();
        let within = hierarchy.is_within_subtree(caller, target).await?;
        if !within || hierarchy.is_standalone(target).await? {
            return Err(DomainError::Unauthorized {
                resource: settings_service_sdk::gts::VALUE_SCHEMA,
            });
        }
    }
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-3
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-2
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-4
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-4
    Ok(if target == root {
        ScopeTarget::Platform
    } else {
        ScopeTarget::Tenant(target)
    })
}

/// The target's chain, root first, for the upward disclosure boundary to read
/// what lies above the reader tenant off. A platform administrator has nothing
/// above it, so its rendering needs no chain and none is asked for.
///
/// # Errors
/// [`DomainError`] when a tenant reader's chain cannot be read.
pub(crate) async fn target_chain(
    resolver: &ConcreteResolver,
    reader: Reader,
    target: Uuid,
) -> Result<Vec<Uuid>, DomainError> {
    match reader {
        Reader::Platform => Ok(Vec::new()),
        Reader::Tenant(_) => resolver.chain_of(target).await,
    }
}

/// Whether the caller may see `pii` values unmasked: a separate decision on
/// the value resource, asked only when the page carries something it
/// governs — a `pii` value, or a setter identity, which is an
/// administrator's and so PII as the audit actor is.
pub(crate) async fn may_read_pii(
    enforcer: &authz_resolver_sdk::PolicyEnforcer,
    ctx: &SecurityContext,
) -> bool {
    authz::access_scope(enforcer, ctx, &resource::VALUE, READ_UNMASKED, None)
        .await
        .is_ok()
}

// @cpt-dod:cpt-cf-settings-service-dod-value-resolution-rest-read-surface:p1
/// `GET /settings-service/v1/settings/{key}?tenant={tenant_id}`
///
/// # Errors
/// 400 on a malformed key or `tenant`; 403 when the caller may not read
/// values or the target is outside its subtree or standalone; 404 when no
/// declaration exists at the key or it is outside the caller's administrative
/// domain; 410 when the declaration is retired; 503 when a dependency of the
/// walk cannot answer.
pub async fn get_setting(
    Extension(ctx): Extension<SecurityContext>,
    Extension(resolver): Extension<Arc<ConcreteResolver>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(key): Path<String>,
    Query(params): Query<TenantParam>,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-1
    let key = parse_key(&key)?;
    let requested = parse_tenant(params.tenant.as_deref())?;
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-1
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-2
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-3
    // A denied or unobtainable decision is a denial: `access_scope` maps both
    // to the unauthorized outcome, which answers 403.
    let scope = authz::access_scope(&enforcer, &ctx, &resource::VALUE, READ, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-3
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-2
    let target = gate_target(&resolver, &ctx, requested).await?;
    let conn = db.conn().map_err(|e| conn_error(&e))?;
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-6
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-7
    // The resolver answers not-found, the distinct retired outcome, or the
    // value — cache first, with the trail recorded on the way.
    let effective = resolver.resolve(&conn, &key, target).await?;
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-7
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-6
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-5
    // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-3
    // Absent, never forbidden: a declaration outside the caller's
    // administrative domain, and a setting whose effective access for the
    // target tenant is `hidden`, both answer 404 so existence is not disclosed.
    // For a `global` setting this is the visibility rule that gates whether a
    // tenant is served the platform value at all.
    // Whose access decides is the caller's: an administrator above a tenant it
    // restricted still sees the setting; the restricted tenant does not.
    let caller =
        ScopeTarget::Tenant(ctx.subject_tenant_id()).normalize(resolver.root_tenant().await?);
    let hidden = resolver
        .effective_access(&conn, effective.declaration_id, caller)
        .await?
        .is_hidden();
    if hidden
        || !visibility::is_visible(
            &visibility::domain_visibility(&scope),
            effective.domain_affinity.as_deref(),
        )
    {
        return Err(DomainError::NotFound {
            resource: "declaration",
        }
        .into());
    }
    // @cpt-end:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-3
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-5
    let governed = effective.data_classification == "pii"
        || effective.trail.iter().any(|entry| entry.set_by.is_some());
    let pii = governed && may_read_pii(&enforcer, &ctx).await;
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-13
    // The reader is the caller's own tenant; what lies above it is collapsed
    // and each identity is shown only where it belongs, per request, never in
    // the cache.
    let reader = Reader::new(ctx.subject_tenant_id(), resolver.root_tenant().await?);
    // The target's chain: what is above the reader tenant is read off it.
    let chain = target_chain(&resolver, reader, effective.tenant_id).await?;
    let dto: EffectiveValueDto = render(&effective, pii, reader, &chain);
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-13
    let etag = dto.etag.clone();
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-9
    Ok(([(header::ETAG, super::etag_header(&etag))], Json(dto)))
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-9
}

/// An entry with the diagnostic of an internal failure, for the caller to log
/// when the entry is served.
type BrowseEntry = (SettingItemDto, Option<String>);

/// One browse entry from a declaration and the outcome of resolving it; see
/// [`BrowseEntry`].
fn browse_entry(
    declaration: &Declaration,
    outcome: Result<Arc<EffectiveValue>, DomainError>,
    pii: bool,
    reader: Reader,
    chain: &[Uuid],
) -> BrowseEntry {
    match outcome {
        Ok(effective) => (
            // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-16
            SettingItemDto::resolved(render(&effective, pii, reader, chain))
                // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-16
                .with_mode(&declaration.mode)
                .with_display_name(declaration.display_name.as_deref()),
            None,
        ),
        Err(err) => (
            SettingItemDto::failed(&declaration.key, &err)
                .with_mode(&declaration.mode)
                .with_display_name(declaration.display_name.as_deref()),
            err.internal_diagnostic().map(str::to_owned),
        ),
    }
}

/// Log the internal failure of an entry that is on the page.
fn log_failure(key: &str, diagnostic: Option<&str>) {
    if let Some(diagnostic) = diagnostic {
        tracing::error!(
            key = %key,
            diagnostic = %LogSafe(diagnostic),
            "a browse entry failed internally"
        );
    }
}

/// Encode a cursor, an encoding failure being the service's own.
fn mint(cursor: &CursorV1) -> Result<String, DomainError> {
    cursor.encode().map_err(|err| DomainError::Internal {
        diagnostic: format!("a browse cursor failed to encode: {err}"),
    })
}

/// A refusal of a cursor, reported against the `cursor` field.
fn cursor_refused(message: &str) -> DomainError {
    DomainError::Validation {
        field: "cursor".to_owned(),
        code: field::ODATA_QUERY,
        message: message.to_owned(),
    }
}

/// What a narrowed browse resolves at, decided before its first batch.
struct NarrowedBrowse<'a> {
    scope: &'a AccessScope,
    caller_chain: &'a [Uuid],
    flagged_at: Option<Uuid>,
    target: ScopeTarget,
    needle: &'a Needle,
    /// Whether the caller reads `pii` unmasked: decided once, so the corpus of
    /// the declaration text and the rendering of every row agree.
    pii: bool,
    /// Who the page is rendered for.
    reader: Reader,
    /// The target's chain, root first: what the reader tenant is above.
    chain: &'a [Uuid],
    postgres: bool,
}

/// A row the narrowing kept, with its place in the listing.
struct KeptRow {
    item: SettingItemDto,
    diagnostic: Option<String>,
    position: CursorV1,
}

/// Whether a candidate qualifies, and if so its entry.
///
/// By its declaration text, or by the value or fallback the caller receives
/// unmasked. A `retired` candidate qualifies by its text alone. A candidate
/// whose value could not be read because a dependency is unavailable, and
/// whose value the caller would have seen unmasked, cannot be decided: the
/// whole request is refused with that failure rather than paged through. One
/// that fails on its own keeps its `error` entry under the same condition, so
/// a defect is never hidden behind the narrowing; a value the caller could
/// never match leaves the decision to the text.
///
/// # Errors
/// The `Unavailable` failure of an undecidable candidate.
fn qualify(
    declaration: &Declaration,
    text_matched: bool,
    outcome: Result<Arc<EffectiveValue>, DomainError>,
    needle: &Needle,
    pii: bool,
    reader: Reader,
    chain: &[Uuid],
) -> Result<Option<BrowseEntry>, DomainError> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-11
    let readable = is_shown_unmasked(&declaration.data_classification, pii);
    match outcome {
        Ok(effective) => {
            // Rendered once: the entry is this rendering, kept or not.
            let dto = render(&effective, pii, reader, chain);
            let qualifies =
                text_matched || value_part_matches(&dto, effective.secret_backed, needle);
            Ok(qualifies.then(|| {
                (
                    SettingItemDto::resolved(dto)
                        .with_mode(&declaration.mode)
                        .with_display_name(declaration.display_name.as_deref()),
                    None,
                )
            }))
        }
        Err(err @ DomainError::Unavailable { .. }) if !text_matched && readable => Err(err),
        Err(err) => {
            let keep = text_matched || (readable && matches!(err, DomainError::Internal { .. }));
            Ok(keep.then(|| browse_entry(declaration, Err(err), pii, reader, chain)))
        }
    }
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-11
}

/// Assemble one page under the text narrowing: read candidate batches wider
/// than the page, resolve each in one pass, keep the ones that qualify, and go
/// on until the page is full — and one qualifying row beyond it is found, to
/// know whether a cursor is owed — or the candidates run out.
async fn narrowed_page<C: toolkit_db::secure::DBRunner>(
    resolver: &ConcreteResolver,
    conn: &C,
    base: &ODataQuery,
    ctx: &NarrowedBrowse<'_>,
) -> Result<(Vec<SettingItemDto>, PageInfo), DomainError> {
    let limit = page_size(base.limit);
    let wanted = usize::try_from(limit).unwrap_or(usize::MAX);
    let backward = base.cursor.as_ref().is_some_and(|c| c.d == "bwd");
    let had_cursor = base.cursor.is_some();
    let narrowing = BrowseNarrowing {
        needle: ctx.needle,
        corpus: Corpus::for_caller(ctx.pii),
        postgres: ctx.postgres,
    };
    let mut cursor = base.cursor.clone();
    // In the order walked: ascending, or descending under a backward cursor.
    let mut kept: Vec<KeptRow> = Vec::new();
    let mut more = false;
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-12
    'rounds: loop {
        let batch_query = ODataQuery {
            limit: Some(candidate_batch(limit)),
            cursor: cursor.clone(),
            ..base.clone()
        };
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-12-fetch
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-12-grant
        // The grant is not decided again here: it is the secure scope pushed
        // into this batch query, as the narrowed-grant step states, so the
        // batch holds only what the caller may read and the two refills are
        // one loop.
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-6
        let batch = resolver
            .list_candidates(
                conn,
                ctx.scope,
                ctx.caller_chain,
                ctx.flagged_at,
                &batch_query,
                &narrowing,
            )
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-6
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-12-grant
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-12-fetch
        let continuation = if backward {
            batch.page_info.prev_cursor.clone()
        } else {
            batch.page_info.next_cursor.clone()
        };
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-12-match
        let (declarations, marks): (Vec<Declaration>, Vec<(bool, CursorV1)>) = batch
            .items
            .into_iter()
            .map(|c| (c.declaration, (c.text_matched, c.position)))
            .unzip();
        let outcomes = resolver
            .resolve_declarations(conn, &declarations, ctx.target)
            .await;
        let mut rows: Vec<_> = declarations.iter().zip(marks).zip(outcomes).collect();
        if backward {
            rows.reverse();
        }
        for ((declaration, (text_matched, position)), outcome) in rows {
            let qualified = match qualify(
                declaration,
                text_matched,
                outcome,
                ctx.needle,
                ctx.pii,
                ctx.reader,
                ctx.chain,
            ) {
                Ok(qualified) => qualified,
                // The page is full and this row is only looked at to know
                // whether a cursor is owed: one that may qualify owes it, and
                // the next page request decides it.
                Err(DomainError::Unavailable { .. }) if kept.len() == wanted => {
                    more = true;
                    break 'rounds;
                }
                Err(err) => return Err(err),
            };
            let Some((item, diagnostic)) = qualified else {
                continue;
            };
            if kept.len() == wanted {
                more = true;
                break 'rounds;
            }
            kept.push(KeptRow {
                item,
                diagnostic,
                position,
            });
        }
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-12-match
        let Some(token) = continuation else {
            break;
        };
        cursor = Some(
            CursorV1::decode(&token).map_err(|_| cursor_refused("the cursor does not decode"))?,
        );
    }
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-12
    if backward {
        kept.reverse();
    }
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-15
    // After the last row kept, the cursor is bound to the trimmed `q` beside
    // the filter (the batch positions carry the binding); none once the
    // candidates have run out, an empty page included. A walk backward
    // mirrors the toolkit: it always owes the cursor after its last row, and
    // the one before its first only while earlier candidates remain. Only the
    // two rows a cursor is minted from are encoded.
    let after = kept.last().map(|row| mint(&row.position)).transpose()?;
    let before = kept
        .first()
        .map(|row| {
            mint(&CursorV1 {
                d: "bwd".to_owned(),
                ..row.position.clone()
            })
        })
        .transpose()?;
    let page_info = if backward {
        PageInfo {
            next_cursor: after,
            prev_cursor: if more { before } else { None },
            limit,
        }
    } else {
        PageInfo {
            next_cursor: if more { after } else { None },
            prev_cursor: if had_cursor { before } else { None },
            limit,
        }
    };
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-15
    let items = kept
        .into_iter()
        .map(|row| {
            log_failure(&row.item.key, row.diagnostic.as_deref());
            row.item
        })
        .collect();
    Ok((items, page_info))
}

/// `GET /settings-service/v1/settings?tenant={tenant_id}` with `OData`
/// `$filter`, `$orderby` and cursor pagination, and an optional text `q` that
/// narrows the page.
///
/// # Errors
/// 400 on a malformed `tenant`, an unmapped field or an unsupported operator,
/// a `q` outside 2..200 characters after trimming, or a cursor minted under
/// another `$filter`, `q` or target; 403 when the caller may not read values
/// or the target is outside its subtree or standalone; 503 when a dependency
/// cannot answer, and under `q` when a dependency is unavailable for a setting
/// whose value the caller could have matched — the narrowing cannot be
/// decided; a page already full still answers, with a cursor. A key that
/// fails on its own is reported in its entry, never as a request failure —
/// without `q`, or a failure the narrowing does not depend on.
#[allow(clippy::too_many_lines)]
pub async fn browse_settings(
    Extension(ctx): Extension<SecurityContext>,
    Extension(resolver): Extension<Arc<ConcreteResolver>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Query(params): Query<BrowseParams>,
    OData(query): OData,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-1
    let requested = parse_tenant(params.tenant.as_deref())?;
    crate::domain::odata::reject_unsupported_options(&query, "settings")?;
    crate::domain::odata::reject_unsortable::<crate::domain::odata::SettingOrderField>(
        &query, "settings",
    )?;
    let filter = interpret(query.filter.as_deref())?;
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-1
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-13
    // The search's bounds, the search's trimming: an empty `q` is as refused as
    // a short one.
    let needle = params.q.as_deref().map(Needle::parse).transpose()?;
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-13
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-2
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-3
    // One decision on the value resource for the whole page. Its constraints
    // are the narrowed grant: pushed into the declarations query as the
    // secure scope, so a setting the caller may not read is absent from the
    // page and from the count and the page still comes back full — the
    // candidate batch, its evaluation and the refill happen in the query.
    let scope: AccessScope =
        authz::access_scope(&enforcer, &ctx, &resource::VALUE, READ, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-3
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-2
    let target = gate_target(&resolver, &ctx, requested).await?;
    let root = resolver.root_tenant().await?;
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-14
    // Every cursor is bound to the `$filter` hash, the target and the trimmed
    // `q`, and compared before any batch is read. The toolkit binds the filter
    // alone, and the rows of a page depend on the target too: `needs_review`
    // is decided at it, and the values `q` matches are its.
    let binding = browse_binding(
        query.filter_hash.as_deref(),
        target.tenant_id(root),
        needle.as_ref(),
    );
    if let Some(cursor) = &query.cursor
        && cursor.f.as_deref() != Some(binding.as_str())
    {
        return Err(
            cursor_refused("the cursor was minted under another `$filter`, `q` or target").into(),
        );
    }
    let filter_hash = Some(binding);
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-14
    let conn = db.conn().map_err(|e| conn_error(&e))?;

    let declarations_query = ODataQuery {
        filter: filter.declarations.clone().map(Box::new),
        order: query.order.clone(),
        limit: query.limit,
        cursor: query.cursor.clone(),
        filter_hash,
        select: None,
    };
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-7
    // `needs_review eq true` narrows the page, it does not switch the listing:
    // only the settings whose own override at the target is flagged, decided
    // in the page query below from the target's own rows — no subtree walk,
    // no rows read here. The rows themselves are the review listing's.
    let flagged_at = filter.needs_review.then(|| target.tenant_id(root));
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-7
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-6
    // A setting hidden from the caller leaves the page silently, never
    // marked — in the query, on the caller's own chain, so the page is cut
    // and counted after the exclusion: a page shortened afterwards would tell
    // the caller that something sits in the gap.
    let caller_chain = resolver.chain_of(ctx.subject_tenant_id()).await?;
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-6
    let reader = Reader::new(ctx.subject_tenant_id(), root);
    // One target per page: its chain is read once.
    // When the target is the caller's own tenant it is the chain already read
    // for the hidden-setting exclusion.
    let target_tenant = target.tenant_id(root);
    let target_chain = if target_tenant == ctx.subject_tenant_id() {
        caller_chain.clone()
    } else {
        target_chain(&resolver, reader, target_tenant).await?
    };
    let (mut items, page_info, total_count) = if let Some(needle) = &needle {
        // The entitlement is decided before the first batch: the corpus the
        // declaration text is matched in and the rendering of the value half
        // follow it.
        let pii = may_read_pii(&enforcer, &ctx).await;
        narrowed_page(
            &resolver,
            &conn,
            &declarations_query,
            &NarrowedBrowse {
                scope: &scope,
                caller_chain: &caller_chain,
                flagged_at,
                target,
                needle,
                pii,
                reader,
                chain: &target_chain,
                // A flag, not the backend: the repository cannot read it from
                // its connection; a wrong value is wrong-dialect SQL.
                postgres: db.db().db_engine() == "postgres",
            },
        )
        .await
        .map(|(items, page_info)| (items, page_info, None))?
    } else {
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-6
        let page = resolver
            .list_declarations(
                &conn,
                &scope,
                &caller_chain,
                flagged_at,
                &declarations_query,
            )
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-6
        let has_pii = page.items.iter().any(|d| d.data_classification == "pii");

        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-8
        // The same resolution narrowed or not: a flagged own override is reported
        // on the resolved item through `needs_review` and its detail, and the
        // value served is the fallthrough.
        let outcomes = resolver
            .resolve_declarations(&conn, &page.items, target)
            .await;
        let names_a_setter = outcomes.iter().any(|outcome| {
            outcome
                .as_ref()
                .is_ok_and(|effective| effective.trail.iter().any(|entry| entry.set_by.is_some()))
        });
        let pii = (has_pii || names_a_setter) && may_read_pii(&enforcer, &ctx).await;
        let items: Vec<SettingItemDto> = page
            .items
            .iter()
            .zip(outcomes)
            .map(|(declaration, outcome)| {
                let (item, diagnostic) =
                    browse_entry(declaration, outcome, pii, reader, &target_chain);
                log_failure(&declaration.key, diagnostic.as_deref());
                item
            })
            .collect();
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-8
        (items, page.page_info, Some(page.total_count))
    };

    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-9
    // A named key with no declaration, or one the caller may not see, gets its
    // own entry — on the final page, where its absence is known to be final.
    // Under a narrowing — `needs_review`, or the text `q` — a visible key whose
    // setting the narrowing excluded is absent from the page because of the
    // narrowing, not because it does not exist: it gets no entry, and only a
    // key with no declaration the caller may see is reported `not_found`.
    if let Some(keys) = &filter.keys
        && page_info.next_cursor.is_none()
    {
        let absent: Vec<String> = keys
            .iter()
            .filter(|key| !items.iter().any(|item| &item.key == *key))
            .cloned()
            .collect();
        // One read for every absent key, through the page's own gates, so a
        // key the caller may not see is answered as one that does not exist.
        let visible = if flagged_at.is_some() || needle.is_some() {
            resolver
                .visible_keys(&conn, &scope, &caller_chain, &absent)
                .await?
        } else {
            Vec::new()
        };
        for key in absent {
            if visible.contains(&key) {
                continue;
            }
            items.push(SettingItemDto::not_found(&key));
        }
    }
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-9

    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-10
    // The total is of the settings the walk pages — counted after the hidden
    // exclusion as the page is, and under `needs_review eq true` over the
    // narrowed set, since the narrowing is in the page query — so it tells the
    // caller nothing the page does not. Under `q` the page is assembled from
    // candidate batches matched in process, so a count of the page query
    // would count the wrong set: none is reported, as the review listing does.
    Ok(Json(PageDto {
        items,
        page_info: PageInfoDto::of(page_info, total_count),
    }))
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-10
}

// @cpt-dod:cpt-cf-settings-service-dod-value-resolution-review-listing:p1
/// `GET /settings-service/v1/settings/review?tenant={tenant_id}` with `OData`
/// `$filter`, `limit` and a cursor.
///
/// # Errors
/// 400 on a malformed `tenant`, a `$filter` naming `needs_review`, an unmapped
/// field or an unsupported operator, `$orderby` or `$select`, a cursor minted
/// under another filter or target, or a subtree past the budget; 403 when the
/// caller may not read values or the target is outside its subtree or
/// standalone; 503 when a dependency cannot answer.
pub async fn list_review(
    Extension(ctx): Extension<SecurityContext>,
    Extension(resolver): Extension<Arc<ConcreteResolver>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Query(params): Query<TenantParam>,
    OData(query): OData,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-1
    let requested = parse_tenant(params.tenant.as_deref())?;
    crate::domain::odata::reject_unsupported_options(&query, "the review listing")?;
    // The order is fixed — declaration, then tenant — which a keyset cursor
    // needs; a requested one is refused rather than ignored.
    if !query.order.0.is_empty() {
        return Err(DomainError::Validation {
            field: "$orderby".to_owned(),
            code: field::ODATA_UNSUPPORTED_OPTION,
            message: "$orderby is not supported on the review listing; it is ordered by \
                      setting, then tenant"
                .to_owned(),
        }
        .into());
    }
    let filter = interpret(query.filter.as_deref())?;
    // `needs_review` is the listing itself, not something to narrow it by.
    if filter.needs_review {
        return Err(unsupported(
            "`needs_review` is not a filter of the review listing: every row it lists is \
             flagged for review",
        )
        .into());
    }
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-1
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-2
    // One decision on the value resource for the whole page; its constraints
    // narrow the settings the rows may belong to, in the page query.
    let scope: AccessScope =
        authz::access_scope(&enforcer, &ctx, &resource::VALUE, READ, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-2
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-3
    let target = gate_target(&resolver, &ctx, requested).await?;
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-3
    let root = resolver.root_tenant().await?;
    let target_tenant = target.tenant_id(root);
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-4
    // The rows of the target's subtree, standalone descendants excluded by the
    // hierarchy and at most the subtree budget of tenants: a subtree the
    // budget cuts would hide rows silently, so the request is refused with the
    // bound named.
    // The walk's parent links are kept: they are the path from the target down
    // to each row's tenant, which the identity rule reads.
    let walk = resolver
        .hierarchy()
        .subtree(target_tenant, SUBTREE_BUDGET)
        .await?;
    if walk.truncated {
        return Err(subtree_too_large("tenant", "target's").into());
    }
    let mut tenant_ids = walk.order.clone();
    tenant_ids.push(target_tenant);
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-4

    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-5
    // The cursor is bound to the `$filter` as asked and to the target; the
    // page read compares it before any row is read, and a cursor minted under
    // another filter or another target is refused 400.
    let mut query = query;
    query.filter_hash = Some(review_binding(target_tenant, query.filter.as_deref()));
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-5

    let conn = db.conn().map_err(|e| conn_error(&e))?;
    // A setting hidden from the caller has no rows here, silently: excluded in
    // the page query on the caller's own chain, as the browse excludes it.
    let caller_chain = resolver.chain_of(ctx.subject_tenant_id()).await?;
    let domain_visibility = visibility::domain_visibility(&scope);
    // Inst-vr-review-6 is the page read itself, in the value repository.
    let page = resolver
        .review_page(
            &conn,
            &ReviewRequest {
                scope: &scope,
                visibility: &domain_visibility,
                hidden_for: &caller_chain,
                tenant_ids: &tenant_ids,
                declarations: filter.declarations.as_ref(),
                query: &query,
            },
        )
        .await?;

    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-7
    // The page's declarations in one batch, each row finding its own in
    // constant time.
    let mut ids: Vec<Uuid> = page.items.iter().map(|row| row.declaration_id).collect();
    ids.sort_unstable();
    ids.dedup();
    let by_id: std::collections::HashMap<Uuid, crate::domain::declaration::Declaration> = resolver
        .declarations_by_ids(&conn, &ids)
        .await?
        .into_iter()
        .map(|d| (d.id, d))
        .collect();
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-7
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-8
    // Every row names who set it, an administrator's identity, so the
    // entitlement is asked whenever the page has a row.
    let pii = !page.items.is_empty() && may_read_pii(&enforcer, &ctx).await;
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-10
    // The path from the reader tenant down to the target is the target's chain
    // from the reader; each row's path extends it by the walk's parent links
    // down to the row's tenant.
    let reader = Reader::new(ctx.subject_tenant_id(), root);
    let target_chain = resolver.chain_of(target_tenant).await?;
    let to_target = reader.path_in(&target_chain);
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-10
    let items: Vec<ReviewItemDto> = page
        .items
        .iter()
        .filter_map(|row| {
            // The page query joins each row to a visible declaration, so a
            // miss here means the declaration went away between the two
            // reads; the row is skipped, and the gap is said aloud because
            // the page is shorter than its cursor claims.
            let Some(declaration) = by_id.get(&row.declaration_id) else {
                tracing::warn!(
                    declaration_id = %row.declaration_id,
                    tenant = %row.tenant_id,
                    "a review row lost its declaration between the page and the declaration reads"
                );
                return None;
            };
            let mut path = to_target.to_vec();
            path.extend(
                walk.path_from(target_tenant, row.tenant_id)
                    .into_iter()
                    .skip(1),
            );
            let shown = reader.is_shown(row.set_by_tenant, &path);
            Some(
                render_flagged(&declaration.key, &declaration.mode, row, root, pii, shown)
                    .with_display_name(declaration.display_name.as_deref()),
            )
        })
        .collect();
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-8
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-9
    Ok(Json(toolkit_odata::Page {
        items,
        page_info: page.page_info,
    }))
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-9
}

#[cfg(test)]
#[path = "setting_handlers_tests.rs"]
mod setting_handlers_tests;

// @cpt-dod:cpt-cf-settings-service-dod-audit-store-history-read:p1
/// `GET /settings-service/v1/settings/{key}/history?tenant={tenant_id}` with
/// `limit` and `cursor`.
///
/// # Errors
/// 400 on a malformed key or `tenant`, or on `$filter`, `$orderby` or `$select`,
/// which the history does not take; 403 when the caller may not read values or
/// the target is outside its subtree or standalone; 404 when no declaration
/// exists at the key or it is outside the caller's administrative domain; 503
/// when the store cannot answer.
#[allow(clippy::too_many_arguments)]
pub async fn get_history(
    Extension(ctx): Extension<SecurityContext>,
    Extension(resolver): Extension<Arc<ConcreteResolver>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Extension(names): Extension<Arc<crate::infra::actor_names::ActorNameResolver>>,
    Path(key): Path<String>,
    Query(params): Query<TenantParam>,
    OData(query): OData,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-1
    let key = parse_key(&key)?;
    let requested = parse_tenant(params.tenant.as_deref())?;
    if query.filter.is_some() || !query.order.0.is_empty() || query.select.is_some() {
        return Err(unsupported(
            "history takes `limit` and `cursor` only; it is always newest first",
        )
        .into());
    }
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-1
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-2
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-3
    let scope = authz::access_scope(&enforcer, &ctx, &resource::VALUE, READ, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-3
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-2
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-4
    // A caller that cannot read a tenant's values cannot read their history.
    let target = gate_target(&resolver, &ctx, requested).await?;
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-4
    let root = resolver.root_tenant().await?;
    let conn = db.conn().map_err(|e| conn_error(&e))?;
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-5
    // A retired declaration keeps its history and is read like an active one;
    // absence, and a declaration outside the caller's administrative domain,
    // are 404.
    let declaration = resolver
        .find_declaration(&conn, &key)
        .await?
        .filter(|d| {
            visibility::is_visible(
                &visibility::domain_visibility(&scope),
                d.domain_affinity.as_deref(),
            )
        })
        .ok_or(DomainError::NotFound {
            resource: "declaration",
        })?;
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-5
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-6
    // Hidden from the target tenant: 404 rather than 403, so a hidden setting's
    // existence is not disclosed through its history either.
    let caller = ScopeTarget::Tenant(ctx.subject_tenant_id()).normalize(root);
    if resolver
        .effective_access(&conn, declaration.id, caller)
        .await?
        .is_hidden()
    {
        return Err(DomainError::NotFound {
            resource: "declaration",
        }
        .into());
    }
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-6
    let page = crate::infra::storage::audit_store::AuditStore
        .history(&conn, &scope, key.as_str(), target.tenant_id(root), &query)
        .await?;
    let values_are_pii = declaration.data_classification == "pii";
    // A `pii` image keeps its class after the setting is declassified, so it
    // asks for the entitlement as much as a `pii` setting or actor does.
    let needs_entitlement = values_are_pii
        || page.items.iter().any(|r| {
            r.actor_classification == crate::audit::ActorClassification::Pii || r.has_pii_image()
        });
    let pii = needs_entitlement && may_read_pii(&enforcer, &ctx).await;
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-9
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-10
    // The path from the reader tenant down to the record's tenant is the
    // target's chain from the reader. A definition record has no tenant, so
    // its actor is on no path.
    let reader = Reader::new(ctx.subject_tenant_id(), root);
    let target_chain = target_chain(&resolver, reader, target.tenant_id(root)).await?;
    let to_target = reader.path_in(&target_chain);
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-10
    let mut items: Vec<AuditRecordDto> = page
        .items
        .iter()
        .map(|r| {
            let path: &[Uuid] = if r.tenant_id.is_some() {
                to_target
            } else {
                &[]
            };
            render_record(
                r,
                values_are_pii,
                pii,
                reader.is_shown(r.actor_tenant_id, path),
            )
        })
        .collect();
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-9
    let actors: Vec<crate::infra::actor_names::ActorRef> = page
        .items
        .iter()
        .zip(&items)
        .map(|(record, item)| crate::infra::actor_names::ActorRef {
            actor_tenant_id: record.actor_tenant_id,
            actor: item.actor.clone(),
            masked: item.actor_masked,
        })
        .collect();
    let actor_names = names.names_for_page(&ctx, &actors).await;
    for (item, name) in items.iter_mut().zip(actor_names) {
        item.actor_name = name.as_str().map(str::to_owned);
    }
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-9
    Ok(Json(PageDto {
        items,
        page_info: PageInfoDto::of(page.page_info, Some(page.total_count)),
    }))
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-9
}
