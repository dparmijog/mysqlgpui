//! Bridge between GPUI's executor and the tokio runtime `sqlx` requires.
//!
//! GPUI drives its own foreground/background executors, but `sqlx` (and
//! therefore every `sqldr-core` driver) needs a tokio reactor. A single
//! multi-threaded runtime lives for the whole process; database work is
//! spawned onto it and the resulting `JoinHandle` — a plain future — is
//! awaited from GPUI tasks.

use std::future::Future;
use std::sync::LazyLock;

static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("db-io")
        .enable_all()
        .build()
        .expect("failed to start tokio runtime")
});

/// Runs `fut` on the tokio runtime and resolves with its output. Safe to
/// await from any executor.
pub async fn run<F>(fut: F) -> anyhow::Result<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    Ok(RUNTIME.spawn(fut).await?)
}
