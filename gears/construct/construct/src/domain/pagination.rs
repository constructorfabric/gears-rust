//! The shared pagination of Construct's lists: cursor pages with a maximum
//! page size, `OData` `$filter` and `$orderby` over the fields a list names.
//! Every list Construct serves is one subject's, small enough to hold in
//! memory, so the page is cut from the whole list: filtered, ordered, then
//! taken after the cursor's position.

use std::cmp::Ordering;

use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_odata::filter::{FilterField, FilterNode, FilterOp, ODataValue};
use toolkit_odata::{CursorV1, ODataOrderBy, ODataQuery, OrderKey, Page, PageInfo, SortDir};
use uuid::Uuid;

/// The page size when the request names none.
pub const DEFAULT_LIMIT: u64 = 50;
/// The largest page; a larger `limit` is cut to it, not refused.
pub const MAX_LIMIT: u64 = 200;

/// The key a field's value sorts, filters and resumes by. It is built only
/// from text, a time or an id, each encoded so that keys compare as their
/// values do; a cursor carries keys as their text.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key(String);

impl Key {
    /// A text, which compares as text.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// A time, as its nanoseconds since the epoch, zero-padded to the width
    /// of the latest time `time` represents. A time before the epoch keys as
    /// the epoch: no stored time is earlier.
    #[must_use]
    pub fn time(at: OffsetDateTime) -> Self {
        Self::nanos(at.unix_timestamp_nanos())
    }

    /// An id, as its hyphenated text.
    #[must_use]
    pub fn id(id: Uuid) -> Self {
        Self(id.to_string())
    }

    fn nanos(nanos: i128) -> Self {
        Self(format!("{:024}", nanos.max(0)))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// An item of a paged list: the key of each of its fields.
pub trait Listed {
    type Field: FilterField;

    /// The key of `field`, none when the item has no value for it.
    fn key(&self, field: Self::Field) -> Option<Key>;
}

/// How a list is ordered when the request names no order, and the field
/// that makes every order total.
#[domain_model]
pub struct Listing<F: FilterField> {
    pub default_order: &'static [(F, SortDir)],
    pub tiebreaker: F,
}

/// Why a page cannot be cut. The first three are the caller's mistake: a
/// bad `$filter`, an `$orderby` on a field the list does not have, or a
/// cursor for another query. The last is Construct's own.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageError {
    Filter(String),
    Order(String),
    Cursor(String),
    Internal(String),
}

/// The page of `items` that `query` asks for.
///
/// # Errors
///
/// [`PageError`] when the filter, the order or the cursor does not fit the
/// list, or the next cursor cannot be built.
///
/// @cpt-dod:cpt-cf-construct-dod-subject-control-pagination:p1
pub fn page<T: Listed>(
    items: Vec<T>,
    query: &ODataQuery,
    listing: &Listing<T::Field>,
) -> Result<Page<T>, PageError> {
    // @cpt-begin:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-limit
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    // @cpt-end:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-limit
    // @cpt-begin:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-filter
    let mut items = items;
    if let Some(expr) = query.filter.as_deref() {
        let filter = toolkit_odata::filter::convert_expr_to_filter_node::<T::Field>(expr)
            .map_err(|err| PageError::Filter(err.to_string()))?;
        items.retain(|item| matches(item, &filter));
    }
    // @cpt-end:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-filter
    // @cpt-begin:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-order
    let order = effective_order(query, listing)?;
    let fields = fields_of::<T::Field>(&order)?;
    // Each item's keys are built once, then sorted on, skipped by and
    // resumed from.
    let mut keyed: Vec<(Vec<String>, T)> = items
        .into_iter()
        .map(|item| (keys(&item, &fields), item))
        .collect();
    keyed.sort_by(|(a, _), (b, _)| compare(a, b, &order));
    // @cpt-end:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-order
    // @cpt-begin:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-cursor
    if let Some(cursor) = &query.cursor {
        toolkit_odata::validate_cursor_against(cursor, &order, query.filter_hash.as_deref())
            .map_err(|err| PageError::Cursor(err.to_string()))?;
        keyed.retain(|(keys, _)| compare(keys, &cursor.k, &order).is_gt());
    }
    // @cpt-end:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-cursor
    // @cpt-begin:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-cut
    let take = usize::try_from(limit).unwrap_or(usize::MAX);
    let next_cursor = if keyed.len() > take {
        keyed.truncate(take);
        match keyed.last() {
            Some((last, _)) => Some(
                CursorV1 {
                    k: last.clone(),
                    o: order.0.first().map_or(SortDir::Asc, |key| key.dir),
                    s: order.to_signed_tokens(),
                    f: query.filter_hash.clone(),
                    d: "fwd".to_owned(),
                }
                .encode()
                .map_err(|err| PageError::Internal(format!("the next cursor: {err}")))?,
            ),
            None => None,
        }
    } else {
        None
    };
    Ok(Page::new(
        keyed.into_iter().map(|(_, item)| item).collect(),
        PageInfo {
            next_cursor,
            prev_cursor: None,
            limit,
        },
    ))
    // @cpt-end:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-cut
}

/// The order the page is cut in: the request's, else the one its cursor was
/// issued under, else the list's default; each made total by the
/// tiebreaker.
fn effective_order<F: FilterField>(
    query: &ODataQuery,
    listing: &Listing<F>,
) -> Result<ODataOrderBy, PageError> {
    let order = if !query.order.0.is_empty() {
        query.order.clone()
    } else if let Some(cursor) = &query.cursor {
        ODataOrderBy::from_signed_tokens(&cursor.s)
            .map_err(|err| PageError::Cursor(err.to_string()))?
    } else {
        ODataOrderBy(
            listing
                .default_order
                .iter()
                .map(|(field, dir)| OrderKey {
                    field: field.name().to_owned(),
                    dir: *dir,
                })
                .collect(),
        )
    };
    Ok(order.ensure_tiebreaker(listing.tiebreaker.name(), SortDir::Asc))
}

/// The list's field for each key of `order`.
fn fields_of<F: FilterField>(order: &ODataOrderBy) -> Result<Vec<F>, PageError> {
    order
        .0
        .iter()
        .map(|key| {
            F::from_name(&key.field)
                .ok_or_else(|| PageError::Order(format!("cannot order by {:?}", key.field)))
        })
        .collect()
}

/// The item's keys for `fields`, as text, an absent value as the empty text.
fn keys<T: Listed>(item: &T, fields: &[T::Field]) -> Vec<String> {
    fields
        .iter()
        .map(|field| item.key(*field).map(|key| key.0).unwrap_or_default())
        .collect()
}

/// `a` against `b`, key by key, each in its key's direction.
fn compare(a: &[String], b: &[String], order: &ODataOrderBy) -> Ordering {
    for (index, key) in order.0.iter().enumerate() {
        let left = a.get(index).map_or("", String::as_str);
        let right = b.get(index).map_or("", String::as_str);
        let ordering = match key.dir {
            SortDir::Asc => left.cmp(right),
            SortDir::Desc => right.cmp(left),
        };
        if ordering.is_ne() {
            return ordering;
        }
    }
    Ordering::Equal
}

fn matches<T: Listed>(item: &T, filter: &FilterNode<T::Field>) -> bool {
    match filter {
        FilterNode::Binary { field, op, value } => holds(item.key(*field).as_ref(), *op, value),
        FilterNode::InList { field, values } => {
            let key = item.key(*field);
            values
                .iter()
                .any(|value| holds(key.as_ref(), FilterOp::Eq, value))
        }
        FilterNode::Composite {
            op: FilterOp::Or,
            children,
        } => children.iter().any(|child| matches(item, child)),
        // The parser builds composites only with `and` and `or`.
        FilterNode::Composite { children, .. } => children.iter().all(|child| matches(item, child)),
        FilterNode::Not(inner) => !matches(item, inner),
    }
}

/// Whether an item's key stands in `op` to `value`. An absent key is equal
/// to nothing, so only `ne` holds for it. The typed filter lets through only
/// values of the field's kind, so a value without a key is never compared.
fn holds(key: Option<&Key>, op: FilterOp, value: &ODataValue) -> bool {
    let (Some(key), Some(probe)) = (key, value_key(value)) else {
        return op == FilterOp::Ne;
    };
    let (key, probe) = (key.as_str(), probe.as_str());
    match op {
        FilterOp::Eq => key == probe,
        FilterOp::Ne => key != probe,
        FilterOp::Gt => key > probe,
        FilterOp::Ge => key >= probe,
        FilterOp::Lt => key < probe,
        FilterOp::Le => key <= probe,
        FilterOp::Contains => key.contains(probe),
        FilterOp::StartsWith => key.starts_with(probe),
        FilterOp::EndsWith => key.ends_with(probe),
        FilterOp::In | FilterOp::And | FilterOp::Or => false,
    }
}

/// A filter value as the key [`Listed::key`] gives for the same value.
fn value_key(value: &ODataValue) -> Option<Key> {
    match value {
        ODataValue::String(text) => Some(Key::text(text.clone())),
        ODataValue::Uuid(id) => Some(Key::id(*id)),
        ODataValue::DateTime(at) => Some(Key::nanos(
            i128::from(at.timestamp()) * 1_000_000_000 + i128::from(at.timestamp_subsec_nanos()),
        )),
        ODataValue::Null
        | ODataValue::Bool(_)
        | ODataValue::Number(_)
        | ODataValue::Date(_)
        | ODataValue::Time(_) => None,
    }
}
