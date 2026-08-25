//! Provisioning primitives shared by every facade that mutates Keycloak.
//!
//! This module exists to break a module cycle, not to group things by
//! topic. Both [`crate::domain::tenant_facade`] and
//! [`crate::domain::service_account_facade`] need the ownership marker and
//! the ambiguity re-tag; both used to reach for `tenant_facade`'s copies,
//! which made `service_account_facade` depend on `tenant_facade` at the
//! same time as `tenant_facade` depended on `service_account_facade` for
//! the deprovision purge cascade.
//!
//! Nothing here talks to Keycloak. These are the two facts every mutating
//! saga has to agree on:
//!
//! * which attribute key marks a Keycloak object as owned by an AM tenant, and
//! * what a transport failure *after* a mutation was sent means.

use crate::domain::error::{AmbiguousStage, PluginError};

/// Percent-encode one path segment or query value using the RFC 3986
/// unreserved set (`ALPHA / DIGIT / "-" / "." / "_" / "~"`). This avoids
/// form-style `+` encoding: a plus sign in a path is literal and must never
/// stand in for a space.
#[must_use]
pub fn encode_component(value: &str) -> String {
    const COMPONENT_ENCODE_SET: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
        .add(b' ')
        .add(b'!')
        .add(b'"')
        .add(b'#')
        .add(b'$')
        .add(b'%')
        .add(b'&')
        .add(b'\'')
        .add(b'(')
        .add(b')')
        .add(b'*')
        .add(b'+')
        .add(b',')
        .add(b'/')
        .add(b':')
        .add(b';')
        .add(b'<')
        .add(b'=')
        .add(b'>')
        .add(b'?')
        .add(b'@')
        .add(b'[')
        .add(b'\\')
        .add(b']')
        .add(b'^')
        .add(b'`')
        .add(b'{')
        .add(b'|')
        .add(b'}');
    percent_encoding::utf8_percent_encode(value, COMPONENT_ENCODE_SET).to_string()
}

/// Attribute key stamping the AM tenant that owns a provisioned Keycloak
/// object — a Created-mode realm, or a service-account client.
///
/// Written into the object's `attributes` map at create time so that (run
/// am-functional-22):
///   * on the realm surface, a retried / re-entrant create recognises a realm
///     as its own — the unique-name 409 from a duplicate realm POST reconciles
///     to idempotent success instead of a false `AmbiguousCreated` → AM 500;
///   * service-account ownership lookups reject foreign clients without
///     adopting a same-name account (the service-account contract requires
///     strict rejection rather than resume-on-409); and
///   * the orphan reaper can GC by owning tenant.
///
/// The key is deliberately shared between the realm and service-account
/// surfaces: an ownership check that differed between them would let one
/// facade's reaper walk past the other's objects.
pub const PROVISIONING_TENANT_ID_ATTR: &str = "cf.provisioning.tenant_id";

/// Re-tag a transport-level [`PluginError::KcRest`] surfaced by
/// [`KcAdminClient::raw_request`] as an [`PluginError::AmbiguousCreated`] so the
/// caller's stage attribution is preserved. Mid-saga transport failures are
/// inherently ambiguous (the request may have landed; we just never got the
/// response), so this collapse is correct.
///
/// Call it only at sites that sit AFTER a mutation was sent. Applying it to a
/// pre-mutation read would claim uncertainty the caller does not have, and the
/// caller would defer to a reaper instead of simply retrying.
///
/// [`KcAdminClient::raw_request`]: crate::domain::kc::client::KcAdminClient::raw_request
#[must_use]
pub fn ambiguous_from_kc_error(stage: AmbiguousStage, e: PluginError) -> PluginError {
    match e {
        PluginError::KcRest {
            method,
            path_template,
            status,
            body_first_2kb,
        } => PluginError::AmbiguousCreated {
            stage,
            detail: format!("kc:{method} {path_template} -> {status}: {body_first_2kb}"),
        },
        other => other,
    }
}
