use std::future::Future;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

/// Only a fully verified value is pinned. Failed checks remain fail-closed
/// while allowing a later attempt to recover from unavailable OS services.
pub(super) struct VerifiedCache<T> {
    state: Mutex<State<T>>,
    retry_delay: Duration,
}

struct State<T> {
    value: Option<Arc<T>>,
    retry_after: Option<Instant>,
}

impl<T> VerifiedCache<T> {
    pub(super) const fn new(retry_delay: Duration) -> Self {
        Self {
            state: Mutex::const_new(State {
                value: None,
                retry_after: None,
            }),
            retry_delay,
        }
    }

    pub(super) async fn get_or_try_init<F, Fut>(&self, load: F) -> io::Result<Arc<T>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = io::Result<T>>,
    {
        // Serialize initialization so concurrent callers cannot multiply native
        // checks or install competing trust sets. A pinned value never reloads.
        let mut state = self.state.lock().await;
        if let Some(value) = &state.value {
            return Ok(Arc::clone(value));
        }
        if state.retry_after.is_some_and(|at| Instant::now() < at) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Installation verification is waiting to retry",
            ));
        }
        let result = {
            // Cancellation also starts the cooldown: timed-out callers must not
            // immediately start more work while native checks may still run.
            let _retry = RetryOnDrop {
                retry_after: &mut state.retry_after,
                delay: self.retry_delay,
            };
            load().await
        };
        let value = Arc::new(result?);
        state.value = Some(Arc::clone(&value));
        Ok(value)
    }
}

struct RetryOnDrop<'a> {
    retry_after: &'a mut Option<Instant>,
    delay: Duration,
}

impl Drop for RetryOnDrop<'_> {
    fn drop(&mut self) {
        *self.retry_after = Some(Instant::now() + self.delay);
    }
}

#[cfg(test)]
mod tests;
