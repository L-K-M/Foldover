//! Opt-in measurement of the agent's service path over a disposable catalog.
//! Run this test alone in release mode under an external RSS monitor. It
//! holds the real preview and polls the same status handler as the clients.

use super::*;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};
use std::time::{Duration, Instant};

fn qualification_fixture() -> (Engine, foldover_types::PairConfig) {
    let state = std::path::PathBuf::from(
        std::env::var_os("FOLDOVER_PERF_SERVICE_STATE").expect("explicit fixture catalog"),
    )
    .canonicalize()
    .unwrap();
    assert!(state.components().any(|component| matches!(component,
        Component::Normal(name) if name.as_encoded_bytes().starts_with(b"foldover-perf"))));
    let container = state.parent().unwrap();
    let engine = Engine::open(&StateLocation::Directory(state.clone())).unwrap();
    let pairs = engine.pairs().unwrap();
    assert_eq!(pairs.len(), 1);
    let pair = &pairs[0];
    for root in [pair.draft().left(), pair.draft().right()] {
        let PairRoot::Local(local) = root else {
            panic!("local qualification only")
        };
        let path = Path::new(OsStr::from_bytes(local.as_bytes()))
            .canonicalize()
            .unwrap();
        assert!(
            path.starts_with(container),
            "roots must be inside the fixture container"
        );
    }
    (engine, pair.clone())
}

#[tokio::test]
#[ignore = "manual RSS qualification; requires FOLDOVER_PERF_SERVICE_STATE synthetic catalog"]
async fn million_file_service_preview() {
    let (engine, pair) = qualification_fixture();
    println!("process\t{}", std::process::id());
    let key = serde_json::json!({"id": pair.id().to_string()});
    let service = Service::new(engine);
    let started = Instant::now();
    let mut polls = tokio::time::interval(Duration::from_millis(250));
    polls.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    service
        .handle(Request::call(1, SYNC_PREVIEW, Some(key.clone())))
        .await
        .unwrap();
    let mut maximum_status = Duration::ZERO;
    let mut maximum_poll_delay = Duration::ZERO;
    loop {
        let scheduled = polls.tick().await;
        maximum_poll_delay = maximum_poll_delay.max(tokio::time::Instant::now() - scheduled);
        let polled = Instant::now();
        let status = service
            .handle(Request::call(2, SYNC_STATUS, Some(key.clone())))
            .await
            .unwrap();
        maximum_status = maximum_status.max(polled.elapsed());
        if status["state"] != "working" {
            assert_eq!(status["state"], "previewed", "{status}");
            for kind in ["copies", "updates", "archives", "conflicts"] {
                assert_eq!(status["preview"]["counts"][kind], 0, "{status}");
            }
            println!("preview\t{}", started.elapsed().as_nanos());
            println!("maximum_status_ns\t{}", maximum_status.as_nanos());
            println!("maximum_poll_delay_ns\t{}", maximum_poll_delay.as_nanos());
            println!("result\t{status}");
            // Leave the held preview resident for the external sampler.
            tokio::time::sleep(Duration::from_secs(1)).await;
            service.note_clean_stop().unwrap();
            return;
        }
        if started.elapsed() > Duration::from_secs(600) {
            service
                .handle(Request::call(3, SYNC_CANCEL, Some(key)))
                .await
                .unwrap();
            panic!("qualification preview did not finish within ten minutes");
        }
    }
}

/// Measure the actual capture/mount lookup under the service engine mutex,
/// with another caller queued for it. The barrier handshake is excluded
/// from capture time; this is a focused probe, not an RPC latency sample.
#[test]
#[ignore = "manual lock qualification; requires FOLDOVER_PERF_SERVICE_STATE synthetic catalog"]
fn million_file_service_input_lock() {
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicBool, Ordering};

    let (engine, pair) = qualification_fixture();
    let service = Service::new(engine);
    println!("process\t{}", std::process::id());
    let gate = Arc::new(Barrier::new(2));
    let queued = Arc::new(AtomicBool::new(false));
    let contender = {
        let (engine, gate, queued) = (
            Arc::clone(&service.engine),
            Arc::clone(&gate),
            Arc::clone(&queued),
        );
        std::thread::spawn(move || {
            gate.wait();
            assert!(matches!(
                engine.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ));
            let waiting = Instant::now();
            queued.store(true, Ordering::SeqCst);
            let engine = engine.lock().unwrap();
            let waited = waiting.elapsed();
            assert_eq!(engine.pairs().unwrap().len(), 1);
            waited
        })
    };
    let waiting = Instant::now();
    let engine = service.engine.lock().unwrap();
    let waited = waiting.elapsed();
    gate.wait();
    while !queued.load(Ordering::SeqCst) {
        std::thread::yield_now();
    }
    let capturing = Instant::now();
    let pending = engine.capture_run_inputs(pair.id()).unwrap();
    let _mounts = engine.mounts(pair.id()).unwrap();
    drop(engine);
    let capture_time = capturing.elapsed();
    let contender_wait = contender.join().unwrap();
    let decoding = Instant::now();
    let inputs = pending.decode().unwrap();
    let decode_time = decoding.elapsed();
    assert_eq!(inputs.configuration().id(), pair.id());
    for (name, elapsed) in [
        ("InputLockWait", waited),
        ("InputLockCapture", capture_time),
        ("ContendingEngineLockWait", contender_wait),
        ("InputDecode", decode_time),
    ] {
        println!("metric\t0\t{name}\t{}", elapsed.as_nanos());
    }
    drop(inputs);
    service.note_clean_stop().unwrap();
}

#[tokio::test]
#[ignore = "manual cancellation qualification; requires FOLDOVER_PERF_SERVICE_STATE synthetic catalog"]
async fn million_file_service_scan_cancellation() {
    let (engine, pair) = qualification_fixture();
    let service = Service::new(engine);
    println!("process\t{}", std::process::id());
    let key = serde_json::json!({"id": pair.id().to_string()});
    service
        .handle(Request::call(1, SYNC_PREVIEW, Some(key.clone())))
        .await
        .unwrap();
    let started = Instant::now();
    let checked = loop {
        let status = service
            .handle(Request::call(2, SYNC_STATUS, Some(key.clone())))
            .await
            .unwrap();
        assert_eq!(status["state"], "working", "{status}");
        let checked = status["progress"]["checked"].as_u64().unwrap();
        if checked >= 10_000 {
            break checked;
        }
        if started.elapsed() > Duration::from_secs(60) {
            service
                .handle(Request::call(3, SYNC_CANCEL, Some(key.clone())))
                .await
                .unwrap();
            panic!("qualification did not reach observation within one minute");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let cancelled = Instant::now();
    service
        .handle(Request::call(3, SYNC_CANCEL, Some(key.clone())))
        .await
        .unwrap();
    loop {
        let status = service
            .handle(Request::call(4, SYNC_STATUS, Some(key.clone())))
            .await
            .unwrap();
        if status["state"] != "working" {
            assert_eq!(status["state"], "idle", "{status}");
            println!("counter\t0\tCheckedAtCancellation\t{checked}");
            println!(
                "metric\t0\tScanCancellation\t{}",
                cancelled.elapsed().as_nanos()
            );
            service.note_clean_stop().unwrap();
            return;
        }
        assert!(
            cancelled.elapsed() < Duration::from_secs(60),
            "cancellation did not settle"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// ADR 0053: remove the largest pair of a copied catalog, then purge its
/// rows while a client polls `pair.list`, as the app does. Prints how
/// long the removal took to answer and the slowest list answer.
#[tokio::test]
#[ignore = "manual removal qualification; requires FOLDOVER_PERF_SERVICE_STATE copied catalog"]
async fn million_file_service_removal() {
    let state = std::path::PathBuf::from(
        std::env::var_os("FOLDOVER_PERF_SERVICE_STATE").expect("explicit copied catalog"),
    )
    .canonicalize()
    .unwrap();
    // The test removes a pair: only ever from a disposable copy.
    assert!(state.components().any(|component| matches!(component,
        Component::Normal(name) if name.as_encoded_bytes().starts_with(b"foldover-perf"))));
    let engine = Engine::open(&StateLocation::Directory(state)).unwrap();
    let name = std::env::var("FOLDOVER_PERF_REMOVE_PAIR").expect("the name of the pair to remove");
    let pair = engine
        .pairs()
        .unwrap()
        .into_iter()
        .find(|pair| pair.draft().name() == name)
        .expect("the named pair");
    let service = Service::new(engine);

    let started = Instant::now();
    service
        .handle(Request::call(
            1,
            PAIR_REMOVE,
            Some(serde_json::json!({"id": pair.id().to_string(), "revision": pair.revision()})),
        ))
        .await
        .unwrap();
    println!("remove_ns\t{}", started.elapsed().as_nanos());

    let purging = Instant::now();
    let engine = Arc::clone(&service.engine);
    let task = tokio::spawn(async move { purge::purge(&engine).await });
    let mut slowest_list = Duration::ZERO;
    let mut lists = 0_u32;
    while !task.is_finished() {
        let listed = Instant::now();
        service
            .handle(Request::call(2, PAIR_LIST, None))
            .await
            .unwrap();
        slowest_list = slowest_list.max(listed.elapsed());
        lists += 1;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    task.await.unwrap().unwrap();
    println!("purge_ns\t{}", purging.elapsed().as_nanos());
    println!("lists\t{lists}");
    println!("slowest_list_ns\t{}", slowest_list.as_nanos());
    assert!(!service.engine.lock().unwrap().removals_pending().unwrap());
}
