//! Domain service for the static `IdP` plugin.
//!
//! In-memory echo: every operation succeeds with a deterministic
//! payload derived from its inputs. Provisioned users are retained in
//! a per-tenant `HashMap` so `list_users` and the
//! `$filter=id eq <uuid>` existence-check shape both observe what
//! `provision_user` just wrote -- a real provider would expose the
//! same lifecycle. State lives in-process and is dropped on restart,
//! matching the dev-only contract of every other `static-*-plugin`.
//!
//! Service accounts are retained the same way, in their own
//! per-tenant map. Credentials are fresh random values returned only by
//! provision and rotate; the store retains account identity and scopes but
//! never plaintext secret material.

use std::collections::HashMap;

use parking_lot::Mutex;
use secrecy::SecretString;
use serde_json::{Value, json};
use toolkit_macros::domain_model;
use uuid::Uuid;

use account_management_sdk::{
    IdpNewUser, IdpProvisionTarget, IdpProvisionTenantRequest, IdpServiceAccountCredentials,
    IdpServiceAccountSummary, IdpUser, IdpUserPatch,
};

/// Outcome of [`Service::update_user`], translated to the public
/// `IdpUserOperationFailure` taxonomy by [`crate::domain::client`].
#[domain_model]
pub enum UpdateUserOutcome {
    /// User patched; carries the post-update projection.
    Updated(IdpUser),
    /// No user with that id in this tenant scope.
    NotFound,
    /// A `username` rename collided with another user's login in the
    /// same tenant scope.
    UsernameTaken,
}

/// Outcome of [`Service::provision_account`], translated to the public
/// `IdpServiceAccountFailure` taxonomy by [`crate::domain::client`].
#[domain_model]
pub enum ProvisionAccountOutcome {
    /// Account created; carries the one-time credentials.
    Created(IdpServiceAccountCredentials),
    /// A live account in this tenant scope already carries that name.
    /// The existing account is neither returned nor modified — the
    /// contract forbids resuming or revealing it.
    NameTaken,
    /// The name carries characters that would not survive being embedded
    /// in a client id. Nothing was created.
    NameNotAddressable,
}

/// Outcome of [`Service::rotate_account_secret`].
#[domain_model]
pub enum RotateAccountOutcome {
    /// Secret regenerated; carries the new one-time credentials. The
    /// account's `client_id` and `subject_id` are unchanged.
    Rotated(IdpServiceAccountCredentials),
    /// No account with that `client_id` in this tenant scope.
    NotFound,
}

/// A stored service account. No secret is retained: the plugin mints one per
/// provision / rotate and passes it directly into the returned credential
/// bundle, exactly as the contract requires of a real provider.
#[domain_model]
#[derive(Clone)]
pub struct StoredAccount {
    pub client_id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub subject_id: Uuid,
}

/// In-memory per-tenant user and service-account cache that backs every `IdpPluginClient` method on this plugin.
#[domain_model]
pub struct Service {
    users: Mutex<HashMap<Uuid, HashMap<Uuid, IdpUser>>>,
    /// Per-tenant machine identities keyed by the plugin-assigned
    /// `client_id`. Separate from `users`: a service account is an
    /// `IdP` client, not a user row, and the two are addressed and
    /// authorized independently.
    accounts: Mutex<HashMap<Uuid, HashMap<String, StoredAccount>>>,
}

impl Service {
    #[must_use]
    pub fn new() -> Self {
        Self {
            users: Mutex::new(HashMap::new()),
            accounts: Mutex::new(HashMap::new()),
        }
    }

    /// Build the deterministic echo payload returned from
    /// `provision_tenant`'s `IdpProvisionResult::metadata`.
    ///
    /// Surfaces every field a real provider would normally bind to the
    /// tenant -- `tenant_id`, `tenant_name`, chained `tenant_type`,
    /// the `root` / `child` discriminator (with `parent_id` on the
    /// child arm), and any client-supplied `provisioning_metadata`
    /// echoed back verbatim. The shape is pure-function of the input
    /// request so cross-restart E2E suites can pin byte-for-byte
    /// equality, and AM's `Some(metadata)` activation branch is
    /// exercised on every create-tenant flow.
    #[must_use]
    pub fn echo_tenant_metadata(req: &IdpProvisionTenantRequest) -> Value {
        // `IdpProvisionTarget` is `#[non_exhaustive]`; the wildcard
        // arm collapses any future variant to the `Child` shape with a
        // null `parent_id` so additions to the SDK enum cannot break
        // the echo plugin until it is intentionally taught about them.
        let (target, parent_id) = match &req.target {
            IdpProvisionTarget::Root => ("root", Value::Null),
            IdpProvisionTarget::Child { parent_id } => ("child", json!(parent_id)),
            _ => ("unknown", Value::Null),
        };
        json!({
            "echo": true,
            "tenant_id": req.tenant_id,
            "tenant_name": req.name,
            "tenant_type": req.tenant_type.as_ref(),
            "target": target,
            "parent_id": parent_id,
            "provisioning_metadata": req.metadata.clone().unwrap_or(Value::Null),
        })
    }

    /// Build the echo `IdpUser` returned from `provision_user`.
    ///
    /// The IdP-issued `id` is derived deterministically from the
    /// tenant scope and the supplied username via `UUIDv5` so repeated
    /// calls for the same `(tenant_id, username)` pair return the
    /// same identifier. This matches what real providers expose
    /// (stable user UUID per tenant scope) and keeps E2E assertions
    /// reproducible across server restarts.
    #[must_use]
    pub fn echo_user(tenant_id: Uuid, payload: &IdpNewUser) -> IdpUser {
        let namespace = Uuid::new_v5(&Uuid::NAMESPACE_DNS, tenant_id.as_bytes());
        let id = Uuid::new_v5(&namespace, payload.username.as_bytes());

        let mut user = IdpUser::new(id, payload.username.clone());
        if let Some(email) = &payload.email {
            user = user.with_email(email.clone());
        }
        if let Some(display_name) = &payload.display_name {
            user = user.with_display_name(display_name.clone());
        }
        if let Some(first_name) = &payload.first_name {
            user = user.with_first_name(first_name.clone());
        }
        if let Some(last_name) = &payload.last_name {
            user = user.with_last_name(last_name.clone());
        }
        user
    }

    /// Record a provisioned user in the per-tenant cache and return
    /// the projection AM consumers see. Idempotent: re-provisioning
    /// the same `(tenant_id, username)` pair overwrites the previous
    /// row with the freshly-echoed payload.
    pub fn record_user(&self, tenant_id: Uuid, user: IdpUser) {
        self.users
            .lock()
            .entry(tenant_id)
            .or_default()
            .insert(user.id, user);
    }

    /// Forget a user in this tenant scope. Returns `true` if the user
    /// was present, mirroring the "removed vs already-absent"
    /// distinction the `IdP` contract does NOT propagate to AM (AM
    /// treats both as success).
    pub fn forget_user(&self, tenant_id: Uuid, user_id: Uuid) -> bool {
        let mut guard = self.users.lock();
        let Some(scope) = guard.get_mut(&tenant_id) else {
            return false;
        };
        let removed = scope.remove(&user_id).is_some();
        if scope.is_empty() {
            guard.remove(&tenant_id);
        }
        removed
    }

    /// Apply a JSON Merge Patch to a stored user and return the
    /// post-update projection. Mirrors what a real provider's admin API
    /// exposes: omitted fields are left untouched, `Some(None)` clears a
    /// nullable profile field, and a `username` rename is rejected when
    /// it collides with another login in the same tenant scope. The
    /// `password` field is write-only and not part of the echo
    /// projection, so it is accepted and ignored by the in-memory store.
    pub fn apply_user_patch(
        &self,
        tenant_id: Uuid,
        user_id: Uuid,
        patch: &IdpUserPatch,
    ) -> UpdateUserOutcome {
        let mut guard = self.users.lock();
        let Some(scope) = guard.get_mut(&tenant_id) else {
            return UpdateUserOutcome::NotFound;
        };
        if !scope.contains_key(&user_id) {
            return UpdateUserOutcome::NotFound;
        }
        // Uniqueness guard runs before the mutable borrow: a rename that
        // collides with a *different* user's login is a conflict.
        if let Some(new_username) = &patch.username
            && scope
                .iter()
                .any(|(id, u)| *id != user_id && &u.username == new_username)
        {
            return UpdateUserOutcome::UsernameTaken;
        }
        let Some(user) = scope.get_mut(&user_id) else {
            // Unreachable after the `contains_key` guard above, but this
            // keeps the mutable borrow total without an `expect`.
            return UpdateUserOutcome::NotFound;
        };
        if let Some(username) = &patch.username {
            user.username.clone_from(username);
        }
        if let Some(email) = &patch.email {
            user.email.clone_from(email);
        }
        if let Some(display_name) = &patch.display_name {
            user.display_name.clone_from(display_name);
        }
        if let Some(first_name) = &patch.first_name {
            user.first_name.clone_from(first_name);
        }
        if let Some(last_name) = &patch.last_name {
            user.last_name.clone_from(last_name);
        }
        UpdateUserOutcome::Updated(user.clone())
    }

    /// Returns the full per-tenant user snapshot. Filtering / ordering /
    /// pagination are applied by the caller (`client.rs::list_users`) over
    /// this snapshot. See [`crate::domain::client::list_users`] for the
    /// `FilterNode<IdpUserFilterField>` walker + cursor semantics.
    #[must_use]
    pub fn snapshot_users(&self, tenant_id: Uuid) -> Vec<IdpUser> {
        self.users
            .lock()
            .get(&tenant_id)
            .map(|scope| scope.values().cloned().collect())
            .unwrap_or_default()
    }

    // ---- Service accounts ------------------------------------------

    /// Derive the client id for a `(tenant_id, name)` pair.
    ///
    /// Follows the `svc-<tenant_id>-<name>` convention real adapters
    /// commonly document. It is only a convention: nothing in AM parses
    /// a client id, and callers correlate a submitted name to its id
    /// through the listing instead.
    #[must_use]
    pub fn echo_client_id(tenant_id: Uuid, name: &str) -> String {
        format!("svc-{tenant_id}-{name}")
    }

    /// Characters permitted in a service-account name.
    ///
    /// The unreserved set from RFC 3986 — safe to embed in a single URI
    /// path segment without encoding, which is what keeps the minted
    /// account addressable by the item routes.
    #[must_use]
    fn is_addressable_name_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~')
    }

    /// Mint a fresh secret.
    ///
    /// Random, never derived from the account's identity. An earlier
    /// revision derived this from `(client_id, generation)`, which made
    /// every secret reconstructible by anyone who could read a client id
    /// from the listing — a test double still must not hand out a
    /// guessable credential, because a deployment that selects this
    /// plugin gets real `client_credentials` behaviour from it.
    ///
    /// Two v4 UUIDs supply the entropy (`uuid` is already a dependency
    /// and draws from the OS CSPRNG). The `static-idp-secret-` prefix is
    /// deliberately kept: it is not secret, and it lets assertions and
    /// operators recognise a value minted here.
    #[must_use]
    pub fn mint_secret() -> String {
        format!(
            "static-idp-secret-{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        )
    }

    /// Build a one-time credential bundle from stored identity plus a freshly
    /// minted secret. The `token_url` is a fixed placeholder: this plugin runs
    /// no OAuth endpoint, and nothing in AM or the E2E suite dials it — it
    /// exists so the response shape matches a real provider's.
    #[must_use]
    fn credentials_for(account: &StoredAccount, secret: String) -> IdpServiceAccountCredentials {
        IdpServiceAccountCredentials::new(
            account.client_id.clone(),
            SecretString::from(secret),
            "http://static-idp.localhost/realms/static/protocol/openid-connect/token".to_owned(),
            account.subject_id,
        )
    }

    /// Create a `client_credentials` account owned by `tenant_id`.
    ///
    /// Enforces the contract's `(tenant_id, name)` uniqueness over
    /// **live** accounts: a name already in use is refused and the
    /// existing account is left untouched, so at most one live entry
    /// per tenant carries a given name and the name stays a usable
    /// correlation key. The whole map is held under one lock for the
    /// check-then-insert, which is the in-process equivalent of the
    /// atomicity the contract demands of a real adapter.
    pub fn provision_account(
        &self,
        tenant_id: Uuid,
        name: &str,
        scopes: Vec<String>,
    ) -> ProvisionAccountOutcome {
        // The SDK leaves charset and length to the adapter, and AM
        // deliberately declines to guess them. This plugin embeds the name
        // in `svc-<tenant_id>-<name>` and the item routes address that id
        // as a single path segment, so a name carrying `/` (or other
        // reserved characters) would mint an account that can never be
        // rotated or revoked while permanently holding its name. Reject it
        // instead, before anything is stored.
        if name.is_empty() || !name.chars().all(Self::is_addressable_name_char) {
            return ProvisionAccountOutcome::NameNotAddressable;
        }
        let mut guard = self.accounts.lock();
        let scope = guard.entry(tenant_id).or_default();
        if scope.values().any(|p| p.name == name) {
            return ProvisionAccountOutcome::NameTaken;
        }
        let client_id = Self::echo_client_id(tenant_id, name);
        // Stable subject id per `(tenant_id, name)`, mirroring how
        // `echo_user` derives an IdP-issued user UUID — the subject is
        // what RBAC bindings reference, so it must survive a rotation
        // and be reproducible across restarts.
        //
        // The namespace carries a `service-account:` discriminator so
        // this id space is disjoint from `echo_user`'s. Without it, a
        // user named exactly `svc-<tenant_id>-<name>` would derive the
        // SAME uuid as the service account named `<name>` in that
        // tenant, silently conflating two subjects in RBAC bindings.
        let namespace = Uuid::new_v5(
            &Uuid::NAMESPACE_DNS,
            format!("service-account:{tenant_id}").as_bytes(),
        );
        let subject_id = Uuid::new_v5(&namespace, client_id.as_bytes());
        let account = StoredAccount {
            client_id: client_id.clone(),
            name: name.to_owned(),
            scopes,
            subject_id,
        };
        let credentials = Self::credentials_for(&account, Self::mint_secret());
        scope.insert(client_id, account);
        ProvisionAccountOutcome::Created(credentials)
    }

    /// Regenerate the addressed account's secret without changing its
    /// identity. A `client_id` absent from **this tenant's** scope is
    /// `NotFound`, so one tenant can never rotate another's account
    /// even given its exact client id.
    pub fn rotate_account_secret(&self, tenant_id: Uuid, client_id: &str) -> RotateAccountOutcome {
        let guard = self.accounts.lock();
        let Some(scope) = guard.get(&tenant_id) else {
            return RotateAccountOutcome::NotFound;
        };
        let Some(account) = scope.get(client_id) else {
            return RotateAccountOutcome::NotFound;
        };
        RotateAccountOutcome::Rotated(Self::credentials_for(account, Self::mint_secret()))
    }

    /// Forget an account in this tenant scope. Returns `true` when it
    /// was present. The caller maps absence to the contract's
    /// `NotFound`, which AM then treats as success-equivalent — plugins
    /// map vendor outcomes, AM decides what they mean.
    pub fn forget_account(&self, tenant_id: Uuid, client_id: &str) -> bool {
        let mut guard = self.accounts.lock();
        let Some(scope) = guard.get_mut(&tenant_id) else {
            return false;
        };
        let removed = scope.remove(client_id).is_some();
        if scope.is_empty() {
            guard.remove(&tenant_id);
        }
        removed
    }

    /// Snapshot the tenant's accounts as contract summaries, ordered by
    /// `name` so the unpaginated listing is stable across calls (the
    /// backing map's iteration order is not). Carries no secret.
    #[must_use]
    pub fn snapshot_accounts(&self, tenant_id: Uuid) -> Vec<IdpServiceAccountSummary> {
        let mut summaries: Vec<IdpServiceAccountSummary> = self
            .accounts
            .lock()
            .get(&tenant_id)
            .map(|scope| {
                scope
                    .values()
                    .map(|p| {
                        IdpServiceAccountSummary::new(
                            p.client_id.clone(),
                            // Reported verbatim: this is the only
                            // contractual bridge from a submitted name to
                            // an adapter-assigned client id.
                            p.name.clone(),
                            true,
                            p.scopes.clone(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        summaries.sort_by(|a, b| a.name.cmp(&b.name));
        summaries
    }
}

impl Default for Service {
    fn default() -> Self {
        Self::new()
    }
}
