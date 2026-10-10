#![allow(clippy::unwrap_used, clippy::use_debug)]

//! Wake on a subject: tell the worker *what* changed, not just *that*
//! something did.
//!
//! A gear keeps a computed price per product. Many request handlers change
//! products; one background worker recomputes prices. With a bare `Notify`
//! the worker learns only that something changed and has to rescan every
//! product. With a [`Signal`] each producer names the product it touched, and
//! the worker recomputes exactly those.
//!
//! Two properties keep the worker cheap:
//!
//! - The same product signalled again before the worker gets to it is one
//!   piece of work, not two, and one wakeup, not two.
//! - A wakeup happens only when something was actually added, so a worker that
//!   returns `Idle` after taking an empty set is not woken again for nothing.
//!
//! This is the many-to-one shape. When each subject has its own long-lived
//! worker, give each worker its own `Notify` instead.
//!
//! Run:
//!   cargo run -p cf-gears-toolkit-taskward --example `wake_on_subject`

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use toolkit_taskward::{Directive, Signal, WorkerAction, WorkerBuilder};

type ProductId = u64;

struct PriceRecalculator {
    changed: Arc<Signal<ProductId>>,
    passes: Arc<AtomicU32>,
}

impl WorkerAction for PriceRecalculator {
    type Payload = ();
    type Error = Infallible;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, Infallible> {
        let pass = self.passes.fetch_add(1, Ordering::SeqCst) + 1;
        let mut products = self.changed.take();
        products.sort_unstable();
        println!("pass {pass}: recompute prices for products {products:?}");
        // Simulate the recomputation; producers keep signalling meanwhile.
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(Directive::idle())
    }
}

#[tokio::main]
async fn main() {
    let cancel = CancellationToken::new();
    let changed = Arc::new(Signal::<ProductId>::new());
    let passes = Arc::new(AtomicU32::new(0));

    // Signals sent before the worker starts are kept, not lost.
    println!("handlers touch products 7, 7, 7, 9");
    for product in [7, 7, 7, 9] {
        changed.signal(product);
    }

    let worker = WorkerBuilder::new("price-recalculator", cancel.clone())
        .notifier(changed.notifier())
        .build(PriceRecalculator {
            changed: Arc::clone(&changed),
            passes: Arc::clone(&passes),
        });
    let handle = tokio::spawn(worker.run());

    tokio::time::sleep(Duration::from_millis(10)).await;
    // The worker is mid-pass: these land in the next one, still deduplicated.
    println!("handlers touch products 3, 9, 3 while pass 1 is running");
    for product in [3, 9, 3] {
        changed.signal(product);
    }

    // Nothing is signalled from here on, so the worker stays idle.
    tokio::time::sleep(Duration::from_millis(300)).await;
    cancel.cancel();
    handle.await.unwrap();

    println!(
        "{} passes for 7 signals - no pass ran without a subject",
        passes.load(Ordering::SeqCst)
    );
}
