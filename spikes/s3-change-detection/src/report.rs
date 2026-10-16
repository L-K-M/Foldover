//! Report types serialized to the machine-readable JSON the spike writes.
//! Kept free of macOS dependencies so they build and test on Linux.

use serde::Serialize;
use serde_json::Value;

/// Outcome of one experiment. `pass` and `fail` are measured results;
/// `error` means the harness itself could not run the measurement and the
/// result carries no evidence either way.
#[derive(Serialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Fail,
    Error,
}

#[derive(Serialize)]
pub struct HostInfo {
    pub product_version: String,
    pub build: String,
    pub machine: String,
}

#[derive(Serialize)]
pub struct Experiment {
    pub id: &'static str,
    pub title: &'static str,
    pub status: Status,
    /// True only for a measured pass, never for a harness error.
    pub pass: bool,
    pub detail: Value,
}

/// The pass criteria from PLAN.md section 8, made checkable.
#[derive(Serialize)]
pub struct Verdict {
    /// Replay after a stored event ID reported all 500 changes.
    pub replay_all_500: bool,
    /// DiskArbitration appeared and disappeared callbacks reached this
    /// process on a CFRunLoop.
    pub mount_unmount_callbacks: bool,
    /// Device-relative stream availability was measured, whichever way the
    /// answer came out.
    pub device_stream_reported: bool,
    pub all_criteria_met: bool,
}

#[derive(Serialize)]
pub struct Report {
    pub spike: &'static str,
    pub generated_unix_secs: u64,
    pub host: HostInfo,
    pub experiments: Vec<Experiment>,
    pub verdict: Verdict,
}

// ---- Per-experiment detail structs -------------------------------------

/// One operation's observation in experiment 1.
#[derive(Serialize)]
pub struct OpResult {
    pub op: &'static str,
    pub matched_events: usize,
    /// Union of the flag bits on the events attributed to this operation.
    pub flags: Vec<String>,
    /// Whether every event carried extended data (file ID).
    pub extended_data: bool,
    /// No event for the touched path arrived before the timeout.
    pub missed: bool,
    pub latency_ms_min: Option<f64>,
    pub latency_ms_mean: Option<f64>,
    pub latency_ms_max: Option<f64>,
    /// The operation itself failed (e.g. xattr on ExFAT); not an event miss.
    pub op_error: Option<String>,
}

#[derive(Serialize)]
pub struct FlagsLatencyDetail {
    pub volume: String,
    pub ops: Vec<OpResult>,
    /// Any drop or rescan flag seen during the experiment.
    pub drop_flags_seen: bool,
}

#[derive(Serialize)]
pub struct ReplayDetail {
    pub volume: String,
    /// Event ID the replay stream was started from.
    pub checkpoint_id: u64,
    /// `FSEventsGetLastEventIdForDeviceBeforeTime` before the changes, for
    /// comparison with the callback-derived checkpoint.
    pub device_last_id_before: Option<u64>,
    pub changes_made: usize,
    /// Changed paths visible in events delivered before HistoryDone.
    pub changes_seen: usize,
    /// Changed paths seen at all, including live events after
    /// HistoryDone: counted here but not above means late, not lost.
    pub changes_seen_total: usize,
    pub history_done_arrived: bool,
    pub replay_ms: Option<f64>,
    pub drop_flags_seen: bool,
    /// Up to ten missing paths, to keep the report small.
    pub missing_sample: Vec<String>,
    pub note: Option<String>,
}

#[derive(Serialize)]
pub struct DeviceStreamDetail {
    pub device: i64,
    /// The path list handed to FSEventStreamCreateRelativeToDevice.
    pub paths_watched: Vec<String>,
    pub created: bool,
    pub started: bool,
    pub events_seen: usize,
    /// An event for a marker file we wrote arrived on the stream.
    pub marker_seen: bool,
    /// First paths the stream reported; reveals whether a device stream
    /// reports absolute or device-relative paths.
    pub sample_paths: Vec<String>,
    /// Whether a per-device stream is usable for an unprivileged process;
    /// None when the measurement was inconclusive.
    pub available: Option<bool>,
    pub note: String,
}

#[derive(Serialize)]
pub struct VolumeUuidsDetail {
    pub boot_device: i64,
    pub boot_uuid: Option<String>,
    pub images: Vec<ImageUuid>,
}

#[derive(Serialize)]
pub struct ImageUuid {
    pub filesystem: String,
    pub before_detach: Option<String>,
    pub after_reattach: Option<String>,
    pub equal: Option<bool>,
}

#[derive(Serialize)]
pub struct ImagesDetail {
    pub images: Vec<ImageDetail>,
}

#[derive(Serialize)]
pub struct ImageDetail {
    pub filesystem: String,
    pub volume_name: String,
    pub attached: bool,
    pub device: Option<i64>,
    pub uuid_before_detach: Option<String>,
    pub uuid_after_reattach: Option<String>,
    pub uuid_stayed_equal: Option<bool>,
    /// E1-style operation probe on the mounted image.
    pub ops: Option<FlagsLatencyDetail>,
    /// E2-style replay probe on the mounted image.
    pub replay: Option<ReplayDetail>,
    /// After detach + reattach: stream from the pre-detach event ID.
    pub reattach_replay: Option<ReattachReplay>,
    pub note: Option<String>,
}

#[derive(Serialize)]
pub struct ReattachReplay {
    pub since_event_id: u64,
    pub change_seen: bool,
    pub history_done_arrived: bool,
    pub events_seen: usize,
}

#[derive(Serialize)]
pub struct DiskEventRec {
    pub kind: &'static str,
    pub bsd_name: Option<String>,
    pub volume_path: Option<String>,
    pub volume_uuid: Option<String>,
    pub latency_ms: Option<f64>,
}

#[derive(Serialize)]
pub struct DiskArbDetail {
    pub appeared_received: bool,
    pub appeared_latency_ms: Option<f64>,
    pub disappeared_received: bool,
    pub disappeared_latency_ms: Option<f64>,
    pub appeared: Vec<DiskEventRec>,
    pub disappeared: Vec<DiskEventRec>,
    pub baseline_disks_seen: usize,
    pub note: Option<String>,
}

#[derive(Serialize)]
pub struct FlagCount {
    pub name: String,
    pub count: usize,
}

#[derive(Serialize)]
pub struct BurstDetail {
    pub volume: String,
    pub files_created: usize,
    pub create_seconds: f64,
    pub creates_per_second: f64,
    pub events_received: usize,
    pub distinct_paths_seen: usize,
    pub flag_counts: Vec<FlagCount>,
    pub drop_flags_seen: bool,
    pub drain_seconds: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn report_serializes_expected_shape() {
        let report = Report {
            spike: "s3-change-detection",
            generated_unix_secs: 1,
            host: HostInfo {
                product_version: "26.0".into(),
                build: "25A1".into(),
                machine: "arm64".into(),
            },
            experiments: vec![Experiment {
                id: "e1_item_flags",
                title: "per-operation item flags and latency",
                status: Status::Pass,
                pass: true,
                detail: json!({"ops": []}),
            }],
            verdict: Verdict {
                replay_all_500: true,
                mount_unmount_callbacks: true,
                device_stream_reported: true,
                all_criteria_met: true,
            },
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&report).unwrap()).unwrap();
        assert_eq!(v["spike"], "s3-change-detection");
        assert_eq!(v["experiments"][0]["status"], "pass");
        assert_eq!(v["verdict"]["all_criteria_met"], true);
    }
}
