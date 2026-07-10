//! Progress events for long-running tool execution.
//!
//! The [`Progress`] enum together with [`ProgressStream`] enables tools to
//! report intermediate status updates.  The agent loop pulls from the stream
//! and forwards [`Progress::InProgress`] messages to the TUI in real-time.

use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;

/// A single progress event emitted during tool execution.
#[derive(Debug, Clone)]
pub enum Progress {
    /// Intermediate update — the tool is still running.  The TUI renders this
    /// under the tool's header line.
    InProgress(String),
    /// Final result — tool execution is complete.  This is pushed to memory
    /// and returned to the LLM as the tool observation.
    Done(String),
}

/// A boxed, `Send`-able stream of [`Progress`] events.
///
/// Wraps `Pin<Box<dyn Stream<Item = Progress> + Send>>` and implements
/// [`Stream`] + [`Debug`] so it interoperates with `Result::unwrap`.
pub struct ProgressStream(Pin<Box<dyn Stream<Item = Progress> + Send + 'static>>);

impl ProgressStream {
    /// Wrap a boxed stream.
    pub fn new(inner: Pin<Box<dyn Stream<Item = Progress> + Send + 'static>>) -> Self {
        Self(inner)
    }

    /// Create a stream that emits a single [`Progress::Done`] event.
    ///
    /// This is the common case for tools that complete synchronously.
    pub fn done(result: String) -> Self {
        Self(Box::pin(futures_util::stream::once(async move {
            Progress::Done(result)
        })))
    }
}

impl Stream for ProgressStream {
    type Item = Progress;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Pin<Box<T>> is Unpin for any T (Box is always Unpin), so the
        // inner Pin<Box<dyn Stream>> is safe to project through.
        self.0.as_mut().poll_next(cx)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl fmt::Debug for ProgressStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProgressStream").finish_non_exhaustive()
    }
}

impl From<Pin<Box<dyn Stream<Item = Progress> + Send + 'static>>> for ProgressStream {
    fn from(inner: Pin<Box<dyn Stream<Item = Progress> + Send + 'static>>) -> Self {
        Self(inner)
    }
}

/// Helpers for tests and synchronous consumers.
impl ProgressStream {
    /// Block on the stream and return the first [`Progress::Done`] payload.
    ///
    /// Panics if the stream produces [`Progress::InProgress`] or nothing.
    /// This is a convenience for tests — production code should use
    /// `stream.next().await` in an async context.
    pub fn poll_done(&mut self) -> String {
        use futures_util::StreamExt;
        match futures_executor::block_on(self.next()) {
            Some(Progress::Done(output)) => output,
            Some(Progress::InProgress(msg)) => {
                panic!("expected Progress::Done, got InProgress({msg:?})")
            }
            None => panic!("expected Progress::Done, got None (empty stream)"),
        }
    }
}
