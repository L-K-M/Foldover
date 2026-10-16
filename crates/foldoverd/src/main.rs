//! `foldoverd` — binary layer. The composition root: connects
//! foldover-service to `foldover_ipc::serve`. Depends on
//! foldover-service and foldover-ipc. See PLAN.md §4.4.

use std::env;
use std::process::ExitCode;

/// How a usage failure exits.
const USAGE_EXIT: u8 = 2;

/// What the arguments ask for. Anything but `--version` alone prints
/// usage: the agent itself arrives in P3.
enum Request {
    /// `--version`: print `foldoverd <version>` and exit 0.
    Version,
    /// Any other arguments, or none: a usage line and exit 2.
    Usage,
}

fn main() -> ExitCode {
    match request(env::args().skip(1)) {
        Request::Version => {
            println!("foldoverd {}", foldover_service::version());
            ExitCode::SUCCESS
        }
        Request::Usage => {
            eprintln!("usage: foldoverd --version  (the background agent arrives in P3)");
            ExitCode::from(USAGE_EXIT)
        }
    }
}

/// Exactly `--version` selects the version; everything else is usage.
fn request(args: impl Iterator<Item = String>) -> Request {
    let args: Vec<String> = args.collect();
    match args.as_slice() {
        [arg] if arg == "--version" => Request::Version,
        _ => Request::Usage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(args: &[&str]) -> Request {
        request(args.iter().map(|a| a.to_string()))
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
            &["run"],
            &["--version", "extra"],
            &["-V"],
        ] {
            assert!(matches!(req(args), Request::Usage), "{args:?}");
        }
    }

    #[test]
    fn version_line_names_the_binary() {
        assert_eq!(
            format!("foldoverd {}", foldover_service::version()),
            format!("foldoverd {}", env!("CARGO_PKG_VERSION"))
        );
    }
}
