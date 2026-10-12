use {
    std::{
        future::Future,
        time::{
            Duration,
            Instant,
            SystemTime,
        },
    },
    tokio::{
        sync::oneshot,
        task::JoinHandle,
    },
};

/// One background job. Dropping the slot aborts the task and discards any
/// result that has not been polled yet, so a superseded read cannot land
/// on the UI or keep allocating after the user has moved on.
pub struct Slot<T> {
    handle: JoinHandle<()>,
    rx: oneshot::Receiver<T>,
}

pub enum SlotPoll<T> {
    Pending,
    Ready(T),
    Closed,
}

impl<T: Send+'static> Slot<T> {
    pub fn spawn(fut: impl Future<Output=T>+Send+'static) -> Self {
        let (tx, rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            let value = fut.await;
            let _ = tx.send(value);
        });
        Self { handle, rx }
    }

    pub fn poll(&mut self) -> SlotPoll<T> {
        match self.rx.try_recv() {
            | Ok(value) => SlotPoll::Ready(value),
            | Err(oneshot::error::TryRecvError::Empty) => SlotPoll::Pending,
            | Err(oneshot::error::TryRecvError::Closed) => SlotPoll::Closed,
        }
    }
}

impl<T> Drop for Slot<T> {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Detects a sleep/wake (or a wall-clock step) by comparing wall time to the
/// monotonic clock. `Instant` does not advance while the machine is suspended.
pub struct WakeClock {
    last_wall: SystemTime,
    last_mono: Instant,
}

impl WakeClock {
    pub fn new() -> Self {
        Self {
            last_wall: SystemTime::now(),
            last_mono: Instant::now(),
        }
    }

    pub fn jumped(&mut self, threshold: Duration) -> bool {
        let wall = SystemTime::now();
        let mono = Instant::now();
        let jumped = wall_ahead(self.last_wall, wall, self.last_mono, mono, threshold);
        self.last_wall = wall;
        self.last_mono = mono;
        jumped
    }
}

pub fn wall_ahead(
    prev_wall: SystemTime,
    wall: SystemTime,
    prev_mono: Instant,
    mono: Instant,
    threshold: Duration,
) -> bool {
    let wall_delta = wall.duration_since(prev_wall).unwrap_or_default();
    let mono_delta = mono.saturating_duration_since(prev_mono);
    wall_delta.saturating_sub(mono_delta) >= threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_is_wall_time_without_monotonic_progress() {
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let mono = Instant::now();
        assert!(wall_ahead(
            wall,
            wall + Duration::from_secs(30),
            mono,
            mono,
            Duration::from_secs(2)
        ));
        assert!(!wall_ahead(
            wall,
            wall + Duration::from_millis(40),
            mono,
            mono + Duration::from_millis(40),
            Duration::from_secs(2)
        ));
        assert!(!wall_ahead(
            wall,
            wall - Duration::from_secs(5),
            mono,
            mono,
            Duration::from_secs(2)
        ));
    }

    #[tokio::test]
    async fn slot_delivers_without_the_caller_awaiting() {
        let mut slot = Slot::spawn(async { 7u8 });
        let value = loop {
            match slot.poll() {
                | SlotPoll::Ready(value) => break value,
                | SlotPoll::Pending => tokio::task::yield_now().await,
                | SlotPoll::Closed => panic!("slot closed before a value"),
            }
        };
        assert_eq!(value, 7);
    }
}
