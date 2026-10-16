//! Experiment orchestration: runs E1..E7 against temp directories and
//! hdiutil images, assembles the JSON report and the human summary.

use crate::diskarb::{DaSession, DiskEvent, DiskEventKind};
use crate::flags;
use crate::fsevents::{EventStream, SharedEvents, StreamStart, path_is, wait_until};
use crate::hdiutil::{self, AttachedImage, ImageFs};
use crate::ops::{self, FsOp};
use crate::report::*;
use crate::sys;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// FSEvents `latency` argument for every stream in the spike.
const LATENCY_SECS: f64 = 0.1;
const REPLAY_COUNT: usize = 500;
const BURST_COUNT: usize = 100_000;
const WARMUP_WAIT: Duration = Duration::from_secs(20);
const OP_WAIT: Duration = Duration::from_secs(12);
const OP_SETTLE: Duration = Duration::from_millis(400);
const HISTORY_WAIT: Duration = Duration::from_secs(90);
const DA_WAIT: Duration = Duration::from_secs(30);
const BURST_QUIET: Duration = Duration::from_secs(5);
const BURST_CAP: Duration = Duration::from_secs(180);

fn shell(cmd: &str, arg: &str) -> String {
    Command::new(cmd)
        .arg(arg)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn host_info() -> HostInfo {
    HostInfo {
        product_version: shell("sw_vers", "-productVersion"),
        build: shell("sw_vers", "-buildVersion"),
        machine: shell("uname", "-m"),
    }
}

fn dev_of(path: &Path) -> Result<i32, String> {
    fs::metadata(path)
        .map(|m| m.dev() as i32)
        .map_err(|e| format!("stat {}: {e}", path.display()))
}

/// True when `event_path` names exactly `file_name` (basename match; a
/// device-relative stream may report paths relative to the volume root).
fn event_names_file(event_path: &str, file_name: &str) -> bool {
    event_path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .is_some_and(|n| n == file_name)
}

/// Touch a marker file and wait for its event; proves the stream is live
/// before an experiment starts measuring.
fn warmup(shared: &SharedEvents, dir: &Path, tag: &str) -> bool {
    let mark = shared.inspect(|i| i.events.len());
    let p = dir.join(format!("warmup-{tag}"));
    if fs::write(&p, b"w").is_err() {
        return false;
    }
    wait_until(shared, WARMUP_WAIT, |inner| {
        inner.events[mark..].iter().any(|e| path_is(&e.path, &p))
    })
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One experiment's measured outcome. Err means the harness itself could
/// not produce a measurement.
struct Measured {
    pass: bool,
    detail: Value,
}

fn measured(pass: bool, detail: impl serde::Serialize) -> Result<Measured, String> {
    Ok(Measured {
        pass,
        detail: serde_json::to_value(detail).unwrap_or_else(|_| json!({"serialize": "failed"})),
    })
}

fn attempt(
    id: &'static str,
    title: &'static str,
    f: impl FnOnce() -> Result<Measured, String>,
) -> Experiment {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(m)) => Experiment {
            id,
            title,
            status: if m.pass { Status::Pass } else { Status::Fail },
            pass: m.pass,
            detail: m.detail,
        },
        Ok(Err(e)) => Experiment {
            id,
            title,
            status: Status::Error,
            pass: false,
            detail: json!({"error": e}),
        },
        Err(_) => Experiment {
            id,
            title,
            status: Status::Error,
            pass: false,
            detail: json!({"error": "panic in experiment"}),
        },
    }
}

// ---- E1: per-operation item flags and latency ---------------------------

/// Measure one op: apply it, wait for the first matching event, then let
/// the pipe settle briefly so coalesced flags land in the union.
fn measure_op(shared: &Arc<SharedEvents>, dir: &Path, op: FsOp) -> OpResult {
    let mark = shared.inspect(|i| i.events.len());
    let t0 = Instant::now();
    let outcome = match ops::apply(op, dir) {
        Ok(o) => o,
        Err(e) => {
            return OpResult {
                op: op.name(),
                matched_events: 0,
                flags: vec![],
                extended_data: false,
                missed: false,
                latency_ms_min: None,
                latency_ms_mean: None,
                latency_ms_max: None,
                op_error: Some(e),
            };
        }
    };
    let matches =
        |e: &crate::fsevents::EventRec| outcome.watched_paths.iter().any(|p| path_is(&e.path, p));
    let deadline = t0 + OP_WAIT;
    let mut last_match: Option<Instant> = None;
    loop {
        let now = Instant::now();
        let latest = shared.inspect(|inner| {
            inner.events[mark..]
                .iter()
                .filter(|e| matches(e))
                .map(|e| e.at)
                .max()
        });
        if latest.is_some() {
            last_match = latest;
        }
        if now >= deadline {
            break;
        }
        if last_match.is_some_and(|l| now.duration_since(l) >= OP_SETTLE) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let (union, latencies, all_extended, n) = shared.inspect(|inner| {
        let mut union = 0u32;
        let mut lat = Vec::new();
        let mut all_ext = true;
        let mut n = 0usize;
        for e in &inner.events[mark..] {
            if matches(e) {
                n += 1;
                union |= e.flags;
                lat.push(e.at.duration_since(t0).as_secs_f64() * 1000.0);
                if e.file_id.is_none() {
                    all_ext = false;
                }
            }
        }
        (union, lat, all_ext, n)
    });
    let round = |v: f64| (v * 100.0).round() / 100.0;
    let (min, mean, max) = if latencies.is_empty() {
        (None, None, None)
    } else {
        (
            latencies.iter().cloned().reduce(f64::min).map(round),
            Some(round(
                latencies.iter().sum::<f64>() / latencies.len() as f64,
            )),
            latencies.iter().cloned().reduce(f64::max).map(round),
        )
    };
    OpResult {
        op: op.name(),
        matched_events: n,
        flags: flags::flag_names(union),
        extended_data: n > 0 && all_extended,
        missed: n == 0,
        latency_ms_min: min,
        latency_ms_mean: mean,
        latency_ms_max: max,
        op_error: None,
    }
}

/// The E1 op matrix against `dir` on the volume named `label`.
fn ops_probe(dir: &Path, label: &str) -> Result<FlagsLatencyDetail, String> {
    fs::create_dir_all(dir).map_err(|e| format!("mkdir: {e}"))?;
    let path = dir.to_string_lossy().into_owned();
    let stream = match EventStream::start(&[path], flags::SINCE_NOW, LATENCY_SECS) {
        StreamStart::Started(s) => s,
        StreamStart::FailedCreate => {
            return Err("FSEventStreamCreate returned NULL".into());
        }
        StreamStart::FailedStart => return Err("FSEventStreamStart returned false".into()),
    };
    let shared = stream.shared();
    let _warmed = warmup(&shared, dir, "e1");
    let ops: Vec<OpResult> = FsOp::ALL
        .iter()
        .map(|op| measure_op(&shared, dir, *op))
        .collect();
    let drop_flags = shared.inspect(|i| (i.union_flags & flags::DROP_MASK) != 0);
    drop(stream);
    Ok(FlagsLatencyDetail {
        volume: label.into(),
        ops,
        drop_flags_seen: drop_flags,
    })
}

// ---- E2: replay from a stored event ID ----------------------------------

/// Returns the detail plus the highest event ID the replay stream
/// delivered; the caller uses it as the detach/reattach checkpoint.
fn replay_probe(dir: &Path, count: usize, label: &str) -> Result<(ReplayDetail, u64), String> {
    fs::create_dir_all(dir).map_err(|e| format!("mkdir: {e}"))?;
    let path = dir.to_string_lossy().into_owned();
    let stream1 =
        match EventStream::start(std::slice::from_ref(&path), flags::SINCE_NOW, LATENCY_SECS) {
            StreamStart::Started(s) => s,
            _ => return Err("path stream failed to create or start".into()),
        };
    let shared1 = stream1.shared();
    let warmed = warmup(&shared1, dir, "e2");
    // Checkpoint: the highest event ID the stream delivered.
    let checkpoint = shared1.inspect(|i| i.events.iter().map(|e| e.id).max().unwrap_or(0));
    drop(stream1);

    let dev = dev_of(dir)?;
    let device_last = unsafe {
        sys::FSEventsGetLastEventIdForDeviceBeforeTime(dev, sys::CFAbsoluteTimeGetCurrent())
    };
    // A zero checkpoint would replay the volume's entire event history;
    // fall back to the device's last ID when warmup delivered nothing.
    let since = if checkpoint == 0 {
        device_last
    } else {
        checkpoint
    };

    ops::create_files(dir, "replay", count).map_err(|e| format!("create files: {e}"))?;
    let names: Vec<String> = (0..count).map(|i| format!("replay-{i:06}.dat")).collect();

    let t0 = Instant::now();
    let stream2 = match EventStream::start(&[path], since, LATENCY_SECS) {
        StreamStart::Started(s) => s,
        _ => return Err("replay stream failed to create or start".into()),
    };
    let shared2 = stream2.shared();
    let history_done = wait_until(&shared2, HISTORY_WAIT, |inner| {
        inner
            .events
            .iter()
            .any(|e| (e.flags & flags::event::HISTORY_DONE) != 0)
    });
    let replay_ms = shared2.inspect(|i| {
        i.events
            .iter()
            .find(|e| (e.flags & flags::event::HISTORY_DONE) != 0)
            .map(|e| e.at.duration_since(t0).as_secs_f64() * 1000.0)
    });
    // Everything before the HistoryDone event counts as replayed history.
    let (seen, missing, drop_flags, max_id) = shared2.inspect(|inner| {
        let hd_idx = inner
            .events
            .iter()
            .position(|e| (e.flags & flags::event::HISTORY_DONE) != 0)
            .unwrap_or(inner.events.len());
        let before = &inner.events[..hd_idx.min(inner.events.len())];
        let mut seen = 0usize;
        let mut missing = Vec::new();
        for n in &names {
            let p = dir.join(n);
            if before.iter().any(|e| path_is(&e.path, &p)) {
                seen += 1;
            } else if missing.len() < 10 {
                missing.push(n.clone());
            }
        }
        let drop = before.iter().any(|e| (e.flags & flags::DROP_MASK) != 0);
        let max = before.iter().map(|e| e.id).max().unwrap_or(since);
        (seen, missing, drop, max)
    });
    drop(stream2);
    let note = if warmed {
        None
    } else {
        Some("warmup delivered no events; replayed from device last ID".into())
    };
    Ok((
        ReplayDetail {
            volume: label.into(),
            checkpoint_id: since,
            device_last_id_before: Some(device_last),
            changes_made: count,
            changes_seen: seen,
            history_done_arrived: history_done,
            replay_ms: replay_ms.map(|v| (v * 100.0).round() / 100.0),
            drop_flags_seen: drop_flags,
            missing_sample: missing,
            note,
        },
        max_id,
    ))
}

// ---- E3: device-relative stream as an unprivileged user ------------------

fn device_relative_probe(dir: &Path) -> Result<Measured, String> {
    let dev = dev_of(dir)?;
    // Firmlinks make a lexical device-relative path unreliable (/tmp lives
    // on the data volume but reads as /private/tmp), so the probe watches
    // the whole device and matches a uniquely named marker by basename.
    let marker = format!("s3e3marker-{}", std::process::id());
    let marker_path = dir.join(&marker);
    let watched = vec!["/".to_string()];
    let (created, started, events_seen, marker_seen, samples, note) =
        match EventStream::start_relative(dev, &watched, flags::SINCE_NOW, LATENCY_SECS) {
            StreamStart::FailedCreate => (
                false,
                false,
                0,
                false,
                Vec::new(),
                "FSEventStreamCreateRelativeToDevice returned NULL".to_string(),
            ),
            StreamStart::FailedStart => (
                true,
                false,
                0,
                false,
                Vec::new(),
                "stream created but FSEventStreamStart returned false".to_string(),
            ),
            StreamStart::Started(s) => {
                let shared = s.shared();
                fs::write(&marker_path, b"m").map_err(|e| format!("write marker: {e}"))?;
                let got = wait_until(&shared, WARMUP_WAIT, |inner| {
                    inner
                        .events
                        .iter()
                        .any(|e| event_names_file(&e.path, &marker))
                });
                let (n, samples) = shared.inspect(|i| {
                    (
                        i.events.len(),
                        i.events
                            .iter()
                            .take(5)
                            .map(|e| e.path.clone())
                            .collect::<Vec<_>>(),
                    )
                });
                drop(s);
                let note = if got {
                    "device-relative stream delivered our marker event".to_string()
                } else {
                    "stream started but no marker event in the window".to_string()
                };
                (true, true, n, got, samples, note)
            }
        };
    // Usable means: created, started, and actually delivered our event.
    // Every clean outcome is a reportable answer, so `available` is always
    // Some once we get here.
    let available = Some(created && started && marker_seen);
    eprintln!("[s3] e3: {note}");
    measured(
        available.is_some(),
        DeviceStreamDetail {
            device: dev as i64,
            paths_watched: watched,
            created,
            started,
            events_seen,
            marker_seen,
            sample_paths: samples,
            available,
            note,
        },
    )
}

// ---- E5 helpers ----------------------------------------------------------

struct ReattachOutcome {
    replay: ReattachReplay,
    attached: Option<AttachedImage>,
}

/// Detach, reattach, then test whether the pre-detach event ID still
/// replays history on the reattached volume. The post-reattach file is
/// written before the replay stream starts so only history can report it.
fn reattach_probe(
    dmg: &Path,
    mount_point: &Path,
    watch_dir: &Path,
    since_id: u64,
) -> ReattachOutcome {
    let empty = |attached| ReattachOutcome {
        replay: ReattachReplay {
            since_event_id: since_id,
            change_seen: false,
            history_done_arrived: false,
            events_seen: 0,
        },
        attached,
    };
    let _ = hdiutil::detach(&mount_point.to_string_lossy());
    std::thread::sleep(Duration::from_millis(500));
    let att = match hdiutil::attach(dmg, mount_point) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[s3] reattach failed: {e}");
            return empty(None);
        }
    };
    let post = watch_dir.join("post-reattach-marker");
    let _ = fs::write(&post, b"post");
    let path = watch_dir.to_string_lossy().into_owned();
    let stream = EventStream::start(&[path], since_id, LATENCY_SECS);
    match stream {
        StreamStart::Started(s) => {
            let shared = s.shared();
            let hd = wait_until(&shared, HISTORY_WAIT, |inner| {
                inner
                    .events
                    .iter()
                    .any(|e| (e.flags & flags::event::HISTORY_DONE) != 0)
            });
            let (seen, n) = shared.inspect(|inner| {
                let hd_idx = inner
                    .events
                    .iter()
                    .position(|e| (e.flags & flags::event::HISTORY_DONE) != 0)
                    .unwrap_or(inner.events.len());
                let before = &inner.events[..hd_idx.min(inner.events.len())];
                (
                    before.iter().any(|e| path_is(&e.path, &post)),
                    inner.events.len(),
                )
            });
            drop(s);
            ReattachOutcome {
                replay: ReattachReplay {
                    since_event_id: since_id,
                    change_seen: seen,
                    history_done_arrived: hd,
                    events_seen: n,
                },
                attached: Some(att),
            }
        }
        _ => empty(Some(att)),
    }
}

// ---- E5 body -------------------------------------------------------------

fn disk_images(
    work: &Path,
    apfs_dmg: &mut Option<PathBuf>,
    image_uuids: &mut Vec<ImageUuid>,
) -> Result<Measured, String> {
    let mut images = Vec::new();
    let mut any_measured = false;
    for fs_kind in [ImageFs::Apfs, ImageFs::ExFat] {
        let label = fs_kind.label();
        let mut img = ImageDetail {
            filesystem: label.into(),
            volume_name: format!("folds3{label}"),
            attached: false,
            device: None,
            uuid_before_detach: None,
            uuid_after_reattach: None,
            uuid_stayed_equal: None,
            ops: None,
            replay: None,
            reattach_replay: None,
            note: None,
        };
        let dmg = match hdiutil::create_image(work, fs_kind, 64) {
            Ok(d) => d,
            Err(e) => {
                img.note = Some(format!("create image: {e}"));
                images.push(img);
                continue;
            }
        };
        if let ImageFs::Apfs = fs_kind {
            *apfs_dmg = Some(dmg.clone());
        }
        let mp = work.join(format!("mnt-{label}"));
        let att = match hdiutil::attach(&dmg, &mp) {
            Ok(a) => a,
            Err(e) => {
                img.note = Some(format!("attach: {e}"));
                images.push(img);
                continue;
            }
        };
        img.attached = true;
        img.device = Some(att.dev as i64);
        img.uuid_before_detach = unsafe { sys::fsevents_uuid_for_device(att.dev) };
        let watch = mp.join("s3watch");
        if let Err(e) = fs::create_dir_all(&watch) {
            img.note = Some(format!("mkdir watch: {e}"));
            let _ = hdiutil::detach(&mp.to_string_lossy());
            images.push(img);
            continue;
        }
        if let Err(e) = ops_probe(&watch, label).map(|d| img.ops = Some(d)) {
            img.note = Some(format!("ops probe: {e}"));
        }
        match replay_probe(&watch, REPLAY_COUNT, label) {
            Ok((d, last_id)) => {
                img.replay = Some(d);
                let outcome = reattach_probe(&dmg, &mp, &watch, last_id);
                img.reattach_replay = Some(outcome.replay);
                if let Some(att2) = outcome.attached {
                    img.uuid_after_reattach = unsafe { sys::fsevents_uuid_for_device(att2.dev) };
                    let _ = hdiutil::detach(&att2.mount_point.to_string_lossy());
                }
            }
            Err(e) => {
                img.note = Some(format!("replay probe: {e}"));
                let _ = hdiutil::detach(&mp.to_string_lossy());
            }
        }
        img.uuid_stayed_equal = match (&img.uuid_before_detach, &img.uuid_after_reattach) {
            (Some(a), Some(b)) => Some(a == b),
            _ => None,
        };
        image_uuids.push(ImageUuid {
            filesystem: label.into(),
            before_detach: img.uuid_before_detach.clone(),
            after_reattach: img.uuid_after_reattach.clone(),
            equal: img.uuid_stayed_equal,
        });
        if img.replay.is_some() {
            any_measured = true;
        }
        images.push(img);
    }
    measured(any_measured, ImagesDetail { images })
}

// ---- E6 body -------------------------------------------------------------

fn disk_arbitration_probe(work: &Path, dmg: Option<&Path>) -> Result<Measured, String> {
    let session = DaSession::start()?;
    // Registration delivers appeared events for already-attached disks;
    // let that snapshot land before measuring.
    std::thread::sleep(Duration::from_secs(1));
    let baseline = session
        .shared
        .inner
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .appeared
        .len();

    let dmg_path = match dmg {
        Some(d) => d.to_path_buf(),
        None => hdiutil::create_image(work, ImageFs::Apfs, 64)?,
    };
    let mp = work.join("mnt-da");
    let mp_str = mp.to_string_lossy().into_owned();
    let t_attach = Instant::now();
    let att = hdiutil::attach(&dmg_path, &mp)?;
    eprintln!(
        "[s3] e6 attached {} at {} (dev {})",
        att.dmg.display(),
        mp.display(),
        att.dev
    );
    let shared = session.shared.clone();
    let matches_attach = |e: &DiskEvent, att: &AttachedImage| {
        e.volume_path
            .as_deref()
            .is_some_and(|p| p == mp_str.as_str())
            || e.bsd_name
                .as_deref()
                .is_some_and(|b| att.bsd_names.iter().any(|n| n == b))
    };
    let deadline = t_attach + DA_WAIT;
    let mut appeared_at: Option<Instant> = None;
    while Instant::now() < deadline {
        let hit = shared.inspect(|inner| {
            inner
                .appeared
                .iter()
                .find(|e| e.at >= t_attach && matches_attach(e, &att))
                .map(|e| e.at)
        });
        if hit.is_some() {
            appeared_at = hit;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let t_detach = Instant::now();
    let _ = hdiutil::detach(&mp_str);
    let mut disappeared_at: Option<Instant> = None;
    let deadline = t_detach + DA_WAIT;
    while Instant::now() < deadline {
        let hit = shared.inspect(|inner| {
            inner
                .disappeared
                .iter()
                .find(|e| e.at >= t_detach && matches_attach(e, &att))
                .map(|e| e.at)
        });
        if hit.is_some() {
            disappeared_at = hit;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let (appeared, disappeared) = {
        let inner = shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        let map = |e: &DiskEvent| DiskEventRec {
            kind: match e.kind {
                DiskEventKind::Appeared => "appeared",
                DiskEventKind::Disappeared => "disappeared",
            },
            bsd_name: e.bsd_name.clone(),
            volume_path: e.volume_path.clone(),
            volume_uuid: e.volume_uuid.clone(),
            latency_ms: None,
        };
        (
            inner.appeared.iter().map(map).collect::<Vec<_>>(),
            inner.disappeared.iter().map(map).collect::<Vec<_>>(),
        )
    };
    session.stop();

    let appeared_latency = appeared_at
        .map(|t| (t.duration_since(t_attach).as_secs_f64() * 1000.0 * 100.0).round() / 100.0);
    let disappeared_latency = disappeared_at
        .map(|t| (t.duration_since(t_detach).as_secs_f64() * 1000.0 * 100.0).round() / 100.0);
    let d = DiskArbDetail {
        appeared_received: appeared_at.is_some(),
        appeared_latency_ms: appeared_latency,
        disappeared_received: disappeared_at.is_some(),
        disappeared_latency_ms: disappeared_latency,
        appeared,
        disappeared,
        baseline_disks_seen: baseline,
        note: None,
    };
    let pass = appeared_at.is_some() && disappeared_at.is_some();
    measured(pass, d)
}

// ---- E7 body -------------------------------------------------------------

fn burst_probe(work: &Path) -> Result<Measured, String> {
    let dir = work.join("e7-burst");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let path = dir.to_string_lossy().into_owned();
    let stream = match EventStream::start(&[path], flags::SINCE_NOW, LATENCY_SECS) {
        StreamStart::Started(s) => s,
        _ => return Err("burst stream failed to create or start".into()),
    };
    let shared = stream.shared();
    let _ = warmup(&shared, &dir, "e7");
    let mark = shared.inspect(|i| i.events.len());
    let create_secs = ops::create_files(&dir, "burst", BURST_COUNT)
        .map_err(|e| format!("create burst: {e}"))?
        .as_secs_f64();
    // Drain until the event log is quiet for BURST_QUIET or the cap hits.
    let drain_start = Instant::now();
    let cap = drain_start + BURST_CAP;
    let mut quiet_since: Option<Instant> = None;
    let mut last_n = 0usize;
    while Instant::now() < cap {
        std::thread::sleep(Duration::from_millis(250));
        let n = shared.inspect(|i| i.events.len());
        if n == last_n {
            quiet_since = quiet_since.or(Some(Instant::now()));
        } else {
            quiet_since = None;
            last_n = n;
        }
        if quiet_since.is_some_and(|t| t.elapsed() >= BURST_QUIET) {
            break;
        }
    }
    let drain_seconds = drain_start.elapsed().as_secs_f64();

    let (events_received, distinct, flag_counts, dropped) = shared.inspect(|inner| {
        let mut distinct = HashSet::new();
        let mut hist: BTreeMap<String, usize> = BTreeMap::new();
        let mut dropped = false;
        let mut n = 0usize;
        for e in &inner.events[mark..] {
            n += 1;
            distinct.insert(e.path.clone());
            for name in flags::flag_names(e.flags) {
                *hist.entry(name).or_default() += 1;
            }
            if (e.flags & flags::DROP_MASK) != 0 {
                dropped = true;
            }
        }
        let flag_counts = hist
            .into_iter()
            .map(|(name, count)| FlagCount { name, count })
            .collect();
        (n, distinct.len(), flag_counts, dropped)
    });
    drop(stream);
    let d = BurstDetail {
        volume: "boot".into(),
        files_created: BURST_COUNT,
        create_seconds: (create_secs * 1000.0).round() / 1000.0,
        creates_per_second: (BURST_COUNT as f64 / create_secs).round(),
        events_received,
        distinct_paths_seen: distinct,
        flag_counts,
        drop_flags_seen: dropped,
        drain_seconds: (drain_seconds * 1000.0).round() / 1000.0,
    };
    // Usable signal = per-file events arrived or an explicit drop/rescan
    // flag told us to rescan.
    let pass = events_received > 0 || dropped;
    measured(pass, d)
}

// ---- Human summary -------------------------------------------------------

fn detail_line(exp: &Experiment) -> String {
    let id = exp.id;
    let d = &exp.detail;
    if let Some(e) = d.get("error").and_then(|e| e.as_str()) {
        return format!("{id}: ERROR: {e}");
    }
    match id {
        "e1_item_flags" => {
            let ops: Vec<String> = d["ops"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|o| {
                            let name = o["op"].as_str().unwrap_or("?");
                            if o["missed"].as_bool().unwrap_or(false) {
                                format!("{name}=MISSED")
                            } else if o["op_error"].is_string() {
                                format!("{name}=op-error")
                            } else {
                                let lat = o["latency_ms_mean"].as_f64().unwrap_or(-1.0);
                                let fl = o["flags"]
                                    .as_array()
                                    .map(|f| {
                                        f.iter()
                                            .filter_map(|x| x.as_str())
                                            .collect::<Vec<_>>()
                                            .join(",")
                                    })
                                    .unwrap_or_default();
                                format!("{name}={lat}ms [{fl}]")
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!("{id}: {}", ops.join("; "))
        }
        "e2_replay" => format!(
            "{id}: {}/{} changes replayed, history_done={}, replay_ms={}",
            d["changes_seen"], d["changes_made"], d["history_done_arrived"], d["replay_ms"]
        ),
        "e3_device_stream" => format!(
            "{id}: available={}, {}",
            d["available"],
            d["note"].as_str().unwrap_or("")
        ),
        "e4_volume_uuids" => format!("{id}: boot_uuid={}", d["boot_uuid"]),
        "e5_disk_images" => {
            let imgs = d["images"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|i| {
                            let mut s = format!(
                                "{}: attached={} ops={} replay={}/{} uuid_stable={}",
                                i["filesystem"],
                                i["attached"],
                                i["ops"].is_object(),
                                i["replay"]["changes_seen"],
                                i["replay"]["changes_made"],
                                i["uuid_stayed_equal"]
                            );
                            if let Some(n) = i["note"].as_str() {
                                s.push_str(&format!(" note={n}"));
                            }
                            s
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            format!("{id}: {}", imgs.join("; "))
        }
        "e6_diskarbitration" => format!(
            "{id}: appeared={}({}ms) disappeared={}({}ms)",
            d["appeared_received"],
            d["appeared_latency_ms"],
            d["disappeared_received"],
            d["disappeared_latency_ms"]
        ),
        "e7_burst" => format!(
            "{id}: {} files in {}s, {} events, dropped={}",
            d["files_created"], d["create_seconds"], d["events_received"], d["drop_flags_seen"]
        ),
        _ => format!("{id}: status={:?}", exp.status),
    }
}

// ---- Entry point ---------------------------------------------------------

pub fn run() -> i32 {
    let Some(json_path) = std::env::args().nth(1) else {
        eprintln!("usage: spike-s3-change-detection <output.json>");
        return 64;
    };
    match run_all(Path::new(&json_path)) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("harness error: {e}");
            1
        }
    }
}

fn run_all(json_path: &Path) -> Result<(), String> {
    let base = std::env::temp_dir().join(format!("foldover-s3-{}", std::process::id()));
    fs::create_dir_all(&base).map_err(|e| format!("create workdir: {e}"))?;
    let work = fs::canonicalize(&base).map_err(|e| format!("canonicalize workdir: {e}"))?;
    eprintln!("[s3] workdir {}", work.display());

    let mut experiments: Vec<Experiment> = Vec::new();
    let mut e2_pass = false;
    let mut e3_reported = false;
    let mut e6_pass = false;
    let mut apfs_dmg: Option<PathBuf> = None;
    let mut image_uuids: Vec<ImageUuid> = Vec::new();

    let e1_dir = work.join("e1");
    let e1 = attempt("e1_item_flags", "per-operation flags and latency", || {
        let d = ops_probe(&e1_dir, "boot")?;
        let missed = d.ops.iter().filter(|o| o.missed).count();
        measured(missed == 0, d)
    });
    experiments.push(e1);

    let e2_dir = work.join("e2");
    let e2 = attempt(
        "e2_replay",
        "replay of 500 changes from a stored event ID",
        || {
            let (d, _max) = replay_probe(&e2_dir, REPLAY_COUNT, "boot")?;
            e2_pass = d.changes_seen == d.changes_made && d.history_done_arrived;
            measured(e2_pass, d)
        },
    );
    experiments.push(e2);

    let e3 = attempt(
        "e3_device_stream",
        "device-relative stream as an unprivileged user",
        || {
            let r = device_relative_probe(&work)?;
            e3_reported = r.detail["available"].is_boolean();
            Ok(r)
        },
    );
    experiments.push(e3);

    // E5 runs before E4: image UUIDs fill E4's table, and E5 keeps the
    // APFS dmg for E6 to reattach.
    let e5 = attempt(
        "e5_disk_images",
        "APFS/ExFAT images: watch, replay, detach+reattach",
        || disk_images(&work, &mut apfs_dmg, &mut image_uuids),
    );
    let e4 = attempt("e4_volume_uuids", "per-device FSEvents UUIDs", || {
        let boot_dev = dev_of(Path::new("/"))?;
        let boot_uuid = unsafe { sys::fsevents_uuid_for_device(boot_dev) };
        measured(
            true,
            VolumeUuidsDetail {
                boot_device: boot_dev as i64,
                boot_uuid,
                images: image_uuids,
            },
        )
    });
    experiments.push(e4);
    experiments.push(e5);

    let e6 = attempt(
        "e6_diskarbitration",
        "mount/unmount callbacks in a non-app process",
        || {
            let r = disk_arbitration_probe(&work, apfs_dmg.as_deref())?;
            e6_pass = r.pass;
            Ok(r)
        },
    );
    experiments.push(e6);

    let e7 = attempt("e7_burst", "100k-file burst: drops and throughput", || {
        burst_probe(&work)
    });
    experiments.push(e7);

    let verdict = Verdict {
        replay_all_500: e2_pass,
        mount_unmount_callbacks: e6_pass,
        device_stream_reported: e3_reported,
        all_criteria_met: e2_pass && e6_pass && e3_reported,
    };

    let report = Report {
        spike: "s3-change-detection",
        generated_unix_secs: now_unix(),
        host: host_info(),
        experiments,
        verdict,
    };
    let text =
        serde_json::to_string_pretty(&report).map_err(|e| format!("serialize report: {e}"))?;
    fs::write(json_path, format!("{text}\n"))
        .map_err(|e| format!("write {}: {e}", json_path.display()))?;

    println!("spike s3-change-detection: macOS change detection in a non-app process");
    for e in &report.experiments {
        println!("{}", detail_line(e));
    }
    let v = &report.verdict;
    println!(
        "verdict: replay_500={} mount_callbacks={} device_stream_reported={} => {}",
        v.replay_all_500,
        v.mount_unmount_callbacks,
        v.device_stream_reported,
        if v.all_criteria_met { "PASS" } else { "FAIL" }
    );
    println!("results: {}", json_path.display());

    // Best-effort cleanup; the CI runner is ephemeral anyway.
    let _ = fs::remove_dir_all(&work);
    Ok(())
}
