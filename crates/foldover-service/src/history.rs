//! Finished runs per pair. Jobs record runs, not previews; clients list
//! them newest first.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use foldover_engine::{Engine, HISTORY_LIMIT, HistoryEntry, HistoryOutcome, RunTrigger};
use foldover_proto::local::{
    History, HistoryRecord, HistoryTrigger, INTERNAL_ERROR, INVALID_PARAMS, Request, RpcError,
    SyncKey, SyncStatus,
};
use foldover_types::{PairId, SyncPath};
use serde_json::Value;

use crate::sync::Work;
use crate::{encode, parameters};

const LEGACY_PATH_PREFIX: &str = "SyncPath(";
const LEGACY_PATH_END: char = ')';

pub(crate) async fn handle(
    engine: &Arc<Mutex<Engine>>,
    request: &Request,
) -> Result<Value, RpcError> {
    let params: SyncKey = parameters(request)?;
    let id: PairId = params
        .id
        .parse()
        .map_err(|error: foldover_types::InvalidPair| {
            RpcError::new(INVALID_PARAMS, error.to_string())
        })?;
    let engine = Arc::clone(engine);
    let entries = tokio::task::spawn_blocking(move || {
        engine
            .lock()
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))?
            .history(id, HISTORY_LIMIT)
            .map_err(crate::engine_error)
    })
    .await
    .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))??;
    encode(History {
        runs: entries.iter().map(wire).collect(),
    })
}

/// Add a finished run to the pair's history. Best effort: a failed write
/// loses one row, never sync state.
pub(crate) fn record(
    engine: &Mutex<Engine>,
    id: PairId,
    work: Work,
    started: SystemTime,
    status: &SyncStatus,
) {
    let trigger = match work {
        Work::Preview => return,
        Work::Run(..) => RunTrigger::Manual,
        Work::Scheduled => RunTrigger::Scheduled,
        Work::Undo => RunTrigger::Undo,
    };
    let unwritten = |outcome, message| HistoryEntry {
        started,
        finished: SystemTime::now(),
        trigger,
        outcome,
        copied: 0,
        moved: 0,
        updated: 0,
        archived: 0,
        recovered: 0,
        conflicts: 0,
        failures: 0,
        archive: None,
        message,
    };
    let entry = match status {
        SyncStatus::Finished { result } => HistoryEntry {
            outcome: if result.cancelled {
                HistoryOutcome::Cancelled
            } else {
                HistoryOutcome::Completed
            },
            copied: result.copied,
            moved: result.moved,
            updated: result.updated,
            archived: result.archived,
            recovered: result.recovered,
            conflicts: result.conflicts,
            failures: result.failure_count,
            archive: Some(result.archive.clone()),
            ..unwritten(HistoryOutcome::Completed, None)
        },
        SyncStatus::Undone { result } => HistoryEntry {
            outcome: if result.cancelled {
                HistoryOutcome::Cancelled
            } else {
                HistoryOutcome::Completed
            },
            archived: result.removed,
            recovered: result.restored.saturating_add(result.beside),
            failures: result.not_undone_count,
            archive: Some(result.archive.clone()),
            ..unwritten(HistoryOutcome::Completed, None)
        },
        SyncStatus::NeedsApproval { .. } => unwritten(HistoryOutcome::NeedsApproval, None),
        SyncStatus::Failed { message } => unwritten(HistoryOutcome::Failed, Some(message.clone())),
        // Cancelled before anything was written.
        SyncStatus::Idle => unwritten(HistoryOutcome::Cancelled, None),
        SyncStatus::Working { .. } | SyncStatus::Previewed { .. } | SyncStatus::Waiting { .. } => {
            return;
        }
    };
    let _ = engine
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .record_history(id, &entry);
}

fn wire(entry: &HistoryEntry) -> HistoryRecord {
    let milliseconds = |time: SystemTime| {
        time.duration_since(UNIX_EPOCH).map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
    };
    HistoryRecord {
        started_ms: milliseconds(entry.started),
        finished_ms: milliseconds(entry.finished),
        trigger: match entry.trigger {
            RunTrigger::Manual => HistoryTrigger::Manual,
            RunTrigger::Scheduled => HistoryTrigger::Scheduled,
            RunTrigger::Undo => HistoryTrigger::Undo,
        },
        outcome: match entry.outcome {
            HistoryOutcome::Completed => foldover_proto::local::HistoryOutcome::Completed,
            HistoryOutcome::Cancelled => foldover_proto::local::HistoryOutcome::Cancelled,
            HistoryOutcome::NeedsApproval => foldover_proto::local::HistoryOutcome::NeedsApproval,
            HistoryOutcome::Failed => foldover_proto::local::HistoryOutcome::Failed,
        },
        copied: entry.copied,
        moved: entry.moved,
        updated: entry.updated,
        archived: entry.archived,
        recovered: entry.recovered,
        conflicts: entry.conflicts,
        failures: entry.failures,
        archive: entry.archive.clone(),
        message: entry.message.as_deref().map(readable_message),
    }
}

fn readable_message(message: &str) -> String {
    let mut readable = String::with_capacity(message.len());
    let mut remaining = message;
    let mut quoted = false;

    // Old history stored Debug byte arrays. Decode on read, preserving the record
    // and quoted filenames that happen to contain text such as SyncPath([97]).
    while let Some(character) = remaining.chars().next() {
        if !quoted && let Some((path, suffix)) = legacy_path(remaining) {
            readable.push_str(&format!("{path:?}"));
            remaining = suffix;
            continue;
        }

        readable.push(character);
        remaining = &remaining[character.len_utf8()..];
        if character == '"' {
            quoted = !quoted;
        }
        if quoted
            && character == '\\'
            && let Some(escaped) = remaining.chars().next()
        {
            readable.push(escaped);
            remaining = &remaining[escaped.len_utf8()..];
        }
    }

    readable
}

fn legacy_path(message: &str) -> Option<(SyncPath, &str)> {
    let encoded = message.strip_prefix(LEGACY_PATH_PREFIX)?;
    let (bytes, suffix) = encoded.split_once(LEGACY_PATH_END)?;
    let bytes: Vec<u8> = serde_json::from_str(bytes).ok()?;
    let path = SyncPath::try_from(bytes).ok()?;
    Some((path, suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_paths_are_readable_and_escaped() {
        for (bytes, expected) in [
            ("café/notes\n.txt".as_bytes(), "\"café/notes\\n.txt\""),
            (b"raw/\xff.txt".as_slice(), "\"raw/\u{fffd}.txt\""),
            (b"quote\"\\name", "\"quote\\\"\\\\name\""),
            (b"SyncPath([97])", "\"SyncPath([97])\""),
        ] {
            let message = format!("Reading SyncPath({bytes:?}) failed");
            let expected = format!("Reading {expected} failed");
            assert_eq!(readable_message(&message), expected);
            assert_eq!(readable_message(&expected), expected);
        }
        assert_eq!(
            readable_message("Moving SyncPath([97]) to SyncPath([98]) failed"),
            "Moving \"a\" to \"b\" failed"
        );
    }

    #[test]
    fn quoted_paths_and_malformed_legacy_paths_are_preserved() {
        for message in [
            "Disk full",
            "Reading \"jan/._LICENSE-linenoise\" failed",
            r#"Reading "quote\"SyncPath([97])" failed"#,
            r#"Reading "backslash\\" and "SyncPath([97])" failed"#,
            "SyncPath([])",
            "SyncPath([256])",
            "SyncPath([-1])",
            "SyncPath([abc])",
            "SyncPath([97,])",
            "SyncPath([97",
            "SyncPath([0])",
            "SyncPath([46, 46])",
            "SyncPath([47, 97])",
        ] {
            assert_eq!(readable_message(message), message);
        }
        assert_eq!(
            readable_message(r#"Reading "SyncPath([97])" and SyncPath([98]) failed"#),
            r#"Reading "SyncPath([97])" and "b" failed"#
        );
    }
}
