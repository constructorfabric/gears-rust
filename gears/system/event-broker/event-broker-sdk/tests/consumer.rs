#![cfg(feature = "test-util")]

mod consumer {
    mod common;

    mod batch_handler;
    mod builder;
    mod custom_offset_store;
    #[cfg(feature = "db")]
    mod db_tx;
    mod dlq;
    mod in_memory;
    mod offset_manager;
    mod remote_calls;
    mod routed_handlers;
    mod single_handler;
    mod slow_consumer;
}
