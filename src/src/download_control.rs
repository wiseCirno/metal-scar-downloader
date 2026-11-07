//! Cancellation is registered before SESSION_LOCK, including requests which arrive
//! before the download command. Batch IDs are single-use for this app lifetime.
use anyhow::{Result, bail};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[derive(Debug)]
pub(crate) struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("下载已取消")
    }
}
impl std::error::Error for Cancelled {}

#[derive(Default)]
pub(crate) struct Cancellation {
    // 0: pending/running, 1: cancellation requested, 2: terminal.
    state: AtomicU8,
    changed: Notify,
}
impl Cancellation {
    pub fn cancel(&self) -> bool {
        if self
            .state
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            self.changed.notify_waiters();
            true
        } else {
            false
        }
    }
    pub fn check(&self) -> Result<()> {
        if self.state.load(Ordering::SeqCst) == 1 {
            Err(Cancelled.into())
        } else {
            Ok(())
        }
    }
    pub async fn cancelled(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.state.load(Ordering::SeqCst) == 1 {
                return;
            }
            notified.await;
        }
    }
    /// Only use for operations which can safely be dropped (network/queue waits).
    pub async fn run<T>(&self, future: impl Future<Output = Result<T>>) -> Result<T> {
        self.check()?;
        tokio::select! {
            biased;
            result = future => result,
            _ = self.cancelled() => Err(Cancelled.into()),
        }
    }
    pub fn finish_result<T>(&self, result: Result<T>) -> Result<T> {
        let previous = self.state.swap(2, Ordering::SeqCst);
        if previous == 1 && result.is_err() {
            // Atomically settle a cancellation racing with a network/IO error;
            // requests accepted before termination never create failure logs.
            Err(Cancelled.into())
        } else {
            result
        }
    }
    pub fn finish(&self) {
        self.state.store(2, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct BatchState {
    started: bool,
    all: bool,
    requested: HashSet<String>,
    songs: HashMap<String, Arc<Cancellation>>,
}
#[derive(Default)]
pub(crate) struct Batch {
    state: Mutex<BatchState>,
    finished: AtomicBool,
}
impl Batch {
    pub fn song(&self, id: &str) -> Arc<Cancellation> {
        let mut state = self.state.lock().unwrap();
        let cancelled = state.all || state.requested.contains(id);
        let token = state.songs.entry(id.to_owned()).or_default().clone();
        if cancelled {
            token.cancel();
        }
        token
    }
    /// The explicit queue is known before SESSION_LOCK. Dropping its lock
    /// future is safe; cancelled batches must not wait behind refresh/login.
    pub async fn wait_before_start<T>(
        &self,
        ids: &[String],
        start: impl Future<Output = T>,
    ) -> Option<T> {
        let tokens: Vec<_> = ids.iter().map(|id| self.song(id)).collect();
        tokio::select! {
            biased;
            _ = futures::future::join_all(tokens.iter().map(|token| token.cancelled())) => None,
            result = start => Some(result),
        }
    }
    pub fn finish_initialization_failure(&self, ids: &[String]) -> Vec<String> {
        let mut seen = HashSet::new();
        ids.iter()
            .filter(|id| seen.insert(id.as_str()))
            .filter_map(|id| {
                let result = self
                    .song(id)
                    .finish_result::<()>(Err(anyhow::anyhow!("下载初始化失败")));
                if result.unwrap_err().is::<Cancelled>() {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect()
    }
    pub fn cancel_all(&self) -> usize {
        self.cancel(None)
    }
    fn cancel(&self, ids: Option<&[String]>) -> usize {
        let mut state = self.state.lock().unwrap();
        if self.finished.load(Ordering::SeqCst) {
            return 0;
        }
        match ids {
            Some(ids) => ids
                .iter()
                .map(|id| {
                    if state.all {
                        return 0;
                    }
                    if let Some(token) = state.songs.get(id) {
                        usize::from(token.cancel())
                    } else {
                        usize::from(state.requested.insert(id.clone()))
                    }
                })
                .sum(),
            None => {
                if state.all {
                    return 0;
                }
                state.all = true;
                let count = state.songs.values().filter(|token| token.cancel()).count();
                // Before queue discovery, one request represents the whole batch.
                if state.songs.is_empty() { 1 } else { count }
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct Downloads(Mutex<HashMap<String, Arc<Batch>>>);
impl Downloads {
    pub fn begin(&self, id: &str, ids: Option<&[String]>) -> Result<BatchGuard> {
        anyhow::ensure!(!id.trim().is_empty(), "batch_id 不能为空");
        let batch = self
            .0
            .lock()
            .unwrap()
            .entry(id.to_owned())
            .or_default()
            .clone();
        {
            let mut state = batch.state.lock().unwrap();
            if state.started {
                bail!("batch_id 已使用，请为新下载创建唯一 ID");
            }
            state.started = true;
        }
        if let Some(ids) = ids {
            for id in ids {
                batch.song(id);
            }
        }
        Ok(BatchGuard(batch))
    }
    pub fn cancel(&self, id: &str, ids: Option<&[String]>) -> Result<usize> {
        anyhow::ensure!(!id.trim().is_empty(), "batch_id 不能为空");
        let batch = self
            .0
            .lock()
            .unwrap()
            .entry(id.to_owned())
            .or_default()
            .clone();
        Ok(batch.cancel(ids))
    }
}
pub(crate) struct BatchGuard(pub Arc<Batch>);
impl Drop for BatchGuard {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap();
        self.0.finished.store(true, Ordering::SeqCst);
        // Keep only the single-use batch tombstone after completion.
        state.songs.clear();
        state.requested.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn cancellation_before_registration_and_while_waiting_for_session_is_retained() {
        let downloads = Downloads::default();
        assert_eq!(downloads.cancel("early", Some(&["a".into()])).unwrap(), 1);
        let early = downloads.begin("early", None).unwrap();
        assert!(early.0.song("a").check().is_err());
        assert!(early.0.song("b").check().is_ok());
        let session = tokio::sync::Mutex::new(());
        let _locked = session.lock().await;
        let waiting = downloads.begin("waiting", Some(&["a".into()])).unwrap();
        assert_eq!(downloads.cancel("waiting", None).unwrap(), 1);
        assert!(waiting.0.song("a").check().is_err());
        assert_eq!(downloads.cancel("waiting", None).unwrap(), 0);
    }

    #[tokio::test]
    async fn whole_batch_cancellation_interrupts_wait_for_session_lock() {
        let downloads = Downloads::default();
        let ids = vec!["a".into(), "b".into()];
        let batch = downloads.begin("waiting-session", Some(&ids)).unwrap();
        let session = tokio::sync::Mutex::new(());
        let _occupied = session.lock().await;
        let wait = batch.0.wait_before_start(&ids, session.lock());
        let cancel = async {
            tokio::task::yield_now().await;
            assert_eq!(downloads.cancel("waiting-session", None).unwrap(), 2);
        };
        let (result, _) = tokio::time::timeout(Duration::from_millis(200), async {
            tokio::join!(wait, cancel)
        })
        .await
        .unwrap();
        assert!(result.is_none());
        assert_eq!(batch.0.finish_initialization_failure(&ids), ids);
    }

    #[tokio::test]
    async fn pre_cancelled_batch_wins_even_when_session_lock_is_ready() {
        let downloads = Downloads::default();
        let ids = vec!["a".into()];
        downloads.cancel("pre-cancelled", None).unwrap();
        let batch = downloads.begin("pre-cancelled", Some(&ids)).unwrap();
        let session = tokio::sync::Mutex::new(());
        assert!(
            batch
                .0
                .wait_before_start(&ids, session.lock())
                .await
                .is_none()
        );
        assert!(session.try_lock().is_ok());
    }

    #[test]
    fn initialization_failure_preserves_cancelled_songs_and_finishes_other_songs() {
        let downloads = Downloads::default();
        let ids = vec!["cancelled".into(), "failed".into(), "cancelled".into()];
        let batch = downloads.begin("setup-error", Some(&ids)).unwrap();
        downloads
            .cancel("setup-error", Some(&["cancelled".into()]))
            .unwrap();
        assert_eq!(
            batch.0.finish_initialization_failure(&ids),
            vec!["cancelled"]
        );
        assert_eq!(downloads.cancel("setup-error", None).unwrap(), 0);
        // Terminal cancellation notifications are not emitted twice.
        assert!(batch.0.finish_initialization_failure(&ids).is_empty());
    }

    #[tokio::test]
    async fn queued_and_active_cancellation_does_not_wait_for_slow_work() {
        let active = Arc::new(Cancellation::default());
        let queued = Arc::new(Cancellation::default());
        let slots = tokio::sync::Semaphore::new(1);
        let _occupied = slots.acquire().await.unwrap();
        queued.cancel();
        let result = tokio::time::timeout(
            Duration::from_millis(200),
            queued.run(async {
                let _permit = slots.acquire().await?;
                panic!("cancelled queued song must never start");
                #[allow(unreachable_code)]
                Ok(())
            }),
        )
        .await
        .unwrap();
        assert!(result.unwrap_err().is::<Cancelled>());
        let work = active.run(std::future::pending::<Result<()>>());
        let cancel = async {
            tokio::task::yield_now().await;
            active.cancel();
        };
        let (result, _) = tokio::time::timeout(Duration::from_millis(200), async {
            tokio::join!(work, cancel)
        })
        .await
        .unwrap();
        assert!(result.unwrap_err().is::<Cancelled>());
    }

    #[test]
    fn cancellation_racing_with_failure_is_cancelled_but_committed_success_is_kept() {
        let token = Cancellation::default();
        token.cancel();
        let result = token.finish_result::<()>(Err(anyhow::anyhow!("network error")));
        assert!(result.unwrap_err().is::<Cancelled>());
        assert!(!token.cancel());
        let token = Cancellation::default();
        token.cancel();
        token.finish(); // synchronous publication completed
        assert_eq!(token.finish_result(Ok("published")).unwrap(), "published");
    }

    #[test]
    fn batches_are_isolated_terminal_songs_and_old_batches_cannot_be_cancelled() {
        let downloads = Downloads::default();
        let old = downloads.begin("old", None).unwrap();
        let done = old.0.song("same");
        done.finish();
        assert_eq!(downloads.cancel("old", Some(&["same".into()])).unwrap(), 0);
        drop(old);
        let new = downloads.begin("new", None).unwrap();
        let active = new.0.song("same");
        assert_eq!(downloads.cancel("old", None).unwrap(), 0);
        assert!(active.check().is_ok());
        assert!(downloads.begin("old", None).is_err());
        assert_eq!(
            downloads
                .cancel("new", Some(&["same".into(), "same".into()]))
                .unwrap(),
            1
        );
        assert!(active.check().is_err());
    }
}
