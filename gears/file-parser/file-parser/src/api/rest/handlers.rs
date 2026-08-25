// False positives from #[axum::debug_handler] macro expansion
#![allow(clippy::items_after_statements)]

use axum::body::Body;
use axum::extract::{Extension, Query};
use axum::http::HeaderMap;
use axum::response::Response;
use bytes::Bytes;
use futures_util::stream;
use std::convert::Infallible;
use tracing::{field::Empty, info};

use crate::api::rest::dto::{
    FileParserInfoDto, ParseLocalFileRequest, ParsedDocResponseDto, ParsedDocumentDto, UploadQuery,
};
use crate::domain::error::DomainError;
use crate::domain::markdown::MarkdownRenderer;
use crate::domain::service::FileParserService;
use file_parser_sdk::Detection;
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

/// Query parameter for `render_markdown` flag
#[derive(Debug, serde::Deserialize)]
pub struct RenderMarkdownQuery {
    #[serde(default)]
    pub render_markdown: Option<bool>,
}

/// Get information about available file parsers
#[tracing::instrument(
    skip(svc, _ctx),
    fields(
        request_id = Empty
    )
)]
#[axum::debug_handler]
pub async fn get_parser_info(
    Extension(_ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<FileParserService>>,
) -> ApiResult<JsonBody<FileParserInfoDto>> {
    info!("Getting file parser info");

    let info = svc.info();

    Ok(Json(FileParserInfoDto::from(info)))
}

/// Parse a file from a local path
#[tracing::instrument(
    skip(svc, req_body, _ctx, query),
    fields(
        file_path = %req_body.file_path,
        render_markdown = ?query.render_markdown,
        request_id = Empty
    )
)]
#[axum::debug_handler]
pub async fn parse_local(
    Extension(_ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<FileParserService>>,
    Query(query): Query<RenderMarkdownQuery>,
    Json(req_body): Json<ParseLocalFileRequest>,
) -> ApiResult<JsonBody<ParsedDocResponseDto>> {
    let render_md = query.render_markdown.unwrap_or(false);

    info!(
        file_path = %req_body.file_path,
        render_markdown = render_md,
        "Parsing file from local path"
    );

    let path = std::path::Path::new(&req_body.file_path);
    let document = svc.parse_local(path).await?;

    // Optionally render markdown
    let markdown = if render_md {
        Some(MarkdownRenderer::render(&document))
    } else {
        None
    };

    let response = ParsedDocResponseDto {
        document: ParsedDocumentDto::from(document),
        markdown,
    };

    Ok(Json(response))
}

/// Upload and parse a file
#[tracing::instrument(
    skip(svc, body, _ctx, query, headers),
    fields(
        filename = ?query.filename,
        render_markdown = ?query.render_markdown,
        size = body.len(),
        request_id = Empty
    )
)]
#[axum::debug_handler]
pub async fn upload_and_parse(
    Extension(_ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<FileParserService>>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<JsonBody<ParsedDocResponseDto>> {
    let render_md = query.render_markdown.unwrap_or(false);
    let filename_opt = query.filename.as_deref();

    // Extract Content-Type from headers
    let content_type_str = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    info!(
        filename = ?filename_opt,
        content_type = ?content_type_str,
        render_markdown = render_md,
        size = body.len(),
        "Uploading and parsing raw file bytes"
    );

    if body.is_empty() {
        return Err(DomainError::invalid_request(
            "Empty request body, expected file bytes".to_owned(),
        )
        .into());
    }

    // REST is a public, untrusted-caller entry point — always detect,
    // regardless of `Detection::Skip` (SDK/ClientHub-only, see
    // `file_parser_sdk::ParseBytesRequest`).
    let document = svc
        .parse_bytes(
            filename_opt,
            content_type_str.as_deref(),
            body,
            Detection::Auto,
        )
        .await?;

    // Optionally render markdown
    let markdown = if render_md {
        Some(MarkdownRenderer::render(&document))
    } else {
        None
    };

    let response = ParsedDocResponseDto {
        document: ParsedDocumentDto::from(document),
        markdown,
    };

    Ok(Json(response))
}

/// Parse a local file and stream Markdown response
#[tracing::instrument(
    skip(svc, req_body, _ctx),
    fields(
        file_path = %req_body.file_path,
        request_id = Empty
    )
)]
#[axum::debug_handler]
pub async fn parse_local_markdown(
    Extension(_ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<FileParserService>>,
    Json(req_body): Json<ParseLocalFileRequest>,
) -> ApiResult<Response> {
    info!(
        file_path = %req_body.file_path,
        "Parsing file from local path and streaming Markdown"
    );

    let path = std::path::Path::new(&req_body.file_path);
    let document = svc.parse_local(path).await?;

    // Create streaming response - render_iter takes ownership of document
    let stream = stream::iter(
        MarkdownRenderer::render_iter(document)
            .map(|chunk| Ok::<Bytes, Infallible>(Bytes::from(chunk))),
    );

    let body = Body::from_stream(stream);
    let mut resp = Response::new(body);
    *resp.status_mut() = axum::http::StatusCode::OK;
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/markdown; charset=utf-8"),
    );

    Ok(resp)
}

/// Upload and parse a file, streaming Markdown response
#[tracing::instrument(
    skip(svc, multipart, _ctx),
    fields(
        request_id = Empty
    )
)]
#[axum::debug_handler]
pub async fn upload_and_parse_markdown(
    Extension(_ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<FileParserService>>,
    mut multipart: axum::extract::Multipart,
) -> ApiResult<Response> {
    info!("Uploading and parsing file, streaming Markdown");

    // Extract the first file field (reuse logic from upload_and_parse)
    let mut file_name: Option<String> = None;
    let mut file_bytes: Option<bytes::Bytes> = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        CanonicalError::from(DomainError::invalid_request(format!(
            "Multipart error: {e}"
        )))
    })? {
        let field_name = field.name().unwrap_or("").to_owned();
        if field_name == "file" {
            file_name = field.file_name().map(str::to_owned);
            file_bytes = Some(field.bytes().await.map_err(|e| {
                CanonicalError::from(DomainError::io_error(format!("Failed to read file: {e}")))
            })?);
            break;
        }
    }

    let file_name = file_name.ok_or_else(|| {
        CanonicalError::from(DomainError::invalid_request(
            "No file field found in multipart request",
        ))
    })?;

    let file_bytes = file_bytes.ok_or_else(|| {
        CanonicalError::from(DomainError::invalid_request(
            "No file data found in multipart request",
        ))
    })?;

    info!(
        file_name = %file_name,
        size = file_bytes.len(),
        "Processing uploaded file for Markdown streaming"
    );

    let document = svc
        .parse_bytes(Some(&file_name), None, file_bytes, Detection::Auto)
        .await?;

    // Create streaming response - render_iter takes ownership of document
    let stream = stream::iter(
        MarkdownRenderer::render_iter(document)
            .map(|chunk| Ok::<Bytes, Infallible>(Bytes::from(chunk))),
    );

    let body = Body::from_stream(stream);
    let mut resp = Response::new(body);
    *resp.status_mut() = axum::http::StatusCode::OK;
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/markdown; charset=utf-8"),
    );

    Ok(resp)
}
