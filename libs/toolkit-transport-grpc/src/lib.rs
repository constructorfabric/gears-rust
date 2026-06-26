#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
mod backoff;
pub mod client;
pub mod internal_auth;
pub mod rpc_retry;

#[cfg(windows)]
pub mod windows_named_pipe;

#[cfg(windows)]
pub use windows_named_pipe::{NamedPipeConnection, NamedPipeIncoming, create_named_pipe_incoming};

pub use internal_auth::{
    InternalAuthInterceptor, attach_internal_token_grpc, extract_internal_token_grpc,
};

pub const SECCTX_METADATA_KEY: &str = "x-secctx-bin";

use tonic::Status;
use tonic::metadata::{MetadataMap, MetadataValue};
use toolkit_security::{SecurityContext, decode_bin, encode_bin};

/// Encode `SecurityContext` into gRPC metadata.
///
/// # Errors
/// Returns `Status::internal` if encoding fails.
pub fn attach_secctx(meta: &mut MetadataMap, ctx: &SecurityContext) -> Result<(), Status> {
    let encoded = encode_bin(ctx).map_err(|e| Status::internal(format!("secctx encode: {e}")))?;

    meta.insert_bin(SECCTX_METADATA_KEY, MetadataValue::from_bytes(&encoded));
    Ok(())
}

/// Decode `SecurityContext` from gRPC metadata.
///
/// # Errors
/// Returns `Status::unauthenticated` if the metadata is missing or decoding fails.
pub fn extract_secctx(meta: &MetadataMap) -> Result<SecurityContext, Status> {
    let raw = meta
        .get_bin(SECCTX_METADATA_KEY)
        .ok_or_else(|| Status::unauthenticated("missing secctx metadata"))?;

    let bytes = raw
        .to_bytes()
        .map_err(|e| Status::unauthenticated(format!("invalid secctx metadata: {e}")))?;

    decode_bin(bytes.as_ref()).map_err(|e| Status::unauthenticated(format!("secctx decode: {e}")))
}
