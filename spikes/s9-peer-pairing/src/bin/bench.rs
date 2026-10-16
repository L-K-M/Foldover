//! Throughput bench for the S9 spike: bytes per second for 1 GiB over
//! loopback QUIC on one stream and on four parallel uni streams, with
//! the same TLS 1.3 mutual-auth stack the pairing tests exercise.
//!
//! Usage: bench [--out FILE] [--bytes N]
//!
//! `--out` writes a JSON object with both measurements; the same object
//! is printed on stdout. `--bytes` overrides the transferred total
//! (default 1 GiB) for quicker local runs.

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use s9_peer_pairing::{Error, Identity, PeerNode, VerifierMode};
use tokio::task::JoinSet;

const ONE_GIB: u64 = 1 << 30;

/// Loopback-sized chunk: large enough to amortize syscalls, small
/// enough to keep flow-control granular.
const CHUNK: usize = 256 * 1024;

const FOUR_STREAMS: u32 = 4;

/// Hard deadline for one scenario: loopback at any sane rate finishes
/// in seconds; anything longer means the bench wedged.
const SCENARIO_TIMEOUT: Duration = Duration::from_secs(300);

struct Measurement {
    streams: u32,
    seconds: f64,
    bytes_per_sec: f64,
    mib_per_sec: f64,
}

impl Measurement {
    fn json(&self) -> String {
        format!(
            "{{\"streams\": {}, \"seconds\": {:.3}, \"bytes_per_sec\": {:.0}, \"mib_per_sec\": {:.1}}}",
            self.streams, self.seconds, self.bytes_per_sec, self.mib_per_sec
        )
    }
}

/// Read exactly `streams` uni streams to EOF on one accepted
/// connection, counting every byte.
async fn receive_all(receiver: PeerNode, streams: u32, expected: u64) -> Result<u64, Error> {
    let conn = receiver.accept().await?;
    let counted = Arc::new(AtomicU64::new(0));
    let mut readers = JoinSet::new();
    for _ in 0..streams {
        let mut stream = conn.accept_uni().await?;
        let counted = Arc::clone(&counted);
        readers.spawn(async move {
            let mut buf = vec![0u8; CHUNK];
            loop {
                // Ok(None) is the sender's finish(): end of stream.
                let Some(n) = stream.read(&mut buf).await? else {
                    break;
                };
                counted.fetch_add(n as u64, Ordering::Relaxed);
            }
            Ok::<(), Error>(())
        });
    }
    while let Some(done) = readers.join_next().await {
        done??;
    }
    let total = counted.load(Ordering::SeqCst);
    if total != expected {
        return Err(Error::Other(format!(
            "receiver saw {total} bytes, expected {expected}"
        )));
    }
    Ok(total)
}

/// Send `total` bytes over `streams` parallel uni streams and return
/// the measured throughput. The clock starts at the first stream open
/// and stops when the receiver has confirmed the last byte.
async fn run_scenario(total: u64, streams: u32) -> Result<Measurement, Error> {
    let sender = PeerNode::bind(Arc::new(Identity::generate()?), VerifierMode::Pairing)?;
    let receiver = PeerNode::bind(Arc::new(Identity::generate()?), VerifierMode::Pairing)?;

    let receiver_addr = receiver.addr();
    let drain = tokio::spawn(receive_all(receiver, streams, total));
    let conn = sender.connect(receiver_addr, VerifierMode::Pairing).await?;

    let per_stream = total / u64::from(streams);
    let start = Instant::now();
    let mut senders = JoinSet::new();
    for _ in 0..streams {
        let conn = conn.clone();
        senders.spawn(async move {
            let mut stream = conn.open_uni().await?;
            let chunk = vec![0u8; CHUNK];
            let mut sent = 0u64;
            while sent < per_stream {
                let take = (per_stream - sent).min(CHUNK as u64) as usize;
                stream.write_all(&chunk[..take]).await?;
                sent += take as u64;
            }
            stream.finish()?;
            Ok::<(), Error>(())
        });
    }
    while let Some(done) = senders.join_next().await {
        done??;
    }
    drain.await??;

    let seconds = start.elapsed().as_secs_f64();
    sender.close();
    Ok(Measurement {
        streams,
        seconds,
        bytes_per_sec: total as f64 / seconds,
        mib_per_sec: total as f64 / 1048576.0 / seconds,
    })
}

async fn timed_scenario(total: u64, streams: u32) -> Result<Measurement, Error> {
    match tokio::time::timeout(SCENARIO_TIMEOUT, run_scenario(total, streams)).await {
        Ok(result) => result,
        Err(_) => Err(Error::Timeout),
    }
}

fn usage() -> String {
    "usage: bench [--out FILE] [--bytes N]".to_string()
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut out_path: Option<String> = None;
    let mut total = ONE_GIB;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => match args.next() {
                Some(path) => out_path = Some(path),
                None => return Err(format!("--out needs a value; {}", usage()).into()),
            },
            "--bytes" => match args.next() {
                Some(value) => total = value.parse()?,
                None => return Err(format!("--bytes needs a value; {}", usage()).into()),
            },
            "--help" | "-h" => {
                println!("{}", usage());
                return Ok(());
            }
            other => return Err(format!("unknown argument {other}; {}", usage()).into()),
        }
    }
    if total == 0 || !total.is_multiple_of(u64::from(FOUR_STREAMS)) {
        return Err(format!("--bytes must be a positive multiple of {FOUR_STREAMS}").into());
    }

    let one = timed_scenario(total, 1).await?;
    let four = timed_scenario(total, FOUR_STREAMS).await?;

    println!(
        "loopback QUIC, TLS 1.3 mutual auth, {total} bytes, {} KiB chunks:",
        CHUNK / 1024
    );
    println!(
        "  1 stream : {:8.1} MiB/s ({:.0} bytes/s in {:.3} s)",
        one.mib_per_sec, one.bytes_per_sec, one.seconds
    );
    println!(
        "  4 streams: {:8.1} MiB/s ({:.0} bytes/s in {:.3} s)",
        four.mib_per_sec, four.bytes_per_sec, four.seconds
    );

    let json = format!(
        "{{\"total_bytes\": {total}, \"chunk_bytes\": {CHUNK}, \"one_stream\": {}, \"four_streams\": {}}}",
        one.json(),
        four.json()
    );
    if let Some(path) = out_path {
        std::fs::write(&path, format!("{json}\n"))?;
    }
    println!("{json}");
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bench: {error}");
            ExitCode::FAILURE
        }
    }
}
