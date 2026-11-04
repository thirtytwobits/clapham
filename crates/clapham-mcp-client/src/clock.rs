//! Copyright (c) Scott A Dixon
//! The monotonic clock and timers that MCP operation deadlines run on.
use std::{future::Future, pin::Pin, time::Duration};
use tokio::time::Instant;

/// A timer future returned by [`Clock::sleep_until`].
pub type Sleep = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// The host's monotonic clock and timers. [`McpConfig::new`](crate::McpConfig::new) uses
/// [`TokioClock`]; a host whose tests advance their own clock supplies it in
/// [`McpConfig::clock`](crate::McpConfig::clock).
pub trait Clock: Send + Sync {
    /// The current instant.
    fn now(&self) -> Instant;
    /// Completes once [`Clock::now`] reaches `deadline`.
    fn sleep_until(&self, deadline: Instant) -> Sleep;
}

/// Tokio's clock and timers.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioClock;
impl Clock for TokioClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep_until(&self, deadline: Instant) -> Sleep {
        Box::pin(tokio::time::sleep_until(deadline))
    }
}

/// A deadline elapsed before its future completed.
#[derive(Debug)]
pub(crate) struct Elapsed;

/// `clock.now() + duration`. Operation deadlines are at most one hour.
pub(crate) fn deadline_after(clock: &dyn Clock, duration: Duration) -> Instant {
    clock.now() + duration
}

/// Runs `future` until `deadline` on `clock`. A future that is ready at the deadline completes.
pub(crate) async fn timeout_at<F: Future>(
    clock: &dyn Clock,
    deadline: Instant,
    future: F,
) -> Result<F::Output, Elapsed> {
    let timer = clock.sleep_until(deadline);
    tokio::pin!(future);
    tokio::select! {
        biased;
        output = &mut future => Ok(output),
        () = timer => Err(Elapsed),
    }
}
