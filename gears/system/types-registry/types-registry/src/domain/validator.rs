//! Freshness validators (SPEC §8.5): computed per read, never stored.

use aws_lc_rs::digest::{Context, SHA256};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use toolkit_macros::domain_model;

use crate::domain::selection::{EntityField, FieldSelection};

/// ponytail: ceiling C7 — no visibility or availability inputs yet; P1 adds them
/// under a new version, which a v1 token then never matches.
const VERSION: u8 = 1;

/// A 128-bit digest of what a representation depends on. Equality is the only
/// operation.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Validator([u8; 16]);

impl Validator {
    /// Instances pass no fingerprint: they have no derived form.
    #[must_use]
    pub fn compute(
        resource_version: i64,
        resolution_fingerprint: Option<&[u8]>,
        selection: FieldSelection,
    ) -> Self {
        // TODO(P1): subject visibility-chain version, Context Tenant availability-chain
        // version, routing generation.
        let mut ctx = Context::new(&SHA256);
        ctx.update(&[VERSION]);
        ctx.update(&resource_version.to_be_bytes());
        match resolution_fingerprint {
            None => ctx.update(&[0]),
            Some(fingerprint) => {
                ctx.update(&[1]);
                update_prefixed(&mut ctx, fingerprint);
            }
        }
        update_selection(&mut ctx, selection);
        let mut digest = [0; 16];
        digest.copy_from_slice(&ctx.finish().as_ref()[..16]);
        Self(digest)
    }

    /// base64url of `version || digest` (DESIGN §3.3).
    #[must_use]
    pub fn encode(self) -> String {
        URL_SAFE_NO_PAD.encode([&[VERSION], self.0.as_slice()].concat())
    }

    /// `None` for anything but a well-formed current-version token, which the
    /// caller then answers with a full result (DESIGN §3.3).
    #[must_use]
    pub fn decode(token: &str) -> Option<Self> {
        let wire = URL_SAFE_NO_PAD.decode(token).ok()?;
        let (&version, digest) = wire.split_first()?;
        if version != VERSION {
            return None;
        }
        Some(Self(digest.try_into().ok()?))
    }
}

/// A read's condition: `*`, or the opaque validators the caller holds.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IfNoneMatch {
    Any,
    Validators(Vec<String>),
}

impl IfNoneMatch {
    #[must_use]
    pub fn matches(&self, current: Validator) -> bool {
        match self {
            Self::Any => true,
            Self::Validators(tokens) => tokens
                .iter()
                .any(|token| Validator::decode(token) == Some(current)),
        }
    }

    /// The longest token, for the caller's size bound.
    pub(crate) fn longest(&self) -> usize {
        match self {
            Self::Any => 0,
            Self::Validators(tokens) => tokens.iter().map(String::len).max().unwrap_or(0),
        }
    }
}

/// Length-prefixed, so no two field splits digest alike.
fn update_prefixed(ctx: &mut Context, bytes: &[u8]) {
    ctx.update(&(bytes.len() as u64).to_be_bytes());
    ctx.update(bytes);
}

/// [`update_prefixed`] over [`FieldSelection::canonical`], streamed: a batch
/// digests one selection per row, so this allocates nothing.
fn update_selection(ctx: &mut Context, selection: FieldSelection) {
    let names = || selection.fields().map(EntityField::name);
    let len = names().map(str::len).sum::<usize>() + names().count().saturating_sub(1);
    ctx.update(&(len as u64).to_be_bytes());
    for (index, name) in names().enumerate() {
        if index > 0 {
            ctx.update(b",");
        }
        ctx.update(name.as_bytes());
    }
}

#[cfg(test)]
#[path = "validator_tests.rs"]
mod validator_tests;
