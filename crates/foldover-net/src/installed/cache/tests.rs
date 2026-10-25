use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::Notify;

use super::VerifiedCache;

const RETRY_DELAY: Duration = Duration::from_secs(5);

#[tokio::test(start_paused = true)]
async fn a_failed_verification_can_recover() {
    let cache = VerifiedCache::<&str>::new(RETRY_DELAY);
    assert!(
        cache
            .get_or_try_init(|| async { Err(io::Error::other("unavailable")) })
            .await
            .is_err()
    );

    tokio::time::advance(RETRY_DELAY).await;
    let value = cache
        .get_or_try_init(|| async { Ok("verified") })
        .await
        .unwrap();
    assert_eq!(*value, "verified");
}

#[tokio::test(start_paused = true)]
async fn failures_wait_for_the_full_cooldown_before_retrying() {
    let cache = VerifiedCache::<&str>::new(RETRY_DELAY);
    let attempts = AtomicUsize::new(0);
    let failure = || async {
        attempts.fetch_add(1, Ordering::SeqCst);
        Err(io::Error::other("unavailable"))
    };
    assert!(cache.get_or_try_init(failure).await.is_err());
    tokio::time::advance(RETRY_DELAY - Duration::from_millis(1)).await;
    for _ in 0..8 {
        assert!(cache.get_or_try_init(failure).await.is_err());
    }
    assert_eq!(attempts.load(Ordering::SeqCst), 1);

    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(cache.get_or_try_init(failure).await.is_err());
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert!(cache.get_or_try_init(failure).await.is_err());
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn concurrent_callers_share_one_verification() {
    let cache = Arc::new(VerifiedCache::<&str>::new(RETRY_DELAY));
    let attempts = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let cache = Arc::clone(&cache);
        let attempts = Arc::clone(&attempts);
        let started = Arc::clone(&started);
        let release = Arc::clone(&release);
        tasks.push(tokio::spawn(async move {
            cache
                .get_or_try_init(|| async {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    started.notify_one();
                    release.notified().await;
                    Ok("verified")
                })
                .await
                .unwrap()
        }));
    }
    started.notified().await;
    tokio::task::yield_now().await;
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    release.notify_one();

    let mut pinned = None;
    for task in tasks {
        let value = tokio::time::timeout(RETRY_DELAY, task)
            .await
            .unwrap()
            .unwrap();
        if let Some(pinned) = &pinned {
            assert!(Arc::ptr_eq(pinned, &value));
        }
        pinned = Some(value);
    }
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn a_successful_verification_is_pinned_for_the_cache_lifetime() {
    let cache = VerifiedCache::<&str>::new(RETRY_DELAY);
    let original = cache
        .get_or_try_init(|| async { Ok("original") })
        .await
        .unwrap();
    tokio::time::advance(RETRY_DELAY * 2).await;
    let changed = cache
        .get_or_try_init(|| async { Ok("replacement") })
        .await
        .unwrap();
    let failed = cache
        .get_or_try_init(|| async { Err(io::Error::other("changed bundle")) })
        .await
        .unwrap();
    assert_eq!(*changed, "original");
    assert!(Arc::ptr_eq(&original, &changed));
    assert!(Arc::ptr_eq(&original, &failed));
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_verification_also_waits_before_retrying() {
    let cache = Arc::new(VerifiedCache::<&str>::new(RETRY_DELAY));
    let started = Arc::new(Notify::new());
    let task_cache = Arc::clone(&cache);
    let task_started = Arc::clone(&started);
    let task = tokio::spawn(async move {
        task_cache
            .get_or_try_init(|| async {
                task_started.notify_one();
                std::future::pending::<io::Result<&str>>().await
            })
            .await
    });
    started.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(
        cache
            .get_or_try_init(|| async { panic!("cancelled checks must cool down") })
            .await
            .is_err()
    );

    tokio::time::advance(RETRY_DELAY).await;
    assert_eq!(
        *cache
            .get_or_try_init(|| async { Ok("recovered") })
            .await
            .unwrap(),
        "recovered"
    );
}
