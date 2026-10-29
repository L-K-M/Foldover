//! Service-level coverage for edits saved while a real worker is held.

use super::*;
use foldover_proto::local::PREVIEW_REQUIRED;
use serde_json::json;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::Duration;

fn fixture() -> (tempfile::TempDir, Service) {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&StateLocation::Directory(directory.path().join("state"))).unwrap();
    (directory, Service::new(engine))
}

fn configuration(directory: &Path, name: &str) -> Value {
    let left = directory.join(format!("{name}-left"));
    let right = directory.join(format!("{name}-right"));
    fs::create_dir(&left).unwrap();
    fs::create_dir(&right).unwrap();
    json!({
        "name": name,
        "left_root": left.as_os_str().as_bytes(),
        "right_root": right.as_os_str().as_bytes(),
        "mode": {"kind": "sync"},
        "options": {
            "modification_times": "sync", "extended_attributes": "sync",
            "permissions": "ignore", "durability": "standard",
            "schedule": {"kind": "manual"}, "filters": [],
        },
    })
}

async fn call(service: &Service, method: &str, params: Value) -> Value {
    service
        .handle(Request::call(1, method, Some(params)))
        .await
        .unwrap()
}

async fn settled(service: &Service, key: &Value) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let status = call(service, SYNC_STATUS, key.clone()).await;
            if status["state"] != "working" {
                return status;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the worker must reach a terminal status")
}

async fn hold_preview(service: &Service, key: &Value) -> Sender<()> {
    let (reached, release) = service.jobs.gate_job();
    let status = call(service, SYNC_PREVIEW, key.clone()).await;
    assert_eq!(status["state"], "working");
    tokio::task::spawn_blocking(move || {
        reached
            .recv_timeout(Duration::from_secs(10))
            .expect("the claimed preview must reach its gate")
    })
    .await
    .unwrap();
    release
}

async fn run_reviewed(service: &Service, key: &Value, preview: &Value) -> Value {
    call(
        service,
        SYNC_RUN,
        json!({
            "id": key["id"], "approved_left_deletions": 0, "approved_right_deletions": 0,
            "review_token": preview["preview"]["review_token"],
        }),
    )
    .await;
    settled(service, key).await
}

#[tokio::test]
async fn an_edit_during_preview_invalidates_every_old_choice() {
    let (directory, service) = fixture();
    let mut settings = configuration(directory.path(), "Conflict");
    let left = directory.path().join("Conflict-left/text.md");
    let right = directory.path().join("Conflict-right/text.md");
    fs::write(&left, b"left original").unwrap();
    fs::write(&right, b"right original").unwrap();
    let pair = call(&service, PAIR_CREATE, settings.clone()).await;
    let key = json!({"id": pair["id"]});
    let release = hold_preview(&service, &key).await;

    settings["name"] = json!("Saved for later runs");
    let saved = call(
        &service,
        PAIR_UPDATE,
        json!({
            "id": pair["id"], "revision": 1, "configuration": settings,
        }),
    )
    .await;
    release.send(()).unwrap();
    assert_eq!(saved["revision"], 2);
    let preview = settled(&service, &key).await;
    assert_eq!(preview["state"], "previewed", "{preview}");
    assert_eq!(preview["preview"]["pair_revision"], 1);
    assert_eq!(
        preview["preview"]["items"][0]["action"],
        json!({
            "kind": "conflict", "conflict": "bothChanged",
        })
    );

    for keep in ["left", "right", "both", "skip"] {
        let error = service
            .handle(Request::call(
                2,
                SYNC_RESOLVE,
                Some(json!({
                    "id": pair["id"], "path": b"text.md", "keep": keep,
                    "review_token": preview["preview"]["review_token"],
                })),
            ))
            .await
            .unwrap_err();
        assert_eq!(
            error.code(),
            PREVIEW_REQUIRED,
            "{keep} must reject the old preview: {error:?}"
        );
    }
    assert_eq!(fs::read(left).unwrap(), b"left original");
    assert_eq!(fs::read(right).unwrap(), b"right original");
    for side in ["Conflict-left", "Conflict-right"] {
        let documents = fs::read_dir(directory.path().join(side))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "md")
            })
            .count();
        assert_eq!(
            documents, 1,
            "stale Keep Both must not create a second file"
        );
    }
}

#[tokio::test]
async fn archive_browsing_keeps_active_roots_until_the_worker_finishes() {
    let (directory, service) = fixture();
    let settings = configuration(directory.path(), "Archived");
    let left = directory.path().join("Archived-left/file");
    fs::write(&left, b"recoverable data").unwrap();
    let pair = call(&service, PAIR_CREATE, settings).await;
    let key = json!({"id": pair["id"]});
    call(&service, SYNC_PREVIEW, key.clone()).await;
    let preview = settled(&service, &key).await;
    let copied = run_reviewed(&service, &key, &preview).await;
    assert_eq!(copied["state"], "finished", "{copied}");
    assert_eq!(copied["result"]["copied"], 1);

    fs::remove_file(left).unwrap();
    call(&service, SYNC_PREVIEW, key.clone()).await;
    let preview = settled(&service, &key).await;
    let archived = run_reviewed(&service, &key, &preview).await;
    assert_eq!(archived["state"], "finished", "{archived}");
    assert_eq!(archived["result"]["archived"], 1);
    let side = json!({"id": pair["id"], "side": "right"});
    let before = call(&service, ARCHIVE_RUNS, side.clone()).await;
    assert_eq!(before["runs"].as_array().unwrap().len(), 1);
    let run = before["runs"][0]["name"].clone();

    let release = hold_preview(&service, &key).await;
    let replacement = configuration(directory.path(), "NewRoots");
    let saved = call(
        &service,
        PAIR_UPDATE,
        json!({
            "id": pair["id"], "revision": 1, "configuration": replacement,
        }),
    )
    .await;
    assert_eq!(saved["revision"], 2);
    let while_running = call(&service, ARCHIVE_RUNS, side.clone()).await;
    assert_eq!(
        while_running, before,
        "pending roots must not hide the running pair's archives"
    );
    let items = call(
        &service,
        ARCHIVE_ITEMS,
        json!({
            "id": pair["id"], "side": "right", "run": run, "after": null,
        }),
    )
    .await;
    assert_eq!(items["items"], json!([{"path": b"file", "size": 16}]));

    release.send(()).unwrap();
    let finished = settled(&service, &key).await;
    assert_eq!(finished["state"], "previewed", "{finished}");
    let after = call(&service, ARCHIVE_RUNS, side).await;
    assert_eq!(
        after["runs"],
        json!([]),
        "completed workers must release the old roots"
    );
    assert!(
        directory
            .path()
            .join("Archived-right/.foldover/archive")
            .is_dir(),
        "switching active roots must leave the old archive intact"
    );
}
