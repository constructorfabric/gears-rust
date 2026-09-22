use crate::SecurityContext;
use postcard::Error as PostcardError;
use thiserror::Error;

/// Format version, written as the first byte of every encoded blob.
///
/// [`decode_bin`] accepts this version and no other, so producer and consumer
/// must agree exactly — there is no backward-compatible read path.
///
/// **Bump this whenever [`SecurityContext`]'s serialized field list changes.**
/// postcard is positional and carries no field names, so adding, removing or
/// reordering a field silently changes the layout: an older decoder would then
/// read the new bytes as whatever its own field order says, rather than
/// rejecting them. The version byte is the only thing that turns that into a
/// clean `UnsupportedVersion` error, and nothing derives it automatically.
pub const SECCTX_BIN_VERSION: u8 = 1;

/// Why a [`SecurityContext`] could not be encoded.
#[derive(Debug, Error)]
pub enum SecCtxEncodeError {
    /// postcard could not serialize the context.
    #[error("security context serialization failed: {0:?}")]
    Postcard(#[from] PostcardError),
}

/// Why a blob could not be decoded into a [`SecurityContext`].
#[derive(Debug, Error)]
pub enum SecCtxDecodeError {
    /// The blob carried no bytes at all, so not even a version byte.
    #[error("empty secctx blob")]
    Empty,

    /// The leading version byte is not [`SECCTX_BIN_VERSION`]; the payload is
    /// left unread rather than guessed at.
    #[error("unsupported secctx version: {0}")]
    UnsupportedVersion(u8),

    /// The version matched but the payload did not deserialize.
    #[error("security context deserialization failed: {0:?}")]
    Postcard(#[from] PostcardError),
}

/// Encode `SecurityContext` into a versioned binary blob using `postcard`.
/// This does not do any signing or encryption, it is just a transport format.
///
/// # Errors
/// Returns `SecCtxEncodeError` if postcard serialization fails.
pub fn encode_bin(ctx: &SecurityContext) -> Result<Vec<u8>, SecCtxEncodeError> {
    let mut buf = Vec::with_capacity(64);
    buf.push(SECCTX_BIN_VERSION);

    let payload = postcard::to_allocvec(ctx)?;
    buf.extend_from_slice(&payload);

    Ok(buf)
}

/// Decode `SecurityContext` from a versioned binary blob produced by `encode_bin()`.
///
/// # This does not authenticate anything
///
/// The blob is neither signed nor encrypted, and the only check here is the
/// version byte. Whoever produced these bytes chose the subject, the tenant and
/// the scopes in the context that comes back — so calling this on input a peer
/// supplied is letting that peer pick its own identity.
///
/// The precondition is that the peer was **already authenticated** and the
/// transport is trusted: in-process, or a link where the sender was validated
/// by other means. Never call it on inbound metadata from an unauthenticated
/// caller, and strip `x-secctx-bin` at any boundary where callers are not
/// already authenticated — a header from outside must never reach this
/// function. ADR `cpt-cf-adr-two-plane-auth` keeps cross-process calls off this
/// path entirely: they carry a re-validated bearer token instead.
///
/// # Errors
/// Returns `SecCtxDecodeError::Empty` if the input is empty.
/// Returns `SecCtxDecodeError::UnsupportedVersion` if the version byte is not supported.
/// Returns `SecCtxDecodeError::Postcard` if postcard deserialization fails.
pub fn decode_bin(bytes: &[u8]) -> Result<SecurityContext, SecCtxDecodeError> {
    if bytes.is_empty() {
        return Err(SecCtxDecodeError::Empty);
    }

    let version = bytes[0];
    if version != SECCTX_BIN_VERSION {
        return Err(SecCtxDecodeError::UnsupportedVersion(version));
    }

    let payload = &bytes[1..];

    let ctx: SecurityContext = postcard::from_bytes(payload)?;

    Ok(ctx)
}
