use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use super::*;

/// Runs a future that is ready at once, without an async runtime.
fn run<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("the stub client answers at once"),
    }
}

/// Minimal in-test implementation of the client trait.
struct StubClient;

#[async_trait]
impl ConstructClientV1 for StubClient {
    async fn create_note(
        &self,
        _ctx: &SecurityContext,
        note: NewFoundationNote,
    ) -> Result<FoundationNote, CanonicalError> {
        Ok(FoundationNote::new(Uuid::nil(), Uuid::nil(), note.text))
    }

    async fn get_note(
        &self,
        _ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<FoundationNote, CanonicalError> {
        Ok(FoundationNote::new(id, Uuid::nil(), String::new()))
    }
}

/// `ClientHub` stores the client as `Arc<dyn ConstructClientV1>`, so both
/// operations must work through a trait object.
#[test]
fn both_operations_work_through_a_trait_object() {
    let client: Arc<dyn ConstructClientV1> = Arc::new(StubClient);
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .build()
        .unwrap();

    let created = run(client.create_note(&ctx, NewFoundationNote::new("hello"))).unwrap();
    assert_eq!(created.text, "hello");

    let id = Uuid::new_v4();
    let read = run(client.get_note(&ctx, id)).unwrap();
    assert_eq!(read.id, id);
}
