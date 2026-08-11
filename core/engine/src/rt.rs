//! Bridge for running async work from synchronous [`AgentHook`] methods.
//!
//! `AgentHook` methods are synchronous, but some hooks need to await
//! async work (e.g. an LLM call for summarisation).  The naive
//! approach — `Handle::current().block_on(fut)` — **panics** with
//! "Cannot block_on from within a runtime" when the hook runs on a
//! tokio worker thread, which is always the case in practice: the
//! agent loop itself is a tokio task on the `#[tokio::main]`
//! multi-threaded runtime.
//!
//! [`block_on`] picks a legal strategy for the ambient runtime.

use std::future::Future;

use tokio::runtime::{Handle, RuntimeFlavor};

/// Run `fut` to completion from synchronous code, blocking the caller.
///
/// Strategy depends on the ambient runtime:
///
/// - **Multi-threaded runtime** (production: `#[tokio::main]`): wraps the
///   call in [`tokio::task::block_in_place`], which migrates this worker's
///   other tasks to the remaining workers before blocking, making the
///   nested `Handle::block_on` legal.  While blocked, the agent loop is
///   stalled but the TUI (main thread) is unaffected.
///
/// - **Current-thread runtime or no runtime** (e.g. `#[tokio::test]`
///   default flavor, unit tests): runs a throwaway current-thread runtime
///   on a scoped OS thread.  `block_in_place` is unavailable there, and
///   *no* runtime may `block_on` from a thread that is already driving
///   one — a dedicated thread sidesteps both restrictions while its own
///   runtime drives the I/O driver, so reqwest-style futures still make
///   progress.
///
/// # Panics
///
/// Panics only if the ad-hoc fallback runtime cannot be built or the
/// bridge thread itself panics — not on any normal runtime configuration.
pub fn block_on<F>(fut: F) -> F::Output
where
    F: Future + Send,
    F::Output: Send,
{
    match Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| handle.block_on(fut))
        }
        _ => std::thread::scope(|s| {
            s.spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build ad-hoc tokio runtime for hook")
                    .block_on(fut)
            })
            .join()
            .expect("ad-hoc hook runtime thread panicked")
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn answer() -> u32 {
        42
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_block_on_inside_multi_thread_runtime() {
        // Runs on a tokio worker thread — the exact context that made
        // the old `Handle::block_on` pattern panic.
        assert_eq!(block_on(answer()), 42);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_block_on_inside_current_thread_runtime() {
        // Falls back to an ad-hoc runtime; must not panic or deadlock.
        assert_eq!(block_on(answer()), 42);
    }

    #[test]
    fn test_block_on_outside_any_runtime() {
        assert_eq!(block_on(answer()), 42);
    }
}
