use std::sync::Once;

static PANIC_HOOK_INIT: Once = Once::new();

pub fn init_panic_tracing() {
    PANIC_HOOK_INIT.call_once(|| {
        std::panic::set_hook(Box::new(|panic_info| {
            let backtrace = std::backtrace::Backtrace::force_capture();
            let location = panic_info.location().map_or_else(
                || "unknown location".to_owned(),
                |loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()),
            );
            let payload = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
                (*s).to_owned()
            } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
                s.clone()
            } else {
                "non-string panic payload".to_owned()
            };

            tracing::error!(%location, %payload, %backtrace, "PANIC");
        }));

        tracing::debug!("tracing of panic is initialized");
    });
}
