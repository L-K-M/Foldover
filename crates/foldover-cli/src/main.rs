//! `foldover-cli` — client layer. The `foldover` CLI (`pairs`,
//! `preview`, `sync`, `conflicts`, `status`). Health/status and version
//! are implemented; sync commands follow. Depends on foldover-client.
//! See PLAN.md §4.4.

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

/// How a usage failure exits.
const USAGE_EXIT: u8 = 2;

enum Request {
    /// `--version`: print `foldover <version>` and exit 0.
    Version,
    Status(foldover_client::SocketLocation),
    Usage,
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
                    "Agent {} connected; health API only (sync unavailable)",
                    status.core_version
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("foldover: {error}");
                ExitCode::FAILURE
            }
        },
        Request::Usage => {
            eprintln!("usage: foldover --version | status [--socket PATH]");
            ExitCode::from(USAGE_EXIT)
        }
    }
}

fn request(args: impl Iterator<Item = OsString>) -> Request {
    let args: Vec<OsString> = args.collect();
    match args.as_slice() {
        [arg] if arg == "--version" => Request::Version,
        [arg] if arg == "status" => Request::Status(foldover_client::SocketLocation::Default),
        [command, option, path] if command == "status" && option == "--socket" => {
            Request::Status(foldover_client::SocketLocation::Path(PathBuf::from(path)))
        }
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
    fn version_line_names_the_binary() {
        assert_eq!(
            format!("foldover {}", foldover_client::version()),
            format!("foldover {}", env!("CARGO_PKG_VERSION"))
        );
    }
}
