//! Opt-in measurement of the agent's service path over a disposable catalog.
//! Run this test alone in release mode under an external RSS monitor. It
//! holds the real preview and polls the same status handler as the clients.

use super::*;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};
use std::time::{Duration, Instant};

#[tokio::test]
#[ignore = "manual RSS qualification; requires FOLDOVER_PERF_SERVICE_STATE synthetic catalog"]
async fn million_file_service_preview() {
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
