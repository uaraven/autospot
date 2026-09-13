//! A minimal blocking executor for WinRT async operations.
//!
//! `windows-future` 0.3 dropped the blocking `IAsyncOperation::get()` helper: the async
//! types only implement `IntoFuture` now. This app has no async runtime and no reason to
//! grow one, so it drives the single future it cares about on the calling thread.
//!
//! Every call is bounded by a timeout. A tethering call that never completes must not
//! wedge the watchdog loop forever.

use std::future::IntoFuture;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Drive `future` to completion on this thread, giving up after `timeout`.
///
/// On timeout the future is dropped; the underlying WinRT operation keeps running but
/// its result is discarded (its completion handler only signals a waker, which is
/// harmless once nobody is parked on it).
pub fn block_on<F>(future: F, timeout: Duration, what: &str) -> Result<F::Output>
where
    F: IntoFuture,
{
    let mut future = Box::pin(future.into_future());
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut cx = Context::from_waker(&waker);
    let deadline = Instant::now() + timeout;

    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return Ok(value);
        }
        let now = Instant::now();
        if now >= deadline {
            bail!("{what} did not complete within {timeout:?}");
        }
        // A spurious unpark just means another loop iteration, which `poll` tolerates.
        thread::park_timeout(deadline - now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_a_ready_value() {
        let out = block_on(std::future::ready(7u32), Duration::from_secs(1), "ready").unwrap();
        assert_eq!(out, 7);
    }

    #[test]
    fn wakes_up_when_another_thread_completes_the_future() {
        let (tx, rx) = std::sync::mpsc::channel::<u32>();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let _ = tx.send(42);
        });

        // A tiny future that polls a channel, so completion arrives from another thread.
        struct Recv(std::sync::mpsc::Receiver<u32>);
        impl std::future::Future for Recv {
            type Output = u32;
            fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u32> {
                match self.0.try_recv() {
                    Ok(v) => Poll::Ready(v),
                    Err(_) => {
                        // Re-arm via the timeout path rather than a real waker.
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                }
            }
        }

        let out = block_on(Recv(rx), Duration::from_secs(5), "recv").unwrap();
        assert_eq!(out, 42);
    }

    #[test]
    fn gives_up_after_the_timeout() {
        let err = block_on(
            std::future::pending::<()>(),
            Duration::from_millis(80),
            "never",
        )
        .unwrap_err();
        assert!(format!("{err}").contains("never did not complete"), "{err}");
    }
}
