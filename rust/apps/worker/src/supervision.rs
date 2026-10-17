//! Cancellation owns peer lifetimes: an error/signal never drops accepted work.
use std::future::Future;
use tokio_util::sync::CancellationToken;

pub(super) async fn join_owned<A, B>(
    left: A,
    right: B,
    stop: &CancellationToken,
) -> (A::Output, B::Output)
where
    A: Future,
    B: Future,
{
    let left = async {
        let result = left.await;
        stop.cancel();
        result
    };
    let right = async {
        let result = right.await;
        stop.cancel();
        result
    };
    tokio::join!(left, right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};
    use tokio::sync::oneshot;
    #[tokio::test]
    async fn peer_error_cancels_but_does_not_drop_an_owned_blocking_join()
    -> Result<(), Box<dyn std::error::Error>> {
        let (release, blocked) = mpsc::channel();
        let (started, ready) = oneshot::channel();
        let stop = CancellationToken::new();
        let cancel = stop.clone();
        let owned = async move {
            let work = tokio::task::spawn_blocking(move || {
                let _ = started.send(());
                blocked.recv()
            });
            work.await
        };
        let task = tokio::spawn(async move {
            join_owned(async { Err::<(), _>("peer failed") }, owned, &cancel).await
        });
        ready.await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(stop.is_cancelled());
        assert!(!task.is_finished());
        release.send(())?;
        let (failed, joined) = tokio::time::timeout(Duration::from_secs(2), task).await??;
        assert!(failed.is_err());
        joined??;
        Ok(())
    }
    #[tokio::test]
    async fn external_stop_waits_for_accepted_async_work_to_finish()
    -> Result<(), Box<dyn std::error::Error>> {
        let stop = CancellationToken::new();
        let signal = stop.clone();
        let run_stop = stop.clone();
        let (finish, accepted) = oneshot::channel();
        let task =
            tokio::spawn(
                async move { join_owned(signal.cancelled_owned(), accepted, &run_stop).await },
            );
        stop.cancel();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!task.is_finished());
        finish
            .send(())
            .map_err(|()| "accepted work receiver dropped")?;
        let ((), finished) = tokio::time::timeout(Duration::from_secs(2), task).await??;
        finished?;
        Ok(())
    }
}
