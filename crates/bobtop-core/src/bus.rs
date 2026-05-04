use tokio::sync::broadcast;

use crate::MetricEvent;

/// Default broadcast channel capacity — chosen to absorb a few seconds of
/// samples for a slow consumer (e.g. a paused TUI) before the broadcast
/// channel starts dropping the oldest message and emitting `RecvError::Lagged`
/// to the receiver.
pub const DEFAULT_BUS_CAPACITY: usize = 256;

/// Pub/sub fan-out for [`MetricEvent`]s.
///
/// Backed by `tokio::sync::broadcast`. Cloning a `DataBus` is cheap — the
/// underlying `Sender` is `Arc`-shared. Each consumer calls
/// [`DataBus::subscribe`] to obtain its own `Receiver`; receivers that lag
/// behind the channel capacity will start receiving `Err(RecvError::Lagged)`.
#[derive(Debug, Clone)]
pub struct DataBus {
    sender: broadcast::Sender<MetricEvent>,
}

impl DataBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Publish an event to all current subscribers.
    ///
    /// Returns the number of receivers the message was sent to. Returns
    /// `Ok(0)` when there are no subscribers — that is the steady state at
    /// startup before the TUI has spun up, so it is **not** treated as an
    /// error.
    pub fn publish<E>(&self, event: E) -> usize
    where
        E: Into<MetricEvent>,
    {
        match self.sender.send(event.into()) {
            Ok(n) => n,
            Err(_no_receivers) => 0,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MetricEvent> {
        self.sender.subscribe()
    }

    pub fn receiver_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl Default for DataBus {
    fn default() -> Self {
        Self::new(DEFAULT_BUS_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::sample::MemorySample;

    fn dummy_mem() -> MemorySample {
        MemorySample {
            timestamp: Instant::now(),
            total_bytes: 1024,
            used_bytes: 256,
            available_bytes: 768,
            swap_total_bytes: 0,
            swap_used_bytes: 0,
            huge_pages: None,
            cached_bytes: 0,
            buffers_bytes: 0,
            free_bytes: 768,
            pressure: None,
        }
    }

    #[tokio::test]
    async fn no_subscribers_is_not_an_error() {
        let bus = DataBus::default();
        assert_eq!(bus.publish(dummy_mem()), 0);
    }

    #[tokio::test]
    async fn subscriber_receives_published_event() {
        let bus = DataBus::default();
        let mut rx = bus.subscribe();
        assert_eq!(bus.publish(dummy_mem()), 1);
        let event = rx.recv().await.expect("recv");
        assert_eq!(event.kind(), "memory");
    }
}
