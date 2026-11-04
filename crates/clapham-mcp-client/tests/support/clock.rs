//! Copyright (c) Scott A Dixon
//! A clock that moves only when a test advances it.
use clapham_mcp_client::{Clock, Sleep};
use std::{sync::Arc, time::Duration};
use tokio::{sync::watch, time::Instant};

/// Its timers complete when an advance reaches their deadline.
#[derive(Clone)]
pub struct ManualClock {
    start: Instant,
    elapsed: Arc<watch::Sender<Duration>>,
}
impl ManualClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            elapsed: Arc::new(watch::Sender::new(Duration::ZERO)),
        }
    }
    /// Move the clock forward by `by`, completing every timer the new time reaches.
    pub fn advance(&self, by: Duration) {
        self.elapsed.send_modify(|elapsed| *elapsed += by);
    }
}
impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.start + *self.elapsed.borrow()
    }
    fn sleep_until(&self, deadline: Instant) -> Sleep {
        let start = self.start;
        let mut elapsed = self.elapsed.subscribe();
        Box::pin(async move {
            // The sender lives as long as this receiver's clock.
            let _ = elapsed
                .wait_for(|elapsed| start + *elapsed >= deadline)
                .await;
        })
    }
}
