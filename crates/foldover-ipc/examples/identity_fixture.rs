//! Native process fixture for scripts/test-installed-identity.py.

use std::env;
use std::io::{self, Write};
use std::path::PathBuf;

use foldover_net::local::{Connection as Channel, Listener};
use foldover_proto::local::{DAEMON_STATUS, PROTOCOL_VERSION, Request, RpcError, SESSION_HELLO};
use serde_json::{Value, json};

const PRIVATE_METHOD: &str = "fixture.private";
const READ_BUFFER_BYTES: usize = 4096;
const MARKER: &str = match option_env!("FOLDOVER_IDENTITY_FIXTURE_VARIANT") {
    Some(marker) => marker,
    None => "trusted",
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let [mode, path] = arguments.as_slice() else {
        return Err("identity_fixture MODE SOCKET".into());
    };
    let location = foldover_ipc::SocketLocation::Path(PathBuf::from(path));
    match mode.as_str() {
        "serve" => {
            let server = foldover_ipc::bind(&location)?;
            ready()?;
            foldover_ipc::serve(server, |request| async move {
                match request.method() {
                    PRIVATE_METHOD => Ok(json!({"marker": MARKER})),
                    DAEMON_STATUS => Ok(json!("public")),
                    SESSION_HELLO => {
                        Ok(json!({"protocol_version":PROTOCOL_VERSION, "core_version":"fixture"}))
                    }
                    _ => Err(RpcError::new(
                        foldover_proto::local::METHOD_NOT_FOUND,
                        "Unknown fixture method",
                    )),
                }
            })
            .await?;
        }
        "call" => {
            let mut connection = foldover_ipc::connect(&location).await?;
            if let Err(error) = connection.authenticate().await {
                println!("{}", json!({"error":error.to_string()}));
                return Ok(());
            }
            connection
                .call(Request::call(
                    1,
                    SESSION_HELLO,
                    Some(json!({"protocol_version":PROTOCOL_VERSION})),
                ))
                .await?;
            let request = Request::call(
                2,
                PRIVATE_METHOD,
                Some(json!({"private": "must-not-reach-a-fake-agent"})),
            );
            let output = match connection.call(request).await {
                Ok(result) => json!({"result": result}),
                Err(error) => json!({"error": error.to_string()}),
            };
            println!("{output}");
        }
        "raw" => {
            let mut connection = Channel::connect(&location).await?;
            let mut request = serde_json::to_vec(&json!([
                Request::call(
                    1,
                    SESSION_HELLO,
                    Some(json!({"protocol_version": PROTOCOL_VERSION}))
                )
                .to_value(),
                Request::call(2, PRIVATE_METHOD, None).to_value()
            ]))?;
            request.push(b'\n');
            connection.write_all(&request).await?;
            let mut bytes = Vec::new();
            while bytes.last() != Some(&b'\n') {
                let mut chunk = [0; READ_BUFFER_BYTES];
                let count = connection.read(&mut chunk).await?;
                if count == 0 || bytes.len() + count > foldover_ipc::MAX_FRAME_BYTES {
                    return Err("Incomplete or oversized fixture response".into());
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            let response: Value = serde_json::from_slice(&bytes)?;
            let private = response
                .as_array()
                .and_then(|batch| batch.iter().find(|reply| reply["id"] == 2))
                .ok_or("Missing private fixture response")?;
            println!("{private}");
        }
        "capture" => {
            let listener = Listener::bind(&location)?;
            ready()?;
            let mut connection = listener.accept().await?;
            let mut bytes = [0; READ_BUFFER_BYTES];
            let count = connection.read(&mut bytes).await?;
            println!("{}", json!({"bytes_received": count}));
        }
        _ => return Err("Unknown fixture mode".into()),
    }
    Ok(())
}

fn ready() -> io::Result<()> {
    println!("ready");
    io::stdout().flush()
}
