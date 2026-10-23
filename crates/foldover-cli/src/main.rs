//! `foldover-cli` — client layer. The `foldover` CLI (`pairs`, `sync`,
//! `status`). Depends on foldover-client. See PLAN.md §4.4.

use foldover_client::{
    DailySchedule, Durability, Filters, FolderRecords, MetadataPolicy, PairConfig, PairDraft,
    PairId, PairOptions, PairRoot, Propagation, ReviewToken, Schedule, ScheduleInterval, Side,
    SocketLocation, SyncMode, SyncPath, TimeOfDay, Weekday, Weekdays,
};
use foldover_client::{DeviceId, PeerStatus, ShareId, SyncStatus};
use foldover_proto::local::PairRecord;
use std::env;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

/// `--max-size` and `--bandwidth-limit` count decimal megabytes, as
/// Finder does.
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
    /// `sync --all`: run every pair, reporting each one's result.
    SyncAll(SocketLocation),
    Archive(SocketLocation, PairId, Side, ArchiveCommand),
    History(SocketLocation, PairId),
    /// Pause automatic syncs for minutes, or until resumed.
    Pause(SocketLocation, Option<u32>),
    Resume(SocketLocation),
    /// `safe-mode off`: sync again after a crash loop.
    LeaveSafeMode(SocketLocation),
    /// Automation status, or a choice from the environment policy.
    Automation(SocketLocation, AutomationCommand),
    Peers(SocketLocation, PeerCommand),
    Shares(SocketLocation, ShareCommand),
    /// SMB file servers seen on the local network (PLAN.md §3.8).
    Servers(SocketLocation),
    /// The folders in a folder on an SFTP server (PLAN.md §4.3).
    Browse(SocketLocation, Vec<u8>),
    Usage,
}

/// Folders this Mac shares with paired Macs.
enum ShareCommand {
    List,
    Add {
        device: DeviceId,
        root: Vec<u8>,
        name: String,
    },
    Remove(ShareId),
}

/// Pairing with other Macs (ADR 0031).
enum PeerCommand {
    Status,
    /// Listen, and look for Macs on the network.
    Start,
    /// Let unpaired Macs reach this one for minutes (10 when absent).
    Open(Option<u32>),
    Close,
    /// At an address, or where the local network says.
    Pair {
        device: DeviceId,
        address: Option<SocketAddr>,
    },
    Confirm(DeviceId),
    Cancel(DeviceId),
    Revoke(DeviceId),
    /// The folders a paired Mac shares with this one.
    Shares(DeviceId),
}

/// The environment policy (ADR 0036).
enum AutomationCommand {
    Status,
    /// Automatic syncs in Low Power Mode: `pause` (default) or `run`.
    LowPower(foldover_client::LowPowerSyncs),
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
    /// Leave a previewed change alone in the next run.
    Skip {
        path: Vec<u8>,
        review: ReviewToken,
    },
}

enum PairCommand {
    List,
    Show(PairId),
    Add(Box<PairDraft>),
    /// Readiness warnings for a pair that has not been saved (PLAN.md
    /// §3.6); same arguments as `add`.
    Check(Box<PairDraft>),
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
    /// `Some(None)` removes the canary.
    canary: Option<Option<SyncPath>>,
    records: Option<FolderRecords>,
    /// `Some(None)` removes the limit.
    bandwidth_limit: Option<Option<u64>>,
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
        .with_canary(self.canary.unwrap_or_else(|| base.canary().cloned()))
        .expect("the canary is checked when parsed")
        .with_records(self.records.unwrap_or(base.records()))
        .with_bandwidth_limit(self.bandwidth_limit.unwrap_or(base.bandwidth_limit()))
        .expect("the bandwidth limit is checked when parsed")
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
                println!("{}", status_line(&status));
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("foldover: {error}");
                ExitCode::FAILURE
            }
        },
        Request::LeaveSafeMode(location) => {
            match foldover_client::leave_safe_mode(&location).await {
                Ok(status) => {
                    println!("{}", status_line(&status));
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("foldover: {error}");
                    ExitCode::FAILURE
                }
            }
        }
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
        Request::SyncAll(location) => match sync_all(&location).await {
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
        Request::Automation(location, command) => match command {
            AutomationCommand::Status => {
                print_status(foldover_client::automation_status(&location).await)
            }
            AutomationCommand::LowPower(syncs) => {
                print_status(foldover_client::set_low_power_syncs(&location, syncs).await)
            }
        },
        Request::History(location, id) => match history(&location, id).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("foldover: {error}");
                ExitCode::FAILURE
            }
        },
        Request::Peers(location, command) => match peers(&location, command).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("foldover: {error}");
                ExitCode::FAILURE
            }
        },
        Request::Shares(location, command) => match shares(&location, command).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("foldover: {error}");
                ExitCode::FAILURE
            }
        },
        Request::Servers(location) => match servers(&location).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("foldover: {error}");
                ExitCode::FAILURE
            }
        },
        Request::Browse(location, root) => match browse(&location, &root).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("foldover: {error}");
                ExitCode::FAILURE
            }
        },
        Request::Usage => {
            eprintln!(
                "usage: foldover --version\n       foldover status [--socket PATH]\n       foldover pairs [--socket PATH]\n       foldover pairs show ID [--socket PATH]\n       foldover pairs add NAME LEFT RIGHT MODE [OPTION VALUE]... [--socket PATH]\n       foldover pairs check NAME LEFT RIGHT MODE [OPTION VALUE]... [--socket PATH]\n       foldover pairs update ID REVISION NAME LEFT RIGHT MODE [OPTION VALUE]... [--socket PATH]\n       foldover pairs remove ID REVISION [--socket PATH]\n       foldover sync ID [--preview | --cancel | --undo | --approve LEFT RIGHT --review TOKEN | --keep-left PATH --review TOKEN | --keep-right PATH --review TOKEN | --keep-both PATH --review TOKEN | --skip PATH --review TOKEN] [--socket PATH]\n       foldover sync --all [--socket PATH]\n       foldover archive ID left|right [RUN [--put-back PATH | --put-back-all]] [--socket PATH]\n       foldover history ID [--socket PATH]\n       foldover pause [MINUTES] [--socket PATH]\n       foldover resume [--socket PATH]\n       foldover safe-mode off [--socket PATH]\n       foldover automation [low-power pause|run] [--socket PATH]\n       foldover peers [start | open [MINUTES] | close | pair DEVICE [IP:PORT] | confirm DEVICE | cancel DEVICE | revoke DEVICE | shares DEVICE] [--socket PATH]\n       foldover shares [add DEVICE PATH NAME | remove ID] [--socket PATH]\n       foldover servers [browse sftp://[user@]host[:port]/path] [--socket PATH]\nLEFT, RIGHT: an absolute path, sftp://[user@]host[:port]/path, or peer://DEVICE/SHARE (see foldover peers shares)\nMODE: sync | mirror-left | mirror-right | backup-left | backup-right\nOPTION: --times, --attributes, --permissions (sync | ignore); --durability (standard | full); --records (both | source: a backup-* target that cannot hold .foldover keeps nothing, archives nothing); --schedule (manual | changes | MINUTES | daily@HH:MM[:DAYS]); --exclude PATTERN (repeatable; '' clears); --max-size (MEGABYTES | none); --max-age (DAYS | none); --bandwidth-limit (MEGABYTES per second | none); --canary (PATH | none)\nUpdates keep options that are not named."
            );
            ExitCode::from(USAGE_EXIT)
        }
    }
}

/// One line saying whether the agent syncs; in crash-loop safe mode, why
/// not and how to sync again.
fn status_line(status: &foldover_client::DaemonStatus) -> String {
    match status.safe_mode {
        None => format!("Agent {} connected; sync available", status.core_version),
        Some(safe_mode) => format!(
            "Agent {} connected; syncing paused after {} unexpected stops in a few minutes \
             (foldover safe-mode off to sync again)",
            status.core_version, safe_mode.unclean_stops
        ),
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
        [command, flag] if command == "sync" && flag == "--all" => Request::SyncAll(location),
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
        [command, state] if command == "safe-mode" && state == "off" => {
            Request::LeaveSafeMode(location)
        }
        [command] if command == "automation" => {
            Request::Automation(location, AutomationCommand::Status)
        }
        [command, setting, value] if command == "automation" && setting == "low-power" => {
            match value.to_str() {
                Some("pause") => Request::Automation(
                    location,
                    AutomationCommand::LowPower(foldover_client::LowPowerSyncs::Pause),
                ),
                Some("run") => Request::Automation(
                    location,
                    AutomationCommand::LowPower(foldover_client::LowPowerSyncs::Run),
                ),
                _ => Request::Usage,
            }
        }
        [command, ..] if command == "automation" => Request::Usage,
        [command, rest @ ..] if command == "peers" => match peer_command(rest) {
            Some(command) => Request::Peers(location, command),
            None => Request::Usage,
        },
        [command, rest @ ..] if command == "shares" => match share_command(rest) {
            Some(command) => Request::Shares(location, command),
            None => Request::Usage,
        },
        [command] if command == "servers" => Request::Servers(location),
        [command, browse, root] if command == "servers" && browse == "browse" => {
            Request::Browse(location, root.as_bytes().to_vec())
        }
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
        [command, name, left, right, mode, options @ ..]
            if command == "add" || command == "check" =>
        {
            let options = option_changes(options)?.apply(PairOptions::default());
            let draft = Box::new(draft(name, left, right, mode)?.with_options(options));
            Some(if command == "add" {
                PairCommand::Add(draft)
            } else {
                PairCommand::Check(draft)
            })
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
        [flag, path, rest @ ..] if flag == "--skip" => Some(SyncCommand::Skip {
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
            "--records" => changes
                .records
                .replace(match value {
                    "both" => FolderRecords::Both,
                    "source" => FolderRecords::SourceOnly,
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
            "--bandwidth-limit" => changes
                .bandwidth_limit
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
            "--canary" => changes
                .canary
                .replace(match value {
                    "none" => None,
                    path => {
                        let path = SyncPath::try_from(path.as_bytes()).ok()?;
                        PairOptions::default()
                            .with_canary(Some(path.clone()))
                            .ok()?;
                        Some(path)
                    }
                })
                .is_some(),
            "--schedule" => changes
                .schedule
                .replace(match value {
                    "manual" => Schedule::Manual,
                    "changes" => Schedule::OnChange,
                    daily if daily.starts_with("daily@") => daily_schedule(&daily[6..])?,
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

/// `--schedule daily@HH:MM[:DAYS]`: a local time of day on the given
/// weekdays — a comma list or `mon-fri` range, every day when omitted.
fn daily_schedule(spec: &str) -> Option<Schedule> {
    let mut fields = spec.split(':');
    let hour: u32 = fields.next()?.parse().ok()?;
    let minute: u32 = fields.next()?.parse().ok()?;
    if hour >= 24 || minute >= 60 {
        return None;
    }
    let at = TimeOfDay::from_minutes(hour * 60 + minute).ok()?;
    let days = match fields.next() {
        Some(list) => day_list(list)?,
        None => Weekdays::ALL,
    };
    if fields.next().is_some() {
        return None;
    }
    Some(Schedule::Daily(DailySchedule::new(at, days)))
}

/// A comma-separated list of day names or `mon-fri` ranges.
fn day_list(list: &str) -> Option<Weekdays> {
    let mut days = Vec::new();
    for item in list.split(',') {
        match item.split_once('-') {
            Some((from, to)) => {
                let (from, to) = (weekday(from)?, weekday(to)?);
                let mut day = from;
                loop {
                    days.push(day);
                    if day == to {
                        break;
                    }
                    day = Weekday::ALL[(day as usize + 1) % 7];
                }
            }
            None => days.push(weekday(item)?),
        }
    }
    Weekdays::of(&days).ok()
}

fn weekday(name: &str) -> Option<Weekday> {
    Some(match name {
        "mon" | "monday" => Weekday::Monday,
        "tue" | "tuesday" => Weekday::Tuesday,
        "wed" | "wednesday" => Weekday::Wednesday,
        "thu" | "thursday" => Weekday::Thursday,
        "fri" | "friday" => Weekday::Friday,
        "sat" | "saturday" => Weekday::Saturday,
        "sun" | "sunday" => Weekday::Sunday,
        _ => return None,
    })
}

fn peer_command(args: &[OsString]) -> Option<PeerCommand> {
    let device = |device: &OsString| device.to_str()?.parse().ok();
    let words: Vec<&str> = args.iter().map(|arg| arg.to_str()).collect::<Option<_>>()?;
    match words.as_slice() {
        [] => Some(PeerCommand::Status),
        ["start"] => Some(PeerCommand::Start),
        ["open"] => Some(PeerCommand::Open(None)),
        ["open", minutes] => Some(PeerCommand::Open(Some(minutes.parse().ok()?))),
        ["close"] => Some(PeerCommand::Close),
        ["pair", _] => Some(PeerCommand::Pair {
            device: device(&args[1])?,
            address: None,
        }),
        ["pair", _, address] => Some(PeerCommand::Pair {
            device: device(&args[1])?,
            address: Some(address.parse().ok()?),
        }),
        ["confirm", _] => Some(PeerCommand::Confirm(device(&args[1])?)),
        ["cancel", _] => Some(PeerCommand::Cancel(device(&args[1])?)),
        ["revoke", _] => Some(PeerCommand::Revoke(device(&args[1])?)),
        ["shares", _] => Some(PeerCommand::Shares(device(&args[1])?)),
        _ => None,
    }
}

fn share_command(args: &[OsString]) -> Option<ShareCommand> {
    match args {
        [] => Some(ShareCommand::List),
        [command, device, root, name] if command == "add" => Some(ShareCommand::Add {
            device: device.to_str()?.parse().ok()?,
            root: root.as_bytes().to_vec(),
            name: name.to_str()?.to_owned(),
        }),
        [command, id] if command == "remove" => {
            Some(ShareCommand::Remove(id.to_str()?.parse().ok()?))
        }
        _ => None,
    }
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
        PairCommand::Check(draft) => {
            warn_sftp([draft.left(), draft.right()]);
            // JSON lines keep the machine-readable kinds and raw sides.
            for warning in foldover_client::check_pair(location, &draft).await? {
                println!("{}", serde_json::to_string(&warning)?);
            }
            Ok(())
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
        SyncCommand::Skip { path, review } => {
            foldover_client::skip_once(location, id, &path, review).await?
        }
    };
    while matches!(status, SyncStatus::Working { .. }) {
        tokio::time::sleep(POLL_INTERVAL).await;
        status = foldover_client::sync_status(location, id).await?;
    }
    println!("{}", serde_json::to_string(&status)?);
    if let SyncStatus::NeedsApproval { preview } = &status {
        eprintln!(
            "foldover: review these changes; rerun with --approve {} {} --review {}",
            preview.left.deletions, preview.right.deletions, preview.review_token
        );
    }
    Ok(status_code(&status))
}

/// The exit code for a settled status: success, approval needed, or
/// failure.
fn status_code(status: &SyncStatus) -> ExitCode {
    match status {
        SyncStatus::Idle | SyncStatus::Previewed { .. } => ExitCode::SUCCESS,
        SyncStatus::Finished { result } if result.failure_count == 0 && !result.cancelled => {
            ExitCode::SUCCESS
        }
        // Entries left as they were are listed and fail the command.
        SyncStatus::Undone { result } if result.not_undone_count == 0 && !result.cancelled => {
            ExitCode::SUCCESS
        }
        SyncStatus::NeedsApproval { .. } => ExitCode::from(APPROVAL_EXIT),
        _ => ExitCode::FAILURE,
    }
}

/// Start a plain run on the pair and wait for its final status.
async fn run_one(
    location: &SocketLocation,
    id: PairId,
) -> Result<SyncStatus, foldover_client::Error> {
    let mut status = foldover_client::start_run(location, id, 0, 0, None).await?;
    while matches!(status, SyncStatus::Working { .. }) {
        tokio::time::sleep(POLL_INTERVAL).await;
        status = foldover_client::sync_status(location, id).await?;
    }
    Ok(status)
}

/// Run every pair concurrently, printing each pair's outcome as one JSON
/// line; the exit code is the worst outcome across pairs.
async fn sync_all(location: &SocketLocation) -> Result<ExitCode, foldover_client::Error> {
    let mut pairs = Vec::new();
    let mut after = None;
    loop {
        let page = foldover_client::pairs(location, after).await?;
        pairs.extend(page.pairs);
        let Some(next) = page.next else {
            break;
        };
        after = Some(next);
    }
    warn_sftp(
        pairs
            .iter()
            .flat_map(|pair| [pair.draft().left(), pair.draft().right()]),
    );
    let mut jobs = tokio::task::JoinSet::new();
    for pair in pairs {
        let location = location.clone();
        jobs.spawn(async move {
            let outcome = run_one(&location, pair.id()).await;
            (pair, outcome)
        });
    }
    let mut failed = false;
    let mut approvals = false;
    // Each line stands alone, so pairs are reported as they finish.
    while let Some(joined) = jobs.join_next().await {
        // A task panics only on a bug; keep reporting the rest.
        let Ok((pair, outcome)) = joined else {
            continue;
        };
        let line = match &outcome {
            Ok(status) => {
                match status_code(status) {
                    code if code == ExitCode::SUCCESS => {}
                    code if code == ExitCode::from(APPROVAL_EXIT) => approvals = true,
                    _ => failed = true,
                }
                serde_json::json!({
                    "id": pair.id().to_string(),
                    "name": pair.draft().name(),
                    "status": status,
                })
            }
            Err(error) => {
                failed = true;
                serde_json::json!({
                    "id": pair.id().to_string(),
                    "name": pair.draft().name(),
                    "error": error.to_string(),
                })
            }
        };
        println!("{line}");
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else if approvals {
        ExitCode::from(APPROVAL_EXIT)
    } else {
        ExitCode::SUCCESS
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

async fn peers(
    location: &SocketLocation,
    command: PeerCommand,
) -> Result<(), foldover_client::Error> {
    let status = match command {
        PeerCommand::Status => foldover_client::peer_status(location).await?,
        PeerCommand::Start => foldover_client::start_peers(location).await?,
        PeerCommand::Open(minutes) => foldover_client::open_pairing(location, minutes).await?,
        PeerCommand::Close => foldover_client::close_pairing(location).await?,
        PeerCommand::Pair { device, address } => {
            let pending = foldover_client::pair_device(location, device, address).await?;
            println!("{}", serde_json::to_string(&pending)?);
            eprintln!(
                "foldover: check that {:?} shows code {:06}, then run `foldover peers confirm {device}`",
                pending.name, pending.code
            );
            return Ok(());
        }
        PeerCommand::Confirm(device) => foldover_client::confirm_pairing(location, device).await?,
        PeerCommand::Cancel(device) => foldover_client::cancel_pairing(location, device).await?,
        PeerCommand::Revoke(device) => foldover_client::revoke_device(location, device).await?,
        PeerCommand::Shares(device) => {
            let offered = foldover_client::peer_shares(location, device).await?;
            println!("{}", serde_json::to_string(&offered)?);
            return Ok(());
        }
    };
    print_peers(&status)
}

async fn shares(
    location: &SocketLocation,
    command: ShareCommand,
) -> Result<(), foldover_client::Error> {
    let listed = match command {
        ShareCommand::List => foldover_client::shares(location).await?,
        ShareCommand::Add { device, root, name } => {
            let shared = foldover_client::add_share(location, device, &root, &name).await?;
            println!("{}", serde_json::to_string(&shared)?);
            return Ok(());
        }
        ShareCommand::Remove(id) => foldover_client::remove_share(location, id).await?,
    };
    // JSON lines keep raw root bytes and escape control characters.
    for share in &listed.shares {
        println!("{}", serde_json::to_string(share)?);
    }
    Ok(())
}

/// Print each server seen on the network as one JSON line.
async fn servers(location: &SocketLocation) -> Result<(), foldover_client::Error> {
    for server in foldover_client::network_servers(location).await? {
        println!("{}", serde_json::to_string(&server)?);
    }
    Ok(())
}

async fn browse(location: &SocketLocation, root: &[u8]) -> Result<(), foldover_client::Error> {
    let listed = foldover_client::browse_server(location, root).await?;
    println!("{}", serde_json::to_string(&listed)?);
    Ok(())
}

fn print_peers(status: &PeerStatus) -> Result<(), foldover_client::Error> {
    println!("{}", serde_json::to_string(status)?);
    for pending in &status.pending {
        eprintln!(
            "foldover: {:?} wants to pair with code {:06}; compare it, then run `foldover peers confirm {}` or `foldover peers cancel {}`",
            pending.name, pending.code, pending.device, pending.device
        );
    }
    for nearby in status
        .nearby
        .iter()
        .filter(|nearby| nearby.pairing.is_some())
    {
        eprintln!(
            "foldover: {:?} is open for pairing; run `foldover peers pair {}`",
            nearby.name, nearby.device
        );
    }
    if let Some(problem) = &status.discovery {
        eprintln!("foldover: warning: {problem}");
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
    fn share_commands_name_a_device_folder_and_name() {
        let device = "ab".repeat(32);
        let share = "cd".repeat(16);
        assert!(matches!(
            req(&["shares"]),
            Request::Shares(_, ShareCommand::List)
        ));
        let Request::Shares(_, ShareCommand::Add { root, name, .. }) =
            req(&["shares", "add", &device, "/Users/a/Photos", "Photos"])
        else {
            panic!("not a share");
        };
        assert_eq!(
            (root.as_slice(), name.as_str()),
            (&b"/Users/a/Photos"[..], "Photos")
        );
        assert!(matches!(
            req(&["shares", "remove", &share]),
            Request::Shares(_, ShareCommand::Remove(_))
        ));
        assert!(matches!(
            req(&["peers", "shares", &device]),
            Request::Peers(_, PeerCommand::Shares(_))
        ));
        for args in [
            &["shares", "add", "abc", "/x", "X"][..],
            &["shares", "remove", "abc"],
            &["shares", "add", &device, "/x"],
        ] {
            assert!(matches!(req(args), Request::Usage), "{args:?}");
        }
    }

    #[test]
    fn peer_commands_name_a_device_and_an_ip_address() {
        let device = "ab".repeat(32);
        assert!(matches!(
            req(&["peers"]),
            Request::Peers(_, PeerCommand::Status)
        ));
        assert!(matches!(
            req(&["peers", "start"]),
            Request::Peers(_, PeerCommand::Start)
        ));
        assert!(matches!(
            req(&["peers", "open", "15"]),
            Request::Peers(_, PeerCommand::Open(Some(15)))
        ));
        assert!(matches!(
            req(&["peers", "pair", &device, "192.168.1.5:50123"]),
            Request::Peers(_, PeerCommand::Pair { address: Some(address), .. })
                if address.port() == 50123
        ));
        assert!(matches!(
            req(&["peers", "pair", &device]),
            Request::Peers(_, PeerCommand::Pair { address: None, .. })
        ));
        assert!(matches!(
            req(&["peers", "confirm", &device, "--socket", "/tmp/s"]),
            Request::Peers(SocketLocation::Path(_), PeerCommand::Confirm(_))
        ));
        for args in [
            &["peers", "open", "soon"][..],
            &["peers", "pair", &device, "studio.local"],
            &["peers", "confirm", "abc"],
            &["peers", "revoke"],
        ] {
            assert!(matches!(req(args), Request::Usage), "{args:?}");
        }
    }

    #[test]
    fn servers_browses_the_network() {
        assert!(matches!(req(&["servers"]), Request::Servers(_)));
        assert!(matches!(
            req(&["servers", "--socket", "/tmp/s"]),
            Request::Servers(SocketLocation::Path(_))
        ));
        assert!(matches!(req(&["servers", "extra"]), Request::Usage));
        let Request::Browse(_, root) = req(&["servers", "browse", "sftp://nas/~"]) else {
            panic!("expected a browse");
        };
        assert_eq!(root, b"sftp://nas/~");
        assert!(matches!(
            req(&["servers", "browse", "sftp://nas/~", "--socket", "/tmp/s"]),
            Request::Browse(SocketLocation::Path(_), _)
        ));
        assert!(matches!(req(&["servers", "browse"]), Request::Usage));
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
        assert!(matches!(
            req(&[
                "pairs", "check", "Photos", "/a", "/b", "sync", "--times", "ignore"
            ]),
            Request::Pairs(_, PairCommand::Check(_))
        ));
        for args in [
            vec!["pairs", "show", "bad"],
            vec!["pairs", "check", "Photos", "relative", "/b", "sync"],
            vec!["pairs", "check", "Photos", "/a", "/b"],
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
                "pairs",
                "add",
                "Photos",
                "/a",
                "/b",
                "sync",
                "--schedule",
                "daily@24:00",
            ],
            vec![
                "pairs",
                "add",
                "Photos",
                "/a",
                "/b",
                "sync",
                "--schedule",
                "daily@18:60",
            ],
            vec![
                "pairs",
                "add",
                "Photos",
                "/a",
                "/b",
                "sync",
                "--schedule",
                "daily@18:00:",
            ],
            vec![
                "pairs",
                "add",
                "Photos",
                "/a",
                "/b",
                "sync",
                "--schedule",
                "daily@18:00:funday",
            ],
            vec![
                "pairs",
                "add",
                "Photos",
                "/a",
                "/b",
                "sync",
                "--schedule",
                "daily@18:00:mon-",
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
    fn sync_all_selects_every_pair() {
        assert!(matches!(req(&["sync", "--all"]), Request::SyncAll(_)));
        assert!(matches!(
            req(&["sync", "--all", "--socket", "/tmp/s"]),
            Request::SyncAll(_)
        ));
        assert!(matches!(req(&["sync", "--all", "extra"]), Request::Usage));
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
            req(&["sync", id, "--skip", "a.txt", "--review", &token]),
            Request::Sync(_, _, SyncCommand::Skip { ref path, .. }) if path == b"a.txt"
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
        assert!(matches!(
            req(&["safe-mode", "off"]),
            Request::LeaveSafeMode(_)
        ));
        for args in [&["safe-mode"][..], &["safe-mode", "on"]] {
            assert!(matches!(req(args), Request::Usage), "{args:?}");
        }
        let mut status = foldover_client::DaemonStatus {
            core_version: "1.0.0".into(),
            capability: foldover_client::AgentCapability::Sync,
            safe_mode: None,
        };
        assert_eq!(
            status_line(&status),
            "Agent 1.0.0 connected; sync available"
        );
        status.safe_mode = Some(foldover_client::SafeMode { unclean_stops: 3 });
        assert!(status_line(&status).contains("paused after 3 unexpected stops"));
        assert!(matches!(
            req(&["automation"]),
            Request::Automation(_, AutomationCommand::Status)
        ));
        assert!(matches!(
            req(&["automation", "low-power", "pause"]),
            Request::Automation(
                _,
                AutomationCommand::LowPower(foldover_client::LowPowerSyncs::Pause)
            )
        ));
        assert!(matches!(
            req(&["automation", "low-power", "run"]),
            Request::Automation(
                _,
                AutomationCommand::LowPower(foldover_client::LowPowerSyncs::Run)
            )
        ));
        for args in [
            vec!["history"],
            vec!["history", "bad"],
            vec!["automation", "low-power"],
            vec!["automation", "low-power", "sometimes"],
            vec!["automation", "bogus", "pause"],
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
            "--bandwidth-limit",
            "10",
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
        assert_eq!(draft.options().bandwidth_limit(), Some(10_000_000));
        assert_eq!(
            draft.options().schedule(),
            Schedule::Every(ScheduleInterval::from_minutes(15).unwrap())
        );
        for (value, expected) in [
            (
                "daily@18:00",
                DailySchedule::new(TimeOfDay::from_minutes(18 * 60).unwrap(), Weekdays::ALL),
            ),
            (
                "daily@7:30:mon-fri",
                DailySchedule::new(
                    TimeOfDay::from_minutes(7 * 60 + 30).unwrap(),
                    Weekdays::MONDAY_TO_FRIDAY,
                ),
            ),
            (
                "daily@02:00:mon,wed,sun",
                DailySchedule::new(
                    TimeOfDay::from_minutes(2 * 60).unwrap(),
                    Weekdays::of(&[Weekday::Monday, Weekday::Wednesday, Weekday::Sunday]).unwrap(),
                ),
            ),
        ] {
            let Request::Pairs(_, PairCommand::Add(draft)) = req(&[
                "pairs",
                "add",
                "Photos",
                "/a",
                "/b",
                "sync",
                "--schedule",
                value,
            ]) else {
                panic!("expected an add command for {value}");
            };
            assert_eq!(
                draft.options().schedule(),
                Schedule::Daily(expected),
                "{value}"
            );
        }
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

        let capped = PairOptions::default()
            .with_bandwidth_limit(Some(5_000_000))
            .unwrap();
        let Request::Pairs(_, PairCommand::Update { options, .. }) = req(&[
            "pairs",
            "update",
            id,
            "1",
            "Photos",
            "/a",
            "/b",
            "sync",
            "--bandwidth-limit",
            "10",
        ]) else {
            panic!("expected an update command");
        };
        assert_eq!(
            options.apply(capped.clone()).bandwidth_limit(),
            Some(10_000_000)
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
            "--bandwidth-limit",
            "none",
        ]) else {
            panic!("expected an update command");
        };
        assert_eq!(options.apply(capped).bandwidth_limit(), None);
        for value in ["0", "-1", "fast"] {
            assert!(matches!(
                req(&[
                    "pairs",
                    "add",
                    "P",
                    "/a",
                    "/b",
                    "sync",
                    "--bandwidth-limit",
                    value
                ]),
                Request::Usage
            ));
        }

        let Request::Pairs(_, PairCommand::Update { options, .. }) = req(&[
            "pairs",
            "update",
            id,
            "1",
            "Photos",
            "/a",
            "/b",
            "sync",
            "--canary",
            "docs/canary.txt",
        ]) else {
            panic!("expected an update command");
        };
        let current = PairOptions::default()
            .with_canary(SyncPath::try_from(b"old.txt".as_slice()).ok())
            .unwrap()
            .with_records(FolderRecords::SourceOnly);
        let updated = options.apply(current.clone());
        assert_eq!(
            updated.canary(),
            Some(&SyncPath::try_from(b"docs/canary.txt".as_slice()).unwrap())
        );
        assert_eq!(updated.records(), FolderRecords::SourceOnly);

        let Request::Pairs(_, PairCommand::Update { options, .. }) = req(&[
            "pairs",
            "update",
            id,
            "1",
            "Photos",
            "/a",
            "/b",
            "backup-left",
            "--records",
            "both",
        ]) else {
            panic!("expected an update command");
        };
        assert_eq!(
            options.apply(current.clone()).records(),
            FolderRecords::Both
        );
        assert!(matches!(
            req(&[
                "pairs",
                "add",
                "P",
                "/a",
                "/b",
                "backup-left",
                "--records",
                "target"
            ]),
            Request::Usage
        ));

        let Request::Pairs(_, PairCommand::Update { options, .. }) = req(&[
            "pairs", "update", id, "1", "Photos", "/a", "/b", "sync", "--canary", "none",
        ]) else {
            panic!("expected an update command");
        };
        assert_eq!(options.apply(current).canary(), None);
        for value in ["/abs.txt", ".foldover/id", ""] {
            assert!(matches!(
                req(&["pairs", "add", "P", "/a", "/b", "sync", "--canary", value]),
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
