//! `foldoverd` — binary layer. The composition root: connects
//! foldover-service to `foldover_ipc::serve`. Depends on
//! foldover-service and foldover-ipc. See PLAN.md §4.4.

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

/// How a usage failure exits.
const USAGE_EXIT: u8 = 2;
const USAGE: &str = "usage: foldoverd [--socket PATH] | --version | --help";

enum Request {
    /// `--version`: print `foldoverd <version>` and exit 0.
    Version,
    /// `--help` or `-h`: print usage to stdout and exit 0.
    Help,
    Run(foldover_ipc::SocketLocation),
    Usage,
}

#[tokio::main]
async fn main() -> ExitCode {
    match request(env::args_os().skip(1)) {
        Request::Version => {
            println!("foldoverd {}", foldover_service::version());
            ExitCode::SUCCESS
        }
        Request::Run(location) => match run(location).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("foldoverd: {error}");
                ExitCode::FAILURE
            }
        },
        Request::Help => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Request::Usage => {
            eprintln!("{USAGE}");
            ExitCode::from(USAGE_EXIT)
        }
    }
}

async fn run(location: foldover_ipc::SocketLocation) -> Result<(), Box<dyn std::error::Error>> {
    // Hear a stop before starting anything slow, so one that comes while
    // the agent starts still ends cleanly instead of counting as a crash.
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let listener = foldover_ipc::bind(&location)?;
    let service = foldover_service::Service::open()?;
    service.start_scheduler();
    // Folder sync works without other Macs.
    if let Err(error) = service.start_peers().await {
        eprintln!("foldoverd: Mac-to-Mac is unavailable: {}", error.message());
    }
    let serving = service.clone();
    tokio::select! {
        result = foldover_ipc::serve(listener, move |request| {
            let service = serving.clone();
            async move { service.handle(request).await }
        }) => result?,
        _ = interrupt.recv() => (),
        _ = terminate.recv() => (),
    }
    // A stop that gets here is clean; the next start must not count it
    // toward crash-loop safe mode.
    service.note_clean_stop()?;
    Ok(())
}

fn request(args: impl Iterator<Item = OsString>) -> Request {
    let args: Vec<OsString> = args.collect();
    match args.as_slice() {
        [] => Request::Run(foldover_ipc::SocketLocation::Default),
        [arg] if arg == "--version" => Request::Version,
        [arg, path] if arg == "--socket" => {
            Request::Run(foldover_ipc::SocketLocation::Path(PathBuf::from(path)))
        }
        _ if args.iter().any(|arg| arg == "--help" || arg == "-h") => Request::Help,
        _ => Request::Usage,
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
    fn help_flags_select_help() {
        assert!(matches!(req(&["--help"]), Request::Help));
        assert!(matches!(req(&["-h"]), Request::Help));
    }

    #[test]
    fn a_help_looking_socket_path_is_still_a_value() {
        assert!(matches!(
            req(&["--socket", "-h"]),
            Request::Run(foldover_ipc::SocketLocation::Path(path)) if path.as_os_str() == "-h"
        ));
    }

    #[test]
    fn anything_else_is_usage() {
        for args in [&["run"][..], &["--version", "extra"], &["-V"]] {
            assert!(matches!(req(args), Request::Usage), "{args:?}");
        }
    }

    #[test]
    fn no_arguments_or_an_explicit_socket_start_the_agent() {
        assert!(matches!(req(&[]), Request::Run(_)));
        assert!(matches!(req(&["--socket", "/tmp/test/s"]), Request::Run(_)));
    }

    #[test]
    fn version_line_names_the_binary() {
        assert_eq!(
            format!("foldoverd {}", foldover_service::version()),
            format!("foldoverd {}", env!("CARGO_PKG_VERSION"))
        );
    }
}
