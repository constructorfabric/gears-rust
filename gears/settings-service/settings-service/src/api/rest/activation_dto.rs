//! Wire shapes of the activation read.

use uuid::Uuid;

use crate::api::rest::setting_dto::rfc3339;
use crate::domain::activation::{ActivationItem, ActivationRead};

/// One subscriber's activation state of the current value, as
/// `GET /settings-service/v1/settings/{key}/activation` returns it. It carries
/// no value of any kind: no expected value, applied value or hash.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct ActivationItemDto {
    /// The consumer identity.
    pub subscriber: String,
    /// `awaiting`, `succeeded` or `failed`. Treat the set as open: a client
    /// meets an unknown status as "not known to have activated".
    pub status: String,
    /// The change the state answers: the id of its audit record in the history.
    pub change_ref: Uuid,
    /// When the settling report arrived, RFC 3339; absent while `awaiting`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reported_at: Option<String>,
    /// The consumer's failure detail; present only on a `failed` state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The activation of a setting at one tenant: a single resource, not a page.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct ActivationDto {
    /// The tenant the answer is for, a bare lowercase UUID string.
    pub tenant: String,
    /// One item per subscriber of the setting at the tenant, ordered by
    /// subscriber; empty when nobody subscribed or no change has affected the
    /// subscriptions yet.
    pub items: Vec<ActivationItemDto>,
}

impl From<ActivationItem> for ActivationItemDto {
    fn from(item: ActivationItem) -> Self {
        Self {
            subscriber: item.subscriber,
            status: item.status.as_str().to_owned(),
            change_ref: item.change_ref,
            reported_at: item.reported_at.map(rfc3339),
            detail: item.detail,
        }
    }
}

impl From<ActivationRead> for ActivationDto {
    fn from(read: ActivationRead) -> Self {
        Self {
            tenant: read.tenant.as_hyphenated().to_string(),
            items: read.items.into_iter().map(Into::into).collect(),
        }
    }
}
