//! `foldover-cli` — client layer. The `foldover` CLI (`pairs`, `sync`,
//! `status`). Depends on foldover-client. See PLAN.md §4.4.

use foldover_client::SyncStatus;
use foldover_client::{
    Durability, Filters, MetadataPolicy, PairConfig, PairDraft, PairId, PairOptions, PairRoot,
    Propagation, ReviewToken, Schedule, ScheduleInterval, Side, SocketLocation, SyncMode,
};
use foldover_proto::local::PairRecord;
use std::env;
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

/// `--max-size` counts decimal megabytes, as Finder does.
const BYTES_PER_MEGABYTE: u64 = 1_000_000;
/// How a usage failure exits.
const USAGE_EXIT: u8 = 2;
/// A run stopped for deletion approval; nothing was written.
const APPROVAL_EXIT: u8 = 3;
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const SFTP_WARNING: &str = concat!(
    "SFTP assumes a trusted server; server-side folder changes can redirect access ",
    "outside the selected root. Extended attributes and ACLs are omitted; files ",
    "with resource forks cannot be copied. Power-loss-safe mode is unavailable; ",
    "Standard follows server durability."
);

enum Request {
    /// `--version`: print `foldover <version>` and exit 0.
    Version,
    Status(foldover_client::SocketLocation),
    Pairs(SocketLocation, PairCommand),
    Sync(SocketLocation, PairId, SyncCommand),
    Archive(SocketLocation, PairId, Side, ArchiveCommand),
    History(SocketLocation, PairId),
    /// Pause automatic syncs for minutes, or until resumed.
    Pause(SocketLocation, Option<u32>),
    Resume(SocketLocation),
    Usage,
}

enum ArchiveCommand {
    Runs,
    Items(String),
    PutBack { run: String, path: Vec<u8> },
    PutBackAll(String),
}

enum SyncCommand {
    Preview,
    /// Deletions approved per side beyond the automatic limits; `review`
    /// names the preview being approved.
    Run {
        left: u64,
        right: u64,
        review: Option<ReviewToken>,
    },
    Cancel,
    /// Undo the newest sync that changed anything (ADR 0029).
    Undo,
    /// Keep one side of a conflict from the preview `review` names.
    Keep {
        side: Side,
        path: Vec<u8>,
        review: ReviewToken,
    },
    /// Save B's version of a both-changed conflict beside it; A's wins.
    KeepBoth {
        path: Vec<u8>,
        review: ReviewToken,
    },
}

enum PairCommand {
    List,
    Show(PairId),
    Add(Box<PairDraft>),
    /// Options not named on the command line keep their current values.
    Update {
        id: PairId,
        revision: u64,
        draft: Box<PairDraft>,
        options: OptionChanges,
    },
    Remove {
        id: PairId,
        revision: u64,
    },
}

/// Pair options named on the command line; the rest keep a base value.
#[derive(Clone, Default)]
struct OptionChanges {
    modification_times: Option<Propagation>,
    extended_attributes: Option<Propagation>,
    permissions: Option<Propagation>,
    durability: Option<Durability>,
    schedule: Option<Schedule>,
    filters: Option<Filters>,
    /// `Some(None)` removes the limit.
    size_limit: Option<Option<u64>>,
    /// Days; `Some(None)` removes the limit.
    age_limit: Option<Option<u32>>,
}

impl OptionChanges {
    fn apply(self, base: PairOptions) -> PairOptions {
        let metadata = base.metadata();
        PairOptions::new(
            MetadataPolicy::new(
                self.modification_times
                    .unwrap_or(metadata.modification_times()),
                self.extended_attributes
                    .unwrap_or(metadata.extended_attributes()),
                self.permissions.unwrap_or(metadata.permissions()),
            ),
            self.durability.unwrap_or(base.durability()),
        )
        .with_schedule(self.schedule.unwrap_or(base.schedule()))
        .with_filters(
            self.filters
                .unwrap_or_else(|| base.filters().clone())
                .with_size_limit(self.size_limit.unwrap_or(base.filters().size_limit()))
                .and_then(|filters| {
                    filters.with_age_limit(self.age_limit.unwrap_or(base.filters().age_limit()))
                })
                .expect("limits are checked when parsed"),
        )
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match request(env::args_os().skip(1)) {
        Request::Version => {
            println!("foldover {}", foldover_client::version());
            ExitCode::SUCCESS
        }
        Request::Status(location) => match foldover_client::agent_status(&location).await {
            Ok(status) => {
                println!("Agent {} connected; sync available", status.core_version);
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("foldover: {error}");
                ExitCode::FAILURE
            }
        },
        Request::Pairs(location, command) => match pairs(&location, command).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("foldover: {error}");
                ExitCode::FAILURE
            }
        },
        Request::Sync(location, id, command) => match sync(&location, id, command).await {
            Ok(code) => code,
            Err(error) => {
                eprintln!("foldover: {error}");
                ExitCode::FAILURE
            }
        },
        Request::Archive(location, id, side, command) => {
            match archive(&location, id, side, command).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("foldover: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Request::Pause(location, minutes) => {
            print_status(foldover_client::pause_automation(&location, minutes).await)
        }
        Request::Resume(location) => {
            print_status(foldover_client::resume_automation(&location).await)
        }
        Request::History(location, id) => match history(&location, id).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("foldover: {error}");
                ExitCode::FAILURE
            }
        },
        Request::Usage => {
            eprintln!(
                "usage: foldover --version\n       foldover status [--socket PATH]\n       foldover pairs [--socket PATH]\n       foldover pairs show ID [--socket PATH]\n       foldover pairs add NAME LEFT RIGHT MODE [OPTION VALUE]... [--socket PATH]\n       foldover pairs update ID REVISION NAME LEFT RIGHT MODE [OPTION VALUE]... [--socket PATH]\n       foldover pairs remove ID REVISION [--socket PATH]\n       foldover sync ID [--preview | --cancel | --undo | --approve LEFT RIGHT --review TOKEN | --keep-left PATH --review TOKEN | --keep-right PATH --review TOKEN | --keep-both PATH --review TOKEN] [--socket PATH]\n       foldover archive ID left|right [RUN [--put-back PATH | --put-back-all]] [--socket PATH]\n       foldover history ID [--socket PATH]\n       foldover pause [MINUTES] [--socket PATH]\n       foldover resume [--socket PATH]\nLEFT, RIGHT: an absolute path, or sftp://[user@]host[:port]/path\nMODE: sync | mirror-left | mirror-right | backup-left | backup-right\nOPTION: --times, --attributes, --permissions (sync | ignore); --durability (standard | full); --schedule (manual | changes | MINUTES); --exclude PATTERN (repeatable; '' clears); --max-size (MEGABYTES | none); --max-age (DAYS | none)\nUpdates keep options that are not named."
            );
            ExitCode::from(USAGE_EXIT)
        }
    }
}

fn request(args: impl Iterator<Item = OsString>) -> Request {
    let args: Vec<OsString> = args.collect();
    if matches!(args.as_slice(), [arg] if arg == "--version") {
        return Request::Version;
    }
    let (args, location) = if args.len() >= 2 && args[args.len() - 2] == "--socket" {
        (
            &args[..args.len() - 2],
            SocketLocation::Path(PathBuf::from(&args[args.len() - 1])),
        )
    } else {
        (args.as_slice(), SocketLocation::Default)
    };
    match args {
        [arg] if arg == "status" => Request::Status(location),
        [command, rest @ ..] if command == "pairs" => match pair_command(rest) {
            Some(command) => Request::Pairs(location, command),
            None => Request::Usage,
        },
        [command, id, rest @ ..] if command == "sync" => {
            match (
                id.to_str().and_then(|id| id.parse().ok()),
                sync_command(rest),
            ) {
                (Some(id), Some(command)) => Request::Sync(location, id, command),
                _ => Request::Usage,
            }
        }
        [command] if command == "pause" => Request::Pause(location, None),
        [command, minutes] if command == "pause" => {
            match minutes.to_str().and_then(|minutes| minutes.parse().ok()) {
                Some(minutes) => Request::Pause(location, Some(minutes)),
                None => Request::Usage,
            }
        }
        [command] if command == "resume" => Request::Resume(location),
        [command, id] if command == "history" => match id.to_str().and_then(|id| id.parse().ok()) {
            Some(id) => Request::History(location, id),
            None => Request::Usage,
        },
        [command, id, side, rest @ ..] if command == "archive" => {
            let side = match side.to_str() {
                Some("left") => Some(Side::Left),
                Some("right") => Some(Side::Right),
                _ => None,
            };
            match (
                id.to_str().and_then(|id| id.parse().ok()),
                side,
                archive_command(rest),
            ) {
                (Some(id), Some(side), Some(command)) => {
                    Request::Archive(location, id, side, command)
                }
                _ => Request::Usage,
            }
        }
        _ => Request::Usage,
    }
}

fn archive_command(args: &[OsString]) -> Option<ArchiveCommand> {
    match args {
        [] => Some(ArchiveCommand::Runs),
        [run] => Some(ArchiveCommand::Items(run.to_str()?.to_owned())),
        [run, flag] if flag == "--put-back-all" => {
            Some(ArchiveCommand::PutBackAll(run.to_str()?.to_owned()))
        }
        [run, flag, path] if flag == "--put-back" => Some(ArchiveCommand::PutBack {
            run: run.to_str()?.to_owned(),
            path: path.as_bytes().to_vec(),
        }),
        _ => None,
    }
}

fn pair_command(args: &[OsString]) -> Option<PairCommand> {
    match args {
        [] => Some(PairCommand::List),
        [command, id] if command == "show" => Some(PairCommand::Show(id.to_str()?.parse().ok()?)),
        [command, id, revision] if command == "remove" => Some(PairCommand::Remove {
            id: id.to_str()?.parse().ok()?,
            revision: revision
                .to_str()?
                .parse()
                .ok()
                .filter(|revision| (1..=i64::MAX as u64).contains(revision))?,
        }),
        [command, name, left, right, mode, options @ ..] if command == "add" => {
            let options = option_changes(options)?.apply(PairOptions::default());
            Some(PairCommand::Add(Box::new(
                draft(name, left, right, mode)?.with_options(options),
            )))
        }
        [command, id, revision, name, left, right, mode, options @ ..] if command == "update" => {
            let revision = revision
                .to_str()?
                .parse()
                .ok()
                .filter(|revision| (1..=i64::MAX as u64).contains(revision))?;
            Some(PairCommand::Update {
                id: id.to_str()?.parse().ok()?,
                revision,
                draft: Box::new(draft(name, left, right, mode)?),
                options: option_changes(options)?,
            })
        }
        _ => None,
    }
}

fn sync_command(args: &[OsString]) -> Option<SyncCommand> {
    let count = |value: &OsString| value.to_str()?.parse().ok();
    let review = |args: &[OsString]| match args {
        [flag, token] if flag == "--review" => token.to_str()?.parse().ok(),
        _ => None,
    };
    match args {
        [] => Some(SyncCommand::Run {
            left: 0,
            right: 0,
            review: None,
        }),
        [flag] if flag == "--preview" => Some(SyncCommand::Preview),
        [flag] if flag == "--cancel" => Some(SyncCommand::Cancel),
        [flag] if flag == "--undo" => Some(SyncCommand::Undo),
        [flag, path, rest @ ..] if flag == "--keep-left" || flag == "--keep-right" => {
            Some(SyncCommand::Keep {
                side: if flag == "--keep-left" {
                    Side::Left
                } else {
                    Side::Right
                },
                path: path.as_bytes().to_vec(),
                review: review(rest)?,
            })
        }
        [flag, path, rest @ ..] if flag == "--keep-both" => Some(SyncCommand::KeepBoth {
            path: path.as_bytes().to_vec(),
            review: review(rest)?,
        }),
        // Approval counts are meaningless without the preview they were
        // read from: `--review` is required, never looked up implicitly.
        [flag, left, right, rest @ ..] if flag == "--approve" => Some(SyncCommand::Run {
            left: count(left)?,
            right: count(right)?,
            review: Some(review(rest)?),
        }),
        _ => None,
    }
}

fn option_changes(args: &[OsString]) -> Option<OptionChanges> {
    let mut changes = OptionChanges::default();
    // Each `--exclude` adds a rule; together they replace the pair's rules.
    let mut excludes: Option<Vec<String>> = None;
    for pair in args.chunks(2) {
        let [option, value] = pair else {
            return None;
        };
        let value = value.to_str()?;
        if option == "--exclude" {
            excludes.get_or_insert_with(Vec::new).push(value.to_owned());
            continue;
        }
        let propagation = || match value {
            "sync" => Some(Propagation::Sync),
            "ignore" => Some(Propagation::Ignore),
            _ => None,
        };
        // Each option may appear once.
        let replaced = match option.to_str()? {
            "--times" => changes.modification_times.replace(propagation()?).is_some(),
            "--attributes" => changes
                .extended_attributes
                .replace(propagation()?)
                .is_some(),
            "--permissions" => changes.permissions.replace(propagation()?).is_some(),
            "--durability" => changes
                .durability
                .replace(match value {
                    "standard" => Durability::Standard,
                    "full" => Durability::Full,
                    _ => return None,
                })
                .is_some(),
            "--max-size" => changes
                .size_limit
                .replace(match value {
                    "none" => None,
                    megabytes => Some(
                        megabytes
                            .parse::<u64>()
                            .ok()
                            .filter(|megabytes| *megabytes > 0)?
                            .checked_mul(BYTES_PER_MEGABYTE)?,
                    ),
                })
                .is_some(),
            "--max-age" => changes
                .age_limit
                .replace(match value {
                    "none" => None,
                    days => Some(
                        days.parse::<u32>()
                            .ok()
                            .filter(|days| (1..=Filters::MAX_AGE_DAYS).contains(days))?,
                    ),
                })
                .is_some(),
            "--schedule" => changes
                .schedule
                .replace(match value {
                    "manual" => Schedule::Manual,
                    "changes" => Schedule::OnChange,
                    minutes => {
                        Schedule::Every(ScheduleInterval::from_minutes(minutes.parse().ok()?).ok()?)
                    }
                })
                .is_some(),
            _ => return None,
        };
        if replaced {
            return None;
        }
    }
    if let Some(rules) = excludes {
        // A single empty rule clears the list.
        let rules = rules.into_iter().filter(|rule| !rule.is_empty()).collect();
        changes.filters = Some(Filters::new(rules).ok()?);
    }
    Some(changes)
}

fn draft(name: &OsString, left: &OsString, right: &OsString, mode: &OsString) -> Option<PairDraft> {
    let mode = match mode.to_str()? {
        "sync" => SyncMode::Sync,
        "mirror-left" => SyncMode::Mirror { source: Side::Left },
        "mirror-right" => SyncMode::Mirror {
            source: Side::Right,
        },
        "backup-left" => SyncMode::Backup { source: Side::Left },
        "backup-right" => SyncMode::Backup {
            source: Side::Right,
        },
        _ => return None,
    };
    PairDraft::new(
        name.to_str()?.to_owned(),
        PairRoot::from_bytes(left.as_bytes().to_vec()).ok()?,
        PairRoot::from_bytes(right.as_bytes().to_vec()).ok()?,
        mode,
    )
    .ok()
}

async fn pairs(
    location: &SocketLocation,
    command: PairCommand,
) -> Result<(), foldover_client::Error> {
    match command {
        PairCommand::List => {
            let mut after = None;
            loop {
                let page = foldover_client::pairs(location, after).await?;
                for pair in page.pairs {
                    print_pair(&pair)?;
                }
                let Some(next) = page.next else {
                    return Ok(());
                };
                after = Some(next);
            }
        }
        PairCommand::Show(id) => print_pair(&foldover_client::pair(location, id).await?),
        PairCommand::Add(draft) => {
            warn_sftp([draft.left(), draft.right()]);
            print_pair(&foldover_client::create_pair(location, &draft).await?)
        }
        PairCommand::Update {
            id,
            revision,
            draft,
            options,
        } => {
            // A stale read is harmless: the revision check rejects the update.
            let current = foldover_client::pair(location, id).await?;
            let draft = draft.with_options(options.apply(current.draft().options().clone()));
            warn_sftp([draft.left(), draft.right()]);
            print_pair(&foldover_client::update_pair(location, id, revision, &draft).await?)
        }
        PairCommand::Remove { id, revision } => {
            print_pair(&foldover_client::remove_pair(location, id, revision).await?)
        }
    }
}

/// Start the job, wait for it and print its final status as one JSON line.
async fn sync(
    location: &SocketLocation,
    id: PairId,
    command: SyncCommand,
) -> Result<ExitCode, foldover_client::Error> {
    // Cancellation must keep bypassing the configuration queue, even when
    // another request is waiting on a disconnected server.
    if !matches!(&command, SyncCommand::Cancel) {
        let pair = foldover_client::pair(location, id).await?;
        warn_sftp([pair.draft().left(), pair.draft().right()]);
    }
    let mut status = match command {
        SyncCommand::Preview => foldover_client::start_preview(location, id).await?,
        SyncCommand::Run {
            left,
            right,
            review,
        } => foldover_client::start_run(location, id, left, right, review).await?,
        SyncCommand::Cancel => foldover_client::cancel_sync(location, id).await?,
        SyncCommand::Undo => foldover_client::undo_sync(location, id).await?,
        SyncCommand::Keep { side, path, review } => {
            foldover_client::resolve_conflict(location, id, &path, side, review).await?
        }
        SyncCommand::KeepBoth { path, review } => {
            foldover_client::keep_both(location, id, &path, review).await?
        }
    };
    while matches!(status, SyncStatus::Working { .. }) {
        tokio::time::sleep(POLL_INTERVAL).await;
        status = foldover_client::sync_status(location, id).await?;
    }
    println!("{}", serde_json::to_string(&status)?);
    Ok(match &status {
        SyncStatus::Idle | SyncStatus::Previewed { .. } => ExitCode::SUCCESS,
        SyncStatus::Finished { result } if result.failure_count == 0 && !result.cancelled => {
            ExitCode::SUCCESS
        }
        // Entries left as they were are listed and fail the command.
        SyncStatus::Undone { result } if result.not_undone_count == 0 && !result.cancelled => {
            ExitCode::SUCCESS
        }
        SyncStatus::NeedsApproval { preview } => {
            eprintln!(
                "foldover: review these changes; rerun with --approve {} {} --review {}",
                preview.left.deletions, preview.right.deletions, preview.review_token
            );
            ExitCode::from(APPROVAL_EXIT)
        }
        _ => ExitCode::FAILURE,
    })
}

/// Print an automation status as one JSON line.
fn print_status(
    status: Result<foldover_client::AutomationStatus, foldover_client::Error>,
) -> ExitCode {
    let printed = status.and_then(|status| Ok(serde_json::to_string(&status)?));
    match printed {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("foldover: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Print the pair's finished runs as JSON lines, newest first.
async fn history(location: &SocketLocation, id: PairId) -> Result<(), foldover_client::Error> {
    for run in foldover_client::history(location, id).await?.runs {
        println!("{}", serde_json::to_string(&run)?);
    }
    Ok(())
}

/// Print runs or every item as JSON lines, or put an item back and print
/// where it went.
async fn archive(
    location: &SocketLocation,
    id: PairId,
    side: Side,
    command: ArchiveCommand,
) -> Result<(), foldover_client::Error> {
    if matches!(
        &command,
        ArchiveCommand::PutBack { .. } | ArchiveCommand::PutBackAll(_)
    ) {
        let pair = foldover_client::pair(location, id).await?;
        let root = match side {
            Side::Left => pair.draft().left(),
            Side::Right => pair.draft().right(),
        };
        warn_sftp([root]);
    }
    match command {
        ArchiveCommand::Runs => {
            let runs = foldover_client::archive_runs(location, id, side).await?;
            for run in &runs.runs {
                println!("{}", serde_json::to_string(run)?);
            }
            if runs.more {
                eprintln!("foldover: older runs are not listed");
            }
        }
        ArchiveCommand::Items(run) => {
            let mut after = None;
            loop {
                let page =
                    foldover_client::archive_items(location, id, side, &run, after.as_deref())
                        .await?;
                for item in &page.items {
                    println!("{}", serde_json::to_string(item)?);
                }
                after = page.items.last().map(|item| item.path.clone());
                if !page.more || after.is_none() {
                    break;
                }
            }
        }
        ArchiveCommand::PutBack { run, path } => {
            let placed = foldover_client::put_back(location, id, side, &run, &path).await?;
            println!("{}", serde_json::json!({ "path": placed }));
        }
        ArchiveCommand::PutBackAll(run) => {
            let restored = foldover_client::put_back_all(location, id, side, &run).await?;
            println!("{}", serde_json::to_string(&restored)?);
        }
    }
    Ok(())
}

fn print_pair(pair: &PairConfig) -> Result<(), foldover_client::Error> {
    // JSON lines preserve raw path bytes and escape terminal control characters.
    println!("{}", serde_json::to_string(&PairRecord::from(pair))?);
    Ok(())
}

fn sftp_warning<'a>(roots: impl IntoIterator<Item = &'a PairRoot>) -> Option<&'static str> {
    roots
        .into_iter()
        .any(|root| matches!(root, PairRoot::Sftp(_)))
        .then_some(SFTP_WARNING)
}

fn warn_sftp<'a>(roots: impl IntoIterator<Item = &'a PairRoot>) {
    if let Some(warning) = sftp_warning(roots) {
        eprintln!("foldover: warning: {warning}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(args: &[&str]) -> Request {
        request(args.iter().map(|argument| OsString::from(*argument)))
    }

    #[test]
    fn version_flag_selects_version() {
        assert!(matches!(req(&["--version"]), Request::Version));
    }

    #[test]
    fn remote_warnings_apply_only_to_selected_sftp_roots() {
        let root = |text: &str| PairRoot::from_bytes(text.as_bytes().to_vec()).unwrap();
        let local_a = root("/left");
        let local_b = root("/right");
        let remote = root("sftp://nas/photos");
        assert_eq!(sftp_warning([&local_a, &local_b]), None);
        assert_eq!(sftp_warning([&remote, &local_b]), Some(SFTP_WARNING));
        assert_eq!(sftp_warning([&local_a, &remote]), Some(SFTP_WARNING));
        assert_eq!(sftp_warning([&remote]), Some(SFTP_WARNING));
        assert_eq!(sftp_warning([&local_a]), None);
    }

    #[test]
    fn anything_else_is_usage() {
        for args in [
            &[][..],
            &["--help"],
            &["status", "extra"],
            &["--version", "extra"],
            &["-V"],
        ] {
            assert!(matches!(req(args), Request::Usage), "{args:?}");
        }
    }

    #[test]
    fn status_uses_the_default_or_explicit_socket() {
        assert!(matches!(req(&["status"]), Request::Status(_)));
        assert!(matches!(
            req(&["status", "--socket", "/tmp/test/s"]),
            Request::Status(_)
        ));
    }

    #[test]
    fn pair_commands_validate_ids_revisions_roots_and_modes() {
        assert!(matches!(
            req(&["pairs"]),
            Request::Pairs(_, PairCommand::List)
        ));
        assert!(matches!(
            req(&[
                "pairs",
                "add",
                "Photos",
                "/a",
                "/b",
                "backup-left",
                "--socket",
                "/tmp/s"
            ]),
            Request::Pairs(_, PairCommand::Add(_))
        ));
        let id = "01010101010101010101010101010101";
        assert!(matches!(
            req(&["pairs", "update", id, "1", "Photos", "/a", "/b", "sync"]),
            Request::Pairs(_, PairCommand::Update { .. })
        ));
        for args in [
            vec!["pairs", "show", "bad"],
            vec!["pairs", "add", "Photos", "relative", "/b", "sync"],
            vec!["pairs", "update", id, "0", "Photos", "/a", "/b", "sync"],
            vec!["pairs", "add", "Photos", "/a", "/b", "unknown"],
            vec!["pairs", "add", "Photos", "/a", "/b", "sync", "--times"],
            vec![
                "pairs", "add", "Photos", "/a", "/b", "sync", "--times", "maybe",
            ],
            vec![
                "pairs", "add", "Photos", "/a", "/b", "sync", "--owner", "sync",
            ],
            vec![
                "pairs",
                "add",
                "Photos",
                "/a",
                "/b",
                "sync",
                "--schedule",
                "0",
            ],
            vec![
                "pairs",
                "add",
                "Photos",
                "/a",
                "/b",
                "sync",
                "--schedule",
                "often",
            ],
            vec![
                "pairs", "add", "Photos", "/a", "/b", "sync", "--times", "sync", "--times",
                "ignore",
            ],
        ] {
            assert!(matches!(req(&args), Request::Usage), "{args:?}");
        }
    }

    #[test]
    fn sync_commands_select_preview_run_cancel_or_approval() {
        let id = "01010101010101010101010101010101";
        assert!(matches!(
            req(&["sync", id]),
            Request::Sync(
                _,
                _,
                SyncCommand::Run {
                    left: 0,
                    right: 0,
                    review: None
                }
            )
        ));
        assert!(matches!(
            req(&["sync", id, "--preview", "--socket", "/tmp/s"]),
            Request::Sync(_, _, SyncCommand::Preview)
        ));
        assert!(matches!(
            req(&["sync", id, "--cancel"]),
            Request::Sync(_, _, SyncCommand::Cancel)
        ));
        assert!(matches!(
            req(&["sync", id, "--undo"]),
            Request::Sync(_, _, SyncCommand::Undo)
        ));
        let token = "ab".repeat(32);
        assert!(matches!(
            req(&["sync", id, "--keep-right", "docs/a.txt", "--review", &token]),
            Request::Sync(_, _, SyncCommand::Keep { side: Side::Right, ref path, .. }) if path == b"docs/a.txt"
        ));
        assert!(matches!(
            req(&["sync", id, "--keep-both", "a.txt", "--review", &token]),
            Request::Sync(_, _, SyncCommand::KeepBoth { ref path, .. }) if path == b"a.txt"
        ));
        assert!(matches!(
            req(&["sync", id, "--approve", "60", "0", "--review", &token]),
            Request::Sync(
                _,
                _,
                SyncCommand::Run {
                    left: 60,
                    right: 0,
                    review: Some(_)
                }
            )
        ));
        // An approval or conflict choice without its preview's token is a
        // usage error: the CLI never looks a token up itself.
        for args in [
            vec!["sync"],
            vec!["sync", "bad"],
            vec!["sync", id, "--approve", "60"],
            vec!["sync", id, "--approve", "60", "0"],
            vec!["sync", id, "--approve", "60", "0", "--review", "zz"],
            vec!["sync", id, "--approve", "-1", "0", "--review", &token],
            vec!["sync", id, "--keep-both", "a.txt"],
            vec!["sync", id, "--keep-left", "a.txt", "--review"],
            vec!["sync", id, "--preview", "--cancel"],
        ] {
            assert!(matches!(req(&args), Request::Usage), "{args:?}");
        }
    }

    #[test]
    fn archive_commands_select_runs_items_or_put_back() {
        let id = "01010101010101010101010101010101";
        assert!(matches!(
            req(&["archive", id, "left"]),
            Request::Archive(_, _, Side::Left, ArchiveCommand::Runs)
        ));
        assert!(matches!(
            req(&["archive", id, "right", "2026-10-18T09-30-00.000Z"]),
            Request::Archive(_, _, Side::Right, ArchiveCommand::Items(ref run)) if run == "2026-10-18T09-30-00.000Z"
        ));
        assert!(matches!(
            req(&["archive", id, "left", "RUN", "--put-back", "docs/a"]),
            Request::Archive(_, _, Side::Left, ArchiveCommand::PutBack { ref path, .. }) if path == b"docs/a"
        ));
        assert!(matches!(
            req(&["archive", id, "right", "RUN", "--put-back-all"]),
            Request::Archive(_, _, Side::Right, ArchiveCommand::PutBackAll(ref run)) if run == "RUN"
        ));
        assert!(matches!(req(&["history", id]), Request::History(..)));
        assert!(matches!(req(&["pause"]), Request::Pause(_, None)));
        assert!(matches!(req(&["pause", "60"]), Request::Pause(_, Some(60))));
        assert!(matches!(req(&["pause", "soon"]), Request::Usage));
        assert!(matches!(req(&["resume"]), Request::Resume(_)));
        for args in [
            vec!["history"],
            vec!["history", "bad"],
            vec!["archive", id],
            vec!["archive", id, "a"],
            vec!["archive", "bad", "left"],
            vec!["archive", id, "left", "RUN", "--put-back"],
        ] {
            assert!(matches!(req(&args), Request::Usage), "{args:?}");
        }
    }

    #[test]
    fn named_options_change_only_themselves() {
        let Request::Pairs(_, PairCommand::Add(draft)) = req(&[
            "pairs",
            "add",
            "Photos",
            "/a",
            "/b",
            "sync",
            "--permissions",
            "sync",
            "--durability",
            "full",
            "--schedule",
            "15",
            "--exclude",
            "*.tmp",
            "--exclude",
            "build/",
            "--max-size",
            "500",
            "--max-age",
            "90",
            "--socket",
            "/tmp/s",
        ]) else {
            panic!("expected an add command");
        };
        let defaults = PairOptions::default();
        assert_eq!(draft.options().durability(), Durability::Full);
        assert_eq!(draft.options().filters().rules(), ["*.tmp", "build/"]);
        assert_eq!(draft.options().filters().size_limit(), Some(500_000_000));
        assert_eq!(draft.options().filters().age_limit(), Some(90));
        assert_eq!(
            draft.options().schedule(),
            Schedule::Every(ScheduleInterval::from_minutes(15).unwrap())
        );
        assert_eq!(draft.options().metadata().permissions(), Propagation::Sync);
        assert_eq!(
            draft.options().metadata().modification_times(),
            defaults.metadata().modification_times()
        );

        let id = "01010101010101010101010101010101";
        let Request::Pairs(_, PairCommand::Update { options, .. }) = req(&[
            "pairs", "update", id, "1", "Photos", "/a", "/b", "sync", "--times", "ignore",
        ]) else {
            panic!("expected an update command");
        };
        let limited = Filters::default().with_size_limit(Some(7)).unwrap();
        let current = PairOptions::new(
            MetadataPolicy::new(Propagation::Sync, Propagation::Ignore, Propagation::Sync),
            Durability::Full,
        )
        .with_filters(limited.clone());
        assert_eq!(
            options.apply(current.clone()),
            PairOptions::new(
                MetadataPolicy::new(Propagation::Ignore, Propagation::Ignore, Propagation::Sync),
                Durability::Full,
            )
            .with_filters(limited)
        );

        let Request::Pairs(_, PairCommand::Update { options, .. }) = req(&[
            "pairs",
            "update",
            id,
            "1",
            "Photos",
            "/a",
            "/b",
            "sync",
            "--max-size",
            "none",
        ]) else {
            panic!("expected an update command");
        };
        assert_eq!(options.apply(current).filters().size_limit(), None);
        for value in ["0", "-1", "lots"] {
            assert!(matches!(
                req(&["pairs", "add", "P", "/a", "/b", "sync", "--max-size", value]),
                Request::Usage
            ));
        }
    }

    #[test]
    fn version_line_names_the_binary() {
        assert_eq!(
            format!("foldover {}", foldover_client::version()),
            format!("foldover {}", env!("CARGO_PKG_VERSION"))
        );
    }
}
