//! Read-only request latency probe for a disposable SFTP fixture.
//!
//! Without `--relay`, the probe uses the trusted `Session::connect` SSH path.
//! With `--relay RELAY_SCRIPT LOG_PATH`, it spawns the relay through
//! `/usr/bin/python3` with `Session::spawn` and forwards the same fixed
//! `/usr/bin/ssh` transport arguments, so the relay's JSONL log counts the
//! packets. That path is benchmark-only; `Session::connect` is unchanged.

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Instant;

use foldover_net::sftp::{Session, SshTarget};

const SSH: &str = "/usr/bin/ssh";
const PYTHON: &str = "/usr/bin/python3";
const USAGE: &str = "usage: sftp_probe USER@HOST FIXTURE_FILE ROUNDS bounded|unknown [--relay RELAY_SCRIPT LOG_PATH]";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadMode {
    Bounded,
    Unknown,
}

struct Probe {
    user: String,
    host: String,
    path: Vec<u8>,
    rounds: usize,
    mode: ReadMode,
    relay: Option<(OsString, OsString)>,
}

/// The same component check `Session::connect` applies, so the relay path
/// cannot turn a host or user into an `ssh` option.
fn valid_component(text: &str) -> bool {
    !text.is_empty() && !text.starts_with('-') && !text.contains(char::is_whitespace)
}

fn parse(args: &[OsString]) -> Result<Probe, Box<dyn Error>> {
    let relay = match args.len() {
        4 => None,
        7 if args[4].as_os_str() == OsStr::new("--relay") => {
            Some((args[5].clone(), args[6].clone()))
        }
        _ => return Err(USAGE.into()),
    };
    let target = args[0].to_str().ok_or("target must be UTF-8")?;
    let (user, host) = target.rsplit_once('@').ok_or("specify USER@HOST")?;
    if !valid_component(host) || !valid_component(user) {
        return Err("host and user must be nonempty without a leading dash or whitespace".into());
    }
    let path = args[1].as_bytes();
    if !path
        .windows(b"foldover-perf".len())
        .any(|bytes| bytes == b"foldover-perf")
    {
        return Err("use a disposable foldover-perf fixture file".into());
    }
    let rounds: usize = args[2].to_str().ok_or("rounds must be UTF-8")?.parse()?;
    if !(1..=10_000).contains(&rounds) {
        return Err("rounds must be between 1 and 10000".into());
    }
    let mode = match args[3].to_str() {
        Some("bounded") => ReadMode::Bounded,
        Some("unknown") => ReadMode::Unknown,
        _ => return Err("read mode must be bounded or unknown".into()),
    };
    if let Some((script, log)) = &relay {
        let script_bytes = script.as_bytes();
        if script_bytes.is_empty() || script_bytes.starts_with(b"-") {
            return Err("relay script must be nonempty without a leading dash".into());
        }
        if log.is_empty() {
            return Err("relay log path must be nonempty".into());
        }
    }
    Ok(Probe {
        user: user.into(),
        host: host.into(),
        path: path.to_vec(),
        rounds,
        mode,
        relay,
    })
}

/// The relay reads `--log`, then a `--`-separated server command. The
/// server command is the fixed production transport, without a port.
fn relay_args(probe: &Probe) -> Vec<&OsStr> {
    let (script, log) = probe.relay.as_ref().expect("a relay was requested");
    vec![
        script.as_os_str(),
        OsStr::new("--log"),
        log.as_os_str(),
        OsStr::new("--"),
        OsStr::new(SSH),
        OsStr::new("-o"),
        OsStr::new("BatchMode=yes"),
        OsStr::new("-o"),
        OsStr::new("ServerAliveInterval=15"),
        OsStr::new("-o"),
        OsStr::new("ServerAliveCountMax=4"),
        OsStr::new("-o"),
        OsStr::new("ConnectTimeout=60"),
        OsStr::new("-l"),
        OsStr::new(&probe.user),
        OsStr::new("-s"),
        OsStr::new("--"),
        OsStr::new(&probe.host),
        OsStr::new("sftp"),
    ]
}

fn connect(probe: &Probe) -> io::Result<Session> {
    if probe.relay.is_none() {
        return Session::connect(&SshTarget {
            host: probe.host.clone(),
            user: Some(probe.user.clone()),
            port: None,
        });
    }
    Session::spawn(Path::new(PYTHON), &relay_args(probe))
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let probe = parse(&args)?;
    let mut session = connect(&probe)?;
    println!("round,lstat_ns,open_ns,read_ns,close_ns,poststat_ns,data_bytes");
    for round in 0..probe.rounds {
        let started = Instant::now();
        let before = session.lstat(&probe.path)?;
        let stat_time = started.elapsed();
        let size = before.size.ok_or("server did not return a file size")?;
        let started = Instant::now();
        let handle = session.open_read(&probe.path)?;
        let open_time = started.elapsed();
        let mut bytes = 0u64;
        let mut consume = |chunk: &[u8]| {
            bytes += chunk.len() as u64;
            Ok(())
        };
        let started = Instant::now();
        let read = match probe.mode {
            ReadMode::Bounded => session.read_from_bounded(&handle, 0, size, &mut consume),
            ReadMode::Unknown => session.read_all(&handle, &mut consume),
        };
        let read_time = started.elapsed();
        let started = Instant::now();
        let closed = session.close(handle);
        let close_time = started.elapsed();
        read?;
        closed?;
        let started = Instant::now();
        let after = session.lstat(&probe.path)?;
        let after_time = started.elapsed();
        if bytes != size
            || before.size != after.size
            || before.owner != after.owner
            || before.permissions != after.permissions
            || before.modified() != after.modified()
        {
            return Err("fixture changed while reading".into());
        }
        println!(
            "{round},{},{},{},{},{},{}",
            stat_time.as_nanos(),
            open_time.as_nanos(),
            read_time.as_nanos(),
            close_time.as_nanos(),
            after_time.as_nanos(),
            bytes,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(parts: &[&str]) -> Result<Probe, Box<dyn Error>> {
        let args: Vec<OsString> = parts.iter().map(OsString::from).collect();
        parse(&args)
    }

    const FIXTURE: &str = "/tmp/foldover-perf/fixture";

    #[test]
    fn keeps_the_existing_four_argument_invocation() {
        let probe = parse_args(&["user@host", FIXTURE, "3", "bounded"]).unwrap();
        assert_eq!(probe.user, "user");
        assert_eq!(probe.host, "host");
        assert_eq!(probe.path, FIXTURE.as_bytes());
        assert_eq!(probe.rounds, 3);
        assert_eq!(probe.mode, ReadMode::Bounded);
        assert!(probe.relay.is_none());
    }

    #[test]
    fn accepts_the_relay_triplet_and_unknown_mode() {
        let probe = parse_args(&[
            "user@host",
            FIXTURE,
            "1",
            "unknown",
            "--relay",
            "perf/sftp-relay.py",
            "events.jsonl",
        ])
        .unwrap();
        assert_eq!(probe.mode, ReadMode::Unknown);
        let (script, log) = probe.relay.expect("relay arguments");
        assert_eq!(script, OsString::from("perf/sftp-relay.py"));
        assert_eq!(log, OsString::from("events.jsonl"));
    }

    #[test]
    fn relay_forwards_the_fixed_ssh_transport() {
        let probe = parse_args(&[
            "user@host",
            FIXTURE,
            "1",
            "bounded",
            "--relay",
            "relay.py",
            "events.jsonl",
        ])
        .unwrap();
        let args: Vec<_> = relay_args(&probe)
            .into_iter()
            .map(|arg| arg.to_str().expect("UTF-8 argument"))
            .collect();
        assert_eq!(
            args,
            [
                "relay.py",
                "--log",
                "events.jsonl",
                "--",
                "/usr/bin/ssh",
                "-o",
                "BatchMode=yes",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=4",
                "-o",
                "ConnectTimeout=60",
                "-l",
                "user",
                "-s",
                "--",
                "host",
                "sftp",
            ]
        );
    }

    #[test]
    fn rejects_a_host_or_user_that_could_inject_ssh_options() {
        for target in [
            "-oProxyCommand=evil@host",
            "user@-oProxyCommand=evil",
            "user@host name",
            "user name@host",
            "user@host\n",
            "@host",
            "user@",
        ] {
            assert!(
                parse_args(&[target, FIXTURE, "1", "bounded"]).is_err(),
                "{target:?} must be rejected"
            );
        }
    }

    #[test]
    fn rejects_bad_rounds_modes_and_fixtures() {
        assert!(parse_args(&["user@host", FIXTURE, "0", "bounded"]).is_err());
        assert!(parse_args(&["user@host", FIXTURE, "10001", "bounded"]).is_err());
        assert!(parse_args(&["user@host", FIXTURE, "one", "bounded"]).is_err());
        assert!(parse_args(&["user@host", FIXTURE, "1", "sometimes"]).is_err());
        assert!(parse_args(&["user@host", "/tmp/other/fixture", "1", "bounded"]).is_err());
        assert!(parse_args(&["host", FIXTURE, "1", "bounded"]).is_err());
    }

    #[test]
    fn rejects_malformed_relay_arguments() {
        assert!(parse_args(&["user@host", FIXTURE, "1", "bounded"]).is_ok());
        assert!(parse_args(&["user@host", FIXTURE, "1", "bounded", "--relay"]).is_err());
        assert!(
            parse_args(&["user@host", FIXTURE, "1", "bounded", "--relay", "relay.py"]).is_err()
        );
        assert!(
            parse_args(&[
                "--relay",
                "relay.py",
                "events.jsonl",
                "user@host",
                FIXTURE,
                "1",
                "bounded"
            ])
            .is_err()
        );
        assert!(
            parse_args(&[
                "user@host",
                FIXTURE,
                "1",
                "bounded",
                "--relay",
                "",
                "events.jsonl"
            ])
            .is_err()
        );
        assert!(
            parse_args(&[
                "user@host",
                FIXTURE,
                "1",
                "bounded",
                "--relay",
                "-x.py",
                "events.jsonl"
            ])
            .is_err()
        );
        assert!(
            parse_args(&[
                "user@host",
                FIXTURE,
                "1",
                "bounded",
                "--relay",
                "relay.py",
                ""
            ])
            .is_err()
        );
    }
}
