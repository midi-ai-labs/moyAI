//! Stage tracking for the explicit Desktop exit and the notice shown when it is slow.
//!
//! The exit sequence is never cancelled here: stopping it midway would leave a
//! Runner shutdown or resource drain running unowned, and a second Exit would
//! start them again. A slow exit only gives the window back with the stage it waits on.
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long the exit may wait before the window shows what it is waiting on.
pub(crate) const EXIT_NOTICE_AFTER: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// The stage the exit sequence is currently waiting on, shared with the notice.
#[derive(Clone)]
pub(crate) struct ExitStage(Arc<Mutex<&'static str>>);

impl ExitStage {
    pub(crate) fn new(first: &'static str) -> Self {
        Self(Arc::new(Mutex::new(first)))
    }

    pub(crate) fn enter(&self, stage: &'static str) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = stage;
    }

    pub(crate) fn current(&self) -> &'static str {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub(crate) fn slow_exit_notice(stage: &str, notice_after: Duration) -> String {
    format!(
        "終了処理が{}秒を超えています。待っている処理: {stage}。終わりしだい自動でmoyAIを終了します。この間、新しい仕事は開始できません。この表示が続く場合は、待っている処理の名前を管理者へ伝えてください。",
        notice_after.as_secs()
    )
}

/// Await `sequence` to completion. Once `notice_after` has passed, call
/// `notify` with the stage being waited on, and again whenever it changes.
pub(crate) async fn await_exit_with_notice<T>(
    sequence: impl Future<Output = T>,
    stage: &ExitStage,
    notice_after: Duration,
    mut notify: impl FnMut(&'static str),
) -> T {
    let started = tokio::time::Instant::now();
    let mut announced: Option<&'static str> = None;
    tokio::pin!(sequence);
    loop {
        tokio::select! {
            result = &mut sequence => return result,
            _ = tokio::time::sleep(POLL_INTERVAL) => {
                if started.elapsed() < notice_after {
                    continue;
                }
                let current = stage.current();
                if announced != Some(current) {
                    announced = Some(current);
                    notify(current);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_stalled_stage_is_announced_once_and_the_sequence_keeps_running() {
        let stage = ExitStage::new("Hubとの接続の終了");
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let sequence_stage = stage.clone();
        let sequence = async move {
            sequence_stage.enter("実行機能（Runner）の終了");
            // Reproduces a stage that does not finish until something else happens.
            released.await.unwrap();
            "finished"
        };
        let notices = Arc::new(Mutex::new(Vec::new()));
        let recorded = notices.clone();
        let waiter = tokio::spawn({
            let stage = stage.clone();
            async move {
                await_exit_with_notice(sequence, &stage, Duration::from_millis(300), |current| {
                    recorded.lock().unwrap().push(current)
                })
                .await
            }
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            notices.lock().unwrap().is_empty(),
            "no notice before the limit"
        );
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(*notices.lock().unwrap(), vec!["実行機能（Runner）の終了"]);
        assert!(!waiter.is_finished(), "the exit sequence is not cancelled");
        release.send(()).unwrap();
        assert_eq!(waiter.await.unwrap(), "finished");
        assert_eq!(notices.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn each_new_stage_after_the_limit_is_announced() {
        let stage = ExitStage::new("Hubとの接続の終了");
        let (to_second, at_second) = tokio::sync::oneshot::channel::<()>();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let sequence_stage = stage.clone();
        let sequence = async move {
            at_second.await.unwrap();
            sequence_stage.enter("共有プロジェクトの公開の停止");
            released.await.unwrap();
        };
        let notices = Arc::new(Mutex::new(Vec::new()));
        let recorded = notices.clone();
        let waiter = tokio::spawn({
            let stage = stage.clone();
            async move {
                await_exit_with_notice(sequence, &stage, Duration::from_millis(100), |current| {
                    recorded.lock().unwrap().push(current)
                })
                .await
            }
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        to_second.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        release.send(()).unwrap();
        waiter.await.unwrap();
        assert_eq!(
            *notices.lock().unwrap(),
            vec!["Hubとの接続の終了", "共有プロジェクトの公開の停止"]
        );
    }

    #[tokio::test]
    async fn a_fast_exit_shows_no_notice() {
        let stage = ExitStage::new("Hubとの接続の終了");
        let mut notified = false;
        let result =
            await_exit_with_notice(async { 7 }, &stage, EXIT_NOTICE_AFTER, |_| notified = true)
                .await;
        assert_eq!(result, 7);
        assert!(!notified);
    }

    #[test]
    fn the_notice_names_the_stage_and_the_limit() {
        let notice = slow_exit_notice("実行機能（Runner）の終了", EXIT_NOTICE_AFTER);
        assert!(notice.contains("60秒"));
        assert!(notice.contains("実行機能（Runner）の終了"));
    }
}
