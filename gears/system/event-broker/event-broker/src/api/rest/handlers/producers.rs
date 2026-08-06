//! `POST /v1/producers`, `GET .../cursors`, `POST .../{id}:reset`
//! (`DESIGN.md:580`). Bodies land with #4346. Note for that registration:
//! matchit cannot mix a path parameter with literal text in one segment, so
//! `{id}:reset` cannot be an axum route template as-is - see
//! `eb-dispatcher-routing`'s `routes/mod.rs` for how the dispatcher's own
//! (proxying) registration works around this by registering bare `{id}` and
//! forwarding the untouched request; a real handler will instead need to
//! split the `:reset` suffix out of the captured `id` value itself.

pub async fn register_producer() {
    todo!("lands with #4346")
}

pub async fn get_producer_cursors() {
    todo!("lands with #4346")
}

pub async fn reset_producer() {
    todo!("lands with #4346")
}
