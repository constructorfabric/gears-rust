use super::*;
use crate::body::{sized_stream_body, stream_body};

fn chunks(parts: &[&'static str]) -> ProxyBody {
    let items: Vec<Result<Bytes, BoxError>> = parts
        .iter()
        .map(|p| Ok(Bytes::from_static(p.as_bytes())))
        .collect();
    stream_body(futures_util::stream::iter(items))
}

async fn next_data(body: &mut ProxyBody) -> Option<Bytes> {
    let frame = body.frame().await?.ok()?;
    frame.into_data().ok()
}

async fn collect(body: ProxyBody) -> Vec<u8> {
    body.collect()
        .await
        .map(|c| c.to_bytes().to_vec())
        .unwrap_or_default()
}

#[tokio::test]
async fn a_body_read_whole_within_the_limit_resends_the_copy() {
    let (mut body, tee) = replay_tee(chunks(&["ab", "cd"]), 4);
    while next_data(&mut body).await.is_some() {}
    assert!(tee.is_replayable());
    let resent = tee.resume().expect("whole body copied");
    assert_eq!(resent.size_hint().exact(), Some(4));
    assert_eq!(collect(resent).await, b"abcd");
}

#[tokio::test]
async fn a_resend_continues_where_the_first_attempt_stopped() {
    let inner = sized_stream_body(
        futures_util::stream::iter(["ab", "cd", "ef"].map(|p| Ok::<_, BoxError>(Bytes::from(p)))),
        6,
    );
    let (mut body, tee) = replay_tee(inner, 1024);
    assert_eq!(next_data(&mut body).await.as_deref(), Some(&b"ab"[..]));
    let resent = tee.resume().expect("copy complete so far");
    // The length check of the sized body still covers the whole body.
    assert_eq!(resent.size_hint().exact(), Some(6));
    assert_eq!(collect(resent).await, b"abcdef");
    // The first attempt's body must fail, not end short.
    assert!(!body.is_end_stream());
    assert!(body.frame().await.is_some_and(|f| f.is_err()));
}

#[tokio::test]
async fn the_copy_is_freed_once_the_body_passes_the_limit() {
    let (mut body, tee) = replay_tee(chunks(&["abc", "de", "f"]), 4);
    next_data(&mut body).await;
    assert!(tee.retained_bytes() >= 3);
    next_data(&mut body).await;
    assert_eq!(tee.retained_bytes(), 0);
    assert!(!tee.is_replayable());
    // The body itself keeps streaming.
    assert_eq!(next_data(&mut body).await.as_deref(), Some(&b"f"[..]));
    assert!(tee.resume().is_none());
}

#[tokio::test]
async fn an_error_or_trailers_end_the_copy() {
    let items: Vec<Result<Bytes, BoxError>> =
        vec![Ok(Bytes::from_static(b"a")), Err("gone".into())];
    let (mut body, tee) = replay_tee(stream_body(futures_util::stream::iter(items)), 16);
    while body.frame().await.is_some() {}
    assert!(tee.resume().is_none());

    let frames = futures_util::stream::iter([
        Ok::<_, BoxError>(Frame::data(Bytes::from_static(b"a"))),
        Ok(Frame::trailers(http::HeaderMap::new())),
    ]);
    let inner = http_body_util::StreamBody::new(frames).boxed_unsync();
    let (mut body, tee) = replay_tee(inner, 16);
    while body.frame().await.is_some() {}
    assert!(tee.resume().is_none());
}

#[tokio::test]
async fn release_frees_the_copy() {
    let (body, tee) = replay_tee(chunks(&["abcd"]), 16);
    let _ = collect(body).await;
    assert!(tee.retained_bytes() >= 4);
    tee.release();
    assert_eq!(tee.retained_bytes(), 0);
    assert!(tee.resume().is_none());
}

#[tokio::test]
async fn a_zero_limit_never_copies() {
    let (mut body, tee) = replay_tee(chunks(&["a"]), 0);
    let _ = body.frame().await;
    assert!(!tee.is_replayable());
}

#[tokio::test]
async fn the_copy_never_holds_more_than_the_limit() {
    // 1 MiB limit, 1000-byte chunks: doubling would reach 2,048,000 bytes.
    const LIMIT: usize = 1 << 20;
    const CHUNKS: usize = 1048;
    let parts: Vec<Result<Bytes, BoxError>> = (0..CHUNKS)
        .map(|_| Ok(Bytes::from(vec![b'x'; 1000])))
        .collect();
    let (mut body, tee) = replay_tee(stream_body(futures_util::stream::iter(parts)), LIMIT);
    while next_data(&mut body).await.is_some() {
        assert!(tee.retained_bytes() <= LIMIT, "{}", tee.retained_bytes());
    }
    assert!(tee.is_replayable());
    let resent = tee.resume().expect("within the limit");
    assert_eq!(collect(resent).await.len(), CHUNKS * 1000);
}
