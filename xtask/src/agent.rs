use crate::{ToolError, cargo, run, workspace_root};

pub fn smoke() -> bool {
    match check() {
        Ok(output) => {
            eprintln!("{}", output.trim());
            true
        }
        Err(error) => {
            eprintln!("agent-smoke: {error}");
            false
        }
    }
}

fn check() -> Result<String, ToolError> {
    let root = workspace_root();
    let output = run(
        &cargo(),
        &[
            "build",
            "--locked",
            "-p",
            "foldoverd",
            "-p",
            "foldover-cli",
            "--message-format=json",
        ],
        &root,
        None,
    )?;
    let mut agent = None;
    let mut cli = None;
    for line in output.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value["reason"] != "compiler-artifact" {
            continue;
        }
        let Some(path) = value["executable"].as_str() else {
            continue;
        };
        match value["target"]["name"].as_str() {
            Some("foldoverd") => agent = Some(path.to_owned()),
            Some("foldover") => cli = Some(path.to_owned()),
            _ => {}
        }
    }
    let agent =
        agent.ok_or_else(|| ToolError("Cargo reported no foldoverd executable".to_owned()))?;
    let cli = cli.ok_or_else(|| ToolError("Cargo reported no foldover executable".to_owned()))?;
    let output = run(
        "python3",
        &[
            "scripts/test-agent.py",
            &agent,
            &cli,
            "--configuration",
            if cfg!(target_os = "macos") {
                "denied"
            } else {
                "required"
            },
        ],
        &root,
        None,
    )?;
    // Linux admits a same-user fixture; macOS authenticates signed helpers.
    if cfg!(target_os = "linux") {
        let streamed = run(
            "python3",
            &["scripts/test-cli-streaming.py", &cli],
            &root,
            None,
        )?;
        return Ok(format!("{output}{streamed}"));
    }
    Ok(output)
}
