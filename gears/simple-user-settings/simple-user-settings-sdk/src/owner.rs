//! Who a set of settings belongs to.
//!
//! Settings are stored under a user key, and by default that key is the token
//! subject — the identity that signed in. For a deployment where one human has
//! exactly one way to sign in, subject and human are the same thing and there is
//! nothing to decide.
//!
//! They come apart as soon as a product lets somebody reach the same account
//! through more than one identity: an e-mail login and a brokered GitHub login,
//! two accounts merged after the fact, a migration between identity providers.
//! Then the token subject names *a way in*, not the person, and settings keyed
//! on it silently fork — the same human signs in the other way and their theme,
//! their language and everything else stored here is gone.
//!
//! A deployment that knows better can say so by publishing a
//! [`SettingsOwnerResolver`] on the `ClientHub`. The gear keeps its storage, its
//! endpoints and its authorization; only the user half of the key comes from
//! somewhere better informed. That key is also what
//! [`SimpleUserSettings::user_id`](crate::SimpleUserSettings::user_id) reports,
//! so with a resolver in place a caller sees the resolved owner there, not its
//! own subject id.
//!
//! # Scope: one tenant
//!
//! Settings are keyed on `(user, tenant)` and the resolver answers only the
//! user half; the tenant is always the caller's `subject_tenant_id()`. Two logins
//! of one human therefore share settings *within a tenant*, and a person who
//! works in two tenants still has two sets. Scoping settings to the person
//! across tenants is a different decision about what a user setting is, and it
//! is not made here.
//!
//! # Turning it on over existing data
//!
//! Rows written before a resolver was registered are keyed on the token
//! subject. Once the resolver answers with a different key, those rows are no
//! longer read. Only the deployment knows the subject → owner mapping, so it
//! re-keys them when it enables the resolver — for each login it maps,
//! `UPDATE settings SET user_id = <owner> WHERE user_id = <subject> AND tenant_id = <tenant>`.
//! A mapping that keeps one of a person's existing subject ids as their owner
//! key needs no re-keying for that login.

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Resolve the caller to the user key their settings are filed under.
///
/// Register an implementation on the `ClientHub`:
///
/// ```ignore
/// let resolver: Arc<dyn SettingsOwnerResolver> = Arc::new(MyResolver);
/// hub.register(resolver);
/// ```
///
/// The gear looks the resolver up on the hub on every read and write, so it
/// takes effect from the first request after it is registered, whichever
/// lifecycle phase that is. A request served before the registration uses the
/// token subject, so register it in your gear's `init`. The hub holds one
/// resolver: registering a second replaces the first, as for any other client
/// on the hub.
///
/// With no implementation registered, the gear uses `ctx.subject_id()` exactly
/// as it always has, so adding this costs an existing deployment nothing.
///
/// # Trust
///
/// The answer is both the storage key and the resource id the gear asks the PDP
/// about, so an implementation decides whose settings a request reads and
/// writes. It is deployment code in the same process, at the same trust level
/// as the `AuthZResolverApi` the gear takes from the same hub. It must:
///
/// - answer only with a key the caller actually holds — `ctx.subject_id()`
///   itself or the person that subject is linked to, never an unrelated one;
/// - answer only with a key that belongs to `ctx.subject_tenant_id()`, because
///   that tenant is the other half of the key;
/// - answer the same way for the same caller from one request to the next. A
///   write and a read that get different answers land on different rows, which
///   is the fork this trait exists to remove.
///
/// The PDP still sees the caller: it is asked whether `ctx` may act on the
/// resolved key. A policy that only allows `resource_id == subject.id` therefore
/// denies resolved callers instead of letting them through.
///
/// # Called on every request
///
/// Every `get`, `update` and `patch` awaits it once, before authorization,
/// because its answer is the resource authorization is asked about. Keep it
/// cheap, and cache on your side if it is backed by a directory or a database.
/// The gear bounds each call by its `owner_resolver_timeout_ms` setting and
/// fails the request with `ServiceUnavailable` when the call runs over.
///
/// The future can be dropped at any await point — when that timeout fires, or
/// when the HTTP client disconnects — so a lookup must be safe to abandon
/// half-way, which a read-only lookup is.
///
/// `ctx` is the caller's full security context, bearer token included. Use its
/// identity only; do not log, persist or forward the token.
#[async_trait]
pub trait SettingsOwnerResolver: Send + Sync + 'static {
    /// The key this caller's settings belong under, or `None` to fall back to
    /// the token subject.
    ///
    /// Returning `None` rather than an error is deliberate: a deployment whose
    /// resolver cannot answer for some callers (a service account, an identity
    /// it has never seen) should degrade to the subject, which is always
    /// available, rather than deny somebody their preferences.
    ///
    /// # Errors
    ///
    /// Only for a genuine failure to look the caller up — a database that is
    /// down, not a caller the resolver has no opinion about. The gear reports it
    /// rather than guessing, because silently filing a person's settings under
    /// the wrong key is worse than telling them the read failed.
    ///
    /// That is the gear's default, not a requirement on the deployment. An
    /// implementation that prefers availability, and knows which of its callers
    /// can safely fall back to the token subject (for example, callers that have
    /// only one login), may map its own transient failures to `Ok(None)` for
    /// them. The gear cannot make that call itself, because it cannot tell a
    /// caller with one login from one with several.
    ///
    /// The category carries through: `ServiceUnavailable`, `DeadlineExceeded`,
    /// `ResourceExhausted` and `Aborted` answer the request with
    /// `ServiceUnavailable`; `PermissionDenied` and `Unauthenticated` with the
    /// gear's usual denial; anything else with an internal error. The error's
    /// text goes to the log, not to the caller.
    async fn settings_owner(&self, ctx: &SecurityContext) -> Result<Option<Uuid>, CanonicalError>;
}
