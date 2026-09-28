mod consumer {
    mod common;
    mod doubles;

    mod batch_handler;
    mod builder;
    mod commit;
    mod custom_offset_store;
    #[cfg(feature = "db")]
    mod db_tx;
    mod dispatch_scope;
    mod dlq;
    mod in_memory;
    mod lifecycle;
    mod offset_manager;
    mod recovery;
    mod remote_calls;
    mod routed_handlers;
    mod runtime_behaviour;
    mod single_handler;
    mod slow_consumer;
    #[cfg(feature = "db")]
    mod transactional;
}
