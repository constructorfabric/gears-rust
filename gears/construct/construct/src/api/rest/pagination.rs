//! The shared pagination of Construct's lists: cursor pages with a maximum
//! page size, `OData` `$filter` and `$orderby` over the fields a list names.
//! Every list Construct serves is one subject's, small enough to hold in
//! memory, so the page is cut from the whole list: filtered, ordered, then
//! taken after the cursor's position.

use std::cmp::Ordering;

use toolkit_odata::filter::{FilterField, FilterNode, FilterOp, ODataValue};
use toolkit_odata::{CursorV1, ODataOrderBy, ODataQuery, OrderKey, Page, PageInfo, SortDir};

/// The page size when the request names none.
pub const DEFAULT_LIMIT: u64 = 50;
/// The largest page; a larger `limit` is cut to it, not refused.
pub const MAX_LIMIT: u64 = 200;

/// An item of a paged list: the value of each of its fields as a key that
/// sorts the way the value does.
pub trait Listed {
    type Field: FilterField;

    /// The key of `field`, none when the item has no value for it.
    fn key(&self, field: Self::Field) -> Option<String>;
}

/// How a list is ordered when the request names no order, and the field
/// that makes every order total.
pub struct Listing<F: FilterField> {
    pub default_order: &'static [(F, SortDir)],
    pub tiebreaker: F,
}

/// Why a page cannot be cut: a bad `$filter`, an `$orderby` on a field the
/// list does not have, or a cursor for another query. Each is the caller's
/// mistake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageError {
    Filter(String),
    Order(String),
    Cursor(String),
}

/// The key of a time: its nanoseconds since the epoch, zero-padded, so keys
/// sort as the times do. A time before the epoch keys as the epoch.
#[must_use]
pub fn time_key(nanos: i128) -> String {
    format!("{:020}", nanos.max(0))
}

/// The page of `items` that `query` asks for.
///
/// # Errors
///
/// [`PageError`] when the filter, the order or the cursor does not fit the
/// list.
///
/// @cpt-dod:cpt-cf-construct-dod-subject-control-pagination:p1
pub fn page<T: Listed>(
    mut items: Vec<T>,
    query: &ODataQuery,
    listing: &Listing<T::Field>,
) -> Result<Page<T>, PageError> {
    // @cpt-begin:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-limit
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    // @cpt-end:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-limit
    // @cpt-begin:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-filter
    if let Some(expr) = query.filter.as_deref() {
        let filter = toolkit_odata::filter::convert_expr_to_filter_node::<T::Field>(expr)
            .map_err(|err| PageError::Filter(err.to_string()))?;
        items.retain(|item| matches(item, &filter));
    }
    // @cpt-end:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-filter
    // @cpt-begin:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-order
    let order = effective_order(query, listing)?;
    let fields = fields_of::<T::Field>(&order)?;
    items.sort_by(|a, b| compare(&keys(a, &fields), &keys(b, &fields), &order));
    // @cpt-end:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-order
    // @cpt-begin:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-cursor
    let mut rest: Vec<T> = match &query.cursor {
        Some(cursor) => {
            toolkit_odata::validate_cursor_against(cursor, &order, query.filter_hash.as_deref())
                .map_err(|err| PageError::Cursor(err.to_string()))?;
            items
                .into_iter()
                .filter(|item| compare(&keys(item, &fields), &cursor.k, &order).is_gt())
                .collect()
        }
        None => items,
    };
    // @cpt-end:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-cursor
    // @cpt-begin:cpt-cf-construct-algo-subject-control-paginate:p1:inst-page-cut
    let take = usize::try_from(limit).unwrap_or(usize::MAX);
    let next_cursor = if rest.len() > take {
        rest.truncate(take);
        rest.last()
            .map(|last| CursorV1 {
                k: keys(last, &fields),
                o: order.0.first().map_or(SortDir::Asc, |key| key.dir),
                s: order.to_signed_tokens(),
                f: query.filter_hash.clone(),
                d: "fwd".to_owned(),
            })
            .map(|cursor| cursor.encode())
            .transpose()
            .map_err(|err| PageError::Cursor(err.to_string()))?
    } else {
        None
    };
    Ok(Page::new(
        rest,
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

fn keys<T: Listed>(item: &T, fields: &[T::Field]) -> Vec<String> {
    fields
        .iter()
        .map(|field| item.key(*field).unwrap_or_default())
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
        FilterNode::Binary { field, op, value } => holds(item.key(*field), *op, value),
        FilterNode::InList { field, values } => {
            let key = item.key(*field);
            values
                .iter()
                .any(|value| holds(key.clone(), FilterOp::Eq, value))
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
/// to nothing, so only `ne` holds for it.
fn holds(key: Option<String>, op: FilterOp, value: &ODataValue) -> bool {
    let Some(key) = key else {
        return op == FilterOp::Ne;
    };
    let Some(probe) = value_key(value) else {
        return op == FilterOp::Ne;
    };
    match op {
        FilterOp::Eq => key == probe,
        FilterOp::Ne => key != probe,
        FilterOp::Gt => key > probe,
        FilterOp::Ge => key >= probe,
        FilterOp::Lt => key < probe,
        FilterOp::Le => key <= probe,
        FilterOp::Contains => key.contains(&probe),
        FilterOp::StartsWith => key.starts_with(&probe),
        FilterOp::EndsWith => key.ends_with(&probe),
        FilterOp::In | FilterOp::And | FilterOp::Or => false,
    }
}

/// A filter value as a key of the same form [`Listed::key`] gives.
fn value_key(value: &ODataValue) -> Option<String> {
    match value {
        ODataValue::String(text) => Some(text.clone()),
        ODataValue::Uuid(id) => Some(id.to_string()),
        ODataValue::DateTime(at) => at.timestamp_nanos_opt().map(|n| time_key(i128::from(n))),
        ODataValue::Bool(flag) => Some(flag.to_string()),
        ODataValue::Null | ODataValue::Number(_) | ODataValue::Date(_) | ODataValue::Time(_) => {
            None
        }
    }
}
