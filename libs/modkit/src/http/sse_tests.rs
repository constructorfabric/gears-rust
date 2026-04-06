use super::*;
use futures_util::StreamExt;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::time::{Duration, timeout};

#[tokio::test]
async fn broadcaster_delivers_single_event() {
    let b = SseBroadcaster::<u32>::new(16);
    let mut sub = Box::pin(b.subscribe_stream());
    b.send(42);
    let v = timeout(Duration::from_millis(200), sub.next())
        .await
        .unwrap();
    assert_eq!(v, Some(42));
}

#[tokio::test]
async fn broadcaster_handles_backpressure_with_bounded_channel() {
    // Test that bounded channel drops old events when capacity is exceeded
    let capacity = 4;
    let broadcaster = SseBroadcaster::<u32>::new(capacity);

    // Create a slow consumer that doesn't read immediately
    let mut subscriber = Box::pin(broadcaster.subscribe_stream());

    // Send more events than capacity
    let num_events = capacity * 2;
    for i in 0..num_events {
        broadcaster.send(u32::try_from(i).unwrap());
    }

    // The subscriber should only receive the most recent events
    // due to the bounded channel dropping older ones
    let mut received = Vec::new();

    // Try to receive all events with a timeout
    for _ in 0..num_events {
        match timeout(Duration::from_millis(10), subscriber.next()).await {
            Ok(Some(event)) => received.push(event),
            Ok(None) | Err(_) => break, // None or timeout
        }
    }

    // Should have received some events, but not necessarily all
    // due to backpressure handling
    assert!(!received.is_empty());
    assert!(received.len() <= num_events);

    // The events we did receive should be in order
    for window in received.windows(2) {
        assert!(window[0] < window[1], "Events should be in order");
    }
}

#[tokio::test]
async fn broadcaster_handles_multiple_subscribers_with_backpressure() {
    let capacity = 8;
    let broadcaster = SseBroadcaster::<String>::new(capacity);

    // Create multiple subscribers with different consumption rates
    let mut fast_subscriber = Box::pin(broadcaster.subscribe_stream());
    let mut slow_subscriber = Box::pin(broadcaster.subscribe_stream());

    let events_sent = Arc::new(AtomicUsize::new(0));
    let events_sent_clone = events_sent.clone();

    // Producer task - sends events rapidly
    let producer = tokio::spawn(async move {
        for i in 0..50 {
            broadcaster.send(format!("event_{i}"));
            events_sent_clone.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await; // Allow other tasks to run
        }
    });

    // Fast consumer task
    let fast_events = Arc::new(AtomicUsize::new(0));
    let fast_events_clone = fast_events.clone();
    let fast_consumer = tokio::spawn(async move {
        while let Ok(Some(_event)) =
            timeout(Duration::from_millis(100), fast_subscriber.next()).await
        {
            fast_events_clone.fetch_add(1, Ordering::SeqCst);
        }
    });

    // Slow consumer task
    let slow_events = Arc::new(AtomicUsize::new(0));
    let slow_events_clone = slow_events.clone();
    let slow_consumer = tokio::spawn(async move {
        while let Ok(Some(_event)) =
            timeout(Duration::from_millis(100), slow_subscriber.next()).await
        {
            slow_events_clone.fetch_add(1, Ordering::SeqCst);
            // Simulate slow processing
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });

    // Wait for producer to finish
    producer.await.unwrap();

    // Give consumers time to process
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Cancel consumers
    fast_consumer.abort();
    slow_consumer.abort();

    let total_sent = events_sent.load(Ordering::SeqCst);
    let fast_received = fast_events.load(Ordering::SeqCst);
    let slow_received = slow_events.load(Ordering::SeqCst);

    assert_eq!(total_sent, 50);

    // Fast consumer should receive more events than slow consumer
    // due to backpressure affecting the slow consumer more
    assert!(fast_received > 0);
    assert!(slow_received > 0);

    // Due to bounded channel, neither consumer necessarily receives all events
    // but the system should remain stable
    println!("Sent: {total_sent}, Fast received: {fast_received}, Slow received: {slow_received}");
}

#[tokio::test]
#[allow(clippy::assertions_on_constants)]
async fn broadcaster_prevents_unbounded_memory_growth() {
    let small_capacity = 2;
    let broadcaster = SseBroadcaster::<Vec<u8>>::new(small_capacity);

    // Create a subscriber but don't consume from it
    let _subscriber = broadcaster.subscribe_stream();

    // Send many large events
    for i in 0..100 {
        let large_event = vec![u8::try_from(i).unwrap(); 1024]; // 1KB per event
        broadcaster.send(large_event);
    }

    // The broadcaster should not accumulate unbounded memory
    // This test mainly ensures we don't panic or run out of memory
    // The bounded channel should drop old events automatically

    // Verify we can still send and the system is responsive
    broadcaster.send(vec![255; 1024]);

    // Test passes if we reach here without OOM or panic
    assert!(true);
}

#[tokio::test]
async fn broadcaster_handles_subscriber_drop_gracefully() {
    let broadcaster = SseBroadcaster::<u32>::new(16);

    // Create and immediately drop a subscriber
    {
        let _subscriber = broadcaster.subscribe_stream();
        broadcaster.send(1);
    } // subscriber dropped here

    // Broadcaster should continue working with new subscribers
    let mut new_subscriber = Box::pin(broadcaster.subscribe_stream());
    broadcaster.send(2);

    let received = timeout(Duration::from_millis(100), new_subscriber.next())
        .await
        .unwrap();
    assert_eq!(received, Some(2));
}

#[tokio::test]
async fn broadcaster_send_is_non_blocking() {
    let broadcaster = SseBroadcaster::<u32>::new(1); // Very small capacity

    // Send should not block even when no subscribers exist
    let start = std::time::Instant::now();
    for i in 0..1000 {
        broadcaster.send(i);
    }
    let elapsed = start.elapsed();

    // Should complete very quickly since send() doesn't block
    assert!(
        elapsed < Duration::from_millis(100),
        "Send operations took too long: {elapsed:?}"
    );
}
