//! `foldoverd` — binary layer. The composition root: connects
//! foldover-service to `foldover_ipc::serve`. Depends on
//! foldover-service and foldover-ipc. See PLAN.md §4.4.

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

/// How a usage failure exits.
const USAGE_EXIT: u8 = 2;

enum Request {
    /// `--version`: print `foldoverd <version>` and exit 0.
    Version,
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
        Request::Usage => {
            eprintln!("usage: foldoverd [--socket PATH] | --version");
            ExitCode::from(USAGE_EXIT)
        }
    }
}

async fn run(location: foldover_ipc::SocketLocation) -> Result<(), Box<dyn std::error::Error>> {
    let listener = foldover_ipc::bind(&location)?;
    let service = foldover_service::Service::open()?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = foldover_ipc::serve(listener, move |request| {
            let service = service.clone();
            async move { service.handle(request).await }
        }) => result?,
        signal = tokio::signal::ctrl_c() => signal?,
        _ = terminate.recv() => (),
    }
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
        for args in [&["--help"][..], &["run"], &["--version", "extra"], &["-V"]] {
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
