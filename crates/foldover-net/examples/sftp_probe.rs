//! Read-only request latency probe for a disposable SFTP fixture.

use std::error::Error;
use std::os::unix::ffi::OsStrExt;
use std::time::Instant;

use foldover_net::sftp::{Session, SshTarget};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err("usage: sftp_probe USER@HOST FIXTURE_FILE ROUNDS bounded|unknown".into());
    }
    let target = args[0].to_str().ok_or("target must be UTF-8")?;
    let (user, host) = target.rsplit_once('@').ok_or("specify USER@HOST")?;
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
    enum ReadMode {
        Bounded,
        Unknown,
    }
    let mode = match args[3].to_str() {
        Some("bounded") => ReadMode::Bounded,
        Some("unknown") => ReadMode::Unknown,
        _ => return Err("read mode must be bounded or unknown".into()),
    };
    let mut session = Session::connect(&SshTarget {
        host: host.into(),
        user: Some(user.into()),
        port: None,
    })?;
    println!("round,lstat_ns,open_ns,read_ns,close_ns,poststat_ns");
    for round in 0..rounds {
        let started = Instant::now();
        let before = session.lstat(path)?;
        let stat_time = started.elapsed();
        let size = before.size.ok_or("server did not return a file size")?;
        let started = Instant::now();
        let handle = session.open_read(path)?;
        let open_time = started.elapsed();
        let mut bytes = 0u64;
        let mut consume = |chunk: &[u8]| {
            bytes += chunk.len() as u64;
            Ok(())
        };
        let started = Instant::now();
        let read = match mode {
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
        let after = session.lstat(path)?;
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
            "{round},{},{},{},{},{}",
            stat_time.as_nanos(),
            open_time.as_nanos(),
            read_time.as_nanos(),
            close_time.as_nanos(),
            after_time.as_nanos()
        );
    }
    Ok(())
}
