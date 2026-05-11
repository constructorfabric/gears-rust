#![cfg(feature = "mock")]

mod consumer {
    mod common;

    mod batch_handler;
    mod custom_offset_store;
    #[cfg(feature = "db")]
    mod db_tx;
    mod dlq;
    mod in_memory;
    mod remote_calls;
    mod routed_handlers;
    mod single_handler;
    mod slow_consumer;
}
