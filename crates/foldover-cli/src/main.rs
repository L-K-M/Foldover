//! `foldover-cli` — client layer. The `foldover` CLI (`pairs`,
//! `preview`, `sync`, `conflicts`, `status`). Configuration and health
//! are implemented; sync commands follow. Depends on foldover-client.
//! See PLAN.md §4.4.

use foldover_client::{LocalRoot, PairConfig, PairDraft, PairId, Side, SocketLocation, SyncMode};
use foldover_proto::local::PairRecord;
use std::env;
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::ExitCode;

/// How a usage failure exits.
const USAGE_EXIT: u8 = 2;

enum Request {
    /// `--version`: print `foldover <version>` and exit 0.
    Version,
    Status(foldover_client::SocketLocation),
    Pairs(SocketLocation, PairCommand),
    Usage,
}

enum PairCommand {
    List,
    Show(PairId),
    Add(PairDraft),
    Update {
        id: PairId,
        revision: u64,
        draft: PairDraft,
    },
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
                println!(
                    "Agent {} connected; pair configuration available (sync unavailable)",
                    status.core_version
                );
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
        Request::Usage => {
            eprintln!(
                "usage: foldover --version\n       foldover status [--socket PATH]\n       foldover pairs [--socket PATH]\n       foldover pairs show ID [--socket PATH]\n       foldover pairs add NAME ABSOLUTE_LEFT ABSOLUTE_RIGHT MODE [--socket PATH]\n       foldover pairs update ID REVISION NAME ABSOLUTE_LEFT ABSOLUTE_RIGHT MODE [--socket PATH]\nMODE: sync | mirror-left | mirror-right | backup-left | backup-right"
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
        _ => Request::Usage,
    }
}

fn pair_command(args: &[OsString]) -> Option<PairCommand> {
    match args {
        [] => Some(PairCommand::List),
        [command, id] if command == "show" => Some(PairCommand::Show(id.to_str()?.parse().ok()?)),
        [command, name, left, right, mode] if command == "add" => {
            Some(PairCommand::Add(draft(name, left, right, mode)?))
        }
        [command, id, revision, name, left, right, mode] if command == "update" => {
            let revision = revision
                .to_str()?
                .parse()
                .ok()
                .filter(|revision| (1..=i64::MAX as u64).contains(revision))?;
            Some(PairCommand::Update {
                id: id.to_str()?.parse().ok()?,
                revision,
                draft: draft(name, left, right, mode)?,
            })
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
        LocalRoot::from_bytes(left.as_bytes().to_vec()).ok()?,
        LocalRoot::from_bytes(right.as_bytes().to_vec()).ok()?,
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
            print_pair(&foldover_client::create_pair(location, &draft).await?)
        }
        PairCommand::Update {
            id,
            revision,
            draft,
        } => print_pair(&foldover_client::update_pair(location, id, revision, &draft).await?),
    }
}

fn print_pair(pair: &PairConfig) -> Result<(), foldover_client::Error> {
    // JSON lines preserve raw path bytes and escape terminal control characters.
    println!("{}", serde_json::to_string(&PairRecord::from(pair))?);
    Ok(())
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
        ] {
            assert!(matches!(req(&args), Request::Usage));
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
