// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Injectable clocks.
//!
//! Every wait in this workspace that expresses a policy (a backoff, an idle
//! timeout, a probe interval, a start deadline) goes through a [`Clock`], so
//! tests drive time by hand instead of sleeping and hoping. Production code
//! uses [`SystemClock`]; tests use [`ManualClock`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

/// A boxed sleep future.
pub type Sleep = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// A monotonic clock that can also wait.
pub trait Clock: Send + Sync + 'static {
    /// The current instant on this clock.
    fn now(&self) -> Instant;

    /// Completes once `duration` has passed on this clock.
    fn sleep(&self, duration: Duration) -> Sleep;
}

/// Run `future`, giving up after `limit` on `clock`. `None` means it timed out.
pub async fn timeout<F: Future>(
    clock: &dyn Clock,
    limit: Duration,
    future: F,
) -> Option<F::Output> {
    tokio::select! {
        output = future => Some(output),
        _ = clock.sleep(limit) => None,
    }
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) -> Sleep {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// A clock that only moves when told to. Sleeps complete when [`advance`]
/// carries the clock past their deadline, never on their own.
///
/// [`advance`]: ManualClock::advance
#[derive(Debug)]
pub struct ManualClock {
    base: Instant,
    state: Mutex<ManualState>,
}

#[derive(Debug, Default)]
struct ManualState {
    elapsed: Duration,
    sleepers: Vec<(Duration, oneshot::Sender<()>)>,
}

impl ManualClock {
    pub fn new() -> Self {
        Self {
            base: Instant::now(),
            state: Mutex::new(ManualState::default()),
        }
    }

    /// Move the clock forward and wake every sleeper whose deadline passed.
    pub fn advance(&self, by: Duration) {
        let due = {
            let mut state = self.state.lock().expect("manual clock lock");
            state.elapsed += by;
            let now = state.elapsed;
            let (due, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut state.sleepers)
                .into_iter()
                .partition(|(deadline, _)| *deadline <= now);
            state.sleepers = waiting;
            due
        };
        for (_, wake) in due {
            let _ = wake.send(());
        }
    }

    /// Time moved since the clock was created.
    pub fn elapsed(&self) -> Duration {
        self.state.lock().expect("manual clock lock").elapsed
    }

    /// How many sleeps are currently waiting. Sleeps whose future was dropped
    /// are not counted. Tests use this to know a task has reached its wait
    /// before they advance the clock.
    pub fn sleepers(&self) -> usize {
        let mut state = self.state.lock().expect("manual clock lock");
        state.sleepers.retain(|(_, wake)| !wake.is_closed());
        state.sleepers.len()
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.base + self.elapsed()
    }

    fn sleep(&self, duration: Duration) -> Sleep {
        if duration.is_zero() {
            return Box::pin(std::future::ready(()));
        }
        let (wake, woken) = oneshot::channel();
        {
            let mut state = self.state.lock().expect("manual clock lock");
            let deadline = state.elapsed + duration;
            state.sleepers.push((deadline, wake));
        }
        Box::pin(async move {
            let _ = woken.await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn a_manual_sleep_completes_only_when_the_clock_passes_its_deadline() {
        let clock = Arc::new(ManualClock::new());
        let start = clock.now();
        let mut sleep = clock.sleep(Duration::from_secs(10));
        assert_eq!(clock.sleepers(), 1);

        clock.advance(Duration::from_secs(9));
        assert!(
            futures_now(&mut sleep).is_none(),
            "must not complete one second early"
        );

        clock.advance(Duration::from_secs(1));
        assert!(futures_now(&mut sleep).is_some());
        assert_eq!(clock.sleepers(), 0);
        assert_eq!(clock.now() - start, Duration::from_secs(10));
        assert_eq!(clock.elapsed(), Duration::from_secs(10));
    }

    #[tokio::test]
    async fn a_zero_sleep_is_ready_and_a_dropped_sleep_is_not_counted() {
        let clock = ManualClock::new();
        let mut zero = clock.sleep(Duration::ZERO);
        assert!(futures_now(&mut zero).is_some());

        let dropped = clock.sleep(Duration::from_secs(1));
        assert_eq!(clock.sleepers(), 1);
        drop(dropped);
        assert_eq!(clock.sleepers(), 0);
    }

    #[tokio::test]
    async fn timeout_reports_which_side_won() {
        let clock = Arc::new(ManualClock::new());
        let fast = timeout(clock.as_ref(), Duration::from_secs(5), async { 7 }).await;
        assert_eq!(fast, Some(7));

        let waiting = Arc::clone(&clock);
        let slow = tokio::spawn(async move {
            timeout(
                waiting.as_ref(),
                Duration::from_secs(5),
                std::future::pending::<()>(),
            )
            .await
        });
        while clock.sleepers() == 0 {
            tokio::task::yield_now().await;
        }
        clock.advance(Duration::from_secs(5));
        assert_eq!(slow.await.unwrap(), None);
    }

    #[tokio::test]
    async fn the_system_clock_never_goes_backwards() {
        let clock = SystemClock;
        let before = clock.now();
        clock.sleep(Duration::ZERO).await;
        assert!(clock.now() >= before);
    }

    /// Poll a sleep once without waiting.
    fn futures_now(sleep: &mut Sleep) -> Option<()> {
        use std::task::{Context, Poll, Waker};
        let mut cx = Context::from_waker(Waker::noop());
        match sleep.as_mut().poll(&mut cx) {
            Poll::Ready(()) => Some(()),
            Poll::Pending => None,
        }
    }
}
