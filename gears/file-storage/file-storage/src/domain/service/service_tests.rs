use crate::domain::error::DomainError;
use crate::domain::service::content_verb;
use crate::infra::signed_url::Op;

#[test]
fn content_verb_maps_ops_and_rejects_multipart_part() {
    // `Op::MultipartPart` has no two-segment mapping; `sign_url` must reject it
    // rather than mint a URL that would 404 on the sidecar.
    let err = content_verb(Op::MultipartPart).unwrap_err();
    assert!(matches!(err, DomainError::InternalError), "got {err:?}");

    assert_eq!(content_verb(Op::Get).unwrap(), "download");
    assert_eq!(content_verb(Op::Put).unwrap(), "upload");
}
